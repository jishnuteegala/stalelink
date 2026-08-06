use std::{collections::BTreeMap, io, path::Path};

use serde::Serialize;
use serde_json::{Map, Value, json};
use stalelink_core::{
    model::{Confidence, DocFormat, Finding, Location, Reason},
    scan::ScanReport,
};
use url::Url;

pub fn write_json(writer: &mut impl io::Write, report: &ScanReport) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *writer, &JsonReport::from(report))
        .map_err(io::Error::other)?;
    writeln!(writer)
}

pub fn write_sarif(
    writer: &mut impl io::Write,
    report: &ScanReport,
    exit_code: u8,
) -> io::Result<()> {
    let rules = report
        .findings
        .iter()
        .map(rule_for)
        .map(|rule| (rule["id"].as_str().unwrap_or_default().to_owned(), rule))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect::<Vec<_>>();
    let results = report.findings.iter().map(sarif_result).collect::<Vec<_>>();
    let report = json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": { "driver": { "name": "stalelink", "version": env!("CARGO_PKG_VERSION"), "rules": rules } },
            "invocations": [{ "executionSuccessful": true, "exitCode": exit_code }],
            "results": results,
        }],
    });
    serde_json::to_writer_pretty(&mut *writer, &report).map_err(io::Error::other)?;
    writeln!(writer)
}

#[derive(Serialize)]
struct JsonReport<'a> {
    schema_version: u8,
    run: JsonRun,
    findings: &'a [Finding],
}

#[derive(Serialize)]
struct JsonRun {
    files_scanned: usize,
    links_checked: usize,
    links_unique: usize,
    findings_by_confidence: ConfidenceCounts,
    duration_ms: u128,
}

#[derive(Serialize)]
struct ConfidenceCounts {
    dead_certain: usize,
    likely_dead: usize,
    auth_walled: usize,
    outdated: usize,
    suspect: usize,
}

impl<'a> From<&'a ScanReport> for JsonReport<'a> {
    fn from(report: &'a ScanReport) -> Self {
        let mut counts = ConfidenceCounts {
            dead_certain: 0,
            likely_dead: 0,
            auth_walled: 0,
            outdated: 0,
            suspect: 0,
        };
        for finding in &report.findings {
            match finding.verdict.confidence {
                Confidence::DeadCertain => counts.dead_certain += 1,
                Confidence::LikelyDead => counts.likely_dead += 1,
                Confidence::AuthWalled => counts.auth_walled += 1,
                Confidence::Outdated => counts.outdated += 1,
                Confidence::Suspect => counts.suspect += 1,
            }
        }
        Self {
            schema_version: 1,
            run: JsonRun {
                files_scanned: report.files_scanned,
                links_checked: report.links_checked,
                links_unique: report.links_unique,
                findings_by_confidence: counts,
                duration_ms: report.duration.as_millis(),
            },
            findings: &report.findings,
        }
    }
}

fn rule_for(finding: &Finding) -> Value {
    let id = rule_id(&finding.verdict.reason);
    json!({
        "id": id,
        "name": reason_name(&finding.verdict.reason),
        "shortDescription": { "text": reason_name(&finding.verdict.reason) },
    })
}

fn sarif_result(finding: &Finding) -> Value {
    let mut result = Map::new();
    result.insert("ruleId".into(), json!(rule_id(&finding.verdict.reason)));
    result.insert(
        "level".into(),
        json!(sarif_level(finding.verdict.confidence)),
    );
    result.insert("rank".into(), json!(sarif_rank(finding.verdict.confidence)));
    result.insert(
        "message".into(),
        json!({ "text": format!("{}: {}", reason_name(&finding.verdict.reason), finding.url) }),
    );
    result.insert("locations".into(), json!([sarif_location(finding)]));
    let mut properties = json!({
        "url": finding.url,
        "resolved_url": finding.resolved_url,
        "reason": finding.verdict.reason,
        "confidence": finding.verdict.confidence,
        "evidence": finding.verdict.evidence,
        "checked_at": finding.verdict.checked_at,
        "tier": finding.verdict.tier,
    });
    if !text_format(finding.source.format) {
        properties["source_location"] = serde_json::to_value(&finding.source.location)
            .expect("core source locations are serializable");
    }
    result.insert("properties".into(), properties);
    if text_format(finding.source.format)
        && let (Some(fix), Some(span)) = (&finding.fix, &finding.source.byte_span)
    {
        result.insert(
            "fixes".into(),
            json!([{
                "description": { "text": format!("Replace with {}", fix.replacement_url) },
                "artifactChanges": [{
                    "artifactLocation": { "uri": artifact_uri(&finding.source.path) },
                    "replacements": [{
                        "deletedRegion": {
                            "byteOffset": span.start,
                            "byteLength": span.end - span.start,
                        },
                        "insertedContent": { "text": fix.replacement_url },
                    }],
                }],
            }]),
        );
    }
    Value::Object(result)
}

