use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use super::document::{Diagnostics, Either, Item, Reason, Table};

const MAX_ACCOUNT_LEN: usize = 39;
const MAX_REPOSITORY_NAME_LEN: usize = 100;
const CREDENTIAL_MARKERS: [&str; 7] = [
    "ghp_",
    "github_pat_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "-----BEGIN",
];

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Auth {
    Token(CredentialSource),
    App {
        client_id: String,
        private_key: CredentialSource,
    },
}

impl fmt::Display for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Token(token) => write!(f, "token from {token}"),
            Self::App {
                client_id,
                private_key,
            } => write!(f, "GitHub App {client_id}, private key from {private_key}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSource {
    Env(String),
    File(PathBuf),
}

impl fmt::Display for CredentialSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Env(name) => write!(f, "environment variable {name}"),
            Self::File(path) => write!(f, "file {}", path.display()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Method {
    Token,
    App,
}

pub(super) fn parse(
    mut table: Table<'_>,
    base_dir: &Path,
    diagnostics: &mut Diagnostics<'_>,
) -> Option<Auth> {
    let method = table.required("auth", diagnostics)?.choice(
        &[("token", Method::Token), ("app", Method::App)],
        diagnostics,
    )?;
    let auth = match method {
        Method::Token => credential(&mut table, "token_env", "token_file", base_dir, diagnostics)
            .map(Auth::Token),
        Method::App => {
            let client_id = table
                .required("client_id", diagnostics)
                .and_then(|item| item.non_empty_str(diagnostics).map(str::to_owned));
            let private_key = credential(
                &mut table,
                "private_key_env",
                "private_key_file",
                base_dir,
                diagnostics,
            );
            client_id
                .zip(private_key)
                .map(|(client_id, private_key)| Auth::App {
                    client_id,
                    private_key,
                })
        }
    };
    table.finish(diagnostics);
    auth
}

fn credential(
    table: &mut Table<'_>,
    env_key: &'static str,
    file_key: &'static str,
    base_dir: &Path,
    diagnostics: &mut Diagnostics<'_>,
) -> Option<CredentialSource> {
    match table.either(env_key, file_key, diagnostics)? {
        Either::First(item) => {
            let name = credential_reference(&item, diagnostics)?;
            if !is_env_name(name) {
                item.report(diagnostics, Reason::EnvName);
                return None;
            }
            Some(CredentialSource::Env(name.to_owned()))
        }
        Either::Second(item) => {
            let path = credential_reference(&item, diagnostics)?;
            Some(CredentialSource::File(base_dir.join(path)))
        }
    }
}

fn credential_reference<'a>(
    item: &'a Item<'_>,
    diagnostics: &mut Diagnostics<'_>,
) -> Option<&'a str> {
    let text = item.non_empty_str(diagnostics)?;
    if CREDENTIAL_MARKERS
        .iter()
        .any(|marker| text.starts_with(marker))
    {
        item.report(diagnostics, Reason::Credential);
        return None;
    }
    Some(text)
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Account(String);

