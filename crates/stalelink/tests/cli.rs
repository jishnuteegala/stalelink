use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn help_lists_commands_and_examples() {
    Command::cargo_bin("stalelink")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("scan"))
        .stdout(predicate::str::contains("fix"))
        .stdout(predicate::str::contains("cache"))
        .stdout(predicate::str::contains("completions"))
        .stdout(predicate::str::contains("Examples:"));
}

#[test]
fn scan_requires_paths_or_stdin() {
    Command::cargo_bin("stalelink")
        .unwrap()
        .arg("scan")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Usage:"));
}

#[test]
fn bash_completions_are_generated() {
    Command::cargo_bin("stalelink")
        .unwrap()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::is_match("(?s).+stalelink").unwrap());
}

#[test]
fn scan_is_not_implemented_yet() {
    let file = tempfile::NamedTempFile::new().unwrap();
    Command::cargo_bin("stalelink")
        .unwrap()
        .args(["scan", file.path().to_str().unwrap()])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("not implemented"));
}
