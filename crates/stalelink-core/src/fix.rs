use std::{
    io::{Cursor, Read, Write},
    ops::Range,
};

use crate::{
    extract::{SourceDocument, encode_pdf_string, extract},
    model::{DocFormat, Finding},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub span: Range<usize>,
    pub replacement: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixError(pub String);

/// Transforms a document's bytes using the supplied scanned findings.
pub trait Fixer {
    fn fix(&self, original: &[u8], findings: &[Finding]) -> Result<Vec<u8>, FixError>;
}

pub struct TextFixer;

impl Fixer for TextFixer {
    fn fix(&self, original: &[u8], findings: &[Finding]) -> Result<Vec<u8>, FixError> {
        let edits = findings
            .iter()
            .map(|finding| {
                let span =
                    finding.source.byte_span.clone().ok_or_else(|| {
                        FixError(format!("{} has no source byte span", finding.url))
                    })?;
                let fix = finding
                    .fix
                    .as_ref()
                    .ok_or_else(|| FixError(format!("{} has no suggested fix", finding.url)))?;
                let start = usize::try_from(span.start)
                    .map_err(|_| FixError("source byte span exceeds platform size".into()))?;
                let end = usize::try_from(span.end)
                    .map_err(|_| FixError("source byte span exceeds platform size".into()))?;
                // Markdown escapes make the raw bytes differ from the semantic URL.
                // Refuse them rather than silently changing the source representation.
                if original.get(start..end) != Some(finding.url.as_bytes()) {
                    return Err(FixError(format!(
                        "{} source bytes are not the semantic URL",
                        finding.url
                    )));
                }
                Ok(Edit {
                    span: start..end,
                    replacement: fix.replacement_url.as_bytes().to_vec(),
                })
            })
            .collect::<Result<Vec<_>, FixError>>()?;
        apply_edits(original, &edits)
    }
}

pub struct HtmlFixer;

pub struct OoxmlFixer;
pub struct PdfFixer;

impl Fixer for OoxmlFixer {
    fn fix(&self, original: &[u8], findings: &[Finding]) -> Result<Vec<u8>, FixError> {
        let replacements = replacements(findings)?;
        let mut source = zip::ZipArchive::new(Cursor::new(original))
            .map_err(|error| FixError(format!("reading OOXML archive: {error}")))?;
        let mut changed = vec![false; replacements.len()];
        let mut touched = std::collections::HashMap::new();
        for index in 0..source.len() {
            let mut entry = source
                .by_index(index)
                .map_err(|error| FixError(format!("reading OOXML entry: {error}")))?;
            let name = entry.name().to_owned();
            if !is_ooxml_link_part(&name) {
                continue;
            }
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .map_err(|error| FixError(format!("reading OOXML entry {name}: {error}")))?;
            let fixed = replace_urls(&name, &bytes, &replacements, &mut changed);
            if fixed != bytes {
                touched.insert(name, fixed);
            }
        }
        reject_unpreservable_untouched_metadata(original, &touched)?;
        for (index, (old, _)) in replacements.iter().enumerate() {
            if !changed[index] {
                return Err(FixError(format!(
                    "{old} was not found in an OOXML link part"
                )));
            }
        }
        let mut source = zip::ZipArchive::new(Cursor::new(original))
            .map_err(|error| FixError(format!("reading OOXML archive: {error}")))?;
        let mut output = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for index in 0..source.len() {
            let entry = source
                .by_index(index)
                .map_err(|error| FixError(format!("reading OOXML entry: {error}")))?;
            let name = entry.name().to_owned();
            if let Some(fixed) = touched.get(&name) {
                output
                    .start_file(name, zip::write::SimpleFileOptions::default())
                    .map_err(|error| FixError(format!("writing OOXML entry: {error}")))?;
                output
                    .write_all(fixed)
                    .map_err(|error| FixError(format!("writing OOXML entry: {error}")))?;
            } else {
                // Raw-copy preserves the original member's compressed bytes and metadata.
                output
                    .raw_copy_file(entry)
                    .map_err(|error| FixError(format!("copying OOXML entry {name}: {error}")))?;
            }
        }
        output
            .finish()
            .map(|cursor| cursor.into_inner())
            .map_err(|error| FixError(format!("finishing OOXML archive: {error}")))
    }
}

impl Fixer for PdfFixer {
    fn fix(&self, original: &[u8], findings: &[Finding]) -> Result<Vec<u8>, FixError> {
        let document = lopdf::Document::load_mem(original)
            .map_err(|error| FixError(format!("reading PDF: {error}")))?;
        pdf_refusal(&document)?;
        if findings.iter().any(|finding| {
            matches!(
                finding.source.location,
                crate::model::Location::Pdf {
                    annotation: None,
                    ..
                }
            )
        }) {
            return Err(FixError("bare PDF text URLs require manual editing".into()));
        }
        let annotation_ids = document
            .objects
            .iter()
            .filter_map(|(id, object)| {
                let dict = object.as_dict().ok()?;
                (dict.get(b"Subtype").ok()?.as_name().ok()? == b"Link").then_some(*id)
            })
            .collect::<Vec<_>>();
        let replacements = replacements(findings)?;
        let mut incremental = lopdf::IncrementalDocument::create_from(original.to_vec(), document);
        let mut changed = vec![false; replacements.len()];
        for annotation_id in annotation_ids {
            incremental
                .opt_clone_object_to_new_document(annotation_id)
                .map_err(|error| FixError(format!("cloning PDF annotation: {error}")))?;
            let action_id = incremental
                .new_document
                .get_object(annotation_id)
                .and_then(lopdf::Object::as_dict)
                .ok()
                .and_then(|annotation| annotation.get(b"A").ok())
                .and_then(|action| action.as_reference().ok());
            if let Some(action_id) = action_id {
                incremental
                    .opt_clone_object_to_new_document(action_id)
                    .map_err(|error| FixError(format!("cloning PDF action: {error}")))?;
            }
            let action = if let Some(action_id) = action_id {
                incremental
                    .new_document
                    .get_object_mut(action_id)
                    .and_then(lopdf::Object::as_dict_mut)
            } else {
                incremental
                    .new_document
                    .get_object_mut(annotation_id)
                    .and_then(lopdf::Object::as_dict_mut)
                    .and_then(|annotation| annotation.get_mut(b"A"))
                    .and_then(lopdf::Object::as_dict_mut)
            }
            .map_err(|error| FixError(format!("reading PDF action: {error}")))?;
            if !action
                .get(b"S")
                .and_then(lopdf::Object::as_name)
                .is_ok_and(|name| name == b"URI")
            {
                continue;
            }
            let Ok(uri) = action.get_mut(b"URI").and_then(lopdf::Object::as_str_mut) else {
                continue;
            };
            for (index, (old, replacement)) in replacements.iter().enumerate() {
                if uri.as_slice() == old.as_bytes() {
                    *uri = encode_pdf_string(replacement);
                    changed[index] = true;
                }
            }
        }
        for (index, (old, _)) in replacements.iter().enumerate() {
            if !changed[index] {
                return Err(FixError(format!(
                    "{old} was not found in a PDF annotation URI"
                )));
            }
        }
        let mut fixed = Vec::new();
        incremental
            .save_to(&mut fixed)
            .map_err(|error| FixError(format!("writing incremental PDF: {error}")))?;
        Ok(fixed)
    }
}

fn replacements(findings: &[Finding]) -> Result<Vec<(String, String)>, FixError> {
    let mut replacements = Vec::new();
    for finding in findings {
        let replacement = finding
            .fix
            .as_ref()
            .ok_or_else(|| FixError(format!("{} has no suggested fix", finding.url)))?
            .replacement_url
            .clone();
        if let Some((_, existing)) = replacements.iter().find(|(old, _)| old == &finding.url) {
            if existing != &replacement {
                return Err(FixError(format!(
                    "conflicting replacements for {}: {existing} and {replacement}",
                    finding.url
                )));
            }
        } else {
            replacements.push((finding.url.clone(), replacement));
        }
    }
    Ok(replacements)
}

fn is_ooxml_link_part(name: &str) -> bool {
    name.ends_with(".rels") || name.ends_with(".xml")
}

fn replace_urls(
    name: &str,
    bytes: &[u8],
    replacements: &[(String, String)],
    changed: &mut [bool],
) -> Vec<u8> {
    let mut edits = Vec::new();
    for (range, value, field_code) in xml_values(name, bytes) {
        for (index, (old, replacement)) in replacements.iter().enumerate() {
            if (name.ends_with(".rels") || field_code)
                && let Some(range) = xml_subrange(bytes, &range, &value, old)
            {
                edits.push(Edit {
                    span: range,
                    replacement: xml_escape(replacement).into_bytes(),
                });
                changed[index] = true;
            }
        }
    }
    apply_edits(bytes, &edits).unwrap_or_else(|_| bytes.to_vec())
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn xml_values(name: &str, bytes: &[u8]) -> Vec<(Range<usize>, String, bool)> {
    let text = String::from_utf8_lossy(bytes);
    let mut values = Vec::new();
    if name.ends_with(".rels") {
        let mut position = 0;
        while let Some(offset) = text[position..].find("Target=") {
            let quote = position + offset + "Target=".len();
            let Some(&delimiter) = bytes.get(quote) else {
                break;
            };
            if !matches!(delimiter, b'\'' | b'\"') {
                position = quote;
                continue;
            }
            let start = quote + 1;
            let Some(end) = bytes[start..].iter().position(|&byte| byte == delimiter) else {
                break;
            };
            let end = start + end;
            values.push((start..end, xml_unescape(&text[start..end]), false));
            position = end + 1;
        }
        return values;
    }

    let mut position = 0;
    while let Some(tag_offset) = text[position..].find('<') {
        let tag_start = position + tag_offset;
        let Some(tag_end) = text[tag_start..].find('>') else {
            break;
        };
        let tag_end = tag_start + tag_end;
        let tag = &text[tag_start..=tag_end];
        let instruction = tag.contains("instrText") || tag.contains("fldSimple");
        if instruction {
            if tag.contains("fldSimple") {
                if let Some((range, value)) = attribute_value(bytes, tag_start, tag_end, b"instr") {
                    values.push((range, value, true));
                }
            } else if !tag.starts_with("</") {
                let content_start = tag_end + 1;
                if let Some(close) = text[content_start..].find("</") {
                    let content_end = content_start + close;
                    values.push((
                        content_start..content_end,
                        xml_unescape(&text[content_start..content_end]),
                        true,
                    ));
                }
            }
        }
        position = tag_end + 1;
    }
    values
}

fn attribute_value(
    bytes: &[u8],
    tag_start: usize,
    tag_end: usize,
    name: &[u8],
) -> Option<(Range<usize>, String)> {
    let tag = &bytes[tag_start..=tag_end];
    let offset = tag.windows(name.len() + 2).position(|window| {
        window[..name.len()] == *name
            && window[name.len()] == b'='
            && matches!(window[name.len() + 1], b'\'' | b'\"')
    })?;
    let start = tag_start + offset + name.len() + 2;
    let delimiter = bytes[start - 1];
    let end = start
        + bytes[start..tag_end]
            .iter()
            .position(|&byte| byte == delimiter)?;
    Some((
        start..end,
        xml_unescape(&String::from_utf8_lossy(&bytes[start..end])),
    ))
}

fn xml_unescape(value: &str) -> String {
    html_escape::decode_html_entities(value).to_string()
}

fn xml_subrange(
    bytes: &[u8],
    range: &Range<usize>,
    value: &str,
    old: &str,
) -> Option<Range<usize>> {
    let semantic_start = value.find(old)?;
    let semantic_end = semantic_start + old.len();
    let raw = std::str::from_utf8(&bytes[range.clone()]).ok()?;
    let mut semantic = 0;
    let mut start = None;
    let mut end = None;
    let mut raw_position = 0;
    while raw_position < raw.len() {
        if semantic == semantic_start {
            start = Some(raw_position);
        }
        if semantic == semantic_end {
            end = Some(raw_position);
            break;
        }
        let next = if raw.as_bytes()[raw_position] == b'&' {
            raw[raw_position..]
                .find(';')
                .map(|offset| raw_position + offset + 1)?
        } else {
            raw_position + raw[raw_position..].chars().next()?.len_utf8()
        };
        semantic += xml_unescape(&raw[raw_position..next]).len();
        raw_position = next;
    }
    if semantic == semantic_end {
        end.get_or_insert(raw_position);
    }
    Some(range.start + start?..range.start + end?)
}

fn reject_unpreservable_untouched_metadata(
    original: &[u8],
    touched: &std::collections::HashMap<String, Vec<u8>>,
) -> Result<(), FixError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(original))
        .map_err(|error| FixError(format!("reading OOXML archive: {error}")))?;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| FixError(error.to_string()))?;
        if !touched.contains_key(entry.name())
            && entry.extra_data().is_some_and(|extra| !extra.is_empty())
        {
            return Err(FixError(format!(
                "untouched OOXML entry {} has ZIP extra metadata that cannot be preserved",
                entry.name()
            )));
        }
    }
    let mut position = 0;
    while let Some(offset) = original[position..]
        .windows(4)
        .position(|value| value == b"PK\x01\x02")
    {
        let header = position + offset;
        let Some(lengths) = original.get(header + 28..header + 34) else {
            break;
        };
        let name_length = u16::from_le_bytes([lengths[0], lengths[1]]) as usize;
        let extra_length = u16::from_le_bytes([lengths[2], lengths[3]]) as usize;
        let name_start = header + 46;
        let Some(name) = original.get(name_start..name_start + name_length) else {
            break;
        };
        let name = String::from_utf8_lossy(name);
        if !touched.contains_key(name.as_ref()) && extra_length != 0 {
            return Err(FixError(format!(
                "untouched OOXML entry {name} has central ZIP extra metadata that cannot be preserved"
            )));
        }
        position = name_start + name_length + extra_length;
    }
    Ok(())
}

