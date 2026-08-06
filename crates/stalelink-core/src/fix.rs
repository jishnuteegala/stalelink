use std::ops::Range;

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
                let quote = start.checked_sub(1).and_then(|index| original.get(index));
                Ok(Edit {
                    span: start..end,
                    replacement: encode_html_attribute(&fix.replacement_url, quote),
                })
            })
            .collect::<Result<Vec<_>, FixError>>()?;
        apply_edits(original, &edits)
    }
}

fn decode_html_attribute(raw: &[u8]) -> String {
    let raw = String::from_utf8_lossy(raw);
    let mut decoded = String::with_capacity(raw.len());
    let mut rest = raw.as_ref();
    while let Some(start) = rest.find('&') {
        decoded.push_str(&rest[..start]);
        let entity = &rest[start + 1..];
        let Some(end) = entity.find(';') else {
            decoded.push_str(&rest[start..]);
            break;
        };
        let name = &entity[..end];
        let character = match name {
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "lt" => Some('<'),
            "gt" => Some('>'),
            _ => name
                .strip_prefix("#x")
                .or_else(|| name.strip_prefix("#X"))
                .and_then(|number| u32::from_str_radix(number, 16).ok())
                .or_else(|| {
                    name.strip_prefix('#')
                        .and_then(|number| number.parse().ok())
                })
                .and_then(char::from_u32),
        };
        if let Some(character) = character {
            decoded.push(character);
            rest = &entity[end + 1..];
        } else {
            decoded.push_str(&rest[..start + end + 2]);
            rest = &entity[end + 1..];
        }
    }
    decoded.push_str(rest);
    decoded
}

fn encode_html_attribute(value: &str, quote: Option<&u8>) -> Vec<u8> {
    let mut encoded = value.replace('&', "&amp;");
    match quote {
        Some(b'"') => encoded = encoded.replace('"', "&quot;"),
        Some(b'\'') => encoded = encoded.replace('\'', "&apos;"),
        _ => {
            encoded = encoded.replace('"', "&quot;").replace('\'', "&apos;");
        }
    }
    encoded.into_bytes()
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
    fn html_attribute_encoding_preserves_value_safety() {
        assert_eq!(
            encode_html_attribute("https://x.test/?a=1&b=2", Some(&b'\'')),
            b"https://x.test/?a=1&amp;b=2"
        );
        assert_eq!(
            html_escape::decode_html_entities("https://x.test/?a=1&#38;b=2"),
            "https://x.test/?a=1&b=2"
        );
    }
}
