use std::ops::Range;

use crate::{
    extract::{SourceDocument, extract},
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
    html_escape::decode_html_entities(&String::from_utf8_lossy(raw)).to_string()
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
}
