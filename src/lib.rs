pub mod cli;
pub mod config;
pub mod logging;

use std::io::{self, Write};

use cli::{Cli, Command, ConfigCommand};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RunError {
    #[error("failed to write output")]
    Output(#[source] io::Error),
    #[error("failed to locate the config file")]
    LocateConfig(#[from] config::LocateError),
    #[error("failed to load the config file")]
    LoadConfig(#[from] config::LoadError),
}

pub fn run(cli: Cli, mut output: impl Write) -> Result<(), RunError> {
    match cli.command {
        Command::Run => unimplemented!("dayfly run"),
        Command::Config(ConfigCommand::Check) => {
            let path = config::locate(cli.config)?;
            let config = config::load(&path)?;
            write!(output, "{} is valid\n\n{config}", path.display())
        }
        Command::Version => writeln!(output, "dayfly {}", env!("CARGO_PKG_VERSION")),
    }
    .map_err(RunError::Output)
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    #[test]
    fn version_prints_the_version() {
        let mut output = Vec::new();

        run(Cli::parse_from(["dayfly", "version"]), &mut output).unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!("dayfly {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
}
