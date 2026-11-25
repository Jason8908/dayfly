use std::io;
use std::process::ExitCode;

use anyhow::Context as _;

fn main() -> ExitCode {
    match dayfly::run(io::stdout().lock()).context("failed to run dayfly") {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
