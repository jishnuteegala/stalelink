use std::{
    collections::HashMap,
    io::{Cursor, Read},
    ops::Range,
    path::PathBuf,
};

use html5tokenizer::{
    NaiveParser, Token, TracingEmitter,
    offset::PosTrackingReader,
    trace::{AttrValueSyntax, Trace},
};
use linkify::{LinkFinder, LinkKind};
use pulldown_cmark::{Event, LinkType, Parser, Tag};
use quick_xml::{NsReader, Reader, events::Event as XmlEvent, name::ResolveResult};
use unicase::UniCase;
use url::Url;

use crate::model::{DocFormat, FoundLink, Location, SourceRef};

pub struct SourceDocument {
    pub path: PathBuf,
    pub format: DocFormat,
    pub bytes: Vec<u8>,
}
#[derive(Debug)]
pub struct ExtractError(pub String);
pub trait Extractor {
    fn extract(&self, doc: &SourceDocument) -> Result<Vec<FoundLink>, ExtractError>;
}
pub struct MarkdownExtractor;
pub struct HtmlExtractor;
pub struct TextExtractor;
pub struct PdfExtractor;
pub struct OoxmlExtractor;

fn location(bytes: &[u8], offset: usize) -> Location {
    let prefix = &bytes[..offset];
    let line = prefix.iter().filter(|&&b| b == b'\n').count() as u32 + 1;
    let start = prefix
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |i| i + 1);
    let column = std::str::from_utf8(&bytes[start..offset])
        .map(|text| text.chars().count() as u32)
        .unwrap_or((offset - start) as u32)
        + 1;
    Location::Text { line, column }
}

fn is_http(url: &str) -> bool {
    matches!(
        Url::parse(url).map(|parsed| parsed.scheme().to_owned()),
        Ok(scheme) if scheme == "http" || scheme == "https"
    )
}

fn is_local_or_contact(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("mailto:") || lower.starts_with("tel:") || url.starts_with('#') {
        return true;
    }
    Url::parse(url).is_err()
}

// `url` is the semantic (decoded/unescaped) link target; `span` is the exact
// byte range of the raw source text it came from.
fn link(doc: &SourceDocument, url: &str, span: Range<usize>) -> FoundLink {
    FoundLink {
        url: url.to_owned(),
        source: SourceRef {
            path: doc.path.clone(),
            format: doc.format,
            location: location(&doc.bytes, span.start),
            byte_span: Some(Range {
                start: span.start as u64,
                end: span.end as u64,
            }),
        },
    }
}

fn binary_link(doc: &SourceDocument, url: &str, location: Location) -> FoundLink {
    FoundLink {
        url: url.to_owned(),
        source: SourceRef {
            path: doc.path.clone(),
            format: doc.format,
            location,
            // Binary container offsets cannot identify a source-level URL for fixing.
            byte_span: None,
        },
    }
}

fn text_of(doc: &SourceDocument) -> Result<&str, ExtractError> {
    std::str::from_utf8(&doc.bytes).map_err(|e| ExtractError(e.to_string()))
}

// A span is only usable if it is well-ordered, in bounds, and lands on UTF-8
// boundaries; otherwise slicing the source would panic. Some HTML attribute
// syntaxes (e.g. unquoted values) yield inverted or out-of-range traces.
fn valid_span(text: &str, span: &Range<usize>) -> bool {
    span.start <= span.end && text.get(span.clone()).is_some()
}

// html5tokenizer 0.5.2 leaves `value_span.end` unset (0) for unquoted attribute
// values that run to the tag close, producing an inverted range. The start
// offset is reliable, so recompute the end by scanning to the first whitespace
// or `>` when the raw span is unusable.
fn repair_span(
    text: &str,
    span: Range<usize>,
    syntax: Option<AttrValueSyntax>,
) -> Option<Range<usize>> {
    if valid_span(text, &span) {
        return Some(span);
    }
    if syntax != Some(AttrValueSyntax::Unquoted) {
        return None;
    }
    // Per the HTML tokenizer spec an unquoted value is terminated only by
    // whitespace or `>` (a `/` is an ordinary value character, e.g. in a URL).
    let rest = text.get(span.start..)?;
    let len = rest
        .find(|c: char| c.is_whitespace() || c == '>')
        .unwrap_or(rest.len());
    let end = span.start + len;
    valid_span(text, &(span.start..end)).then_some(span.start..end)
}

// Best-effort raw source span of a Markdown inline link/image destination.
// pulldown owns the semantic `dest_url`; here we only need the *raw* byte range
// for later in-place fixing, so we locate the destination structurally and, if
// an exotic form defeats the scan, the caller falls back to the event span.
// We find the `](` that closes the label and opens the destination, then parse
// either an angle-bracket `<...>` form or a bare balanced-parenthesis form.
fn inline_dest_span(text: &str, event: &Range<usize>) -> Option<Range<usize>> {
    let open = label_close(text, event)? + 1;
    let rest = text.get(open..event.end)?;
    if rest.starts_with('<') {
        angle_dest(open, rest)
    } else {
        bare_dest(open, rest)
    }
}
// The label runs from the event's first `[` to the matching `]` at bracket
// depth zero (labels may contain balanced or escaped brackets), which must be
// immediately followed by `(` for an inline link. Returns the index of that
// `(`. An image event begins with `!`, so scanning starts at the first `[`.
fn label_close(text: &str, event: &Range<usize>) -> Option<usize> {
    let slice = text.get(event.clone())?;
    let bracket = slice.find('[')?;
    let mut escaped = false;
    let mut depth: i32 = 0;
    for (i, c) in slice[bracket..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    let paren = event.start + bracket + i + 1;
                    return (text.as_bytes().get(paren) == Some(&b'(')).then_some(paren);
                }
            }
            _ => {}
        }
    }
    None
}
// Best-effort raw destination span within an exact `[label]: dest` definition.
// Uses the definition's own source span (parser-provided) and finds the last
// unescaped `]` that closes the label, then the following `:` and whitespace.
fn dest_in_definition(text: &str, def_span: &Range<usize>) -> Option<Range<usize>> {
    let slice = text.get(def_span.clone())?;
    let mut escaped = false;
    let mut depth: i32 = 0;
    let mut colon = None;
    for (i, c) in slice.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 && slice[i + 1..].trim_start().starts_with(':') {
                    colon = slice[i + 1..].find(':').map(|c| i + 1 + c);
                    break;
                }
            }
            _ => {}
        }
    }
    let colon = colon? + 1;
    let after = &slice[colon..];
    let dest_start = def_span.start + colon + (after.len() - after.trim_start().len());
    let rest = text.get(dest_start..def_span.end)?;
    if rest.starts_with('<') {
        angle_dest(dest_start, rest)
    } else {
        bare_dest(dest_start, rest)
    }
}
// `<...>` destination: the angle delimiters are syntax, not URL bytes.
fn angle_dest(open: usize, rest: &str) -> Option<Range<usize>> {
    let mut escaped = false;
    for (i, c) in rest.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '>' && i > 0 {
            return Some(open + 1..open + i);
        }
    }
    None
}
// Bare destination: balanced parentheses, ending at the first unescaped
// whitespace (before an optional title) or the closing `)` at depth zero.
fn bare_dest(open: usize, rest: &str) -> Option<Range<usize>> {
    let mut escaped = false;
    let mut depth: i32 = 0;
    for (i, c) in rest.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '(' => depth += 1,
            ')' if depth == 0 => return Some(open..open + i),
            ')' => depth -= 1,
            c if c.is_whitespace() => return Some(open..open + i),
            _ => {}
        }
    }
    // No terminator: the destination runs to the end of the slice (e.g. a
    // reference definition that ends at the line/span boundary).
    (depth == 0 && !rest.is_empty()).then_some(open..open + rest.len())
}

