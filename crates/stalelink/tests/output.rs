use std::{
    collections::BTreeSet,
    ops::Range,
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::{TimeZone, Utc};
use jsonschema::validator_for;
use serde_json::Value;
use stalelink_core::{
    model::{
        Confidence, DocFormat, Evidence, Finding, FixOrigin, Fixability, Location, NetKind, Reason,
        SourceRef, SuggestedFix, Verdict,
    },
    scan::ScanReport,
};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

mod util;

async fn fixture() -> (tempfile::TempDir, MockServer) {
    let server = MockServer::start().await;
    Mock::given(path("/missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("note.md"),
        format!("[broken]({}/missing)\n", server.uri()),
    )
    .unwrap();
    (directory, server)
}

async fn mixed_fixture() -> (tempfile::TempDir, MockServer) {
    let server = MockServer::start().await;
    Mock::given(path("/missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(path("/old"))
        .respond_with(
            ResponseTemplate::new(301).insert_header("location", format!("{}/new", server.uri())),
        )
        .mount(&server)
        .await;
    Mock::given(path("/new"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("note.md"),
        format!(
            "[broken]({}/missing)\n[old]({}/old)\n",
            server.uri(),
            server.uri()
        ),
    )
    .unwrap();
    (directory, server)
}

async fn outdated_fixture() -> (tempfile::TempDir, MockServer) {
    let server = MockServer::start().await;
    Mock::given(path("/old"))
        .respond_with(
            ResponseTemplate::new(301).insert_header("location", format!("{}/new", server.uri())),
        )
        .mount(&server)
        .await;
    Mock::given(path("/new"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("note.md"),
        format!("[old]({}/old)\n", server.uri()),
    )
    .unwrap();
    (directory, server)
}

fn scan(directory: &Path, format: &str) -> assert_cmd::assert::Assert {
    let mut command = util::command();
    command.args([
        "scan",
        "--no-cache",
        "--retries",
        "0",
        "--format",
        format,
        directory.to_str().unwrap(),
    ]);
    tokio::task::block_in_place(|| command.assert())
}

fn validate(schema: &str, value: &Value) {
    let schema: Value = serde_json::from_str(schema).unwrap();
    let validator = validator_for(&schema).unwrap();
    if let Err(error) = validator.validate(value) {
        panic!("schema validation failed: {error}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn real_json_scan_validates_against_shipped_schema() {
    let (directory, _) = fixture().await;
    let output = scan(directory.path(), "json")
        .code(1)
        .stderr("")
        .get_output()
        .clone();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    validate(
        include_str!("../../../schema/stalelink-report.v1.json"),
        &value,
    );
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["findings"].as_array().unwrap().len(), 1);
}

#[test]
fn schema_validates_every_core_finding_variant_and_optional_form() {
    let reasons = vec![
        Reason::HttpStatus(404),
        Reason::NetworkError(NetKind::Dns),
        Reason::NetworkError(NetKind::Tls),
        Reason::NetworkError(NetKind::Timeout),
        Reason::NetworkError(NetKind::ConnRefused),
        Reason::NetworkError(NetKind::Other),
        Reason::Soft404,
        Reason::LoginWall,
        Reason::PermanentRedirect,
        Reason::StalenessBanner,
        Reason::VersionDrift,
        Reason::FarPastLastModified,
        Reason::AnomalousResponse,
        Reason::LocalMissing,
        Reason::SyntaxInvalid,
    ];
    let locations = [
        Location::Pdf {
            page: 1,
            annotation: Some(2),
        },
        Location::Pdf {
            page: 2,
            annotation: None,
        },
        Location::Docx { paragraph: 3 },
        Location::Xlsx {
            sheet: "Sheet 1".into(),
            cell: "A1".into(),
        },
        Location::Pptx { slide: 4 },
        Location::Text { line: 5, column: 6 },
    ];
    let findings = reasons
        .into_iter()
        .enumerate()
        .map(|(index, reason)| Finding {
            url: format!("https://example.test/{index}"),
            resolved_url: (index % 2 == 0).then(|| "https://resolved.test/".into()),
            source: SourceRef {
                path: PathBuf::from(format!("document-{index}")),
                format: DocFormat::Markdown,
                location: locations[index % locations.len()].clone(),
                byte_span: (index % 2 == 0).then_some(Range { start: 1, end: 4 }),
            },
            verdict: Verdict {
                confidence: Confidence::Suspect,
                reason,
                evidence: vec![Evidence {
                    kind: "test".into(),
                    detail: "coverage".into(),
                }],
                checked_at: Utc.with_ymd_and_hms(2026, 8, 6, 0, 0, 0).unwrap(),
                tier: 1,
            },
            fix: match index % 4 {
                0 => Some(SuggestedFix {
                    replacement_url: "https://replacement.test/".into(),
                    origin: FixOrigin::RedirectTarget,
                    fixable: Fixability::Auto,
                }),
                1 => Some(SuggestedFix {
                    replacement_url: "https://replacement.test/".into(),
                    origin: FixOrigin::VersionUpgrade,
                    fixable: Fixability::Manual,
                }),
                2 => Some(SuggestedFix {
                    replacement_url: "https://replacement.test/".into(),
                    origin: FixOrigin::HttpsUpgrade,
                    fixable: Fixability::Refused {
                        reason: "unsafe".into(),
                    },
                }),
                _ => None,
            },
        })
        .collect::<Vec<_>>();
    let report = ScanReport {
        findings,
        files_scanned: 9,
        links_checked: 11,
        links_unique: 10,
        duration: Duration::ZERO,
    };
    let value = serde_json::json!({
        "schema_version": 1,
        "run": { "files_scanned": 9, "links_checked": 11, "links_unique": 10, "findings_by_confidence": { "dead_certain": 0, "likely_dead": 0, "auth_walled": 0, "outdated": 0, "suspect": report.findings.len() }, "duration_ms": 0 },
        "findings": report.findings,
    });
    validate(
        include_str!("../../../schema/stalelink-report.v1.json"),
        &value,
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn real_sarif_scan_validates_against_official_schema_and_has_text_region() {
    let (directory, _) = fixture().await;
    let output = scan(directory.path(), "sarif")
        .code(1)
        .stderr("")
        .get_output()
        .clone();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    validate(include_str!("../../../schema/sarif-2.1.0.json"), &value);
    let result = &value["runs"][0]["results"][0];
    assert_eq!(result["ruleId"], "SL0001");
    assert_eq!(result["level"], "error");
    assert_eq!(result["rank"], 100.0);
    assert_eq!(
        value["runs"][0]["invocations"][0]["executionSuccessful"],
        true
    );
    assert_eq!(value["runs"][0]["invocations"][0]["exitCode"], 1);
    assert_eq!(
        result["locations"][0]["physicalLocation"]["region"]["startLine"],
        1
    );
    assert_eq!(
        result["locations"][0]["physicalLocation"]["region"]["startColumn"],
        10
    );
}

#[test]
fn clean_sarif_records_a_successful_zero_exit() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("note.md"),
        "[valid](mailto:person@example.test)",
    )
    .unwrap();
    let mut command = util::command();
    command.args([
        "scan",
        "--no-cache",
        "--format",
        "sarif",
        directory.path().to_str().unwrap(),
    ]);
    let output = command.assert().code(0).stderr("").get_output().clone();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["runs"][0]["invocations"][0]["executionSuccessful"],
        true
    );
    assert_eq!(value["runs"][0]["invocations"][0]["exitCode"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn json_retains_scan_totals_but_counts_only_confidence_filtered_findings() {
    let (directory, _) = mixed_fixture().await;
    let mut command = util::command();
    command.args([
        "scan",
        "--no-cache",
        "--retries",
        "0",
        "--format",
        "json",
        "--min-confidence",
        "dead-certain",
        directory.path().to_str().unwrap(),
    ]);
    let output = tokio::task::block_in_place(|| command.assert())
        .code(1)
        .stderr("")
        .get_output()
        .clone();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["run"]["files_scanned"], 1);
    assert_eq!(report["run"]["links_checked"], 2);
    assert_eq!(report["run"]["links_unique"], 2);
    assert_eq!(report["run"]["findings_by_confidence"]["dead_certain"], 1);
    assert_eq!(report["run"]["findings_by_confidence"]["outdated"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn formats_share_minimum_confidence_filtering() {
    let (directory, server) = mixed_fixture().await;
    let expected_url = format!("{}/missing", server.uri());
    let expected_source = directory.path().join("note.md");
    let mut table_findings = None;
    let mut json_findings = None;
    let mut sarif_findings = None;
    for format in ["table", "json", "sarif"] {
        let mut command = util::command();
        command.args([
            "scan",
            "--no-cache",
            "--retries",
            "0",
            "--format",
            format,
            "--min-confidence",
            "dead-certain",
            "--fail-on",
            "dead-certain",
            directory.path().to_str().unwrap(),
        ]);
        let output = tokio::task::block_in_place(|| command.assert())
            .code(1)
            .get_output()
            .stdout
            .clone();
        match format {
            "table" => {
                let table = String::from_utf8(output).unwrap();
                assert!(table.contains(&expected_url));
                assert!(!table.contains("/old"));
                assert!(table.contains(&format!("{}:1:10", expected_source.display())));
                assert!(table.contains("HTTP 404"));
                table_findings = Some(
                    table
                        .lines()
                        .skip(1)
                        .map(|row| {
                            let columns = row
                                .split("  ")
                                .filter(|column| !column.is_empty())
                                .collect::<Vec<_>>();
                            (
                                columns[1].to_owned(),
                                "http-status".to_owned(),
                                columns[2].strip_suffix(":1:10").unwrap().to_owned(),
                            )
                        })
                        .collect::<BTreeSet<_>>(),
                );
            }
            "json" => {
                let findings =
                    serde_json::from_slice::<Value>(&output).unwrap()["findings"].clone();
                assert_eq!(findings.as_array().unwrap().len(), 1);
                assert_eq!(findings[0]["url"], expected_url);
                assert_eq!(findings[0]["verdict"]["reason"]["kind"], "http-status");
                assert_eq!(
                    findings[0]["source"]["path"],
                    expected_source.to_string_lossy().as_ref()
                );
                json_findings = Some(
                    findings
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|finding| {
                            (
                                finding["url"].as_str().unwrap().to_owned(),
                                finding["verdict"]["reason"]["kind"]
                                    .as_str()
                                    .unwrap()
                                    .to_owned(),
                                finding["source"]["path"].as_str().unwrap().to_owned(),
                            )
                        })
                        .collect::<BTreeSet<_>>(),
                );
            }
            "sarif" => {
                let results =
                    serde_json::from_slice::<Value>(&output).unwrap()["runs"][0]["results"].clone();
                assert_eq!(results.as_array().unwrap().len(), 1);
                assert_eq!(results[0]["properties"]["url"], expected_url);
                assert_eq!(results[0]["properties"]["reason"]["kind"], "http-status");
                let uri = results[0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"]
                    .as_str()
                    .unwrap();
                assert_eq!(
                    url::Url::parse(uri).unwrap().to_file_path().unwrap(),
                    expected_source
                );
                sarif_findings = Some(
                    results
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|result| {
                            let uri = result["locations"][0]["physicalLocation"]["artifactLocation"]
                                ["uri"]
                                .as_str()
                                .unwrap();
                            (
                                result["properties"]["url"].as_str().unwrap().to_owned(),
                                result["properties"]["reason"]["kind"]
                                    .as_str()
                                    .unwrap()
                                    .to_owned(),
                                url::Url::parse(uri)
                                    .unwrap()
                                    .to_file_path()
                                    .unwrap()
                                    .to_string_lossy()
                                    .into_owned(),
                            )
                        })
                        .collect::<BTreeSet<_>>(),
                );
            }
            _ => unreachable!(),
        }
    }
    assert_eq!(table_findings, json_findings);
    assert_eq!(json_findings, sarif_findings);
}

#[tokio::test(flavor = "multi_thread")]
async fn filters_do_not_change_fail_on_exit_status() {
    let (directory, _) = outdated_fixture().await;
    for (fail_on, exit_code) in [("outdated", 1), ("likely-dead", 0)] {
        let mut command = util::command();
        command.args([
            "scan",
            "--no-cache",
            "--retries",
            "0",
            "--format",
            "json",
            "--min-confidence",
            "dead-certain",
            "--fail-on",
            fail_on,
            directory.path().to_str().unwrap(),
        ]);
        let output = tokio::task::block_in_place(|| command.assert())
            .code(exit_code)
            .get_output()
            .clone();
        assert!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap()["findings"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn json_alias_and_output_file_keep_stdout_empty() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("note.md"),
        "[invalid email](mailto:not-an-address)",
    )
    .unwrap();
    let output = directory.path().join("report.json");
    let mut command = util::command();
    command.args([
        "scan",
        "--no-cache",
        "--json",
        "-o",
        output.to_str().unwrap(),
        directory.path().to_str().unwrap(),
    ]);
    command.assert().code(1).stdout("").stderr("");
    let report: Value = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    assert_eq!(report["findings"].as_array().unwrap().len(), 1);
}
