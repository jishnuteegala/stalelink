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
use quick_xml::{Reader, events::Event as XmlEvent};
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
// `<...>` destination: spans from `<` to the first unescaped `>`.
fn angle_dest(open: usize, rest: &str) -> Option<Range<usize>> {
    let mut escaped = false;
    for (i, c) in rest.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '>' && i > 0 {
            return Some(open..open + i + 1);
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
            if !is_http(&dest_url) {
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
                if attribute.name() != attr || !is_http(attribute.value()) {
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
        let mut links = Vec::new();
        for (page_number, page_id) in pdf.get_pages() {
            let annotations = pdf
                .get_page_annotations(page_id)
                .map_err(|error| ExtractError(error.to_string()))?;
            for (index, annotation) in annotations.iter().enumerate() {
                let Ok(action) = annotation.get(b"A").and_then(lopdf::Object::as_dict) else {
                    continue;
                };
                let Ok(uri) = action.get(b"URI").and_then(lopdf::Object::as_str) else {
                    continue;
                };
                let Ok(uri) = std::str::from_utf8(uri) else {
                    continue;
                };
                if is_http(uri) {
                    links.push(binary_link(
                        doc,
                        uri,
                        Location::Pdf {
                            page: page_number,
                            annotation: Some(index as u32),
                        },
                    ));
                }
            }
            let text = pdf
                .extract_text(&[page_number])
                .map_err(|error| ExtractError(error.to_string()))?;
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

fn zip_text(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
) -> Result<String, ExtractError> {
    let mut file = archive
        .by_name(name)
        .map_err(|error| ExtractError(error.to_string()))?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|error| ExtractError(error.to_string()))?;
    Ok(text)
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

fn relationships(xml: &str, external_only: bool) -> HashMap<String, String> {
    let mut reader = Reader::from_str(xml);
    let mut relationships = HashMap::new();
    loop {
        match reader.read_event() {
            Ok(XmlEvent::Empty(event)) | Ok(XmlEvent::Start(event))
                if event.name().as_ref().ends_with(b"Relationship") =>
            {
                let attributes = attributes(&event);
                if (!external_only
                    || attributes
                        .get("TargetMode")
                        .is_some_and(|mode| mode == "External"))
                    && let (Some(id), Some(target)) =
                        (attributes.get("Id"), attributes.get("Target"))
                    && (!external_only || is_http(target))
                {
                    relationships.insert(id.clone(), target.clone());
                }
            }
            Ok(XmlEvent::Eof) | Err(_) => break,
            _ => {}
        }
    }
    relationships
}

fn attribute<'a>(attributes: &'a HashMap<String, String>, suffix: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find_map(|(key, value)| key.ends_with(suffix).then_some(value.as_str()))
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
    let relations = relationships(&zip_text(archive, "word/_rels/document.xml.rels")?, true);
    let document = zip_text(archive, "word/document.xml")?.replace("&quot;", "\"");
    let mut reader = Reader::from_str(&document);
    let mut links = Vec::new();
    let mut paragraph = 0;
    loop {
        match reader.read_event() {
            Ok(XmlEvent::Start(event)) | Ok(XmlEvent::Empty(event)) => {
                let name = event.name().as_ref().to_vec();
                let attributes = attributes(&event);
                if name.ends_with(b"p") {
                    paragraph += 1;
                }
                if name.ends_with(b"hyperlink")
                    && let Some(id) = attribute(&attributes, ":id")
                    && let Some(url) = relations.get(id)
                {
                    links.push(binary_link(doc, url, Location::Docx { paragraph }));
                }
            }
            Ok(XmlEvent::Text(event)) => {
                let text = event
                    .xml_content()
                    .map_err(|error| ExtractError(error.to_string()))?;
                if let Some(url) = link_from_field(&text) {
                    links.push(binary_link(doc, url, Location::Docx { paragraph }));
                }
            }
            Ok(XmlEvent::Eof) => break,
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
    let workbook_relations =
        relationships(&zip_text(archive, "xl/_rels/workbook.xml.rels")?, false);
    let mut reader = Reader::from_str(&workbook);
    let mut sheets = Vec::new();
    loop {
        match reader.read_event() {
            Ok(XmlEvent::Empty(event)) | Ok(XmlEvent::Start(event))
                if event.name().as_ref().ends_with(b"sheet") =>
            {
                let attributes = attributes(&event);
                if let (Some(name), Some(id)) =
                    (attributes.get("name"), attribute(&attributes, ":id"))
                    && let Some(target) = workbook_relations.get(id)
                {
                    sheets.push((name.clone(), format!("xl/{target}")));
                }
            }
            Ok(XmlEvent::Eof) => break,
            _ => {}
        }
    }
    let mut links = Vec::new();
    for (sheet, path) in sheets {
        let rel_path = path.replacen("xl/worksheets/", "xl/worksheets/_rels/", 1) + ".rels";
        let relations = zip_text(archive, &rel_path)
            .map(|xml| relationships(&xml, true))
            .unwrap_or_default();
        let xml = zip_text(archive, &path)?.replace("&quot;", "\"");
        let mut reader = Reader::from_str(&xml);
        let mut cell = String::new();
        let mut formula = false;
        loop {
            match reader.read_event() {
                Ok(XmlEvent::Start(event)) => {
                    let name = event.name().as_ref().to_vec();
                    let attributes = attributes(&event);
                    if name.ends_with(b"c") {
                        cell = attributes.get("r").cloned().unwrap_or_default();
                    }
                    if name.ends_with(b"f") {
                        formula = true;
                    }
                    if name.ends_with(b"hyperlink")
                        && let (Some(reference), Some(id)) =
                            (attributes.get("ref"), attribute(&attributes, ":id"))
                        && let Some(url) = relations.get(id)
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
                Ok(XmlEvent::Empty(event)) if event.name().as_ref().ends_with(b"hyperlink") => {
                    let attributes = attributes(&event);
                    if let (Some(reference), Some(id)) =
                        (attributes.get("ref"), attribute(&attributes, ":id"))
                        && let Some(url) = relations.get(id)
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
                Ok(XmlEvent::Text(event)) if formula => {
                    let text = event
                        .xml_content()
                        .map_err(|error| ExtractError(error.to_string()))?;
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
                Ok(XmlEvent::End(event)) if event.name().as_ref().ends_with(b"f") => {
                    formula = false
                }
                Ok(XmlEvent::Eof) => break,
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
    let mut slides: Vec<String> = archive
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
    let mut links = Vec::new();
    for (index, path) in slides.iter().enumerate() {
        let rel_path = path.replacen("ppt/slides/", "ppt/slides/_rels/", 1) + ".rels";
        let relations = relationships(&zip_text(archive, &rel_path)?, true);
        let slide = zip_text(archive, path)?;
        let mut reader = Reader::from_str(&slide);
        loop {
            match reader.read_event() {
                Ok(XmlEvent::Empty(event)) | Ok(XmlEvent::Start(event)) => {
                    let attributes = attributes(&event);
                    if let Some(id) = attribute(&attributes, ":id")
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
                Ok(XmlEvent::Eof) => break,
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
            b"<https://example.test/a b>"
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
        assert!(
            links.is_empty(),
            "non-http destination and its title must not be reported, got {links:?}"
        );
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
    fn docx_links_are_anchored_to_their_paragraphs() {
        let bytes = zip(&[
            (
                "word/_rels/document.xml.rels",
                r#"<Relationships><Relationship Id="rId1" TargetMode="External" Target="https://relationship.test/x"/></Relationships>"#,
            ),
            (
                "word/document.xml",
                r#"<w:document xmlns:w="w" xmlns:r="r"><w:body><w:p><w:hyperlink r:id="rId1"/></w:p><w:p><w:r><w:instrText> HYPERLINK &quot;https://field.test/x&quot; </w:instrText></w:r></w:p></w:body></w:document>"#,
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
    fn xlsx_and_pptx_links_have_cell_and_slide_locations() {
        let xlsx = zip(&[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Links" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet xmlns:r="r"><sheetData><row><c r="B2"><f>HYPERLINK(&quot;https://formula.test/x&quot;)</f></c></row></sheetData><hyperlinks><hyperlink ref="A1" r:id="rId1"/></hyperlinks></worksheet>"#,
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
                r#"<p:sld xmlns:p="p" xmlns:r="r"><p:cNvPr><a:hlinkClick r:id="rId1"/></p:cNvPr></p:sld>"#,
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
}
