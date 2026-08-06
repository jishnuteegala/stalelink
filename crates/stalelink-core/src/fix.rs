use std::{collections::HashMap, ops::Range};

use lol_html::{HtmlRewriter, Settings, element};

use crate::model::{DocFormat, Finding};

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

impl Fixer for HtmlFixer {
    fn fix(&self, original: &[u8], findings: &[Finding]) -> Result<Vec<u8>, FixError> {
        let replacements = findings
            .iter()
            .map(|finding| {
                let fix = finding
                    .fix
                    .as_ref()
                    .ok_or_else(|| FixError(format!("{} has no suggested fix", finding.url)))?;
                Ok((finding.url.as_str(), fix.replacement_url.as_str()))
            })
            .collect::<Result<HashMap<_, _>, FixError>>()?;
        let mut output = Vec::with_capacity(original.len());
        let mut rewriter = HtmlRewriter::new(
            Settings {
                element_content_handlers: vec![
                    element!("a, link", |element| {
                        rewrite_attribute(element, "href", &replacements)
                    }),
                    element!("img, script", |element| {
                        rewrite_attribute(element, "src", &replacements)
                    }),
                ],
                ..Settings::default()
            },
            |chunk: &[u8]| output.extend_from_slice(chunk),
        );
        rewriter
            .write(original)
            .map_err(|error| FixError(error.to_string()))?;
        rewriter
            .end()
            .map_err(|error| FixError(error.to_string()))?;
        Ok(output)
    }
}

fn rewrite_attribute(
    element: &mut lol_html::html_content::Element,
    name: &str,
    replacements: &HashMap<&str, &str>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(value) = element.get_attribute(name)
        && let Some(replacement) = replacements.get(value.as_str())
    {
        element.set_attribute(name, replacement)?;
    }
    Ok(())
}

pub fn fixer_for(format: DocFormat) -> Option<&'static dyn Fixer> {
    static TEXT: TextFixer = TextFixer;
    static HTML: HtmlFixer = HtmlFixer;
    match format {
        DocFormat::Markdown | DocFormat::Text => Some(&TEXT),
        DocFormat::Html => Some(&HTML),
        DocFormat::Pdf | DocFormat::Docx | DocFormat::Xlsx | DocFormat::Pptx => None,
    }
}

pub fn apply_edits(original: &[u8], edits: &[Edit]) -> Result<Vec<u8>, FixError> {
    let mut ordered = edits.to_vec();
    ordered.sort_by_key(|edit| std::cmp::Reverse(edit.span.start));
    for pair in ordered.windows(2) {
        let later = &pair[0].span;
        let earlier = &pair[1].span;
        if earlier.end > later.start {
            return Err(FixError(format!(
                "overlapping edits at {}..{} and {}..{}",
                earlier.start, earlier.end, later.start, later.end
            )));
        }
    }
    let mut fixed = original.to_vec();
    for edit in ordered {
        if edit.span.start > edit.span.end || edit.span.end > fixed.len() {
            return Err(FixError(format!(
                "edit span {}..{} is outside document length {}",
                edit.span.start,
                edit.span.end,
                fixed.len()
            )));
        }
        fixed.splice(edit.span, edit.replacement);
    }
    Ok(fixed)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

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
            replacements in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..20), 0..15),
        ) {
            let mut cursor = 0usize;
            let mut edits = Vec::new();
            for replacement in replacements {
                if cursor > original.len() { break; }
                let remaining = original.len() - cursor;
                let width = remaining.min(2);
                edits.push(Edit { span: cursor..cursor + width, replacement });
                cursor += width.saturating_add(1);
            }
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
}
