use std::{cmp::Ordering, ops::Range, path::PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub url: String,
    pub resolved_url: Option<String>,
    pub source: SourceRef,
    pub verdict: Verdict,
    pub fix: Option<SuggestedFix>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    pub path: PathBuf,
    pub format: DocFormat,
    pub location: Location,
    pub byte_span: Option<Range<u64>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocFormat {
    Pdf,
    Docx,
    Xlsx,
    Pptx,
    Markdown,
    Html,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Location {
    Pdf { page: u32, annotation: Option<u32> },
    Docx { paragraph: u32 },
    Xlsx { sheet: String, cell: String },
    Pptx { slide: u32 },
    Text { line: u32, column: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub confidence: Confidence,
    pub reason: Reason,
    pub evidence: Vec<Evidence>,
    pub checked_at: DateTime<Utc>,
    pub tier: u8,
}

/// Severity order: `DeadCertain > LikelyDead > Outdated > AuthWalled > Suspect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Confidence {
    DeadCertain,
    LikelyDead,
    AuthWalled,
    Outdated,
    Suspect,
}

impl Ord for Confidence {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank().cmp(&other.rank())
    }
}

impl PartialOrd for Confidence {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Confidence {
    const fn rank(self) -> u8 {
        match self {
            Self::DeadCertain => 5,
            Self::LikelyDead => 4,
            Self::Outdated => 3,
            Self::AuthWalled => 2,
            Self::Suspect => 1,
        }
    }
}

/// Stable adjacent JSON tagging; for example, `HttpStatus(404)` is `{"kind":"http-status","status":404}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "status", rename_all = "kebab-case")]
pub enum Reason {
    HttpStatus(u16),
    NetworkError(NetKind),
    Soft404,
    LoginWall,
    PermanentRedirect,
    StalenessBanner,
    VersionDrift,
    FarPastLastModified,
    AnomalousResponse,
    LocalMissing,
    SyntaxInvalid,
}

impl Reason {
    pub const fn confidence(&self) -> Confidence {
        match self {
            Self::HttpStatus(401 | 403) => Confidence::AuthWalled,
            Self::HttpStatus(404 | 410 | 451) => Confidence::DeadCertain,
            Self::HttpStatus(_) => Confidence::Suspect,
            Self::NetworkError(_) | Self::Soft404 => Confidence::LikelyDead,
            Self::LoginWall => Confidence::AuthWalled,
            Self::PermanentRedirect | Self::StalenessBanner | Self::VersionDrift => {
                Confidence::Outdated
            }
            Self::FarPastLastModified | Self::AnomalousResponse => Confidence::Suspect,
            Self::LocalMissing | Self::SyntaxInvalid => Confidence::DeadCertain,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetKind {
    Dns,
    Tls,
    Timeout,
    ConnRefused,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestedFix {
    pub replacement_url: String,
    pub origin: FixOrigin,
    pub fixable: Fixability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FixOrigin {
    RedirectTarget,
    VersionUpgrade,
    HttpsUpgrade,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Fixability {
    Auto,
    Manual,
    Refused { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoundLink {
    pub url: String,
    pub source: SourceRef,
}

#[cfg(test)]
mod tests {
    use std::{ops::Range, path::PathBuf};

    use chrono::{TimeZone, Utc};

    use super::*;

    #[test]
    fn reason_confidence_mapping() {
        let cases = [
            (Reason::HttpStatus(404), Confidence::DeadCertain),
            (Reason::HttpStatus(410), Confidence::DeadCertain),
            (Reason::HttpStatus(451), Confidence::DeadCertain),
            (Reason::HttpStatus(401), Confidence::AuthWalled),
            (Reason::HttpStatus(403), Confidence::AuthWalled),
            (Reason::HttpStatus(200), Confidence::Suspect),
            (Reason::NetworkError(NetKind::Dns), Confidence::LikelyDead),
            (Reason::Soft404, Confidence::LikelyDead),
            (Reason::LoginWall, Confidence::AuthWalled),
            (Reason::PermanentRedirect, Confidence::Outdated),
            (Reason::StalenessBanner, Confidence::Outdated),
            (Reason::VersionDrift, Confidence::Outdated),
            (Reason::FarPastLastModified, Confidence::Suspect),
            (Reason::AnomalousResponse, Confidence::Suspect),
            (Reason::LocalMissing, Confidence::DeadCertain),
            (Reason::SyntaxInvalid, Confidence::DeadCertain),
        ];

        for (reason, confidence) in cases {
            assert_eq!(reason.confidence(), confidence);
        }
    }

    #[test]
    fn location_uses_tagged_json() {
        let location = Location::Pdf {
            page: 3,
            annotation: None,
        };
        assert_eq!(
            serde_json::to_string(&location).unwrap(),
            r#"{"type":"pdf","page":3,"annotation":null}"#
        );
        assert_eq!(
            serde_json::from_str::<Location>(r#"{"type":"pdf","page":3,"annotation":null}"#)
                .unwrap(),
            location
        );
    }

    #[test]
    fn confidence_uses_kebab_case_json() {
        assert_eq!(
            serde_json::to_string(&Confidence::DeadCertain).unwrap(),
            r#""dead-certain""#
        );
        assert_eq!(
            serde_json::from_str::<Confidence>(r#""likely-dead""#).unwrap(),
            Confidence::LikelyDead
        );
    }

    #[test]
    fn finding_round_trips_with_expected_json() {
        let finding = Finding {
            url: "https://old.example".into(),
            resolved_url: Some("https://new.example".into()),
            source: SourceRef {
                path: PathBuf::from("notes.md"),
                format: DocFormat::Markdown,
                location: Location::Text { line: 2, column: 5 },
                byte_span: Some(Range { start: 8, end: 27 }),
            },
            verdict: Verdict {
                confidence: Confidence::Outdated,
                reason: Reason::PermanentRedirect,
                evidence: vec![Evidence {
                    kind: "redirect".into(),
                    detail: "301".into(),
                }],
                checked_at: Utc.with_ymd_and_hms(2026, 8, 4, 12, 0, 0).unwrap(),
                tier: 1,
            },
            fix: Some(SuggestedFix {
                replacement_url: "https://new.example".into(),
                origin: FixOrigin::RedirectTarget,
                fixable: Fixability::Auto,
            }),
        };
        let expected = r#"{"url":"https://old.example","resolved_url":"https://new.example","source":{"path":"notes.md","format":"markdown","location":{"type":"text","line":2,"column":5},"byte_span":{"start":8,"end":27}},"verdict":{"confidence":"outdated","reason":{"kind":"permanent-redirect"},"evidence":[{"kind":"redirect","detail":"301"}],"checked_at":"2026-08-04T12:00:00Z","tier":1},"fix":{"replacement_url":"https://new.example","origin":"redirect-target","fixable":{"kind":"auto"}}}"#;
        assert_eq!(serde_json::to_string(&finding).unwrap(), expected);
        assert_eq!(serde_json::from_str::<Finding>(expected).unwrap(), finding);
    }
}
