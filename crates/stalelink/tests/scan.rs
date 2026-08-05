use std::time::{Duration, Instant};

use assert_cmd::Command;
use predicates::prelude::*;
use stalelink_core::{
    check::HttpChecker,
    model::{Confidence, Reason},
    scan::{NoProgress, ScanInput, scan as core_scan},
    walk::WalkOptions,
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

async fn serve(routes: &[(&str, u16)]) -> MockServer {
    let server = MockServer::start().await;
    for (route, status) in routes {
        Mock::given(path(*route))
            .respond_with(ResponseTemplate::new(*status))
            .mount(&server)
            .await;
    }
    server
}

fn scan(dir: &std::path::Path, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut command = Command::cargo_bin("stalelink").unwrap();
    isolated(&mut command);
    command.args(["scan", "--retries", "0", "--no-cache"]);
    command.args(args);
    command.arg(dir);
    tokio::task::block_in_place(|| command.assert())
}

fn isolated(command: &mut Command) {
    for variable in [
        "STALELINK_NETWORK_TIMEOUT",
        "STALELINK_NETWORK_MAX_CONCURRENCY",
        "STALELINK_NETWORK_PER_HOST",
        "STALELINK_NETWORK_RETRIES",
        "STALELINK_NETWORK_USER_AGENT",
        "STALELINK_CACHE_TTL",
        "STALELINK_CACHE_DIR",
        "STALELINK_IGNORE_LOCAL_LINKS",
        "STALELINK_OUTPUT_FAIL_ON",
        "STALELINK_AUTH_AUTH",
        "STALELINK_AUTH_BROWSER",
    ] {
        command.env_remove(variable);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn dead_links_exit_one_with_table_rows() {
    let server = serve(&[("/ok", 200), ("/missing", 404), ("/gone", 410)]).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("docs.md"),
        format!("[a]({0}/missing)\nfine: {0}/ok\n", server.uri()),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("page.html"),
        format!(r#"<a href="{}/gone">x</a>"#, server.uri()),
    )
    .unwrap();
    let assert = scan(dir.path(), &[]).code(1).stderr("");
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("DEAD-CERTAIN"));
    assert!(stdout.contains(&format!("{}/missing", server.uri())));
    assert!(stdout.contains(&format!("{}/gone", server.uri())));
    assert!(stdout.contains("HTTP 404"));
    assert!(stdout.contains("HTTP 410"));
    assert!(stdout.contains("docs.md:1:5"));
    assert!(!stdout.contains(&format!("{}/ok", server.uri())));
}

#[tokio::test(flavor = "multi_thread")]
async fn clean_corpus_exits_zero_with_empty_stdout() {
    let server = serve(&[("/ok", 200)]).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("note.txt"),
        format!("{}/ok\n", server.uri()),
    )
    .unwrap();
    scan(dir.path(), &[]).code(0).stdout("").stderr("");
}

#[tokio::test(flavor = "multi_thread")]
async fn fail_on_dead_certain_ignores_suspect_findings() {
    let server = serve(&[("/flaky", 500)]).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("note.txt"),
        format!("{}/flaky\n", server.uri()),
    )
    .unwrap();
    scan(dir.path(), &["--fail-on", "dead-certain"])
        .code(0)
        .stdout(predicate::str::contains("SUSPECT"));
}

#[tokio::test(flavor = "multi_thread")]
async fn min_confidence_does_not_suppress_fail_on() {
    let server = serve(&[("/missing", 404)]).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("note.txt"),
        format!("{}/missing\n", server.uri()),
    )
    .unwrap();
    scan(
        dir.path(),
        &["--min-confidence", "dead-certain", "--fail-on", "suspect"],
    )
    .code(1)
    .stdout(predicate::str::contains("DEAD-CERTAIN"));
}

#[tokio::test(flavor = "multi_thread")]
async fn table_has_header_when_findings_exist() {
    let server = serve(&[("/missing", 404)]).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("note.txt"),
        format!("{}/missing\n", server.uri()),
    )
    .unwrap();
    scan(dir.path(), &[]).code(1).stdout(
        predicate::str::contains("CONFIDENCE")
            .and(predicate::str::contains("URL"))
            .and(predicate::str::contains("SOURCE"))
            .and(predicate::str::contains("REASON")),
    );
}