pub fn pdf_refusal(document: &lopdf::Document) -> Result<(), FixError> {
    if document.is_encrypted() {
        return Err(FixError("encrypted PDF files are not modified".into()));
    }
    if signed_pdf(document) {
        return Err(FixError("signed PDF files are not modified".into()));
    }
    if direct_link_annotation(document) {
        return Err(FixError(
            "PDF files with direct link annotations are not modified".into(),
        ));
    }
    Ok(())
}

fn direct_link_annotation(document: &lopdf::Document) -> bool {
    document.get_pages().into_values().any(|page_id| {
        document
            .get_dictionary(page_id)
            .ok()
            .and_then(|page| page.get(b"Annots").ok())
            .and_then(|annotations| annotations.as_array().ok())
            .is_some_and(|annotations| {
                annotations.iter().any(|annotation| {
                    annotation
                        .as_dict()
                        .ok()
                        .and_then(|annotation| annotation.get(b"Subtype").ok())
                        .and_then(|subtype| subtype.as_name().ok())
                        .is_some_and(|subtype| subtype == b"Link")
                })
            })
    })
}

fn signed_pdf(document: &lopdf::Document) -> bool {
    let Ok((_, root)) = document
        .trailer
        .get(b"Root")
        .and_then(|root| document.dereference(root))
    else {
        return false;
    };
    let Ok(root) = root.as_dict() else {
        return false;
    };
    if root.has(b"Perms") {
        return true;
    }
    root.get(b"AcroForm")
        .and_then(|form| document.dereference(form))
        .and_then(|(_, form)| form.as_dict())
        .ok()
        .is_some_and(|form| signature_field(document, form.get(b"Fields").ok()))
}