fn sarif_location(finding: &Finding) -> Value {
    let artifact = json!({ "uri": artifact_uri(&finding.source.path) });
    if let Location::Text { line, column } = finding.source.location {
        json!({
            "physicalLocation": {
                "artifactLocation": artifact,
                "region": { "startLine": line, "startColumn": column },
            },
        })
    } else {
        json!({ "physicalLocation": { "artifactLocation": artifact } })
    }
}

fn artifact_uri(path: &Path) -> String {
    if path.is_absolute() {
        Url::from_file_path(path)
            .expect("absolute source path must convert to a file URI")
            .into()
    } else {
        // Encode through an absolute sentinel, then remove it to retain SARIF's
        // relative URI behavior. Url::set_path intentionally preserves '%' here.
        #[cfg(windows)]
        let sentinel = Path::new(r"C:\stalelink-relative");
        #[cfg(not(windows))]
        let sentinel = Path::new("/stalelink-relative");
        let uri = Url::from_file_path(sentinel.join(path))
            .expect("sentinel source path must convert to a file URI");
        #[cfg(windows)]
        let prefix = "file:///C:/stalelink-relative/";
        #[cfg(not(windows))]
        let prefix = "file:///stalelink-relative/";
        uri.as_str()
            .strip_prefix(prefix)
            .expect("sentinel URI has its prefix")
            .to_owned()
    }
}

fn text_format(format: DocFormat) -> bool {
    matches!(
        format,
        DocFormat::Markdown | DocFormat::Html | DocFormat::Text
    )
}

fn rule_id(reason: &Reason) -> &'static str {
    match reason {
        Reason::HttpStatus(_) => "SL0001",
        Reason::NetworkError(_) => "SL0002",
        Reason::Soft404 => "SL0003",
        Reason::LoginWall => "SL0101",
        Reason::PermanentRedirect => "SL0201",
        Reason::StalenessBanner => "SL0202",
        Reason::VersionDrift => "SL0203",
        Reason::FarPastLastModified => "SL0204",
        Reason::AnomalousResponse => "SL0301",
        Reason::LocalMissing => "SL0401",
        Reason::SyntaxInvalid => "SL0402",
    }
}

fn reason_name(reason: &Reason) -> &'static str {
    match reason {
        Reason::HttpStatus(_) => "HTTP status",
        Reason::NetworkError(_) => "Network error",
        Reason::Soft404 => "Soft 404",
        Reason::LoginWall => "Login wall",
        Reason::PermanentRedirect => "Permanent redirect",
        Reason::StalenessBanner => "Staleness banner",
        Reason::VersionDrift => "Version drift",
        Reason::FarPastLastModified => "Far-past last modified",
        Reason::AnomalousResponse => "Anomalous response",
        Reason::LocalMissing => "Missing local target",
        Reason::SyntaxInvalid => "Invalid link syntax",
    }
}

fn sarif_level(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::DeadCertain => "error",
        Confidence::LikelyDead | Confidence::Outdated => "warning",
        Confidence::AuthWalled | Confidence::Suspect => "note",
    }
}

