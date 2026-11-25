use std::io;
use std::process::Command;

#[test]
fn closed_stdout_prints_the_error_chain_once_and_fails() {
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);

    let output = Command::new(env!("CARGO_BIN_EXE_dayfly"))
        .stdout(writer)
        .output()
        .unwrap();

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.starts_with("error: failed to run dayfly: failed to write output: "),
        "{stderr}"
    );
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
}