fn signature_field(document: &lopdf::Document, field: Option<&lopdf::Object>) -> bool {
    let Some(field) = field else { return false };
    match field {
        lopdf::Object::Array(fields) => fields
            .iter()
            .any(|field| signature_field(document, Some(field))),
        field => {
            let Ok((_, field)) = document.dereference(field) else {
                return false;
            };
            let Ok(field) = field.as_dict() else {
                return false;
            };
            field
                .get(b"FT")
                .and_then(lopdf::Object::as_name)
                .is_ok_and(|field_type| field_type == b"Sig")
                || signature_field(document, field.get(b"Kids").ok())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttributeSyntax {
    DoubleQuoted,
    SingleQuoted,
    Unquoted,
}

impl Fixer for HtmlFixer {
    fn fix(&self, original: &[u8], findings: &[Finding]) -> Result<Vec<u8>, FixError> {
        let edits = findings
            .iter()
            .map(|finding| {
                let span =
                    finding.source.byte_span.clone().ok_or_else(|| {
                        FixError(format!("{} has no source byte span", finding.url))
                    })?;
                let fix = finding
                    .fix
                    .as_ref()
                    .ok_or_else(|| FixError(format!("{} has no suggested fix", finding.url)))?;
                let start = usize::try_from(span.start)
                    .map_err(|_| FixError("source byte span exceeds platform size".into()))?;
                let end = usize::try_from(span.end)
                    .map_err(|_| FixError("source byte span exceeds platform size".into()))?;
                let raw = original.get(start..end).ok_or_else(|| {
                    FixError(format!(
                        "{} source byte span is outside document",
                        finding.url
                    ))
                })?;
                if decode_html_attribute(raw) != finding.url {
                    return Err(FixError(format!(
                        "{} source bytes do not decode to the semantic URL",
                        finding.url
                    )));
                }
                let syntax = attribute_syntax(original, start);
                Ok(Edit {
                    span: start..end,
                    replacement: encode_html_attribute(&fix.replacement_url, syntax),
                })
            })
            .collect::<Result<Vec<_>, FixError>>()?;
        let fixed = apply_edits(original, &edits)?;
        let links = extract(&SourceDocument {
            path: Default::default(),
            format: DocFormat::Html,
            bytes: fixed.clone(),
        })
        .map_err(|error| FixError(format!("re-extracting fixed HTML: {}", error.0)))?;
        for finding in findings {
            let replacement = &finding
                .fix
                .as_ref()
                .expect("validated before applying HTML edits")
                .replacement_url;
            if !links.iter().any(|link| link.url == *replacement) {
                return Err(FixError(format!(
                    "replacement URL was not extractable: {replacement}"
                )));
            }
        }
        Ok(fixed)
    }
}

fn attribute_syntax(original: &[u8], start: usize) -> AttributeSyntax {
    let tag_start = original[..start]
        .iter()
        .rposition(|&byte| byte == b'<')
        .map_or(0, |index| index + 1);
    let attribute_start = original[tag_start..start]
        .iter()
        .rposition(|&byte| byte == b'=')
        .map_or(start, |index| tag_start + index + 1);
    match original[attribute_start..start]
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
    {
        Some(b'"') => AttributeSyntax::DoubleQuoted,
        Some(b'\'') => AttributeSyntax::SingleQuoted,
        _ => AttributeSyntax::Unquoted,
    }
}

fn decode_html_attribute(raw: &[u8]) -> String {
    let raw = String::from_utf8_lossy(raw);
    let mut normalized = String::with_capacity(raw.len());
    let mut position = 0;
    while let Some(offset) = raw[position..].find("&#") {
        let start = position + offset;
        normalized.push_str(&raw[position..start]);

        let numeric_start = start + 2;
        let (radix, digits_start) = match raw.as_bytes().get(numeric_start) {
            Some(b'x' | b'X') => (16, numeric_start + 1),
            _ => (10, numeric_start),
        };
        let digits_end = raw[digits_start..]
            .find(|character: char| !character.is_digit(radix))
            .map_or(raw.len(), |offset| digits_start + offset);
        let numeric = &raw[digits_start..digits_end];
        let reference_end = if raw.as_bytes().get(digits_end) == Some(&b';') {
            digits_end + 1
        } else {
            digits_end
        };

        if let Ok(value) = u32::from_str_radix(numeric, radix)
            && let Some(character) = html_c1_numeric_reference_override(value)
        {
            normalized.push(character);
        } else {
            normalized.push_str(&raw[start..reference_end]);
        }
        position = reference_end;
    }
    normalized.push_str(&raw[position..]);
    html_escape::decode_html_entities(&normalized).to_string()
}

fn html_c1_numeric_reference_override(value: u32) -> Option<char> {
    Some(match value {
        0x80 => '\u{20AC}',
        0x81 => '\u{0081}',
        0x82 => '\u{201A}',
        0x83 => '\u{0192}',
        0x84 => '\u{201E}',
        0x85 => '\u{2026}',
        0x86 => '\u{2020}',
        0x87 => '\u{2021}',
        0x88 => '\u{02C6}',
        0x89 => '\u{2030}',
        0x8A => '\u{0160}',
        0x8B => '\u{2039}',
        0x8C => '\u{0152}',
        0x8D => '\u{008D}',
        0x8E => '\u{017D}',
        0x8F => '\u{008F}',
        0x90 => '\u{0090}',
        0x91 => '\u{2018}',
        0x92 => '\u{2019}',
        0x93 => '\u{201C}',
        0x94 => '\u{201D}',
        0x95 => '\u{2022}',
        0x96 => '\u{2013}',
        0x97 => '\u{2014}',
        0x98 => '\u{02DC}',
        0x99 => '\u{2122}',
        0x9A => '\u{0161}',
        0x9B => '\u{203A}',
        0x9C => '\u{0153}',
        0x9D => '\u{009D}',
        0x9E => '\u{017E}',
        0x9F => '\u{0178}',
        _ => return None,
    })
}

fn encode_html_attribute(value: &str, syntax: AttributeSyntax) -> Vec<u8> {
    let mut encoded = String::with_capacity(value.len());
    for character in value.chars() {
        let entity = match character {
            '&' => Some("&amp;"),
            '"' if syntax != AttributeSyntax::SingleQuoted => Some("&quot;"),
            '\'' if syntax != AttributeSyntax::DoubleQuoted => Some("&apos;"),
            '<' if syntax == AttributeSyntax::Unquoted => Some("&lt;"),
            '>' if syntax == AttributeSyntax::Unquoted => Some("&gt;"),
            '=' if syntax == AttributeSyntax::Unquoted => Some("&#61;"),
            '`' if syntax == AttributeSyntax::Unquoted => Some("&#96;"),
            character if syntax == AttributeSyntax::Unquoted && character.is_ascii_whitespace() => {
                Some(match character {
                    '\t' => "&#9;",
                    '\n' => "&#10;",
                    '\r' => "&#13;",
                    '\x0C' => "&#12;",
                    ' ' => "&#32;",
                    _ => unreachable!("all ASCII whitespace is covered"),
                })
            }
            _ => None,
        };
        if let Some(entity) = entity {
            encoded.push_str(entity);
        } else {
            encoded.push(character);
        }
    }
    encoded.into_bytes()
}

pub fn fixer_for(format: DocFormat) -> Option<&'static dyn Fixer> {
    static TEXT: TextFixer = TextFixer;
    static HTML: HtmlFixer = HtmlFixer;
    static OOXML: OoxmlFixer = OoxmlFixer;
    static PDF: PdfFixer = PdfFixer;
    match format {
        DocFormat::Markdown | DocFormat::Text => Some(&TEXT),
        DocFormat::Html => Some(&HTML),
        DocFormat::Pdf => Some(&PDF),
        DocFormat::Docx | DocFormat::Xlsx | DocFormat::Pptx => Some(&OOXML),
    }
}

pub fn apply_edits(original: &[u8], edits: &[Edit]) -> Result<Vec<u8>, FixError> {
    let mut ordered = edits.to_vec();
    ordered.sort_by_key(|edit| std::cmp::Reverse(edit.span.start));
    for edit in &ordered {
        if edit.span.start > edit.span.end || edit.span.end > original.len() {
            return Err(FixError(format!(
                "edit span {}..{} is outside document length {}",
                edit.span.start,
                edit.span.end,
                original.len()
            )));
        }
    }
    for pair in ordered.windows(2) {
        let later = &pair[0].span;
        let earlier = &pair[1].span;
        if earlier.end > later.start
            || (earlier.is_empty() && later.is_empty() && earlier.start == later.start)
        {
            return Err(FixError(format!(
                "overlapping edits at {}..{} and {}..{}",
                earlier.start, earlier.end, later.start, later.end
            )));
        }
    }
    let mut fixed = original.to_vec();
    for edit in ordered {
        fixed.splice(edit.span, edit.replacement);
    }
    Ok(fixed)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::Utc;
    use proptest::prelude::*;

    use super::*;
    use crate::model::{
        Confidence, FixOrigin, Fixability, Location, Reason, SourceRef, SuggestedFix, Verdict,
    };

    fn html_finding(original: &str, url: &str, replacement: &str) -> Finding {
        let start = original.find(url).unwrap();
        Finding {
            url: url.into(),
            resolved_url: None,
            source: SourceRef {
                path: PathBuf::new(),
                format: DocFormat::Html,
                location: Location::Text { line: 1, column: 1 },
                byte_span: Some(start as u64..(start + url.len()) as u64),
            },
            verdict: Verdict {
                confidence: Confidence::Outdated,
                reason: Reason::PermanentRedirect,
                evidence: vec![],
                checked_at: Utc::now(),
                tier: 1,
            },
            fix: Some(SuggestedFix {
                replacement_url: replacement.into(),
                origin: FixOrigin::RedirectTarget,
                fixable: Fixability::Auto,
            }),
        }
    }

    fn binary_finding(
        format: DocFormat,
        url: &str,
        replacement: &str,
        location: Location,
    ) -> Finding {
        Finding {
            url: url.into(),
            resolved_url: None,
            source: SourceRef {
                path: PathBuf::new(),
                format,
                location,
                byte_span: None,
            },
            verdict: Verdict {
                confidence: Confidence::Outdated,
                reason: Reason::PermanentRedirect,
                evidence: vec![],
                checked_at: Utc::now(),
                tier: 1,
            },
            fix: Some(SuggestedFix {
                replacement_url: replacement.into(),
                origin: FixOrigin::RedirectTarget,
                fixable: Fixability::Auto,
            }),
        }
    }

    fn archive(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, contents) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn pdf() -> Vec<u8> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Annots [4 0 R] >>",
            "<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] /A << /S /URI /URI (https://old.test/x) >> >>",
        ];
        let mut output = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0];
        for (index, object) in objects.iter().enumerate() {
            offsets.push(output.len());
            output.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = output.len();
        output.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets.iter().skip(1) {
            output.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        output.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        output
    }

    fn reference(original: &[u8], edits: &[Edit]) -> Vec<u8> {
        let mut ordered = edits.to_vec();
        ordered.sort_by_key(|edit| edit.span.start);
        let mut output = Vec::new();
        let mut position = 0;
        for edit in ordered {
            output.extend_from_slice(&original[position..edit.span.start]);
            output.extend_from_slice(&edit.replacement);
            position = edit.span.end;
        }
        output.extend_from_slice(&original[position..]);
        output
    }

    proptest! {
        #[test]
        fn back_to_front_matches_reference(
            original in proptest::collection::vec(any::<u8>(), 0..100),
            specs in proptest::collection::vec((0usize..20, 0usize..20, proptest::collection::vec(any::<u8>(), 0..20)), 0..15),
        ) {
            let mut cursor = 0usize;
            let mut edits = Vec::new();
            for (gap, width, replacement) in specs {
                if cursor > original.len() { break; }
                let remaining = original.len() - cursor;
                let start = cursor + gap.min(remaining);
                let width = width.min(original.len() - start);
                if edits.last().is_some_and(|edit: &Edit| edit.span.is_empty() && edit.span.start == start && width == 0) {
                    continue;
                }
                edits.push(Edit { span: start..start + width, replacement });
                cursor = if width == 0 {
                    start.saturating_add(1)
                } else {
                    start + width
                };
            }
            edits.reverse();
            prop_assert_eq!(apply_edits(&original, &edits).unwrap(), reference(&original, &edits));
        }
    }

    #[test]
    fn rejects_overlapping_edits() {
        let error = apply_edits(
            b"abcdef",
            &[
                Edit {
                    span: 1..4,
                    replacement: b"x".to_vec(),
                },
                Edit {
                    span: 3..5,
                    replacement: b"y".to_vec(),
                },
            ],
        )
        .unwrap_err();
        assert!(error.0.contains("overlapping"));
    }

    #[test]
    fn accepts_adjacent_edits_and_a_single_insertion() {
        assert_eq!(
            apply_edits(
                b"abcd",
                &[
                    Edit {
                        span: 1..2,
                        replacement: b"X".to_vec()
                    },
                    Edit {
                        span: 2..3,
                        replacement: b"Y".to_vec()
                    },
                    Edit {
                        span: 4..4,
                        replacement: b"!".to_vec()
                    },
                ],
            )
            .unwrap(),
            b"aXYd!"
        );
    }

    #[test]
    fn rejects_duplicate_insertions_reversed_and_out_of_bounds_spans() {
        for edits in [
            vec![
                Edit {
                    span: 1..1,
                    replacement: vec![],
                },
                Edit {
                    span: 1..1,
                    replacement: vec![],
                },
            ],
            vec![Edit {
                span: Range { start: 3, end: 2 },
                replacement: vec![],
            }],
            vec![Edit {
                span: 0..5,
                replacement: vec![],
            }],
        ] {
            assert!(apply_edits(b"abcd", &edits).is_err());
        }
    }

    #[test]
    fn html_quoted_replacements_preserve_semantic_urls() {
        for original in [
            "<a href=\"http://old.test/old\">x</a>",
            "<a href='http://old.test/old'>x</a>",
        ] {
            let replacement = "http://new.test/new?x=1&quote=\"double\"&single='single'";
            let finding = html_finding(original, "http://old.test/old", replacement);
            let fixed = HtmlFixer.fix(original.as_bytes(), &[finding]).unwrap();
            let links = extract(&SourceDocument {
                path: PathBuf::new(),
                format: DocFormat::Html,
                bytes: fixed,
            })
            .unwrap();
            assert_eq!(links[0].url, replacement);
        }
    }

    #[test]
    fn html_unquoted_replacements_encode_forbidden_characters() {
        for forbidden in [' ', '\t', '\n', '\r', '\x0C', '<', '>', '=', '`'] {
            let original = "<a href=http://old.test/old>x</a>";
            let replacement = format!("http://new.test/new{forbidden}tail");
            let finding = html_finding(original, "http://old.test/old", &replacement);
            let fixed = HtmlFixer.fix(original.as_bytes(), &[finding]).unwrap();
            let links = extract(&SourceDocument {
                path: PathBuf::new(),
                format: DocFormat::Html,
                bytes: fixed,
            })
            .unwrap();
            assert_eq!(links[0].url, replacement, "failed for {forbidden:?}");
        }
    }

    #[test]
    fn html_raw_attribute_validation_decodes_named_entities() {
        let original = "<a href=http&colon;&sol;&sol;old.test&sol;old>x</a>";
        let finding = html_finding(
            original,
            "http&colon;&sol;&sol;old.test&sol;old",
            "http://new.test/new",
        );
        let finding = Finding {
            url: "http://old.test/old".into(),
            ..finding
        };
        let fixed = HtmlFixer.fix(original.as_bytes(), &[finding]).unwrap();
        assert_eq!(
            String::from_utf8(fixed).unwrap(),
            "<a href=http://new.test/new>x</a>"
        );
    }

    #[test]
    fn html_raw_attribute_validation_decodes_c1_numeric_entities() {
        let original = "<a href='http://old.test/old?currency=&#128;'>x</a>";
        let finding = html_finding(
            original,
            "http://old.test/old?currency=&#128;",
            "http://new.test/new",
        );
        let finding = Finding {
            url: "http://old.test/old?currency=€".into(),
            ..finding
        };
        let fixed = HtmlFixer.fix(original.as_bytes(), &[finding]).unwrap();
        assert_eq!(
            String::from_utf8(fixed).unwrap(),
            "<a href='http://new.test/new'>x</a>"
        );
    }

    #[test]
    fn ooxml_splices_only_link_parts_and_raw_copies_untouched_entries() {
        let original = archive(&[
            (
                "word/document.xml",
                r#"<w:instrText> HYPERLINK \"https://old.test/x\" </w:instrText>"#,
            ),
            (
                "word/_rels/document.xml.rels",
                r#"<Relationship Target=\"https://old.test/x\"/>"#,
            ),
            ("word/media/image.bin", "unchanged"),
        ]);
        let finding = binary_finding(
            DocFormat::Docx,
            "https://old.test/x",
            "https://new.test/y",
            Location::Docx { paragraph: 1 },
        );
        let fixed = OoxmlFixer.fix(&original, &[finding]).unwrap();
        let mut before = zip::ZipArchive::new(Cursor::new(original)).unwrap();
        let mut after = zip::ZipArchive::new(Cursor::new(fixed)).unwrap();
        assert_eq!(before.len(), after.len());
        let mut old_entry = before.by_name("word/media/image.bin").unwrap();
        let mut new_entry = after.by_name("word/media/image.bin").unwrap();
        let mut old = Vec::new();
        let mut new = Vec::new();
        old_entry.read_to_end(&mut old).unwrap();
        new_entry.read_to_end(&mut new).unwrap();
        assert_eq!(old, new);
    }

    #[test]
    fn ooxml_fixes_escaped_relationships_and_field_values() {
        let old = "https://old.test/x?a=1&b=2";
        let replacement = "https://new.test/y?a=3&b=4";
        let original = archive(&[
            (
                "word/_rels/document.xml.rels",
                r#"<Relationship Target="https://old.test/x?a=1&amp;b=2"/>"#,
            ),
            (
                "word/document.xml",
                r#"<w:document><w:fldSimple w:instr="HYPERLINK &quot;https://old.test/x?a=1&amp;b=2&quot;"/><w:instrText>HYPERLINK &quot;https://old.test/x?a=1&amp;b=2&quot;</w:instrText></w:document>"#,
            ),
        ]);
        let finding = binary_finding(
            DocFormat::Docx,
            old,
            replacement,
            Location::Docx { paragraph: 1 },
        );
        let fixed = OoxmlFixer.fix(&original, &[finding]).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(fixed)).unwrap();
        let mut values = String::new();
        archive
            .by_name("word/_rels/document.xml.rels")
            .unwrap()
            .read_to_string(&mut values)
            .unwrap();
        archive
            .by_name("word/document.xml")
            .unwrap()
            .read_to_string(&mut values)
            .unwrap();
        assert_eq!(values.matches("https://new.test/y?a=3&amp;b=4").count(), 3);
    }

    #[test]
    fn ooxml_deduplicates_identical_findings_and_rejects_conflicts() {
        let original = archive(&[(
            "word/_rels/document.xml.rels",
            r#"<Relationship Target="https://old.test/x"/>"#,
        )]);
        let finding = binary_finding(
            DocFormat::Docx,
            "https://old.test/x",
            "https://new.test/y",
            Location::Docx { paragraph: 1 },
        );
        assert!(
            OoxmlFixer
                .fix(&original, &[finding.clone(), finding.clone()])
                .is_ok()
        );
        let conflict = binary_finding(
            DocFormat::Docx,
            "https://old.test/x",
            "https://new.test/z",
            Location::Docx { paragraph: 1 },
        );
        assert!(
            OoxmlFixer
                .fix(&original, &[finding, conflict])
                .unwrap_err()
                .0
                .contains("conflicting")
        );
    }

    #[test]
    fn ooxml_refuses_untouched_extra_metadata() {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "word/_rels/document.xml.rels",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer
            .write_all(br#"<Relationship Target="https://old.test/x"/>"#)
            .unwrap();
        let mut options = zip::write::FileOptions::default();
        options.add_extra_data(0xdead, [1, 2, 3], false).unwrap();
        writer.start_file("word/media/keep.bin", options).unwrap();
        writer.write_all(b"keep").unwrap();
        let original = writer.finish().unwrap().into_inner();
        let finding = binary_finding(
            DocFormat::Docx,
            "https://old.test/x",
            "https://new.test/y",
            Location::Docx { paragraph: 1 },
        );
        assert!(
            OoxmlFixer
                .fix(&original, &[finding])
                .unwrap_err()
                .0
                .contains("extra metadata")
        );
    }

    #[test]
    fn pdf_fix_is_append_only_and_updates_annotation_uri() {
        let original = pdf();
        let finding = binary_finding(
            DocFormat::Pdf,
            "https://old.test/x",
            "https://new.test/y",
            Location::Pdf {
                page: 1,
                annotation: Some(0),
            },
        );
        let fixed = PdfFixer.fix(&original, &[finding]).unwrap();
        assert!(fixed.starts_with(&original));
        assert_eq!(
            fixed
                .windows(b"%%EOF".len())
                .filter(|window| *window == b"%%EOF")
                .count(),
            2
        );
        let links = extract(&SourceDocument {
            path: PathBuf::new(),
            format: DocFormat::Pdf,
            bytes: fixed,
        })
        .unwrap();
        assert_eq!(links[0].url, "https://new.test/y");
    }

    #[test]
    fn pdf_refuses_signed_marker() {
        let mut document = lopdf::Document::load_mem(&pdf()).unwrap();
        document
            .get_object_mut((1, 0))
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Perms", lopdf::Dictionary::new());
        let mut signed = Vec::new();
        document.save_to(&mut signed).unwrap();
        let finding = binary_finding(
            DocFormat::Pdf,
            "https://old.test/x",
            "https://new.test/y",
            Location::Pdf {
                page: 1,
                annotation: Some(0),
            },
        );
        assert!(
            PdfFixer
                .fix(&signed, &[finding])
                .unwrap_err()
                .0
                .contains("signed PDF")
        );
    }

    #[test]
    fn pdf_deduplicates_findings_and_encodes_unicode_replacements() {
        let original = pdf();
        for replacement in ["https://new.test/café", "https://new.test/😀"] {
            let finding = binary_finding(
                DocFormat::Pdf,
                "https://old.test/x",
                replacement,
                Location::Pdf {
                    page: 1,
                    annotation: Some(0),
                },
            );
            let fixed = PdfFixer
                .fix(&original, &[finding.clone(), finding])
                .unwrap();
            let links = extract(&SourceDocument {
                path: PathBuf::new(),
                format: DocFormat::Pdf,
                bytes: fixed,
            })
            .unwrap();
            assert_eq!(links[0].url, replacement);
        }
    }

    #[test]
    fn pdf_ignores_unrelated_perms_and_unsigned_acroform() {
        let mut document = lopdf::Document::load_mem(&pdf()).unwrap();
        document.objects.insert(
            (9, 0),
            lopdf::Object::Dictionary(lopdf::dictionary! { "Perms" => lopdf::Object::Null }),
        );
        document.objects.insert((8, 0), lopdf::Object::Dictionary(lopdf::dictionary! { "Fields" => lopdf::Object::Array(vec![lopdf::Object::Dictionary(lopdf::dictionary! { "FT" => lopdf::Object::Name(b"Tx".to_vec()) })]) }));
        document
            .get_object_mut((1, 0))
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("AcroForm", lopdf::Object::Reference((8, 0)));
        assert!(!signed_pdf(&document));
    }
}
