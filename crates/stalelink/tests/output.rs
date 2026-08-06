use std::path::Path;

use jsonschema::validator_for;
use serde_json::Value;
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
        result["locations"][0]["physicalLocation"]["region"]["startLine"],
        1
    );
    assert_eq!(
        result["locations"][0]["physicalLocation"]["region"]["startColumn"],
        10
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn formats_share_minimum_confidence_filtering() {
    let (directory, _) = fixture().await;
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
            "table" => assert!(String::from_utf8(output).unwrap().contains("/missing")),
            "json" => assert_eq!(
                serde_json::from_slice::<Value>(&output).unwrap()["findings"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            ),
            "sarif" => assert_eq!(
                serde_json::from_slice::<Value>(&output).unwrap()["runs"][0]["results"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            ),
            _ => unreachable!(),
        }
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