fn sarif_rank(confidence: Confidence) -> f64 {
    match confidence {
        Confidence::DeadCertain => 100.0,
        Confidence::LikelyDead => 80.0,
        Confidence::Outdated => 60.0,
        Confidence::AuthWalled => 40.0,
        Confidence::Suspect => 20.0,
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use chrono::Utc;
    use stalelink_core::{
        model::{Evidence, FixOrigin, Fixability, SourceRef, SuggestedFix, Verdict},
        scan::ScanReport,
    };

    use super::*;

    #[test]
    fn sarif_keeps_pdf_location_in_properties() {
        let report = ScanReport {
            findings: vec![Finding {
                url: "https://example.test/missing".into(),
                resolved_url: None,
                source: SourceRef {
                    path: Path::new("docs").join("report.pdf"),
                    format: DocFormat::Pdf,
                    location: Location::Pdf {
                        page: 2,
                        annotation: Some(3),
                    },
                    byte_span: None,
                },
                verdict: Verdict {
                    confidence: Confidence::DeadCertain,
                    reason: Reason::HttpStatus(404),
                    evidence: vec![Evidence {
                        kind: "http-status".into(),
                        detail: "404".into(),
                    }],
                    checked_at: Utc::now(),
                    tier: 1,
                },
                fix: None,
            }],
            files_scanned: 1,
            links_checked: 1,
            links_unique: 1,
            duration: Duration::ZERO,
        };
        let mut output = Vec::new();
        write_sarif(&mut output, &report, 1).unwrap();
        let sarif: Value = serde_json::from_slice(&output).unwrap();
        let result = &sarif["runs"][0]["results"][0];
        assert_eq!(
            result["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
            "docs/report.pdf"
        );
        assert_eq!(result["properties"]["source_location"]["type"], "pdf");
        assert_eq!(result["properties"]["source_location"]["page"], 2);
        assert_eq!(result["properties"]["source_location"]["annotation"], 3);
    }

    #[test]
    fn sarif_fix_replaces_the_text_url_span() {
        let report = ScanReport {
            findings: vec![Finding {
                url: "http://example.test/old".into(),
                resolved_url: None,
                source: SourceRef {
                    path: PathBuf::from("note.md"),
                    format: DocFormat::Markdown,
                    location: Location::Text { line: 1, column: 2 },
                    byte_span: Some(1..24),
                },
                verdict: Verdict {
                    confidence: Confidence::Outdated,
                    reason: Reason::PermanentRedirect,
                    evidence: vec![],
                    checked_at: Utc::now(),
                    tier: 1,
                },
                fix: Some(SuggestedFix {
                    replacement_url: "https://example.test/new".into(),
                    origin: FixOrigin::RedirectTarget,
                    fixable: Fixability::Auto,
                }),
            }],
            files_scanned: 1,
            links_checked: 1,
            links_unique: 1,
            duration: Duration::ZERO,
        };
        let mut output = Vec::new();
        write_sarif(&mut output, &report, 1).unwrap();
        let sarif: Value = serde_json::from_slice(&output).unwrap();
        let replacement =
            &sarif["runs"][0]["results"][0]["fixes"][0]["artifactChanges"][0]["replacements"][0];
        assert_eq!(replacement["deletedRegion"]["byteOffset"], 1);
        assert_eq!(replacement["deletedRegion"]["byteLength"], 23);
        assert_eq!(
            replacement["insertedContent"]["text"],
            "https://example.test/new"
        );
    }

    #[test]
    fn artifact_uris_encode_special_characters() {
        let path = Path::new("docs space")
            .join("hash#percent%")
            .join("unicode-ß.md");
        assert_eq!(
            artifact_uri(&path),
            "docs%20space/hash%23percent%25/unicode-%C3%9F.md"
        );
    }

    #[cfg(windows)]
    #[test]
    fn artifact_uri_round_trips_absolute_windows_paths() {
        let path = PathBuf::from(r"C:\docs space\hash#percent%\unicode-ß.md");
        let uri = artifact_uri(&path);
        assert_eq!(
            uri,
            "file:///C:/docs%20space/hash%23percent%25/unicode-%C3%9F.md"
        );
        assert_eq!(Url::parse(&uri).unwrap().to_file_path().unwrap(), path);
    }

    #[cfg(not(windows))]
    #[test]
    fn artifact_uri_round_trips_absolute_unix_paths() {
        let path = PathBuf::from("/docs space/hash#percent%/unicode-ß.md");
        let uri = artifact_uri(&path);
        assert_eq!(
            uri,
            "file:///docs%20space/hash%23percent%25/unicode-%C3%9F.md"
        );
        assert_eq!(Url::parse(&uri).unwrap().to_file_path().unwrap(), path);
    }

    #[test]
    fn sarif_fix_uses_utf8_byte_offsets() {
        let document = "é [old](http://example.test/old)";
        let old_url = "http://example.test/old";
        let start = document.find(old_url).unwrap() as u64;
        let report = ScanReport {
            findings: vec![Finding {
                url: old_url.into(),
                resolved_url: None,
                source: SourceRef {
                    path: PathBuf::from("note.md"),
                    format: DocFormat::Markdown,
                    location: Location::Text { line: 1, column: 9 },
                    byte_span: Some(start..start + old_url.len() as u64),
                },
                verdict: Verdict {
                    confidence: Confidence::Outdated,
                    reason: Reason::PermanentRedirect,
                    evidence: vec![],
                    checked_at: Utc::now(),
                    tier: 1,
                },
                fix: Some(SuggestedFix {
                    replacement_url: "https://example.test/new".into(),
                    origin: FixOrigin::RedirectTarget,
                    fixable: Fixability::Auto,
                }),
            }],
            files_scanned: 1,
            links_checked: 1,
            links_unique: 1,
            duration: Duration::ZERO,
        };
        let mut output = Vec::new();
        write_sarif(&mut output, &report, 1).unwrap();
        let sarif: Value = serde_json::from_slice(&output).unwrap();
        let region = &sarif["runs"][0]["results"][0]["fixes"][0]["artifactChanges"][0]["replacements"]
            [0]["deletedRegion"];
        let offset = region["byteOffset"].as_u64().unwrap() as usize;
        let length = region["byteLength"].as_u64().unwrap() as usize;
        let replacement = sarif["runs"][0]["results"][0]["fixes"][0]["artifactChanges"][0]
            ["replacements"][0]["insertedContent"]["text"]
            .as_str()
            .unwrap();
        let mut bytes = document.as_bytes().to_vec();
        bytes.splice(offset..offset + length, replacement.bytes());
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "é [old](https://example.test/new)"
        );
    }

    #[test]
    fn sarif_records_completed_scan_exit_code() {
        let report = ScanReport {
            findings: vec![],
            files_scanned: 1,
            links_checked: 0,
            links_unique: 0,
            duration: Duration::ZERO,
        };
        let mut output = Vec::new();
        write_sarif(&mut output, &report, 0).unwrap();
        let sarif: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(
            sarif["runs"][0]["invocations"][0]["executionSuccessful"],
            true
        );
        assert_eq!(sarif["runs"][0]["invocations"][0]["exitCode"], 0);
    }

    #[test]
    fn json_totals_are_pre_filter_while_confidence_counts_are_rendered_findings() {
        let report = ScanReport {
            findings: vec![Finding {
                url: "https://example.test/missing".into(),
                resolved_url: None,
                source: SourceRef {
                    path: PathBuf::from("note.md"),
                    format: DocFormat::Markdown,
                    location: Location::Text { line: 1, column: 1 },
                    byte_span: None,
                },
                verdict: Verdict {
                    confidence: Confidence::DeadCertain,
                    reason: Reason::HttpStatus(404),
                    evidence: vec![],
                    checked_at: Utc::now(),
                    tier: 1,
                },
                fix: None,
            }],
            files_scanned: 4,
            links_checked: 5,
            links_unique: 3,
            duration: Duration::ZERO,
        };
        let mut output = Vec::new();
        write_json(&mut output, &report).unwrap();
        let json: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(json["run"]["files_scanned"], 4);
        assert_eq!(json["run"]["links_checked"], 5);
        assert_eq!(json["run"]["links_unique"], 3);
        assert_eq!(json["run"]["findings_by_confidence"]["dead_certain"], 1);
        assert_eq!(json["run"]["findings_by_confidence"]["likely_dead"], 0);
    }
}