impl Extractor for TextExtractor {
    fn extract(&self, doc: &SourceDocument) -> Result<Vec<FoundLink>, ExtractError> {
        let text = text_of(doc)?;
        let mut finder = LinkFinder::new();
        finder.kinds(&[LinkKind::Url]);
        Ok(finder
            .links(text)
            .filter(|found| is_http(found.as_str()))
            .map(|found| link(doc, found.as_str(), found.start()..found.end()))
            .collect())
    }
}
impl Extractor for MarkdownExtractor {
    fn extract(&self, doc: &SourceDocument) -> Result<Vec<FoundLink>, ExtractError> {
        let text = text_of(doc)?;
        let mut links = Vec::new();
        let mut link_ranges: Vec<Range<usize>> = Vec::new();
        // Reference definitions carry an exact `[label]: dest` source span,
        // keyed by pulldown's own UniCase folding so a reference resolves to its
        // own definition. A second parser drives iteration below (parses are
        // pure). The full definition span is recorded so linkify does not
        // re-report a URL that appears inside the definition (e.g. in a title).
        let ref_defs: HashMap<UniCase<String>, Range<usize>> = Parser::new(text)
            .reference_definitions()
            .iter()
            .map(|(label, def)| (UniCase::new(label.to_owned()), def.span.clone()))
            .collect();
        // Every reference definition is a suppression range so a URL-shaped
        // title (or the destination) is never rediscovered by linkify, whether
        // or not the definition's own destination is HTTP.
        link_ranges.extend(ref_defs.values().cloned());
        for (event, span) in Parser::new(text).into_offset_iter() {
            let (Event::Start(Tag::Link {
                link_type,
                dest_url,
                id,
                ..
            })
            | Event::Start(Tag::Image {
                link_type,
                dest_url,
                id,
                ..
            })) = event
            else {
                continue;
            };
            link_ranges.push(span.clone());
            if !is_http(&dest_url) && !is_local_or_contact(&dest_url) {
                continue;
            }
            // Trust pulldown's decoded `dest_url` as the semantic URL a checker
            // will fetch. We only reconstruct the *raw* byte span for in-place
            // fixing; when an exotic form defeats that, fall back to the event
            // (or definition) span so the link is never dropped.
            let dest_span = match link_type {
                LinkType::Inline => inline_dest_span(text, &span).unwrap_or(span.clone()),
                LinkType::Autolink | LinkType::Email => {
                    // `<url>` autolink: the raw span is the inner bytes.
                    span.start + 1..span.end.saturating_sub(1)
                }
                LinkType::Reference | LinkType::Collapsed | LinkType::Shortcut => {
                    match ref_defs.get(&UniCase::new(id.to_string())) {
                        Some(def_span) => {
                            dest_in_definition(text, def_span).unwrap_or(def_span.clone())
                        }
                        None => span.clone(),
                    }
                }
                _ => span.clone(),
            };
            // Suppress bare-URL rediscovery over the destination (definition
            // spans are already suppressed above).
            link_ranges.push(dest_span.clone());
            links.push(link(doc, &dest_url, dest_span));
        }
        // Bare URLs in prose are not link events; linkify them, but skip any
        // that fall inside a parsed link, destination, or definition so a
        // single Markdown link is never counted twice.
        let mut finder = LinkFinder::new();
        finder.kinds(&[LinkKind::Url]);
        for found in finder.links(text).filter(|found| is_http(found.as_str())) {
            if link_ranges
                .iter()
                .any(|range| range.start <= found.start() && found.start() < range.end)
            {
                continue;
            }
            links.push(link(doc, found.as_str(), found.start()..found.end()));
        }
        links.sort_by_key(|found| found.source.byte_span.as_ref().map(|span| span.start));
        links.dedup_by_key(|found| found.source.byte_span.clone());
        Ok(links)
    }
}
impl Extractor for HtmlExtractor {
    fn extract(&self, doc: &SourceDocument) -> Result<Vec<FoundLink>, ExtractError> {
        let text = text_of(doc)?;
        let parser =
            NaiveParser::new_with_emitter(PosTrackingReader::new(text), TracingEmitter::default());
        let mut links = Vec::new();
        for (token, trace) in parser.flatten() {
            let (Token::StartTag(tag), Trace::StartTag(trace)) = (token, trace) else {
                continue;
            };
            let attr = match tag.name.as_str() {
                "a" | "link" => "href",
                "img" | "script" => "src",
                _ => continue,
            };
            for attribute in &tag.attributes {
                if attribute.name() != attr
                    || (!is_http(attribute.value()) && !is_local_or_contact(attribute.value()))
                {
                    continue;
                }
                let Some(index) = attribute.trace_idx() else {
                    continue;
                };
                let attr_trace = &trace.attribute_traces[index];
                if let Some(span) = attr_trace.value_span()
                    && let Some(span) = repair_span(text, span, attr_trace.value_syntax())
                {
                    links.push(link(doc, attribute.value(), span));
                }
            }
        }
        Ok(links)
    }
}

impl Extractor for PdfExtractor {
    fn extract(&self, doc: &SourceDocument) -> Result<Vec<FoundLink>, ExtractError> {
        let pdf = lopdf::Document::load_mem(&doc.bytes)
            .map_err(|error| ExtractError(error.to_string()))?;
        if pdf.is_encrypted() {
            // lopdf does not expose encrypted annotations without decryption. Surface raw
            // candidates solely so `fix` can refuse the document with its security reason.
            let mut finder = LinkFinder::new();
            finder.kinds(&[LinkKind::Url]);
            return Ok(finder
                .links(&String::from_utf8_lossy(&doc.bytes))
                .filter(|found| is_http(found.as_str()))
                .map(|found| {
                    binary_link(
                        doc,
                        found.as_str(),
                        Location::Pdf {
                            page: 1,
                            annotation: None,
                        },
                    )
                })
                .collect());
        }
        let mut links = Vec::new();
        for (page_number, page_id) in pdf.get_pages() {
            let annotations = pdf
                .get_page_annotations(page_id)
                .map_err(|error| ExtractError(error.to_string()))?;
            for (index, annotation) in annotations.iter().enumerate() {
                let Ok((_, action)) = annotation
                    .get(b"A")
                    .and_then(|object| pdf.dereference(object))
                else {
                    continue;
                };
                let Ok(action) = action.as_dict() else {
                    continue;
                };
                if !action
                    .get(b"S")
                    .and_then(|object| pdf.dereference(object))
                    .is_ok_and(
                        |(_, object)| matches!(object, lopdf::Object::Name(name) if name == b"URI"),
                    )
                {
                    continue;
                }
                let Ok((_, uri)) = action
                    .get(b"URI")
                    .and_then(|object| pdf.dereference(object))
                else {
                    continue;
                };
                let Ok(uri) = uri.as_str() else { continue };
                let Some(uri) = pdf_string(uri) else {
                    continue;
                };
                if is_http(&uri) || is_local_or_contact(&uri) {
                    links.push(binary_link(
                        doc,
                        &uri,
                        Location::Pdf {
                            page: page_number,
                            annotation: Some(index as u32),
                        },
                    ));
                }
            }
            let Ok(text) = pdf.extract_text(&[page_number]) else {
                continue;
            };
            let mut finder = LinkFinder::new();
            finder.kinds(&[LinkKind::Url]);
            links.extend(
                finder
                    .links(&text)
                    .filter(|found| is_http(found.as_str()))
                    .map(|found| {
                        binary_link(
                            doc,
                            found.as_str(),
                            Location::Pdf {
                                page: page_number,
                                annotation: None,
                            },
                        )
                    }),
            );
        }
        Ok(links)
    }
}

// PDFDocEncoding's undefined bytes reject the entire string rather than silently
// changing a URI. PDF strings use UTF-16BE when BOM-prefixed.
fn pdf_string(bytes: &[u8]) -> Option<String> {
    if bytes.starts_with(&[0xfe, 0xff]) {
        if !bytes[2..].len().is_multiple_of(2) {
            return None;
        }
        String::from_utf16(
            &bytes[2..]
                .chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect::<Vec<_>>(),
        )
        .ok()
    } else {
        bytes.iter().copied().map(pdf_doc_encoding).collect()
    }
}

