use std::fs;
use std::io;
use std::process::Command;

fn dayfly() -> Command {
    Command::new(env!("CARGO_BIN_EXE_dayfly"))
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
