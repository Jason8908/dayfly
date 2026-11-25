use std::io;
use std::path::{Path, PathBuf};

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
    }
    Ok(None)
}

fn exists(path: &Path) -> Result<bool, LocateError> {
    path.try_exists().map_err(|source| LocateError::Probe {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use std::assert_matches;
    use std::fs;

    use super::*;

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
}
