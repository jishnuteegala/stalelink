use std::time::Duration;

use assert_cmd::Command;
use predicates::prelude::*;
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
    command.args(["scan", "--retries", "0"]);
    command.args(args);
    command.arg(dir);
    tokio::task::block_in_place(|| command.assert())
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
async fn head_method_not_allowed_falls_back_to_get() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .respond_with(ResponseTemplate::new(405))
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
