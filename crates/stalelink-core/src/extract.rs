use std::{ops::Range, path::PathBuf};

use linkify::{LinkFinder, LinkKind};
use lol_html::{RewriteStrSettings, element, rewrite_str};
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

fn found(doc: &SourceDocument, url: &str, span: Range<usize>) -> FoundLink {
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

// Locate a URL's exact byte span, starting the search at or after `from`.
fn locate(text: &str, url: &str, from: usize) -> Option<Range<usize>> {
    let offset = text[from..].find(url)? + from;
    Some(offset..offset + url.len())
}

impl Extractor for TextExtractor {
    fn extract(&self, doc: &SourceDocument) -> Result<Vec<FoundLink>, ExtractError> {
        let text = text_of(doc)?;
        let mut finder = LinkFinder::new();
        finder.kinds(&[LinkKind::Url]);
        Ok(finder
            .links(text)
            .filter(|link| is_http(link.as_str()))
            .map(|link| found(doc, link.as_str(), link.start()..link.end()))
            .collect())
    }
}
impl Extractor for MarkdownExtractor {
    fn extract(&self, doc: &SourceDocument) -> Result<Vec<FoundLink>, ExtractError> {
        let text = text_of(doc)?;
        let mut links = Vec::new();
        for (event, span) in Parser::new(text).into_offset_iter() {
            if let Event::Start(Tag::Link { dest_url, .. }) = event
                && is_http(&dest_url)
                && let Some(found_span) = locate(text, &dest_url, span.start)
            {
                links.push(found(doc, &dest_url, found_span));
            }
        }
        // Bare URLs in prose are not link tags; linkify the text so autolinks
        // and plain URLs are covered without double-counting inline links.
        let mut finder = LinkFinder::new();
        finder.kinds(&[LinkKind::Url]);
        for link in finder.links(text).filter(|link| is_http(link.as_str())) {
            if links.iter().any(|existing| covers(existing, link.start())) {
                continue;
            }
            links.push(found(doc, link.as_str(), link.start()..link.end()));
        }
        links.sort_by_key(|link| link.source.byte_span.as_ref().map(|span| span.start));
        Ok(links)
    }
}
impl Extractor for HtmlExtractor {
    fn extract(&self, doc: &SourceDocument) -> Result<Vec<FoundLink>, ExtractError> {
        let text = text_of(doc)?;
        // lol_html parses attribute values correctly but does not expose their
        // absolute byte offsets, so each value is located in the raw bytes.
        let values = std::cell::RefCell::new(Vec::new());
        let collect = |value: Option<String>| {
            if let Some(value) = value
                && is_http(&value)
            {
                values.borrow_mut().push(value);
            }
        };
        let settings = RewriteStrSettings::new()
            .append_element_content_handler(element!("a[href]", |el| {
                collect(el.get_attribute("href"));
                Ok(())
            }))
            .append_element_content_handler(element!("link[href]", |el| {
                collect(el.get_attribute("href"));
                Ok(())
            }))
            .append_element_content_handler(element!("img[src]", |el| {
                collect(el.get_attribute("src"));
                Ok(())
            }))
            .append_element_content_handler(element!("script[src]", |el| {
                collect(el.get_attribute("src"));
                Ok(())
            }));
        rewrite_str(text, settings).map_err(|e| ExtractError(e.to_string()))?;
        let mut links = Vec::new();
        let mut from = 0;
        for value in values.into_inner() {
            if let Some(span) = locate(text, &value, from) {
                from = span.end;
                links.push(found(doc, &value, span));
            }
        }
        Ok(links)
    }
}
fn covers(link: &FoundLink, offset: usize) -> bool {
    link.source
        .byte_span
        .as_ref()
        .is_some_and(|span| span.start as usize <= offset && offset < span.end as usize)
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
    fn assert_link(format: DocFormat, text: &str, url: &str, line: u32, column: u32) {
        let doc = document(format, text);
        let links = extract(&doc).unwrap();
        let link = links.first().unwrap();
        let span = link.source.byte_span.clone().unwrap();
        assert_eq!(link.url, url);
        assert_eq!(
            &doc.bytes[span.start as usize..span.end as usize],
            link.url.as_bytes()
        );
        assert_eq!(link.source.location, Location::Text { line, column });
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
}
