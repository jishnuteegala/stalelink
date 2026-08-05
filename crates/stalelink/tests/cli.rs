use assert_cmd::Command;
use predicates::prelude::*;

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
fn backup_conflicts_with_copy() {
    Command::cargo_bin("stalelink")
        .unwrap()
        .args(["fix", "--backup", "--copy", "x.md"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn clean_scan_exits_zero_with_no_stdout() {
    let file = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
    let mut command = Command::cargo_bin("stalelink").unwrap();
    isolated(&mut command);
    command
        .args(["scan", "--no-cache", file.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout("");
}

#[test]
fn unknown_toml_key_is_a_usage_error_with_suggestion() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("stalelink.toml"),
        "[network]\ntimout = \"1s\"\n",
    )
    .unwrap();
    std::fs::write(directory.path().join("note.txt"), "").unwrap();
    let mut command = Command::cargo_bin("stalelink").unwrap();
    isolated(&mut command);
    command
        .args(["scan", "--no-cache", directory.path().to_str().unwrap()])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("did you mean `network.timeout`"));
}

#[test]
fn invalid_auth_config_is_a_usage_error() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("stalelink.toml"),
        "[auth]\nauth = \"cookes\"\n",
    )
    .unwrap();
    std::fs::write(directory.path().join("note.txt"), "").unwrap();
    let mut command = Command::cargo_bin("stalelink").unwrap();
    isolated(&mut command);
    command
        .args(["scan", "--no-cache", directory.path().to_str().unwrap()])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("invalid auth.auth"));
}

#[test]
fn invalid_auth_environment_is_a_usage_error() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("note.txt"), "").unwrap();
    let mut command = Command::cargo_bin("stalelink").unwrap();
    isolated(&mut command);
    command
        .env("STALELINK_AUTH_BROWSER", "safari")
        .args(["scan", "--no-cache", directory.path().to_str().unwrap()])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("invalid auth.browser"));
}

#[test]
fn cache_commands_use_toml_directory_from_current_directory() {
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("configured-cache");
    std::fs::write(
        directory.path().join("stalelink.toml"),
        format!("[cache]\ndir = '''{}'''\n", cache.display()),
    )
    .unwrap();
    let mut stats = Command::cargo_bin("stalelink").unwrap();
    isolated(&mut stats);
    stats
        .current_dir(directory.path())
        .args(["cache", "stats"])
        .assert()
        .success();
    let database = cache.join("verdicts.sqlite3");
    assert!(database.exists());
    let mut clear = Command::cargo_bin("stalelink").unwrap();
    isolated(&mut clear);
    clear
        .current_dir(directory.path())
        .args(["cache", "clear"])
        .assert()
        .success();
    assert!(!database.exists());
}