#[test]
fn zero_concurrency_is_a_usage_error() {
    let mut command = Command::cargo_bin("stalelink").unwrap();
    isolated(&mut command);
    command
        .args(["scan", "--no-cache", "--max-concurrency", "0", "x.txt"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("at least 1"));
}

async fn head_falls_back_to_get(head_status: u16) {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .respond_with(ResponseTemplate::new(head_status))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("note.txt"), format!("{}/x\n", server.uri())).unwrap();
    scan(dir.path(), &[]).code(0).stdout("");
}

#[tokio::test(flavor = "multi_thread")]
async fn head_method_not_allowed_falls_back_to_get() {
    head_falls_back_to_get(405).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn head_forbidden_falls_back_to_get() {
    head_falls_back_to_get(403).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn head_not_implemented_falls_back_to_get() {
    head_falls_back_to_get(501).await;
}

#[test]
fn bad_exclude_url_regex_is_usage_error_even_with_missing_path() {
    let mut command = Command::cargo_bin("stalelink").unwrap();
    isolated(&mut command);
    command
        .args([
            "scan",
            "--no-cache",
            "--exclude-url",
            "[",
            "does-not-exist.md",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("invalid --exclude-url"));
}

#[tokio::test(flavor = "multi_thread")]
async fn timeout_reports_likely_dead() {
    let server = MockServer::start().await;
    Mock::given(path("/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("note.txt"),
        format!("{}/slow\n", server.uri()),
    )
    .unwrap();
    scan(dir.path(), &["--timeout", "1"])
        .code(1)
        .stdout(predicate::str::contains("LIKELY-DEAD"));
}

#[tokio::test(flavor = "multi_thread")]
async fn connection_refused_reports_likely_dead() {
    let url = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!(
            "http://127.0.0.1:{}/x",
            listener.local_addr().unwrap().port()
        )
    };
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("note.txt"), format!("{url}\n")).unwrap();
    scan(dir.path(), &[])
        .code(1)
        .stdout(predicate::str::contains("LIKELY-DEAD"));
}

#[tokio::test(flavor = "multi_thread")]
async fn validates_local_paths_anchors_and_contact_syntax_without_network() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("guides")).unwrap();
    std::fs::write(
        dir.path().join("docs.md"),
        "[missing](missing.md)\n[anchor](#no-such-anchor)\n[valid](guides/setup.md#installation)\n[bad email](mailto:not-an-address)\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("guides/setup.md"), "# Installation!\n").unwrap();

    let assert = scan(dir.path(), &[]).code(1).stderr("");
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("missing.md"));
    assert!(stdout.contains("#no-such-anchor"));
    assert!(stdout.contains("mailto:not-an-address"));
    assert!(!stdout.contains("guides/setup.md#installation"));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn local_validation_returns_the_expected_core_finding_contract() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("docs.md");
    std::fs::write(
        &source,
        "[missing](missing.md)\n[anchor](#no-such-anchor)\n[bad email](mailto:not-an-address)\n",
    )
    .unwrap();
    let checker = HttpChecker::new(Duration::from_secs(1), 0, 1, "stalelink-test".into()).unwrap();
    let report = core_scan(
        ScanInput {
            paths: vec![dir.path().into()],
            walk: WalkOptions::default(),
            max_concurrency: 1,
            exclude_urls: vec![],
            exclude_domains: vec![],
            check_local: true,
        },
        &checker,
        &NoProgress,
    )
    .await
    .unwrap();

    assert_eq!(report.findings.len(), 3, "{:#?}", report.findings);
    let missing = report
        .findings
        .iter()
        .find(|finding| finding.url == "missing.md")
        .unwrap();
    assert_eq!(missing.verdict.reason, Reason::LocalMissing);
    assert_eq!(missing.verdict.confidence, Confidence::DeadCertain);
    assert_eq!(missing.source.path, source);
    assert!(missing.verdict.evidence[0].detail.contains("missing.md"));
    let anchor = report
        .findings
        .iter()
        .find(|finding| finding.url == "#no-such-anchor")
        .unwrap();
    assert_eq!(anchor.verdict.reason, Reason::LocalMissing);
    assert_eq!(anchor.source.path, source);
    let email = report
        .findings
        .iter()
        .find(|finding| finding.url == "mailto:not-an-address")
        .unwrap();
    assert_eq!(email.verdict.reason, Reason::SyntaxInvalid);
    assert_eq!(email.source.path, source);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn no_local_suppresses_local_and_contact_findings() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("docs.md"),
        "[missing](missing.md)\n[bad email](mailto:not-an-address)\n",
    )
    .unwrap();
    scan(dir.path(), &["--no-local"])
        .code(0)
        .stdout("")
        .stderr("");
}

