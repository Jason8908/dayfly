use std::io;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::Parser as _;
use dayfly::cli::Cli;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dayfly::run(cli, io::stdout().lock()).context("failed to run dayfly") {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
