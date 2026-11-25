use std::io;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::Parser as _;
use dayfly::cli::{Cli, LogFormat};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let log_format = cli.log_format;
    if let Err(error) = dayfly::logging::init(log_format).context("failed to set up logging") {
        eprintln!("error: {error:#}");
        return ExitCode::FAILURE;
    }
    match dayfly::run(cli, io::stdout().lock()).context("failed to run dayfly") {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            match log_format {
                LogFormat::Pretty => eprintln!("error: {error:#}"),
                LogFormat::Json => tracing::error!("{error:#}"),
            }
            ExitCode::FAILURE
        }
    }
}
