use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn help_lists_commands_and_examples() {
    let output = Command::cargo_bin("stalelink")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Examples:"))
        .get_output()
        .clone();
    let help = String::from_utf8(output.stdout).unwrap();
    let commands = help.split("Commands:").nth(1).unwrap();
    let commands = commands.split("Options:").next().unwrap();
    for cmd in ["scan", "fix", "cache", "completions"] {
        assert!(commands.contains(cmd), "Commands table missing {cmd}");
    }
}

#[test]
fn json_conflicts_with_format() {
    Command::cargo_bin("stalelink")
        .unwrap()
        .args(["scan", "--json", "--format", "sarif", "x.md"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
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
        .stdout(predicate::str::contains("_stalelink()"));
}

#[test]
fn backup_requires_write() {
    Command::cargo_bin("stalelink")
        .unwrap()
        .args(["fix", "--backup", "x.md"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--write"));
}

#[test]
fn scan_is_not_implemented_yet() {
    // The path is never opened; scan short-circuits before any IO.
    let file = tempfile::NamedTempFile::new().unwrap();
    Command::cargo_bin("stalelink")
        .unwrap()
        .args(["scan", file.path().to_str().unwrap()])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("not implemented"));
}
