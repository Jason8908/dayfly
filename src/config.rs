mod document;
mod github;
mod pools;

use std::fmt;
use std::fs;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use toml::de::DeTable;

use document::{Diagnostics, Table};
pub use document::{Location, Problem, Reason};
pub use github::{Account, AccountError, Auth, CredentialSource, Repository, RepositoryError};
pub use pools::{
    Backend, ByteSize, ByteSizeError, Cpus, CpusError, Image, ImageError, Label, LabelError,
    Limits, Pool, PoolName, PoolNameError, Scope,
};

const SEARCH_PATHS: [&str; 2] = ["./dayfly.toml", "/etc/dayfly/dayfly.toml"];

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LocateError {
    #[error("`{}` does not exist", .0.display())]
    Missing(PathBuf),
    #[error("no config file at {}", SEARCH_PATHS.join(" or "))]
    NotFound,
    #[error("failed to check whether `{}` exists", .path.display())]
    Probe {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub fn locate(explicit: Option<PathBuf>) -> Result<PathBuf, LocateError> {
    let Some(path) = explicit else {
        return first_existing(SEARCH_PATHS.map(Path::new))?.ok_or(LocateError::NotFound);
    };
    if exists(&path)? {
        Ok(path)
    } else {
        Err(LocateError::Missing(path))
    }
}

fn first_existing<'a>(
    candidates: impl IntoIterator<Item = &'a Path>,
) -> Result<Option<PathBuf>, LocateError> {
    for path in candidates {
        if exists(path)? {
            return Ok(Some(path.to_path_buf()));
        }
        tracing::debug!(path = %path.display(), "config file not found");
    }
    Ok(None)
}

fn exists(path: &Path) -> Result<bool, LocateError> {
    path.try_exists().map_err(|source| LocateError::Probe {
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    auth: Auth,
    pools: Vec<Pool>,
    metrics: Option<Metrics>,
}

impl Config {
    pub fn auth(&self) -> &Auth {
        &self.auth
    }

    pub fn pools(&self) -> &[Pool] {
        &self.pools
    }

    pub fn metrics(&self) -> Option<Metrics> {
        self.metrics
    }
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "github")?;
        writeln!(f, "  auth: {}", self.auth)?;
        for pool in &self.pools {
            write!(f, "\n{pool}")?;
        }
        match self.metrics {
            Some(metrics) => write!(f, "\nmetrics\n  listen: {}\n", metrics.listen),
            None => write!(f, "\nmetrics\n  disabled\n"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    listen: SocketAddr,
}

impl Metrics {
    pub fn listen(self) -> SocketAddr {
        self.listen
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoadError {
    #[error("failed to read `{}`", .path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("`{}` is invalid:{}", .path.display(), problem_lines(.path, .problems))]
    Invalid {
        path: PathBuf,
        problems: Vec<Problem>,
    },
}

fn problem_lines<'a>(path: &'a Path, problems: &'a [Problem]) -> impl fmt::Display + 'a {
    fmt::from_fn(move |f| {
        for problem in problems {
            write!(f, "\n  {}:{problem}", path.display())?;
        }
        Ok(())
    })
}

pub fn load(path: &Path) -> Result<Config, LoadError> {
    let source = fs::read_to_string(path).map_err(|source| LoadError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let base_dir = path.parent().unwrap_or(Path::new(""));
    parse(&source, base_dir).map_err(|problems| LoadError::Invalid {
        path: path.to_path_buf(),
        problems,
    })
}

fn parse(source: &str, base_dir: &Path) -> Result<Config, Vec<Problem>> {
    let mut diagnostics = Diagnostics::new(source);
    let (document, syntax_errors) = DeTable::parse_recoverable(source);
    if !syntax_errors.is_empty() {
        for error in syntax_errors {
            let offset = error.span().map_or(0, |span| span.start);
            diagnostics.report(offset, None, Reason::Syntax(error.message().to_owned()));
        }
        return diagnostics.into_result(None);
    }

    let mut root = Table::root(document);
    let auth = root
        .required("github", &mut diagnostics)
        .and_then(|item| item.table(&mut diagnostics))
        .and_then(|table| github::parse(table, base_dir, &mut diagnostics));
    let pools = root
        .required("pool", &mut diagnostics)
        .and_then(|item| pools::parse(item, &mut diagnostics));
    let metrics = match root.optional("metrics") {
        Some(item) => item
            .table(&mut diagnostics)
            .and_then(|table| metrics(table, &mut diagnostics))
            .map(Some),
        None => Some(None),
    };
    root.finish(&mut diagnostics);

    let config = match (auth, pools, metrics) {
        (Some(auth), Some(pools), Some(metrics)) => Some(Config {
            auth,
            pools,
            metrics,
        }),
        _ => None,
    };
    diagnostics.into_result(config)
}

fn metrics(mut table: Table<'_>, diagnostics: &mut Diagnostics<'_>) -> Option<Metrics> {
    let listen = table
        .required("listen", diagnostics)
        .and_then(|item| item.parse("address", diagnostics));
    table.finish(diagnostics);
    listen.map(|listen| Metrics { listen })
}

#[cfg(test)]
mod tests {
    use std::assert_matches;
    use std::num::NonZeroU32;

    use super::*;

    const VALID: &str = r#"
[github]
auth = "token"
token_env = "GITHUB_TOKEN"

[[pool]]
name = "linux"
repository = "octo/app"
labels = ["linux", "x64"]
backend = "docker"
image = "runner:latest"
size = 2
"#;

    fn edited(from: &str, to: &str) -> String {
        assert!(VALID.contains(from), "`{from}` is not in the valid config");
        VALID.replacen(from, to, 1)
    }

    fn parsed(source: &str) -> Config {
        parse(source, Path::new("/etc/dayfly")).unwrap()
    }

    fn problems(source: &str) -> Vec<String> {
        parse(source, Path::new("/etc/dayfly"))
            .unwrap_err()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn explicit_path_is_used_when_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custom.toml");
        fs::write(&path, "").unwrap();

        assert_eq!(locate(Some(path.clone())).unwrap(), path);
    }

    #[test]
    fn missing_explicit_path_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custom.toml");

        assert_matches!(locate(Some(path.clone())), Err(LocateError::Missing(missing)) if missing == path);
    }

    #[test]
    fn earlier_candidates_win() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.toml");
        let second = dir.path().join("second.toml");
        fs::write(&first, "").unwrap();
        fs::write(&second, "").unwrap();

        assert_eq!(
            first_existing([first.as_path(), second.as_path()]).unwrap(),
            Some(first)
        );
    }

    #[test]
    fn missing_candidates_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.toml");
        let second = dir.path().join("second.toml");
        fs::write(&second, "").unwrap();

        assert_eq!(
            first_existing([first.as_path(), second.as_path()]).unwrap(),
            Some(second)
        );
    }

    #[test]
    fn no_existing_candidate_finds_nothing() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(
            first_existing([dir.path().join("dayfly.toml").as_path()]).unwrap(),
            None
        );
    }

    #[test]
    fn valid_config_is_parsed_into_domain_types() {
        let config = parsed(VALID);

        assert_eq!(
            config.auth(),
            &Auth::Token(CredentialSource::Env("GITHUB_TOKEN".to_owned()))
        );
        let [pool] = config.pools() else {
            panic!("expected one pool: {:?}", config.pools());
        };
        assert_eq!(pool.name().as_str(), "linux");
        assert_eq!(
            pool.scope(),
            &Scope::Repository("octo/app".parse().unwrap())
        );
        let labels: Vec<&str> = pool.labels().iter().map(Label::as_str).collect();
        assert_eq!(labels, ["linux", "x64"]);
        assert_eq!(pool.backend(), Backend::Docker);
        assert_eq!(pool.image().as_str(), "runner:latest");
        assert_eq!(pool.size(), NonZeroU32::new(2).unwrap());
        assert_eq!(pool.limits(), Limits::default());
        assert_eq!(config.metrics(), None);
    }

    #[test]
    fn example_config_is_valid() {
        let config = parsed(include_str!("../examples/dayfly.toml"));

        assert_eq!(config.pools().len(), 2);
    }

    #[test]
    fn summary_lists_every_setting() {
        let config = parsed(&format!(
            "{VALID}\n[pool.limits]\ncpus = 1.5\nmemory = \"4GiB\"\npids = 512\ndisk = \"20GiB\"\n\n\
             [metrics]\nlisten = \"127.0.0.1:9180\"\n"
        ));

        assert_eq!(
            config.to_string(),
            "github\n  auth: token from environment variable GITHUB_TOKEN\n\
             \n\
             pool linux\n  scope: repository octo/app\n  labels: linux, x64\n  backend: docker\n  \
             image: runner:latest\n  size: 2\n  limits: cpus 1.5, memory 4GiB, pids 512, disk 20GiB\n\
             \n\
             metrics\n  listen: 127.0.0.1:9180\n"
        );
    }

    #[test]
    fn every_syntax_error_is_reported_in_order() {
        assert_eq!(
            problems("[github\nauth = \"token\"\n[[pool]]\nlabels = [1,\n"),
            [
                "1:8: unclosed table, expected `]`",
                "4:13: unclosed array, expected `]`",
            ]
        );
    }

    #[test]
    fn duplicate_keys_are_rejected() {
        assert_eq!(
            problems(&edited("size = 2", "size = 2\nsize = 3")),
            ["13:1: duplicate key"]
        );
    }

    #[test]
    fn unknown_keys_are_rejected_in_every_table() {
        let source = format!(
            "colour = 1\n{}\n[pool.limits]\ngpus = 1\n\n[metrics]\nlisten = \"127.0.0.1:9180\"\nport = 1\n",
            edited("auth = \"token\"", "auth = \"token\"\ntoken = \"x\"").replacen(
                "size = 2",
                "size = 2\nsizes = 3",
                1
            )
        );

        assert_eq!(
            problems(&source),
            [
                "1:1: `colour`: unknown key, expected one of `github`, `pool`, `metrics`",
                "5:1: `github.token`: unknown key, expected one of `auth`, `token_env`, `token_file`",
                "15:1: `pool[0].sizes`: unknown key, expected one of `name`, `repository`, \
                 `organization`, `labels`, `backend`, `image`, `size`, `limits`",
                "18:1: `pool[0].limits.gpus`: unknown key, expected one of `cpus`, `memory`, \
                 `pids`, `disk`",
                "22:1: `metrics.port`: unknown key, expected `listen`",
            ]
        );
    }

    #[test]
    fn missing_sections_are_reported() {
        assert_eq!(
            problems(""),
            [
                "1:1: `github`: required but not set",
                "1:1: `pool`: required but not set",
            ]
        );
    }

    #[test]
    fn missing_pool_keys_are_reported_at_the_pool_header() {
        assert_eq!(
            problems("[github]\nauth = \"token\"\ntoken_env = \"T\"\n\n[[pool]]\n"),
            [
                "5:1: `pool[0].name`: required but not set",
                "5:1: `pool[0]`: set `repository` or `organization`",
                "5:1: `pool[0].labels`: required but not set",
                "5:1: `pool[0].backend`: required but not set",
                "5:1: `pool[0].image`: required but not set",
                "5:1: `pool[0].size`: required but not set",
            ]
        );
    }

    #[test]
    fn values_of_the_wrong_type_are_reported() {
        let source = edited("size = 2", "size = \"2\"")
            .replacen("labels = [\"linux\", \"x64\"]", "labels = \"linux\"", 1)
            .replacen("name = \"linux\"", "name = 7", 1);

        assert_eq!(
            problems(&source),
            [
                "7:8: `pool[0].name`: expected a string, found an integer",
                "9:10: `pool[0].labels`: expected an array of strings, found a string",
                "12:8: `pool[0].size`: expected an integer, found a string",
            ]
        );
    }

    #[test]
    fn sections_must_be_tables() {
        assert_eq!(
            problems(&format!(
                "metrics = 9180\n{}",
                edited(
                    "[github]\nauth = \"token\"\ntoken_env = \"GITHUB_TOKEN\"\n",
                    "github = \"token\"\n"
                )
            )),
            [
                "1:11: `metrics`: expected a table, found an integer",
                "3:10: `github`: expected a table, found a string",
            ]
        );
    }

    #[test]
    fn pools_must_be_an_array_of_tables() {
        assert_eq!(
            problems(&edited("[[pool]]", "[pool]")),
            ["6:1: `pool`: expected an array of tables (`[[pool]]`), found a table"]
        );
        assert_eq!(
            problems(&format!(
                "pool = [\"linux\"]\n{}",
                &VALID[..VALID.find("[[pool]]").unwrap()]
            )),
            ["1:9: `pool[0]`: expected a table, found a string"]
        );
    }

    #[test]
    fn empty_pool_list_is_rejected() {
        assert_eq!(
            problems(&format!(
                "pool = []\n{}",
                &VALID[..VALID.find("[[pool]]").unwrap()]
            )),
            ["1:8: `pool`: must not be empty"]
        );
    }

    #[test]
    fn duplicate_pool_names_are_rejected() {
        let second = &VALID[VALID.find("[[pool]]").unwrap()..];

        assert_eq!(
            problems(&format!("{VALID}\n{second}")),
            ["15:8: `pool[1].name`: duplicates `linux` at 7:8"]
        );
    }

    #[test]
    fn invalid_pool_name_is_rejected() {
        assert_eq!(
            problems(&edited("name = \"linux\"", "name = \"Linux\"")),
            [
                "7:8: `pool[0].name`: invalid pool name `Linux`: must contain only lowercase \
                 ASCII letters, digits and hyphens"
            ]
        );
    }

    #[test]
    fn organization_scope_is_parsed() {
        let config = parsed(&edited(
            "repository = \"octo/app\"",
            "organization = \"octo-org\"",
        ));

        assert_eq!(
            config.pools()[0].scope(),
            &Scope::Organization("octo-org".parse().unwrap())
        );
    }

    #[test]
    fn repository_and_organization_cannot_both_be_set() {
        assert_eq!(
            problems(&edited(
                "repository = \"octo/app\"",
                "repository = \"octo/app\"\norganization = \"octo\""
            )),
            ["9:16: `pool[0].organization`: cannot be combined with `repository`"]
        );
    }

    #[test]
    fn invalid_scopes_are_rejected() {
        assert_eq!(
            problems(&edited(
                "repository = \"octo/app\"",
                "repository = \"octo\""
            )),
            [
                "8:14: `pool[0].repository`: invalid repository `octo`: must have the form `owner/name`"
            ]
        );
        assert_eq!(
            problems(&edited(
                "repository = \"octo/app\"",
                "organization = \"octo_org\""
            )),
            [
                "8:16: `pool[0].organization`: invalid organization `octo_org`: must contain \
                 only ASCII letters, digits and hyphens"
            ]
        );
    }

    #[test]
    fn labels_must_not_be_empty() {
        assert_eq!(
            problems(&edited("labels = [\"linux\", \"x64\"]", "labels = []")),
            ["9:10: `pool[0].labels`: must not be empty"]
        );
    }

    #[test]
    fn more_than_100_labels_are_rejected() {
        let labels: Vec<String> = (0..101).map(|index| format!("\"l{index}\"")).collect();

        assert_eq!(
            problems(&edited(
                "labels = [\"linux\", \"x64\"]",
                &format!("labels = [{}]", labels.join(", "))
            )),
            ["9:10: `pool[0].labels`: must have at most 100 entries"]
        );
    }

    #[test]
    fn duplicate_labels_are_rejected_ignoring_case() {
        assert_eq!(
            problems(&edited(
                "labels = [\"linux\", \"x64\"]",
                "labels = [\"linux\", \"x64\", \"X64\"]"
            )),
            ["9:27: `pool[0].labels[2]`: duplicates `x64` at 9:20"]
        );
    }

    #[test]
    fn invalid_labels_are_rejected() {
        assert_eq!(
            problems(&edited(
                "labels = [\"linux\", \"x64\"]",
                "labels = [\"linux\", 64, \"x 64\"]"
            )),
            [
                "9:20: `pool[0].labels[1]`: expected a string, found an integer",
                "9:24: `pool[0].labels[2]`: invalid label `x 64`: must not contain whitespace",
            ]
        );
    }

    #[test]
    fn unsupported_backend_is_rejected() {
        assert_eq!(
            problems(&edited("backend = \"docker\"", "backend = \"podman\"")),
            ["10:11: `pool[0].backend`: unsupported value `podman`, expected `docker`"]
        );
    }

    #[test]
    fn invalid_image_is_rejected() {
        assert_eq!(
            problems(&edited("image = \"runner:latest\"", "image = \"\"")),
            ["11:9: `pool[0].image`: invalid image ``: must not be empty"]
        );
    }

    #[test]
    fn size_must_fit_a_positive_u32() {
        for size in ["0", "-1", "4294967296", "0x1_0000_0000"] {
            assert_eq!(
                problems(&edited("size = 2", &format!("size = {size}"))),
                ["12:8: `pool[0].size`: must be from 1 to 4294967295"],
                "{size}"
            );
        }
        assert_eq!(
            parsed(&edited("size = 2", "size = 0x10")).pools()[0].size(),
            NonZeroU32::new(16).unwrap()
        );
    }

    #[test]
    fn limits_are_parsed() {
        let config = parsed(&format!(
            "{VALID}\n[pool.limits]\ncpus = 2\nmemory = \"512MiB\"\npids = 100\ndisk = \"1TiB\"\n"
        ));

        let limits = config.pools()[0].limits();
        assert_eq!(limits.cpus().map(Cpus::get), Some(2.0));
        assert_eq!(limits.memory().map(ByteSize::bytes), Some(512 << 20));
        assert_eq!(limits.pids(), NonZeroU32::new(100));
        assert_eq!(limits.disk().map(ByteSize::bytes), Some(1 << 40));
    }

    #[test]
    fn invalid_limits_are_rejected() {
        let source = format!(
            "{VALID}\n[pool.limits]\ncpus = 0\nmemory = \"4GB\"\npids = 0\ndisk = \"0GiB\"\n"
        );

        assert_eq!(
            problems(&source),
            [
                "15:8: `pool[0].limits.cpus`: invalid CPU count `0`: must be a finite number \
                 greater than 0",
                "16:10: `pool[0].limits.memory`: invalid memory size `4GB`: expected a whole \
                 number followed by KiB, MiB, GiB or TiB, such as `512MiB`",
                "17:8: `pool[0].limits.pids`: must be from 1 to 4294967295",
                "18:8: `pool[0].limits.disk`: invalid disk size `0GiB`: must be greater than 0",
            ]
        );
    }

    #[test]
    fn cpus_must_be_a_decimal_number() {
        for (cpus, problem) in [
            (
                "-0.5",
                "invalid CPU count `-0.5`: must be a finite number greater than 0",
            ),
            (
                "nan",
                "invalid CPU count `NaN`: must be a finite number greater than 0",
            ),
            (
                "inf",
                "invalid CPU count `inf`: must be a finite number greater than 0",
            ),
            ("0x2", "expected a decimal number, found an integer"),
            ("\"2\"", "expected a decimal number, found a string"),
        ] {
            assert_eq!(
                problems(&format!("{VALID}\n[pool.limits]\ncpus = {cpus}\n")),
                [format!("15:8: `pool[0].limits.cpus`: {problem}")],
                "{cpus}"
            );
        }
    }

    #[test]
    fn limits_must_be_a_table() {
        assert_eq!(
            problems(&edited("size = 2", "size = 2\nlimits = \"none\"")),
            ["13:10: `pool[0].limits`: expected a table, found a string"]
        );
    }

    #[test]
    fn metrics_listen_address_is_parsed() {
        let config = parsed(&format!("{VALID}\n[metrics]\nlisten = \"[::1]:9180\"\n"));

        assert_eq!(
            config.metrics().map(Metrics::listen),
            Some("[::1]:9180".parse().unwrap())
        );
    }

    #[test]
    fn invalid_metrics_listen_address_is_rejected() {
        assert_eq!(
            problems(&format!(
                "{VALID}\n[metrics]\nlisten = \"localhost:9180\"\n"
            )),
            [
                "15:10: `metrics.listen`: invalid address `localhost:9180`: invalid socket \
                 address syntax"
            ]
        );
        assert_eq!(
            problems(&format!("{VALID}\n[metrics]\n")),
            ["14:1: `metrics.listen`: required but not set"]
        );
    }

    #[test]
    fn token_can_come_from_a_file_relative_to_the_config() {
        for (path, resolved) in [
            ("token", "/etc/dayfly/token"),
            ("/run/secrets/token", "/run/secrets/token"),
        ] {
            let config = parsed(&edited(
                "token_env = \"GITHUB_TOKEN\"",
                &format!("token_file = \"{path}\""),
            ));

            assert_eq!(
                config.auth(),
                &Auth::Token(CredentialSource::File(PathBuf::from(resolved))),
                "{path}"
            );
        }
    }

    #[test]
    fn token_needs_exactly_one_source() {
        assert_eq!(
            problems(&edited("token_env = \"GITHUB_TOKEN\"\n", "")),
            ["2:1: `github`: set `token_env` or `token_file`"]
        );
        assert_eq!(
            problems(&edited(
                "token_env = \"GITHUB_TOKEN\"",
                "token_file = \"token\"\ntoken_env = \"GITHUB_TOKEN\""
            )),
            ["5:13: `github.token_env`: cannot be combined with `token_file`"]
        );
    }

    #[test]
    fn app_auth_is_parsed() {
        let config = parsed(&edited(
            "auth = \"token\"\ntoken_env = \"GITHUB_TOKEN\"",
            "auth = \"app\"\nclient_id = \"Iv23liExample\"\nprivate_key_file = \"app.pem\"",
        ));

        assert_eq!(
            config.auth(),
            &Auth::App {
                client_id: "Iv23liExample".to_owned(),
                private_key: CredentialSource::File(PathBuf::from("/etc/dayfly/app.pem")),
            }
        );
    }

    #[test]
    fn app_auth_needs_a_client_id_and_one_private_key_source() {
        assert_eq!(
            problems(&edited(
                "auth = \"token\"\ntoken_env = \"GITHUB_TOKEN\"",
                "auth = \"app\"\nclient_id = \"\"\nprivate_key_env = \"KEY\"\nprivate_key_file = \"app.pem\""
            )),
            [
                "4:13: `github.client_id`: must not be empty",
                "6:20: `github.private_key_file`: cannot be combined with `private_key_env`",
            ]
        );
        assert_eq!(
            problems(&edited(
                "auth = \"token\"\ntoken_env = \"GITHUB_TOKEN\"",
                "auth = \"app\""
            )),
            [
                "2:1: `github.client_id`: required but not set",
                "2:1: `github`: set `private_key_env` or `private_key_file`",
            ]
        );
    }

    #[test]
    fn keys_of_the_other_auth_method_are_rejected() {
        assert_eq!(
            problems(&edited(
                "token_env = \"GITHUB_TOKEN\"",
                "token_env = \"GITHUB_TOKEN\"\nclient_id = \"Iv23li\""
            )),
            [
                "5:1: `github.client_id`: unknown key, expected one of `auth`, `token_env`, `token_file`"
            ]
        );
    }

    #[test]
    fn unsupported_auth_method_is_reported_alone() {
        assert_eq!(
            problems(&edited("auth = \"token\"", "auth = \"oauth\"")),
            ["3:8: `github.auth`: unsupported value `oauth`, expected `token` or `app`"]
        );
    }

    #[test]
    fn credentials_written_inline_are_rejected_without_echoing_them() {
        for (github, value) in [
            (
                "auth = \"token\"\ntoken_env",
                "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            ),
            (
                "auth = \"token\"\ntoken_file",
                "github_pat_11ABCDEFG0123456789_abcdefghij",
            ),
            (
                "auth = \"app\"\nclient_id = \"Iv23li\"\nprivate_key_env",
                "-----BEGIN RSA PRIVATE KEY-----",
            ),
        ] {
            let source = edited(
                "auth = \"token\"\ntoken_env = \"GITHUB_TOKEN\"",
                &format!("{github} = \"{value}\""),
            );
            let problems = problems(&source);

            assert_eq!(problems.len(), 1, "{problems:?}");
            assert!(
                problems[0].contains("looks like a credential"),
                "{problems:?}"
            );
            assert!(!problems[0].contains(&value[..8]), "{problems:?}");
        }
    }

    #[test]
    fn env_names_must_be_valid_and_are_not_echoed() {
        for name in ["1TOKEN", "MY-TOKEN", "MY TOKEN", "$TOKEN"] {
            assert_eq!(
                problems(&edited(
                    "token_env = \"GITHUB_TOKEN\"",
                    &format!("token_env = \"{name}\"")
                )),
                [
                    "4:13: `github.token_env`: must name an environment variable: letters, \
                     digits and underscores, not starting with a digit"
                ],
                "{name}"
            );
        }
        assert_eq!(
            parsed(&edited(
                "token_env = \"GITHUB_TOKEN\"",
                "token_env = \"_dayfly_token2\""
            ))
            .auth(),
            &Auth::Token(CredentialSource::Env("_dayfly_token2".to_owned()))
        );
    }

    #[test]
    fn empty_credential_file_is_rejected() {
        assert_eq!(
            problems(&edited("token_env = \"GITHUB_TOKEN\"", "token_file = \"\"")),
            ["4:14: `github.token_file`: must not be empty"]
        );
    }

    #[test]
    fn every_problem_is_reported_at_once_in_source_order() {
        let source = edited("auth = \"token\"", "auth = \"token\"\nextra = 1")
            .replacen("size = 2", "size = 0", 1)
            .replacen("backend = \"docker\"", "backend = \"tart\"", 1);

        assert_eq!(
            problems(&source),
            [
                "4:1: `github.extra`: unknown key, expected one of `auth`, `token_env`, `token_file`",
                "11:11: `pool[0].backend`: unsupported value `tart`, expected `docker`",
                "13:8: `pool[0].size`: must be from 1 to 4294967295",
            ]
        );
    }

    #[test]
    fn columns_count_characters_not_bytes() {
        assert_eq!(
            problems(&edited(
                "labels = [\"linux\", \"x64\"]",
                "labels = [\"café\", \"x 64\"]"
            )),
            ["9:19: `pool[0].labels[1]`: invalid label `x 64`: must not contain whitespace"]
        );
    }

    #[test]
    fn load_names_the_file_in_every_problem() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dayfly.toml");
        fs::write(
            &path,
            edited("size = 2", "size = 0").replacen("name = \"linux\"", "name = \"\"", 1),
        )
        .unwrap();

        let error = load(&path).unwrap_err();

        let shown = path.display();
        assert_eq!(
            error.to_string(),
            format!(
                "`{shown}` is invalid:\n  \
                 {shown}:7:8: `pool[0].name`: invalid pool name ``: must not be empty\n  \
                 {shown}:12:8: `pool[0].size`: must be from 1 to 4294967295"
            )
        );
        assert_matches!(error, LoadError::Invalid { problems, .. } if problems.len() == 2);
    }

    #[test]
    fn unreadable_file_is_a_read_error() {
        let dir = tempfile::tempdir().unwrap();

        assert_matches!(load(dir.path()), Err(LoadError::Read { .. }));
    }
}
