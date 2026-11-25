use std::fs;
use std::io;
use std::process::Command;
use std::str;

fn dayfly() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dayfly"));
    command.env_remove("RUST_LOG");
    command
}

fn json_lines(stream: &[u8]) -> Vec<serde_json::Value> {
    str::from_utf8(stream)
        .unwrap()
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("`{line}` is not JSON: {error}"))
        })
        .collect()
}

fn debug_lines(lines: &[serde_json::Value]) -> Vec<&serde_json::Value> {
    lines
        .iter()
        .filter(|line| line["level"] == "DEBUG")
        .collect()
}

#[test]
fn closed_stdout_prints_the_error_chain_once_and_fails() {
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);

    let output = dayfly().arg("version").stdout(writer).output().unwrap();

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.starts_with("error: failed to run dayfly: failed to write output: "),
        "{stderr}"
    );
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
}

#[test]
fn help_lists_the_subcommands() {
    let output = dayfly().arg("--help").output().unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    for subcommand in ["run", "config", "version"] {
        assert!(
            stdout
                .lines()
                .any(|line| line.trim_start().starts_with(subcommand)),
            "`{subcommand}` missing from:\n{stdout}"
        );
    }
}

#[test]
fn unknown_flag_prints_usage_and_exits_with_2() {
    let output = dayfly().arg("--no-such-flag").output().unwrap();

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("Usage: dayfly"), "{stderr}");
    assert_eq!(output.stdout, b"");
}

#[test]
fn config_check_finds_dayfly_toml_in_the_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("dayfly.toml"), "").unwrap();

    let output = dayfly()
        .args(["config", "check"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "./dayfly.toml\n");
}

#[test]
fn json_format_reports_a_fatal_error_as_one_json_line() {
    let dir = tempfile::tempdir().unwrap();

    let output = dayfly()
        .args(["--log-format", "json", "config", "check", "--config"])
        .arg(dir.path().join("missing.toml"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let lines = json_lines(&output.stderr);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["level"], "ERROR");
    let message = lines[0]["fields"]["message"].as_str().unwrap();
    assert!(
        message.starts_with("failed to run dayfly: failed to locate the config file: "),
        "{message}"
    );
}

#[test]
fn rust_log_changes_the_log_level() {
    let dir = tempfile::tempdir().unwrap();
    let config_check = |rust_log: Option<&str>| {
        let mut command = dayfly();
        command
            .args(["--log-format", "json", "config", "check"])
            .current_dir(dir.path());
        if let Some(rust_log) = rust_log {
            command.env("RUST_LOG", rust_log);
        }
        json_lines(&command.output().unwrap().stderr)
    };

    let default = config_check(None);
    let debug = config_check(Some("debug"));

    assert_eq!(debug_lines(&default), Vec::<&serde_json::Value>::new());
    assert!(
        debug_lines(&debug)
            .iter()
            .any(|line| line["fields"]["path"] == "./dayfly.toml"),
        "{debug:?}"
    );
}

#[test]
fn invalid_rust_log_is_an_error() {
    let output = dayfly()
        .arg("version")
        .env("RUST_LOG", "dayfly=loud")
        .output()
        .unwrap();

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.starts_with("error: failed to set up logging: invalid RUST_LOG filter: "),
        "{stderr}"
    );
    assert_eq!(output.stdout, b"");
}