impl Account {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Account {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AccountError {
    #[error("must not be empty")]
    Empty,
    #[error("must be at most {MAX_ACCOUNT_LEN} characters")]
    TooLong,
    #[error("must contain only ASCII letters, digits and hyphens")]
    InvalidCharacter,
    #[error("must not start with a hyphen")]
    LeadingHyphen,
}

impl FromStr for Account {
    type Err = AccountError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        if name.is_empty() {
            return Err(AccountError::Empty);
        }
        if name.len() > MAX_ACCOUNT_LEN {
            return Err(AccountError::TooLong);
        }
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(AccountError::InvalidCharacter);
        }
        if name.starts_with('-') {
            return Err(AccountError::LeadingHyphen);
        }
        Ok(Self(name.to_owned()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Repository {
    owner: Account,
    name: String,
}

impl Repository {
    pub fn owner(&self) -> &Account {
        &self.owner
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for Repository {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RepositoryError {
    #[error("must have the form `owner/name`")]
    Format,
    #[error("owner {0}")]
    Owner(AccountError),
    #[error("name must be at most {MAX_REPOSITORY_NAME_LEN} characters")]
    NameTooLong,
    #[error("name must contain only ASCII letters, digits, `.`, `-` and `_`")]
    NameInvalidCharacter,
    #[error("name must not be `.` or `..`")]
    NameReserved,
}

impl FromStr for Repository {
    type Err = RepositoryError;

    fn from_str(slug: &str) -> Result<Self, Self::Err> {
        let (owner, name) = slug.split_once('/').ok_or(RepositoryError::Format)?;
        let owner = owner.parse().map_err(RepositoryError::Owner)?;
        if name.is_empty() {
            return Err(RepositoryError::Format);
        }
        if name.len() > MAX_REPOSITORY_NAME_LEN {
            return Err(RepositoryError::NameTooLong);
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            return Err(RepositoryError::NameInvalidCharacter);
        }
        if matches!(name, "." | "..") {
            return Err(RepositoryError::NameReserved);
        }
        Ok(Self {
            owner,
            name: name.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_accept_letters_digits_and_hyphens() {
        let account: Account = "Octo-Org-42".parse().unwrap();

        assert_eq!(account.as_str(), "Octo-Org-42");
    }

    #[test]
    fn empty_account_is_rejected() {
        assert_eq!("".parse::<Account>(), Err(AccountError::Empty));
    }

    #[test]
    fn account_longer_than_39_characters_is_rejected() {
        "a".repeat(39).parse::<Account>().unwrap();
        assert_eq!(
            "a".repeat(40).parse::<Account>(),
            Err(AccountError::TooLong)
        );
    }

    #[test]
    fn account_with_other_characters_is_rejected() {
        for name in ["octo_org", "octo.org", "octo org", "octö"] {
            assert_eq!(
                name.parse::<Account>(),
                Err(AccountError::InvalidCharacter),
                "{name}"
            );
        }
    }

    #[test]
    fn account_starting_with_a_hyphen_is_rejected() {
        assert_eq!("-octo".parse::<Account>(), Err(AccountError::LeadingHyphen));
    }

    #[test]
    fn repository_splits_into_owner_and_name() {
        let repository: Repository = "octo-org/my_app.rs".parse().unwrap();

        assert_eq!(repository.owner().as_str(), "octo-org");
        assert_eq!(repository.name(), "my_app.rs");
        assert_eq!(repository.to_string(), "octo-org/my_app.rs");
    }

    #[test]
    fn repository_without_owner_and_name_is_rejected() {
        for slug in ["octo", "octo/"] {
            assert_eq!(
                slug.parse::<Repository>(),
                Err(RepositoryError::Format),
                "{slug}"
            );
        }
    }

    #[test]
    fn repository_with_an_invalid_owner_is_rejected() {
        assert_eq!(
            "/app".parse::<Repository>(),
            Err(RepositoryError::Owner(AccountError::Empty))
        );
        for slug in ["octo_org/app", "https://github.com/octo/app"] {
            assert_eq!(
                slug.parse::<Repository>(),
                Err(RepositoryError::Owner(AccountError::InvalidCharacter)),
                "{slug}"
            );
        }
    }

    #[test]
    fn repository_name_longer_than_100_characters_is_rejected() {
        format!("octo/{}", "a".repeat(100))
            .parse::<Repository>()
            .unwrap();
        assert_eq!(
            format!("octo/{}", "a".repeat(101)).parse::<Repository>(),
            Err(RepositoryError::NameTooLong)
        );
    }

    #[test]
    fn repository_name_with_other_characters_is_rejected() {
        for slug in ["octo/my app", "octo/app/extra", "octo/äpp"] {
            assert_eq!(
                slug.parse::<Repository>(),
                Err(RepositoryError::NameInvalidCharacter),
                "{slug}"
            );
        }
    }

    #[test]
    fn repository_named_dot_or_dot_dot_is_rejected() {
        for slug in ["octo/.", "octo/.."] {
            assert_eq!(
                slug.parse::<Repository>(),
                Err(RepositoryError::NameReserved),
                "{slug}"
            );
        }
    }
}