fn pdf_doc_encoding(byte: u8) -> Option<char> {
    Some(match byte {
        0x18 => '\u{02d8}',
        0x19 => '\u{02c7}',
        0x1a => '\u{02c6}',
        0x1b => '\u{02d9}',
        0x1c => '\u{02dd}',
        0x1d => '\u{02db}',
        0x1e => '\u{02da}',
        0x1f => '\u{02dc}',
        0x7f | 0x9f => return None,
        0x80 => '\u{2022}',
        0x81 => '\u{2020}',
        0x82 => '\u{2021}',
        0x83 => '\u{2026}',
        0x84 => '\u{2014}',
        0x85 => '\u{2013}',
        0x86 => '\u{0192}',
        0x87 => '\u{2044}',
        0x88 => '\u{2039}',
        0x89 => '\u{203a}',
        0x8a => '\u{2212}',
        0x8b => '\u{2030}',
        0x8c => '\u{201e}',
        0x8d => '\u{201c}',
        0x8e => '\u{201d}',
        0x8f => '\u{2018}',
        0x90 => '\u{2019}',
        0x91 => '\u{201a}',
        0x92 => '\u{2122}',
        0x93 => '\u{fb01}',
        0x94 => '\u{fb02}',
        0x95 => '\u{0141}',
        0x96 => '\u{0152}',
        0x97 => '\u{0160}',
        0x98 => '\u{0178}',
        0x99 => '\u{017d}',
        0x9a => '\u{0131}',
        0x9b => '\u{0142}',
        0x9c => '\u{0153}',
        0x9d => '\u{0161}',
        0x9e => '\u{017e}',
        0xa0 => '\u{20ac}',
        byte => char::from(byte),
    })
}

const ZIP_MEMBER_LIMIT: u64 = 64 * 1024 * 1024;

fn zip_text(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
) -> Result<String, ExtractError> {
    zip_text_with_limit(archive, name, ZIP_MEMBER_LIMIT)
}

fn zip_text_with_limit(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
    limit: u64,
) -> Result<String, ExtractError> {
    let file = archive
        .by_name(name)
        .map_err(|error| ExtractError(error.to_string()))?;
    if file.size() > limit {
        return Err(ExtractError(format!(
            "{name}: uncompressed member exceeds {limit} byte limit"
        )));
    }
    let mut bytes = Vec::with_capacity(file.size() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ExtractError(error.to_string()))?;
    if bytes.len() as u64 > limit {
        return Err(ExtractError(format!(
            "{name}: uncompressed member exceeds {limit} byte limit"
        )));
    }
    String::from_utf8(bytes).map_err(|error| ExtractError(format!("{name}: {error}")))
}

fn optional_zip_text(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
) -> Result<Option<String>, ExtractError> {
    let exists = archive.file_names().any(|entry| entry == name);
    exists.then(|| zip_text(archive, name)).transpose()
}

fn attributes(event: &quick_xml::events::BytesStart<'_>) -> HashMap<String, String> {
    event
        .attributes()
        .flatten()
        .filter_map(|attribute| {
            let key = std::str::from_utf8(attribute.key.as_ref()).ok()?.to_owned();
            let value = attribute.unescape_value().ok()?.into_owned();
            Some((key, value))
        })
        .collect()
}

fn relationships(
    part: &str,
    xml: &str,
    external_only: bool,
) -> Result<HashMap<String, String>, ExtractError> {
    let mut reader = Reader::from_str(xml);
    let mut relationships = HashMap::new();
    loop {
        match reader.read_event() {
            Ok(XmlEvent::Empty(event)) | Ok(XmlEvent::Start(event))
                if event.name().as_ref() == b"Relationship" =>
            {
                let attributes = event
                    .attributes()
                    .map(|attribute| {
                        let attribute =
                            attribute.map_err(|error| ExtractError(format!("{part}: {error}")))?;
                        let key = std::str::from_utf8(attribute.key.as_ref())
                            .map_err(|error| ExtractError(format!("{part}: {error}")))?
                            .to_owned();
                        let value = attribute
                            .unescape_value()
                            .map_err(|error| ExtractError(format!("{part}: {error}")))?
                            .into_owned();
                        Ok((key, value))
                    })
                    .collect::<Result<HashMap<_, _>, ExtractError>>()?;
                if (!external_only
                    || attributes
                        .get("TargetMode")
                        .is_some_and(|mode| mode == "External"))
                    && let (Some(id), Some(target)) =
                        (attributes.get("Id"), attributes.get("Target"))
                    && (!external_only || is_http(target) || is_local_or_contact(target))
                {
                    relationships.insert(id.clone(), target.clone());
                }
            }
            Ok(XmlEvent::Eof) => break,
            Err(error) => return Err(ExtractError(format!("{part}: {error}"))),
            _ => {}
        }
    }
    Ok(relationships)
}

fn link_from_field(text: &str) -> Option<&str> {
    let start = text.find("HYPERLINK")? + "HYPERLINK".len();
    let rest = &text[start..];
    let start = rest.find("http")?;
    let url = &rest[start..];
    let end = url
        .find(|character: char| character.is_whitespace() || matches!(character, '"' | ')' | '&'))
        .unwrap_or(url.len());
    let url = &url[..end];
    is_http(url).then_some(url)
}

fn resolved_name(
    resolved: &ResolveResult<'_>,
    event: &quick_xml::events::BytesStart<'_>,
    namespace: &[u8],
    local: &[u8],
) -> bool {
    matches!(resolved, ResolveResult::Bound(uri) if uri.as_ref() == namespace)
        && event.local_name().as_ref() == local
}

const WORD_NS: &[u8] = b"http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const PRESENTATION_NS: &[u8] = b"http://schemas.openxmlformats.org/presentationml/2006/main";
const DRAWING_NS: &[u8] = b"http://schemas.openxmlformats.org/drawingml/2006/main";
const SPREADSHEET_NS: &[u8] = b"http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const RELATIONSHIPS_NS: &[u8] =
    b"http://schemas.openxmlformats.org/officeDocument/2006/relationships";

fn relationship_part(part: &str) -> String {
    let (directory, name) = part.rsplit_once('/').unwrap_or(("", part));
    format!("{directory}/_rels/{name}.rels")
}

fn resolved_attribute(
    reader: &NsReader<&[u8]>,
    event: &quick_xml::events::BytesStart<'_>,
    namespace: &[u8],
    local: &[u8],
) -> Option<String> {
    event.attributes().flatten().find_map(|attribute| {
        let (resolved, attribute_local) = reader.resolver().resolve_attribute(attribute.key);
        (matches!(resolved, ResolveResult::Bound(uri) if uri.as_ref() == namespace)
            && attribute_local.as_ref() == local)
            .then(|| {
                attribute
                    .unescape_value()
                    .ok()
                    .map(|value| value.into_owned())
            })
            .flatten()
    })
}

fn resolve_target(source: &str, target: &str) -> String {
    let mut segments = if target.starts_with('/') {
        Vec::new()
    } else {
        source.split('/').collect::<Vec<_>>()[..source.matches('/').count()].to_vec()
    };
    for segment in target.trim_start_matches('/').split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            _ => segments.push(segment),
        }
    }
    segments.join("/")
}

impl Extractor for OoxmlExtractor {
    fn extract(&self, doc: &SourceDocument) -> Result<Vec<FoundLink>, ExtractError> {
        let mut archive = zip::ZipArchive::new(Cursor::new(doc.bytes.as_slice()))
            .map_err(|error| ExtractError(error.to_string()))?;
        match doc.format {
            DocFormat::Docx => extract_docx(doc, &mut archive),
            DocFormat::Xlsx => extract_xlsx(doc, &mut archive),
            DocFormat::Pptx => extract_pptx(doc, &mut archive),
            _ => Ok(Vec::new()),
        }
    }
}

