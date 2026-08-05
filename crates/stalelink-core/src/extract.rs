use std::{ops::Range, path::PathBuf};

use html5tokenizer::{NaiveParser, Token, TracingEmitter, offset::PosTrackingReader, trace::Trace};
use linkify::{LinkFinder, LinkKind};
use pulldown_cmark::{Event, Parser, Tag};
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

fn text_of(doc: &SourceDocument) -> Result<&str, ExtractError> {
    std::str::from_utf8(&doc.bytes).map_err(|e| ExtractError(e.to_string()))
}

fn locate(text: &str, needle: &str, within: &Range<usize>) -> Option<Range<usize>> {
    let offset = text[within.clone()].find(needle)? + within.start;
    Some(offset..offset + needle.len())
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
        for (event, span) in Parser::new(text).into_offset_iter() {
            if let Event::Start(Tag::Link { dest_url, .. }) = event {
                link_ranges.push(span.clone());
                // pulldown gives the whole `[text](url)` span and a decoded
                // destination; find the destination's own raw span within the
                // event, preferring the raw (possibly escaped) spelling.
                if is_http(&dest_url) {
                    // Inline links carry the destination inside the event span.
                    // Reference links do not: the URL text lives only at the
                    // `[label]: url` definition, so fall back to locating it in
                    // the whole document and dedupe uses that share it.
                    let dest_span = locate(text, &dest_url, &span)
                        .or_else(|| locate(text, &dest_url, &(0..text.len())))
                        .unwrap_or(span.clone());
                    links.push(link(doc, &dest_url, dest_span));
                }
            }
        }
        // Bare URLs in prose are not link events; linkify them, but skip any
        // that fall inside a parsed link (destination or label text) so a
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
                if let Some(span) = trace.attribute_traces[index].value_span() {
                    links.push(link(doc, attribute.value(), span));
                }
            }
        }
        Ok(links)
    }
}
pub fn extract(doc: &SourceDocument) -> Result<Vec<FoundLink>, ExtractError> {
    match doc.format {
        DocFormat::Markdown => MarkdownExtractor.extract(doc),
        DocFormat::Html => HtmlExtractor.extract(doc),
        DocFormat::Text => TextExtractor.extract(doc),
        _ => Ok(vec![]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let doc = document(
            DocFormat::Markdown,
            "[https://example.test/x](https://example.test/x)",
        );
        let links = extract(&doc).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://example.test/x");
    }
}
