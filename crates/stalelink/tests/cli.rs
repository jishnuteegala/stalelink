use predicates::prelude::*;

mod util;

#[test]
fn help_lists_commands_and_examples() {
    let output = util::command()
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
    util::command()
        .args(["scan", "--json", "--format", "sarif", "x.md"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn scan_requires_paths_or_stdin() {
    util::command()
        .arg("scan")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Usage:"));
}

#[test]
fn supported_shells_generate_non_empty_completions() {
    for shell in ["bash", "zsh", "fish", "powershell"] {
        let output = util::command()
            .args(["completions", shell])
            .assert()
            .success()
            .get_output()
            .clone();
        let completions = String::from_utf8(output.stdout).unwrap();
        assert!(!completions.is_empty(), "{shell} completions were empty");
        assert!(
            completions.contains("stalelink"),
            "{shell} completions lack binary name"
        );
    }
}

#[test]
fn invalid_completion_shell_is_a_usage_error() {
    util::command()
        .args(["completions", "nushell"])
        .assert()
        .code(2);
}

#[test]
fn backup_requires_write() {
    util::command()
        .args(["fix", "--backup", "x.md"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--write"));
}

#[test]
fn backup_conflicts_with_copy() {
    util::command()
        .args(["fix", "--backup", "--copy", "x.md"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn fix_rejects_scan_output_flags() {
    for flag in [
        "--format",
        "--json",
        "--output",
        "--min-confidence",
        "--fail-on",
    ] {
        let mut command = util::command();
        let mut args = vec!["fix", flag];
        if matches!(
            flag,
            "--format" | "--output" | "--min-confidence" | "--fail-on"
        ) {
            args.push("json");
        }
        args.push("x.md");
        command.args(args).assert().code(2);
    }
}

#[test]
fn clean_scan_exits_zero_with_no_stdout() {
    let file = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
    let mut command = util::command();
    command
        .args(["scan", "--no-cache", file.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout("");
}

#[test]
fn quiet_suppresses_verbose_traces() {
    let file = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
    util::command()
        .args([
            "--quiet",
            "-v",
            "scan",
            "--no-cache",
            file.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr("");
}

#[test]
fn verbose_writes_traces_to_stderr_only() {
    let file = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
    util::command()
        .args(["-v", "scan", "--no-cache", file.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout("")
        .stderr(predicate::str::contains("trace: resolving configuration"));
}

#[tokio::test]
async fn verbosity_levels_add_url_and_response_details() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::any())
        .respond_with(wiremock::ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let url = format!("{}/clean", server.uri());
    let file = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
    std::fs::write(file.path(), url).unwrap();

    let output = util::command()
        .args(["-v", "scan", "--no-cache", file.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    let single = String::from_utf8(output.stderr).unwrap();
    assert!(single.contains("configuration"));
    assert!(!single.contains("check url="));

    let output = util::command()
        .args(["-vv", "scan", "--no-cache", file.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    let double = String::from_utf8(output.stderr).unwrap();
    assert!(double.contains("check url="));
    assert!(!double.contains("response url="));

    let output = util::command()
        .args(["-vvv", "scan", "--no-cache", file.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    let triple = String::from_utf8(output.stderr).unwrap();
    assert!(triple.contains("response url="));
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
    let mut command = util::command();
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
    let mut command = util::command();
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
    let mut command = util::command();
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
    let mut stats = util::command();
    stats
        .current_dir(directory.path())
        .args(["cache", "stats"])
        .assert()
        .success();
    let database = cache.join("verdicts.sqlite3");
    assert!(database.exists());
    let mut clear = util::command();
    clear
        .current_dir(directory.path())
        .args(["cache", "clear"])
        .assert()
        .success();
    assert!(!database.exists());
}