#[tokio::test(flavor = "multi_thread")]
async fn cache_stats_record_a_repeat_scan_hit() {
    let server = MockServer::start().await;
    Mock::given(path("/missing"))
        .respond_with(ResponseTemplate::new(404).set_delay(Duration::from_millis(700)))
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("note.txt"),
        format!("{}/missing\n", server.uri()),
    )
    .unwrap();
    let mut elapsed = Vec::new();
    for _ in 0..2 {
        let mut command = Command::cargo_bin("stalelink").unwrap();
        isolated(&mut command);
        command.env("STALELINK_CACHE_DIR", cache.path()).args([
            "scan",
            "--retries",
            "0",
            directory.path().to_str().unwrap(),
        ]);
        let started = Instant::now();
        tokio::task::block_in_place(|| command.assert()).code(1);
        elapsed.push(started.elapsed());
    }
    assert!(elapsed[1] + Duration::from_millis(250) < elapsed[0]);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    let mut stats = Command::cargo_bin("stalelink").unwrap();
    isolated(&mut stats);
    stats
        .env("STALELINK_CACHE_DIR", cache.path())
        .args(["cache", "stats"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hits: 1").and(predicate::str::contains("misses: 1")));
}

#[tokio::test(flavor = "multi_thread")]
async fn refresh_replaces_a_cached_clean_verdict() {
    let server = MockServer::start().await;
    Mock::given(path("/ok"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("note.txt"),
        format!("{}/ok", server.uri()),
    )
    .unwrap();
    let mut requests = Vec::new();
    for refresh in [false, false, true] {
        let mut command = Command::cargo_bin("stalelink").unwrap();
        isolated(&mut command);
        command.env("STALELINK_CACHE_DIR", cache.path()).args([
            "scan",
            "--retries",
            "0",
            directory.path().to_str().unwrap(),
        ]);
        if refresh {
            command.arg("--refresh");
        }
        tokio::task::block_in_place(|| command.assert()).success();
        requests.push(server.received_requests().await.unwrap().len());
    }
    let connection = rusqlite::Connection::open(cache.path().join("verdicts.sqlite3")).unwrap();
    let (hits, misses): (u64, u64) = connection
        .query_row(
            "SELECT hits, misses FROM cache_stats WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((hits, misses), (1, 1));
    assert_eq!(requests[1], requests[0]);
    assert!(requests[2] > requests[1]);
}

#[tokio::test(flavor = "multi_thread")]
async fn flags_override_environment_toml_and_defaults() {
    let server = MockServer::start().await;
    Mock::given(path("/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("stalelink.toml"),
        "[network]\ntimeout = \"3s\"\nretries = 0\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("note.txt"),
        format!("{}/slow\n", server.uri()),
    )
    .unwrap();
    let mut command = Command::cargo_bin("stalelink").unwrap();
    isolated(&mut command);
    command.env("STALELINK_NETWORK_TIMEOUT", "2s").args([
        "scan",
        "--timeout",
        "1",
        directory.path().to_str().unwrap(),
    ]);
    tokio::task::block_in_place(|| command.assert())
        .code(1)
        .stdout(predicate::str::contains("LIKELY-DEAD"));
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_uses_first_path_and_stdin_path_before_config_resolution() {
    let server = MockServer::start().await;
    Mock::given(path("/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
        .mount(&server)
        .await;
    let root = tempfile::tempdir().unwrap();
    let fast = root.path().join("fast");
    let slow = root.path().join("slow");
    std::fs::create_dir(&fast).unwrap();
    std::fs::create_dir(&slow).unwrap();
    std::fs::write(
        fast.join("stalelink.toml"),
        "[network]\ntimeout = \"1s\"\nretries = 0\n",
    )
    .unwrap();
    std::fs::write(
        slow.join("stalelink.toml"),
        "[network]\ntimeout = \"3s\"\nretries = 0\n",
    )
    .unwrap();
    let fast_file = fast.join("note.txt");
    let slow_file = slow.join("note.txt");
    std::fs::write(&fast_file, format!("{}/slow", server.uri())).unwrap();
    std::fs::write(&slow_file, format!("{}/slow", server.uri())).unwrap();
    for (first, second, expected) in [(&fast_file, &slow_file, 1), (&slow_file, &fast_file, 0)] {
        let mut command = Command::cargo_bin("stalelink").unwrap();
        isolated(&mut command);
        command.args([
            "scan",
            "--no-cache",
            first.to_str().unwrap(),
            second.to_str().unwrap(),
        ]);
        tokio::task::block_in_place(|| command.assert()).code(expected);
    }
    let mut command = Command::cargo_bin("stalelink").unwrap();
    isolated(&mut command);
    command
        .args(["scan", "--no-cache", "--stdin"])
        .write_stdin(format!("{}\n", fast_file.display()));
    tokio::task::block_in_place(|| command.assert()).code(1);
}

#[test]
fn cache_clear_removes_the_injected_database() {
    let cache = tempfile::tempdir().unwrap();
    let mut stats = Command::cargo_bin("stalelink").unwrap();
    isolated(&mut stats);
    stats
        .env("STALELINK_CACHE_DIR", cache.path())
        .args(["cache", "stats"])
        .assert()
        .success();
    let database = cache.path().join("verdicts.sqlite3");
    assert!(database.exists());
    let mut clear = Command::cargo_bin("stalelink").unwrap();
    isolated(&mut clear);
    clear
        .env("STALELINK_CACHE_DIR", cache.path())
        .args(["cache", "clear"])
        .assert()
        .success();
    assert!(!database.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn config_ignore_rules_exclude_files_urls_and_domains() {
    let server = serve(&[("/missing", 404)]).await;
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("stalelink.toml"),
        "[ignore]\nexclude = [\"ignored.md\"]\nexclude-url = [\"/skip\"]\nexclude-domain = [\"127.0.0.1\"]\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("ignored.md"),
        format!("{}/missing", server.uri()),
    )
    .unwrap();
    std::fs::write(
        directory.path().join("note.md"),
        format!("{}/skip", server.uri()),
    )
    .unwrap();
    let mut command = Command::cargo_bin("stalelink").unwrap();
    isolated(&mut command);
    command.args([
        "scan",
        "--no-cache",
        "--retries",
        "0",
        directory.path().to_str().unwrap(),
    ]);
    tokio::task::block_in_place(|| command.assert())
        .success()
        .stdout("");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_scans_keep_the_wal_cache_readable() {
    let server = MockServer::start().await;
    Mock::given(path("/"))
        .respond_with(ResponseTemplate::new(404).set_delay(Duration::from_millis(600)))
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for name in ["first", "second"] {
        let path = directory.path().join(format!("{name}.txt"));
        std::fs::write(&path, format!("{server}/{name}", server = server.uri())).unwrap();
        paths.push(path);
    }
    let mut children = Vec::new();
    for path in paths {
        let binary = Command::cargo_bin("stalelink").unwrap();
        let mut command = std::process::Command::new(binary.get_program());
        for variable in [
            "STALELINK_NETWORK_TIMEOUT",
            "STALELINK_NETWORK_MAX_CONCURRENCY",
            "STALELINK_NETWORK_PER_HOST",
            "STALELINK_NETWORK_RETRIES",
            "STALELINK_NETWORK_USER_AGENT",
            "STALELINK_CACHE_TTL",
            "STALELINK_CACHE_DIR",
            "STALELINK_IGNORE_LOCAL_LINKS",
            "STALELINK_OUTPUT_FAIL_ON",
            "STALELINK_AUTH_AUTH",
            "STALELINK_AUTH_BROWSER",
        ] {
            command.env_remove(variable);
        }
        command.env("STALELINK_CACHE_DIR", cache.path()).args([
            "scan",
            "--retries",
            "0",
            path.to_str().unwrap(),
        ]);
        children.push(command.spawn().unwrap());
    }
    for mut child in children {
        assert_eq!(
            tokio::task::block_in_place(|| child.wait()).unwrap().code(),
            Some(1)
        );
    }
    assert!(server.received_requests().await.unwrap().len() >= 2);
    let connection = rusqlite::Connection::open(cache.path().join("verdicts.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(
        connection
            .query_row("SELECT COUNT(*) FROM verdicts", [], |row| row
                .get::<_, u64>(0))
            .unwrap()
            >= 1
    );
}

#[test]
fn no_cache_creates_no_database() {
    let directory = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("note.txt"), "").unwrap();
    let mut command = Command::cargo_bin("stalelink").unwrap();
    isolated(&mut command);
    command.env("STALELINK_CACHE_DIR", cache.path()).args([
        "scan",
        "--no-cache",
        directory.path().to_str().unwrap(),
    ]);
    command.assert().success();
    assert!(!cache.path().join("verdicts.sqlite3").exists());
}
