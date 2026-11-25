use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "dayfly",
    about = "Fresh, single-use self-hosted runners for GitHub Actions jobs"
)]
pub struct Cli {
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Config file [default: ./dayfly.toml, then /etc/dayfly/dayfly.toml]"
    )]
    pub config: Option<PathBuf>,

    #[arg(
        long,
        global = true,
        value_enum,
        value_name = "FORMAT",
        default_value_t = LogFormat::Pretty,
        help = "Log output format"
    )]
    pub log_format: LogFormat,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "Start the runner pools")]
    Run,
    #[command(subcommand, about = "Inspect the config file")]
    Config(ConfigCommand),
    #[command(about = "Print the dayfly version")]
    Version,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    #[command(about = "Check the config file")]
    Check,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LogFormat {
    Pretty,
    Json,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::CommandFactory as _;

    use super::*;

    #[test]
    fn command_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn global_flags_are_accepted_after_the_subcommand() {
        let cli = Cli::try_parse_from([
            "dayfly",
            "config",
            "check",
            "--config",
            "custom.toml",
            "--log-format",
            "json",
        ])
        .unwrap();

        assert_eq!(cli.config.as_deref(), Some(Path::new("custom.toml")));
        assert_eq!(cli.log_format, LogFormat::Json);
    }
}
