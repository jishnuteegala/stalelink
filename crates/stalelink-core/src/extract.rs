use std::{ops::Range, path::PathBuf};

use regex::Regex;

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
    Location::Text {
        line,
        column: (offset - start + 1) as u32,
    }
}

fn links(doc: &SourceDocument, pattern: &str) -> Result<Vec<FoundLink>, ExtractError> {
    let text = std::str::from_utf8(&doc.bytes).map_err(|e| ExtractError(e.to_string()))?;
    let regex = Regex::new(pattern).expect("constant URL pattern");
    Ok(regex
        .find_iter(text)
        .map(|found| {
            let span = found.start()..found.end();
            FoundLink {
                url: found.as_str().to_owned(),
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
        })
        .collect())
}

impl Extractor for TextExtractor {
    fn extract(&self, doc: &SourceDocument) -> Result<Vec<FoundLink>, ExtractError> {
        links(doc, r#"https?://[^\s<>"']+"#)
    }
}
impl Extractor for MarkdownExtractor {
    fn extract(&self, doc: &SourceDocument) -> Result<Vec<FoundLink>, ExtractError> {
        links(doc, r#"https?://[^\s<>"')\]]+"#)
    }
}
impl Extractor for HtmlExtractor {
    fn extract(&self, doc: &SourceDocument) -> Result<Vec<FoundLink>, ExtractError> {
        // Attribute-value spans are located in raw bytes because lol_html does not expose them.
        links(doc, r#"https?://[^\s<>"']+"#)
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
    fn assert_link(format: DocFormat, text: &str, line: u32, column: u32) {
        let doc = document(format, text);
        let links = extract(&doc).unwrap();
        let link = links.first().unwrap();
        let span = link.source.byte_span.clone().unwrap();
        assert_eq!(link.url, "https://example.test/x");
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
            2,
            5,
        );
    }
    #[test]
    fn html_url_has_exact_span_and_location() {
        assert_link(
            DocFormat::Html,
            "<a href=\"https://example.test/x\">x</a>",
            1,
            10,
        );
    }
    #[test]
    fn text_url_has_exact_span_and_location() {
        assert_link(DocFormat::Text, "see https://example.test/x", 1, 5);
    }
}