fn extract_docx(
    doc: &SourceDocument,
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
) -> Result<Vec<FoundLink>, ExtractError> {
    let relations = optional_zip_text(archive, "word/_rels/document.xml.rels")?
        .map(|xml| relationships("word/_rels/document.xml.rels", &xml, true))
        .transpose()?
        .unwrap_or_default();
    let document = zip_text(archive, "word/document.xml")?;
    let mut reader = NsReader::from_str(&document);
    let mut links = Vec::new();
    let mut paragraph = 0;
    let mut field: Option<(u32, String)> = None;
    let mut instruction_text: Option<(u32, String)> = None;
    loop {
        match reader.read_resolved_event() {
            Ok((resolved, XmlEvent::Start(event))) | Ok((resolved, XmlEvent::Empty(event))) => {
                let paragraph_element = resolved_name(&resolved, &event, WORD_NS, b"p");
                let hyperlink = resolved_name(&resolved, &event, WORD_NS, b"hyperlink");
                let field_character = resolved_name(&resolved, &event, WORD_NS, b"fldChar");
                let simple_field = resolved_name(&resolved, &event, WORD_NS, b"fldSimple");
                let instruction_element = resolved_name(&resolved, &event, WORD_NS, b"instrText");
                let _ = resolved;
                if paragraph_element {
                    paragraph += 1;
                }
                if hyperlink
                    && let Some(id) = resolved_attribute(&reader, &event, RELATIONSHIPS_NS, b"id")
                    && let Some(url) = relations.get(&id)
                {
                    links.push(binary_link(doc, url, Location::Docx { paragraph }));
                }
                if field_character {
                    match resolved_attribute(&reader, &event, WORD_NS, b"fldCharType").as_deref() {
                        Some("begin") => field = Some((paragraph, String::new())),
                        Some("end") => {
                            if let Some((paragraph, instruction)) = field.take()
                                && let Some(url) = link_from_field(&instruction)
                            {
                                links.push(binary_link(doc, url, Location::Docx { paragraph }));
                            }
                        }
                        _ => {}
                    }
                }
                if simple_field
                    && let Some(instruction) =
                        resolved_attribute(&reader, &event, WORD_NS, b"instr")
                    && let Some(url) = link_from_field(&instruction)
                {
                    links.push(binary_link(doc, url, Location::Docx { paragraph }));
                }
                if instruction_element {
                    instruction_text = Some((paragraph, String::new()));
                }
            }
            Ok((_, XmlEvent::Text(event))) => {
                let text = event
                    .xml_content()
                    .map_err(|error| ExtractError(error.to_string()))?
                    .replace("&quot;", "\"");
                if let Some((_, instruction)) = &mut field {
                    instruction.push_str(&text);
                } else if let Some((_, instruction)) = &mut instruction_text {
                    instruction.push_str(&text);
                } else if let Some(url) = link_from_field(&text) {
                    links.push(binary_link(doc, url, Location::Docx { paragraph }));
                }
            }
            Ok((_, XmlEvent::CData(event))) => {
                let text = event
                    .decode()
                    .map_err(|error| ExtractError(error.to_string()))?
                    .replace("&quot;", "\"");
                if let Some((_, instruction)) = &mut field {
                    instruction.push_str(&text);
                } else if let Some((_, instruction)) = &mut instruction_text {
                    instruction.push_str(&text);
                } else if let Some(url) = link_from_field(&text) {
                    links.push(binary_link(doc, url, Location::Docx { paragraph }));
                }
            }
            Ok((_, XmlEvent::GeneralRef(event))) => {
                let reference = event
                    .decode()
                    .map_err(|error| ExtractError(error.to_string()))?;
                let reference = if reference == "quot" { "\"" } else { "&" };
                if let Some((_, instruction)) = &mut field {
                    instruction.push_str(reference);
                } else if let Some((_, instruction)) = &mut instruction_text {
                    instruction.push_str(reference);
                }
            }
            Ok((resolved, XmlEvent::End(event)))
                if matches!(resolved, ResolveResult::Bound(uri) if uri.as_ref() == WORD_NS)
                    && event.local_name().as_ref() == b"instrText" =>
            {
                if let Some((paragraph, instruction)) = instruction_text.take()
                    && let Some(url) = link_from_field(&instruction)
                {
                    links.push(binary_link(doc, url, Location::Docx { paragraph }));
                }
            }
            Ok((_, XmlEvent::Eof)) => break,
            Err(error) => return Err(ExtractError(error.to_string())),
            _ => {}
        }
    }
    Ok(links)
}

fn extract_xlsx(
    doc: &SourceDocument,
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
) -> Result<Vec<FoundLink>, ExtractError> {
    let workbook = zip_text(archive, "xl/workbook.xml")?;
    let workbook_relations = optional_zip_text(archive, "xl/_rels/workbook.xml.rels")?
        .map(|xml| relationships("xl/_rels/workbook.xml.rels", &xml, false))
        .transpose()?
        .unwrap_or_default();
    let mut reader = NsReader::from_str(&workbook);
    let mut sheets = Vec::new();
    loop {
        match reader.read_resolved_event() {
            Ok((resolved, XmlEvent::Empty(event))) | Ok((resolved, XmlEvent::Start(event)))
                if resolved_name(&resolved, &event, SPREADSHEET_NS, b"sheet") =>
            {
                let attributes = attributes(&event);
                if let (Some(name), Some(id)) = (
                    attributes.get("name"),
                    resolved_attribute(&reader, &event, RELATIONSHIPS_NS, b"id"),
                ) && let Some(target) = workbook_relations.get(&id)
                {
                    sheets.push((name.clone(), resolve_target("xl/workbook.xml", target)));
                }
            }
            Ok((_, XmlEvent::Eof)) => break,
            _ => {}
        }
    }
    let mut links = Vec::new();
    for (sheet, path) in sheets {
        let rel_path = relationship_part(&path);
        let relations = optional_zip_text(archive, &rel_path)?
            .map(|xml| relationships(&rel_path, &xml, true))
            .transpose()?
            .unwrap_or_default();
        let xml = zip_text(archive, &path)?.replace("&quot;", "\"");
        let mut reader = NsReader::from_str(&xml);
        let mut cell = String::new();
        let mut formula = false;
        loop {
            match reader.read_resolved_event() {
                Ok((resolved, XmlEvent::Start(event))) => {
                    let attributes = attributes(&event);
                    if resolved_name(&resolved, &event, SPREADSHEET_NS, b"c") {
                        cell = attributes.get("r").cloned().unwrap_or_default();
                    }
                    if resolved_name(&resolved, &event, SPREADSHEET_NS, b"f") {
                        formula = true;
                    }
                    if resolved_name(&resolved, &event, SPREADSHEET_NS, b"hyperlink")
                        && let (Some(reference), Some(id)) = (
                            attributes.get("ref"),
                            resolved_attribute(&reader, &event, RELATIONSHIPS_NS, b"id"),
                        )
                        && let Some(url) = relations.get(&id)
                    {
                        links.push(binary_link(
                            doc,
                            url,
                            Location::Xlsx {
                                sheet: sheet.clone(),
                                cell: reference.clone(),
                            },
                        ));
                    }
                }
                Ok((resolved, XmlEvent::Empty(event)))
                    if resolved_name(&resolved, &event, SPREADSHEET_NS, b"hyperlink") =>
                {
                    let attributes = attributes(&event);
                    if let (Some(reference), Some(id)) = (
                        attributes.get("ref"),
                        resolved_attribute(&reader, &event, RELATIONSHIPS_NS, b"id"),
                    ) && let Some(url) = relations.get(&id)
                    {
                        links.push(binary_link(
                            doc,
                            url,
                            Location::Xlsx {
                                sheet: sheet.clone(),
                                cell: reference.clone(),
                            },
                        ));
                    }
                }
                Ok((_, XmlEvent::Text(event))) if formula => {
                    let text = event
                        .xml_content()
                        .map_err(|error| ExtractError(error.to_string()))?
                        .replace("&quot;", "\"");
                    if let Some(url) = link_from_field(&text) {
                        links.push(binary_link(
                            doc,
                            url,
                            Location::Xlsx {
                                sheet: sheet.clone(),
                                cell: cell.clone(),
                            },
                        ));
                    }
                }
                Ok((resolved, XmlEvent::End(event)))
                    if matches!(resolved, ResolveResult::Bound(uri) if uri.as_ref() == SPREADSHEET_NS)
                        && event.local_name().as_ref() == b"f" =>
                {
                    formula = false;
                }
                Ok((_, XmlEvent::Eof)) => break,
                Err(error) => return Err(ExtractError(error.to_string())),
                _ => {}
            }
        }
    }
    Ok(links)
}

