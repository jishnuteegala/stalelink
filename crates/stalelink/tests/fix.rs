use std::fs;

use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

mod util;

async fn redirect_server() -> MockServer {
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
    server
}

fn fix(path: &std::path::Path, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut command = util::command();
    command.args(["fix", "--no-cache", "--retries", "0"]);
    command.args(args);
    command.arg(path);
    tokio::task::block_in_place(|| command.assert())
}

#[tokio::test(flavor = "multi_thread")]
async fn dry_run_prints_diff_and_does_not_modify_text_files() {
    let server = redirect_server().await;
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("note.md");
    let original = format!("before\n[old]({}/old)\nafter\n", server.uri());
    fs::write(&file, &original).unwrap();

    let output = fix(&file, &["--min-fix-confidence", "outdated"])
        .code(0)
        .stderr("")
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--- a/"));
    assert!(stdout.contains(&format!("-[old]({}/old)", server.uri())));
    assert!(stdout.contains(&format!("+[old]({}/new)", server.uri())));
    assert_eq!(fs::read_to_string(&file).unwrap(), original);
}

#[tokio::test(flavor = "multi_thread")]
async fn write_updates_markdown_text_and_html_without_changing_other_bytes() {
    let server = redirect_server().await;
    let directory = tempfile::tempdir().unwrap();
    let markdown = directory.path().join("note.md");
    let text = directory.path().join("note.txt");
    let html = directory.path().join("page.html");
    fs::write(
        &markdown,
        format!("prefix [x]({}/old) suffix\n", server.uri()),
    )
    .unwrap();
    fs::write(&text, format!("prefix {}/old suffix\n", server.uri())).unwrap();
    fs::write(
        &html,
        format!(
            r#"<a href="{0}/old">x</a><img src="{0}/old">"#,
            server.uri()
        ),
    )
    .unwrap();

    fix(
        directory.path(),
        &["--write", "--min-fix-confidence", "outdated"],
    )
    .code(0)
    .stderr("");
    for file in [&markdown, &text, &html] {
        let actual = fs::read_to_string(file).unwrap();
        assert!(!actual.contains("/old"));
        assert!(actual.contains(&format!("{}/new", server.uri())));
    }
    assert_eq!(
        fs::read_to_string(&markdown).unwrap(),
        format!("prefix [x]({}/new) suffix\n", server.uri())
    );
    assert_eq!(
        fs::read_to_string(&text).unwrap(),
        format!("prefix {}/new suffix\n", server.uri())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn backup_and_copy_have_their_documented_effects() {
    let server = redirect_server().await;
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("note.txt");
    let original = format!("{}/old\n", server.uri());
    fs::write(&file, &original).unwrap();

    fix(
        &file,
        &["--write", "--backup", "--min-fix-confidence", "outdated"],
    )
    .code(0)
    .stderr("");
    assert_eq!(
        fs::read_to_string(file.with_extension("txt.bak")).unwrap(),
        original
    );
    assert!(fs::read_to_string(&file).unwrap().contains("/new"));

    fs::write(&file, &original).unwrap();
    fix(&file, &["--copy", "--min-fix-confidence", "outdated"])
        .code(0)
        .stderr("");
    assert_eq!(fs::read_to_string(&file).unwrap(), original);
    assert!(
        fs::read_to_string(directory.path().join("note.fixed.txt"))
            .unwrap()
            .contains("/new")
    );
}

#[test]
fn usage_rejects_copy_and_write_together() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("note.txt"), "https://example.test\n").unwrap();
    fix(directory.path(), &["--copy", "--write"]).code(2);
}