fn extract_pptx(
    doc: &SourceDocument,
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
) -> Result<Vec<FoundLink>, ExtractError> {
    let presentation_relations = optional_zip_text(archive, "ppt/_rels/presentation.xml.rels")?
        .map(|xml| relationships("ppt/_rels/presentation.xml.rels", &xml, false))
        .transpose()?
        .unwrap_or_default();
    let mut slides = Vec::new();
    if let Some(presentation) = optional_zip_text(archive, "ppt/presentation.xml")? {
        let mut reader = NsReader::from_str(&presentation);
        loop {
            match reader.read_resolved_event() {
                Ok((resolved, XmlEvent::Empty(event))) | Ok((resolved, XmlEvent::Start(event)))
                    if resolved_name(&resolved, &event, PRESENTATION_NS, b"sldId") =>
                {
                    if let Some(id) = resolved_attribute(&reader, &event, RELATIONSHIPS_NS, b"id")
                        && let Some(target) = presentation_relations.get(&id)
                    {
                        slides.push(resolve_target("ppt/presentation.xml", target));
                    }
                }
                Ok((_, XmlEvent::Eof)) => break,
                Err(error) => return Err(ExtractError(error.to_string())),
                _ => {}
            }
        }
    }
    if slides.is_empty() {
        slides = archive
            .file_names()
            .filter(|name| name.starts_with("ppt/slides/slide") && name.ends_with(".xml"))
            .map(str::to_owned)
            .collect();
        slides.sort_by_key(|path| {
            path.trim_start_matches("ppt/slides/slide")
                .trim_end_matches(".xml")
                .parse::<u32>()
                .unwrap_or(0)
        });
    }
    let mut links = Vec::new();
    for (index, path) in slides.iter().enumerate() {
        let rel_path = relationship_part(path);
        let relations = optional_zip_text(archive, &rel_path)?
            .map(|xml| relationships(&rel_path, &xml, true))
            .transpose()?
            .unwrap_or_default();
        let slide = zip_text(archive, path)?;
        let mut reader = NsReader::from_str(&slide);
        loop {
            match reader.read_resolved_event() {
                Ok((resolved, XmlEvent::Empty(event))) | Ok((resolved, XmlEvent::Start(event))) => {
                    if resolved_name(&resolved, &event, DRAWING_NS, b"hlinkClick")
                        || resolved_name(&resolved, &event, DRAWING_NS, b"hlinkHover")
                    {
                        let id = resolved_attribute(&reader, &event, RELATIONSHIPS_NS, b"id");
                        if let Some(id) = id.as_deref()
                            && let Some(url) = relations.get(id)
                        {
                            links.push(binary_link(
                                doc,
                                url,
                                Location::Pptx {
                                    slide: index as u32 + 1,
                                },
                            ));
                        }
                    }
                }
                Ok((_, XmlEvent::Eof)) => break,
                Err(error) => return Err(ExtractError(error.to_string())),
                _ => {}
            }
        }
    }
    Ok(links)
}
pub fn extract(doc: &SourceDocument) -> Result<Vec<FoundLink>, ExtractError> {
    match doc.format {
        DocFormat::Markdown => MarkdownExtractor.extract(doc),
        DocFormat::Html => HtmlExtractor.extract(doc),
        DocFormat::Text => TextExtractor.extract(doc),
        DocFormat::Pdf => PdfExtractor.extract(doc),
        DocFormat::Docx | DocFormat::Xlsx | DocFormat::Pptx => OoxmlExtractor.extract(doc),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip(entries: &[(&str, &str)]) -> Vec<u8> {
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
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 6 0 R >> >> /Contents 4 0 R /Annots [5 0 R] >>".to_owned(),
            "<< /Length 51 >>\nstream\nBT /F1 12 Tf 72 720 Td (https://text.test/x) Tj ET\nendstream".to_owned(),
            "<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] /A << /S /URI /URI (https://annotation.test/x) >> >>".to_owned(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ];
        let mut output = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0];
        for (number, object) in objects.iter().enumerate() {
            offsets.push(output.len());
            output
                .extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", number + 1).as_bytes());
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

    fn pdf_with_objects(objects: &[String]) -> Vec<u8> {
        let mut output = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0];
        for (number, object) in objects.iter().enumerate() {
            offsets.push(output.len());
            output
                .extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", number + 1).as_bytes());
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

    fn binary_document(format: DocFormat, bytes: Vec<u8>) -> SourceDocument {
        SourceDocument {
            path: "fixture".into(),
            format,
            bytes,
        }
    }
    fn document(format: DocFormat, text: &str) -> SourceDocument {
        SourceDocument {
            path: "fixture".into(),
            format,
            bytes: text.as_bytes().to_vec(),
        }
    }
    fn only(format: DocFormat, text: &str) -> FoundLink {
        let doc = document(format, text);
        let mut links = extract(&doc).unwrap();
        assert_eq!(links.len(), 1, "expected exactly one link");
        links.pop().unwrap()
    }
    fn assert_link(format: DocFormat, text: &str, url: &str, line: u32, column: u32) {
        let found = only(format, text);
        assert_eq!(found.url, url);
        assert_eq!(found.source.location, Location::Text { line, column });
    }
    fn assert_span_matches_url(format: DocFormat, text: &str) {
        let doc = document(format, text);
        let found = extract(&doc).unwrap().pop().unwrap();
        let span = found.source.byte_span.clone().unwrap();
        assert_eq!(
            &doc.bytes[span.start as usize..span.end as usize],
            found.url.as_bytes()
        );
    }
    #[test]
    fn markdown_url_has_exact_span_and_location() {
        assert_link(
            DocFormat::Markdown,
            "# A\n[x](https://example.test/x)",
            "https://example.test/x",
            2,
            5,
        );
        assert_span_matches_url(DocFormat::Markdown, "# A\n[x](https://example.test/x)");
    }
    #[test]
    fn html_url_has_exact_span_and_location() {
        assert_link(
            DocFormat::Html,
            "<a href=\"https://example.test/x\">x</a>",
            "https://example.test/x",
            1,
            10,
        );
        assert_span_matches_url(DocFormat::Html, "<a href=\"https://example.test/x\">x</a>");
    }
    #[test]
    fn text_url_has_exact_span_and_location() {
        assert_link(
            DocFormat::Text,
            "see https://example.test/x",
            "https://example.test/x",
            1,
            5,
        );
    }
    #[test]
    fn trailing_punctuation_is_not_part_of_url() {
        assert_link(
            DocFormat::Text,
            "See https://example.test/x.",
            "https://example.test/x",
            1,
            5,
        );
    }
    #[test]
    fn non_ascii_prefix_counts_characters_not_bytes() {
        assert_link(
            DocFormat::Text,
            "café https://example.test/x",
            "https://example.test/x",
            1,
            6,
        );
    }
    #[test]
    fn malformed_token_does_not_become_a_link() {
        let doc = document(DocFormat::Text, "https://. and https://example.test/x");
        let links = extract(&doc).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.test/x");
    }
    #[test]
    fn html_collects_multiple_attributes() {
        let doc = document(
            DocFormat::Html,
            "<a href=\"https://a.test/1\">x</a><img src=\"https://b.test/2\">",
        );
        let links = extract(&doc).unwrap();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].url, "https://a.test/1");
        assert_eq!(links[1].url, "https://b.test/2");
    }
    #[test]
    fn html_span_points_at_href_not_visible_text() {
        let text = "https://dup.test/x <a href=\"https://dup.test/x\">x</a>";
        let doc = document(DocFormat::Html, text);
        let found = extract(&doc).unwrap().pop().unwrap();
        let span = found.source.byte_span.clone().unwrap();
        let href_offset = text.rfind("https://dup.test/x").unwrap() as u64;
        assert_eq!(span.start, href_offset);
        assert!(span.start > 18, "span should point at the href, not text");
        assert_eq!(
            &doc.bytes[span.start as usize..span.end as usize],
            found.url.as_bytes()
        );
    }
    #[test]
    fn html_decodes_entities_in_url() {
        let found = only(
            DocFormat::Html,
            "<a href=\"https://example.test/a?x=1&amp;y=2\">x</a>",
        );
        assert_eq!(found.url, "https://example.test/a?x=1&y=2");
    }
    #[test]
    fn markdown_reference_link_is_not_double_counted() {
        let doc = document(
            DocFormat::Markdown,
            "[one][r] [two][r]\n\n[r]: https://example.test/x",
        );
        let links = extract(&doc).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.test/x");
    }
    #[test]
    fn markdown_url_label_is_not_double_counted() {
        let text = "[https://example.test/x](https://example.test/x)";
        let doc = document(DocFormat::Markdown, text);
        let links = extract(&doc).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.test/x");
        // The surviving occurrence must point at the destination, not the label.
        let span = links[0].source.byte_span.clone().unwrap();
        let dest = text.rfind("https://example.test/x").unwrap() as u64;
        assert_eq!(span.start, dest);
        assert_eq!(
            &doc.bytes[span.start as usize..span.end as usize],
            b"https://example.test/x"
        );
    }
    #[test]
    fn markdown_escaped_destination_span_is_raw_not_whole_link() {
        let text = r"[x](https://example.test/a\(b\))";
        let doc = document(DocFormat::Markdown, text);
        let found = extract(&doc).unwrap().pop().unwrap();
        assert_eq!(found.url, "https://example.test/a(b)");
        let span = found.source.byte_span.clone().unwrap();
        assert_eq!(
            &doc.bytes[span.start as usize..span.end as usize],
            br"https://example.test/a\(b\)"
        );
    }
    #[test]
    fn markdown_balanced_parens_destination_has_exact_span() {
        let text = "[x](https://example.test/a(b)c)";
        let doc = document(DocFormat::Markdown, text);
        let found = extract(&doc).unwrap().pop().unwrap();
        assert_eq!(found.url, "https://example.test/a(b)c");
        let span = found.source.byte_span.clone().unwrap();
        assert_eq!(
            &doc.bytes[span.start as usize..span.end as usize],
            b"https://example.test/a(b)c"
        );
    }
    #[test]
    fn markdown_angle_bracket_destination_has_exact_span() {
        let text = "[x](<https://example.test/a b>)";
        let doc = document(DocFormat::Markdown, text);
        let found = extract(&doc).unwrap().pop().unwrap();
        assert_eq!(found.url, "https://example.test/a b");
        let span = found.source.byte_span.clone().unwrap();
        assert_eq!(
            &doc.bytes[span.start as usize..span.end as usize],
            b"https://example.test/a b"
        );
    }
    #[test]
    fn markdown_reference_span_targets_the_definition_not_earlier_bare_url() {
        let text =
            "see https://example.test/x here\n\nuse [it][r]\n\n[r]: https://example.test/x\n";
        let doc = document(DocFormat::Markdown, text);
        let links = extract(&doc).unwrap();
        let reference = links
            .iter()
            .find(|found| {
                found
                    .source
                    .byte_span
                    .as_ref()
                    .is_some_and(|span| span.start as usize > text.find("[r]:").unwrap())
            })
            .expect("reference link resolved to its definition");
        assert_eq!(reference.url, "https://example.test/x");
    }
    #[test]
    fn markdown_inline_destination_with_title_excludes_the_title() {
        let text = "[x](https://example.test/a \"the title\")";
        let doc = document(DocFormat::Markdown, text);
        let found = extract(&doc).unwrap().pop().unwrap();
        assert_eq!(found.url, "https://example.test/a");
        let span = found.source.byte_span.clone().unwrap();
        assert_eq!(
            &doc.bytes[span.start as usize..span.end as usize],
            b"https://example.test/a"
        );
    }
    #[test]
    fn markdown_inline_destination_decodes_entities() {
        let text = "[x](https://example.test/a?x=1&amp;y=2)";
        let doc = document(DocFormat::Markdown, text);
        let found = extract(&doc).unwrap().pop().unwrap();
        assert_eq!(found.url, "https://example.test/a?x=1&y=2");
        let span = found.source.byte_span.clone().unwrap();
        assert_eq!(
            &doc.bytes[span.start as usize..span.end as usize],
            b"https://example.test/a?x=1&amp;y=2"
        );
    }
    #[test]
    fn markdown_reference_destination_decodes_entities() {
        let text = "[it][r]\n\n[r]: https://example.test/a?x=1&amp;y=2\n";
        let doc = document(DocFormat::Markdown, text);
        let found = extract(&doc).unwrap().pop().unwrap();
        assert_eq!(found.url, "https://example.test/a?x=1&y=2");
    }
    #[test]
    fn markdown_label_containing_bracket_paren_finds_real_destination() {
        let text = "[a ](b) c](https://example.test/x)";
        let doc = document(DocFormat::Markdown, text);
        let found = extract(&doc).unwrap().pop().unwrap();
        assert_eq!(found.url, "https://example.test/x");
        let span = found.source.byte_span.clone().unwrap();
        assert_eq!(
            &doc.bytes[span.start as usize..span.end as usize],
            b"https://example.test/x"
        );
    }
    #[test]
    fn markdown_crlf_reference_definition_has_exact_span() {
        let text = "line one\r\n\r\nuse [it][r]\r\n\r\n[r]: https://example.test/a?x=1&amp;y=2\r\n";
        let doc = document(DocFormat::Markdown, text);
        let found = extract(&doc).unwrap().pop().unwrap();
        assert_eq!(found.url, "https://example.test/a?x=1&y=2");
        let span = found.source.byte_span.clone().unwrap();
        assert_eq!(
            &doc.bytes[span.start as usize..span.end as usize],
            b"https://example.test/a?x=1&amp;y=2"
        );
    }
    #[test]
    fn markdown_image_destination_is_extracted_with_semantic_url() {
        let text = "![alt](https://example.test/a?x=1&amp;y=2)";
        let doc = document(DocFormat::Markdown, text);
        let found = extract(&doc).unwrap().pop().unwrap();
        assert_eq!(found.url, "https://example.test/a?x=1&y=2");
        let span = found.source.byte_span.clone().unwrap();
        assert_eq!(
            &doc.bytes[span.start as usize..span.end as usize],
            b"https://example.test/a?x=1&amp;y=2"
        );
    }
    #[test]
    fn markdown_autolink_is_extracted_from_inner_bytes() {
        let text = "see <https://example.test/auto> here";
        let doc = document(DocFormat::Markdown, text);
        let found = extract(&doc).unwrap().pop().unwrap();
        assert_eq!(found.url, "https://example.test/auto");
        let span = found.source.byte_span.clone().unwrap();
        assert_eq!(
            &doc.bytes[span.start as usize..span.end as usize],
            b"https://example.test/auto"
        );
    }
    #[test]
    fn markdown_named_entity_in_destination_uses_pulldown_semantics() {
        let text = "[x](https://example.test/f&ouml;&ouml;)";
        let doc = document(DocFormat::Markdown, text);
        let found = extract(&doc).unwrap().pop().unwrap();
        assert_eq!(found.url, "https://example.test/f\u{f6}\u{f6}");
    }
    #[test]
    fn markdown_unicase_folded_reference_label_resolves() {
        let text = "[x][MASSE]\n\n[Ma\u{df}e]: https://example.test/x\n";
        let doc = document(DocFormat::Markdown, text);
        let found = extract(&doc).unwrap().pop().unwrap();
        assert_eq!(found.url, "https://example.test/x");
    }
    #[test]
    fn markdown_reference_title_url_is_not_a_second_link() {
        let text = "[it][r]\n\n[r]: https://example.test/x \"https://example.test/title\"\n";
        let doc = document(DocFormat::Markdown, text);
        let links = extract(&doc).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.test/x");
    }
    #[test]
    fn markdown_title_url_suppressed_even_with_non_http_destination() {
        let text = "[a][x]\n\n[x]: /relative \"https://title.test/t\"\n";
        let doc = document(DocFormat::Markdown, text);
        let links = extract(&doc).unwrap();
        assert_eq!(
            links.len(),
            1,
            "reference destination must be reported once"
        );
        assert_eq!(links[0].url, "/relative");
        assert!(
            links.iter().all(|link| link.url != "https://title.test/t"),
            "definition title URL must not be reported, got {links:?}"
        );
    }
    #[test]
    fn markdown_root_relative_reference_destinations_are_emitted() {
        let doc = document(
            DocFormat::Markdown,
            "[one][root] [two][] [shortcut]\n\n[root]: /one\n[two]: /two\n[shortcut]: /three\n",
        );
        let links = extract(&doc).unwrap();
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].url, "/one");
        assert_eq!(links[1].url, "/two");
        assert_eq!(links[2].url, "/three");
    }
    #[test]
    fn markdown_reference_local_destinations_are_emitted_from_shared_definition() {
        let doc = document(
            DocFormat::Markdown,
            "[one][missing] [two][missing] [collapsed][] [shortcut]\n\n[missing]: absent.md\n[collapsed]: also-absent.md\n[shortcut]: shortcut-absent.md\n",
        );
        let links = extract(&doc).unwrap();
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].url, "absent.md");
        assert_eq!(links[1].url, "also-absent.md");
        assert_eq!(links[2].url, "shortcut-absent.md");
    }
    #[test]
    fn html_unquoted_mixed_case_attributes_have_valid_spans() {
        let text = "<LINK HREF='https://a.test/x'><ScRiPt SrC=https://b.test/y></sCrIpT>";
        let doc = document(DocFormat::Html, text);
        let links = extract(&doc).unwrap();
        assert_eq!(links.len(), 2);
        for found in &links {
            let span = found.source.byte_span.clone().unwrap();
            assert!(span.start <= span.end);
            assert_eq!(
                &doc.bytes[span.start as usize..span.end as usize],
                found.url.as_bytes()
            );
        }
    }

    #[test]
    fn pdf_extracts_annotation_and_text_links_with_page_locations() {
        let links = extract(&binary_document(DocFormat::Pdf, pdf())).unwrap();
        assert!(
            links
                .iter()
                .any(|link| link.url == "https://annotation.test/x"
                    && link.source.location
                        == Location::Pdf {
                            page: 1,
                            annotation: Some(0)
                        })
        );
        assert!(links.iter().any(|link| link.url == "https://text.test/x"
            && link.source.location
                == Location::Pdf {
                    page: 1,
                    annotation: None
                }));
        assert!(links.iter().all(|link| link.source.byte_span.is_none()));
    }

    #[test]
    fn pdf_extracts_indirect_and_encoded_uri_actions_but_not_other_actions() {
        let bytes = pdf_with_objects(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Annots [4 0 R 5 0 R 6 0 R] >>".into(),
            "<< /Type /Annot /Subtype /Link /A 7 0 R >>".into(),
            "<< /Type /Annot /Subtype /Link /A << /S /URI /URI 8 0 R >> >>".into(),
            "<< /Type /Annot /Subtype /Link /A << /S /GoTo /URI (https://ignored.test/x) >> >>".into(),
            "<< /S /URI /URI (https://indirect.test/x) >>".into(),
            "<FEFF00680074007400700073003A002F002F00750074006600310036002E0074006500730074002F0078>".into(),
        ]);
        let links = extract(&binary_document(DocFormat::Pdf, bytes)).unwrap();
        assert!(
            links
                .iter()
                .any(|link| link.url == "https://indirect.test/x")
        );
        assert!(links.iter().any(|link| link.url == "https://utf16.test/x"));
        assert!(
            !links
                .iter()
                .any(|link| link.url == "https://ignored.test/x")
        );
    }

    #[test]
    fn pdf_doc_encoding_uses_pdf_mapping_and_rejects_undefined_bytes() {
        assert_eq!(
            pdf_string(b"https://example.test/a\x85b"),
            Some("https://example.test/a\u{2013}b".into())
        );
        assert_eq!(pdf_string(b"https://example.test/\x7f"), None);
        assert_eq!(pdf_string(b"\xfe\xff\0h\0t\0t\0p\0s\0:\xff"), None);
    }

    #[test]
    fn pdf_annotation_survives_page_text_extraction_failure() {
        let bytes = pdf_with_objects(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Contents 4 0 R /Annots [5 0 R] >>".into(),
            "<< /Length 8 >>\nstream\nBT Tf ET\nendstream".into(),
            "<< /Type /Annot /Subtype /Link /A << /S /URI /URI (https://annotation-survives.test/x) >> >>".into(),
        ]);
        let pdf = lopdf::Document::load_mem(&bytes).unwrap();
        assert!(pdf.extract_text(&[1]).is_err());
        let links = extract(&binary_document(DocFormat::Pdf, bytes)).unwrap();
        assert!(
            links
                .iter()
                .any(|link| link.url == "https://annotation-survives.test/x")
        );
    }

    #[test]
    fn zip_member_limit_accepts_boundary_and_rejects_overage() {
        let bytes = zip(&[("part.xml", "12345678")]);
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes.as_slice())).unwrap();
        assert_eq!(
            zip_text_with_limit(&mut archive, "part.xml", 8).unwrap(),
            "12345678"
        );
        let error = zip_text_with_limit(&mut archive, "part.xml", 7)
            .unwrap_err()
            .0;
        assert!(error.contains("part.xml"));
        assert!(error.contains("7 byte limit"));
    }

    #[test]
    fn relationships_parse_external_targets() {
        let relations = relationships(
            "part.rels",
            r#"<Relationships><Relationship Id="r1" TargetMode="External" Target="https://example.test/x"/></Relationships>"#,
            true,
        )
        .unwrap();
        assert_eq!(
            relations.get("r1"),
            Some(&"https://example.test/x".to_owned())
        );
    }

    #[test]
    fn malformed_present_relationship_parts_fail_with_part_name() {
        let docx = zip(&[
            ("word/document.xml", "<document/>"),
            (
                "word/_rels/document.xml.rels",
                "<Relationships><Relationship",
            ),
        ]);
        let xlsx = zip(&[
            (
                "xl/workbook.xml",
                "<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets><sheet name=\"S\" r:id=\"r1\"/></sheets></workbook>",
            ),
            (
                "xl/_rels/workbook.xml.rels",
                "<Relationships><Relationship Id=\"r1\" Target=\"worksheets/sheet1.xml\"/></Relationships>",
            ),
            ("xl/worksheets/sheet1.xml", "<worksheet/>"),
            (
                "xl/worksheets/_rels/sheet1.xml.rels",
                "<Relationships><Relationship",
            ),
        ]);
        let pptx = zip(&[
            (
                "ppt/slides/slide1.xml",
                "<p:sld xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\"/>",
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                "<Relationships><Relationship",
            ),
        ]);
        for (format, bytes, part) in [
            (DocFormat::Docx, docx, "word/_rels/document.xml.rels"),
            (DocFormat::Xlsx, xlsx, "xl/worksheets/_rels/sheet1.xml.rels"),
            (DocFormat::Pptx, pptx, "ppt/slides/_rels/slide1.xml.rels"),
        ] {
            assert!(
                extract(&binary_document(format, bytes))
                    .unwrap_err()
                    .0
                    .contains(part)
            );
        }
    }

    #[test]
    fn docx_links_are_anchored_to_their_paragraphs() {
        let bytes = zip(&[
            (
                "word/_rels/document.xml.rels",
                r#"<Relationships><Relationship Id="rId1" TargetMode="External" Target="https://relationship.test/x"/></Relationships>"#,
            ),
            (
                "word/document.xml",
                r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:hyperlink r:id="rId1"/></w:p><w:p><w:r><w:instrText> HYPERLINK &quot;https://field.test/x&quot; </w:instrText></w:r></w:p></w:body></w:document>"#,
            ),
        ]);
        let links = extract(&binary_document(DocFormat::Docx, bytes)).unwrap();
        assert!(
            links
                .iter()
                .any(|link| link.url == "https://relationship.test/x"
                    && link.source.location == Location::Docx { paragraph: 1 })
        );
        assert!(
            links.iter().any(|link| link.url == "https://field.test/x"
                && link.source.location == Location::Docx { paragraph: 2 }),
            "{links:?}"
        );
    }

    #[test]
    fn docx_collects_split_and_simple_fields_without_relationships() {
        let bytes = zip(&[(
            "word/document.xml",
            r#"<x:document xmlns:x="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><x:body><x:p><x:r><x:fldChar x:fldCharType="begin"/></x:r><x:r><x:instrText> HYPER</x:instrText></x:r><x:r><x:instrText>LINK &quot;https://split.test/x&quot;</x:instrText></x:r><x:r><x:fldChar x:fldCharType="end"/></x:r></x:p><x:p><x:fldSimple x:instr="HYPERLINK &quot;https://simple.test/x&quot;"/></x:p><foreign:p xmlns:foreign="foreign"/></x:body></x:document>"#,
        )]);
        let links = extract(&binary_document(DocFormat::Docx, bytes)).unwrap();
        assert!(links.iter().any(|link| link.url == "https://split.test/x"
            && link.source.location == Location::Docx { paragraph: 1 }));
        assert!(links.iter().any(|link| link.url == "https://simple.test/x"
            && link.source.location == Location::Docx { paragraph: 2 }));
    }

    #[test]
    fn docx_namespace_parsing_accepts_alternate_prefix_once_and_rejects_foreign_prefix() {
        let alternate = zip(&[(
            "word/document.xml",
            r#"<z:document xmlns:z="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><z:body><z:p><z:fldSimple z:instr="HYPERLINK &quot;https://alternate.test/x&quot;"/></z:p></z:body></z:document>"#,
        )]);
        let links = extract(&binary_document(DocFormat::Docx, alternate)).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].source.location, Location::Docx { paragraph: 1 });

        let foreign = zip(&[(
            "word/document.xml",
            r#"<w:document xmlns:w="foreign"><w:body><w:p><w:fldSimple w:instr="HYPERLINK &quot;https://foreign.test/x&quot;"/></w:p></w:body></w:document>"#,
        )]);
        assert!(
            extract(&binary_document(DocFormat::Docx, foreign))
                .unwrap()
                .is_empty()
        );

        let one_letter = zip(&[(
            "word/document.xml",
            r#"<q:document xmlns:q="w"><q:body><q:p><q:fldSimple q:instr="HYPERLINK &quot;https://one-letter.test/x&quot;"/></q:p></q:body></q:document>"#,
        )]);
        assert!(
            extract(&binary_document(DocFormat::Docx, one_letter))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn xlsx_allows_workbook_without_relationships() {
        let bytes = zip(&[("xl/workbook.xml", "<workbook><sheets/></workbook>")]);
        assert!(
            extract(&binary_document(DocFormat::Xlsx, bytes))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn xlsx_resolves_absolute_and_dot_segment_sheet_targets() {
        let bytes = zip(&[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Absolute" r:id="r1"/><sheet name="Dot" r:id="r2"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="r1" Target="/xl/worksheets/sheet1.xml"/><Relationship Id="r2" Target="./worksheets/../worksheets/sheet2.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row><c r="A1"><f>HYPERLINK(&quot;https://absolute.test/x&quot;)</f></c></row></sheetData></worksheet>"#,
            ),
            (
                "xl/worksheets/sheet2.xml",
                r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row><c r="B2"><f>HYPERLINK(&quot;https://dot.test/x&quot;)</f></c></row></sheetData></worksheet>"#,
            ),
        ]);
        let links = extract(&binary_document(DocFormat::Xlsx, bytes)).unwrap();
        assert!(
            links
                .iter()
                .any(|link| link.url == "https://absolute.test/x")
        );
        assert!(links.iter().any(|link| link.url == "https://dot.test/x"));
    }

    #[test]
    fn pptx_uses_presentation_order_and_allows_missing_slide_relationships() {
        let bytes = zip(&[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:sldIdLst><p:sldId r:id="second"/><p:sldId r:id="first"/></p:sldIdLst></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="first" Target="slides/slide1.xml"/><Relationship Id="second" Target="slides/slide2.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"/>"#,
            ),
            (
                "ppt/slides/slide2.xml",
                r#"<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:cNvPr><a:hlinkClick r:id="link"/></p:cNvPr></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide2.xml.rels",
                r#"<Relationships><Relationship Id="link" TargetMode="External" Target="https://ordered.test/x"/></Relationships>"#,
            ),
        ]);
        let links = extract(&binary_document(DocFormat::Pptx, bytes)).unwrap();
        assert_eq!(links[0].source.location, Location::Pptx { slide: 1 });
    }

    #[test]
    fn xlsx_and_pptx_links_have_cell_and_slide_locations() {
        let xlsx = zip(&[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Links" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData><row><c r="B2"><f>HYPERLINK(&quot;https://formula.test/x&quot;)</f></c></row></sheetData><hyperlinks><hyperlink ref="A1" r:id="rId1"/></hyperlinks></worksheet>"#,
            ),
            (
                "xl/worksheets/_rels/sheet1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" TargetMode="External" Target="https://cell.test/x"/></Relationships>"#,
            ),
        ]);
        let links = extract(&binary_document(DocFormat::Xlsx, xlsx)).unwrap();
        assert!(
            links.iter().any(|link| link.url == "https://cell.test/x"
                && link.source.location
                    == Location::Xlsx {
                        sheet: "Links".into(),
                        cell: "A1".into()
                    }),
            "{links:?}"
        );
        assert!(links.iter().any(|link| link.url == "https://formula.test/x"
            && link.source.location
                == Location::Xlsx {
                    sheet: "Links".into(),
                    cell: "B2".into()
                }));
        let pptx = zip(&[
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:cNvPr><a:hlinkClick r:id="rId1"/></p:cNvPr></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" TargetMode="External" Target="https://slide.test/x"/></Relationships>"#,
            ),
        ]);
        let links = extract(&binary_document(DocFormat::Pptx, pptx)).unwrap();
        assert_eq!(links[0].source.location, Location::Pptx { slide: 1 });
        assert_eq!(links[0].url, "https://slide.test/x");
    }

    #[test]
    fn pptx_ignores_foreign_id_attributes_and_accepts_drawingml_hyperlinks() {
        let bytes = zip(&[
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:f="foreign"><f:thing r:id="bad"/><a:hlinkClick r:id="good"/></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="bad" TargetMode="External" Target="https://foreign.test/x"/><Relationship Id="good" TargetMode="External" Target="https://real.test/x"/></Relationships>"#,
            ),
        ]);
        let links = extract(&binary_document(DocFormat::Pptx, bytes)).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://real.test/x");
    }

    #[test]
    fn ooxml_rejects_one_letter_namespaces_and_unqualified_relationship_ids() {
        let pptx = zip(&[
            (
                "ppt/slides/slide1.xml",
                r#"<q:sld xmlns:q="a" xmlns:s="r"><q:hlinkClick s:id="one-letter"/></q:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="one-letter" TargetMode="External" Target="https://one-letter.test/x"/></Relationships>"#,
            ),
        ]);
        assert!(
            extract(&binary_document(DocFormat::Pptx, pptx))
                .unwrap()
                .is_empty()
        );

        let docx = zip(&[
            (
                "word/document.xml",
                r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:hyperlink id="unqualified"/></w:p></w:body></w:document>"#,
            ),
            (
                "word/_rels/document.xml.rels",
                r#"<Relationships><Relationship Id="unqualified" TargetMode="External" Target="https://unqualified.test/x"/></Relationships>"#,
            ),
        ]);
        assert!(
            extract(&binary_document(DocFormat::Docx, docx))
                .unwrap()
                .is_empty()
        );
    }
}
