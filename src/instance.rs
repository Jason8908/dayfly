use std::fmt;
use std::str::FromStr;

const ID_LEN: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Id(String);

impl Id {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum IdError {
    #[error("must be exactly {ID_LEN} characters")]
    Length,
    #[error("must contain only lowercase ASCII letters and digits")]
    InvalidCharacter,
}

impl FromStr for Id {
    type Err = IdError;

    fn from_str(id: &str) -> Result<Self, Self::Err> {
        if !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            return Err(IdError::InvalidCharacter);
        }
        if id.len() != ID_LEN {
            return Err(IdError::Length);
        }
        Ok(Self(id.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;

    use super::*;

    #[test]
    fn id_is_eight_lowercase_letters_or_digits() {
        assert_eq!("k3j9x2ab".parse::<Id>().unwrap().as_str(), "k3j9x2ab");
        for id in ["", "k3j9x2a", "k3j9x2abc"] {
            assert_matches!(id.parse::<Id>(), Err(IdError::Length), "{id:?}");
        }
        for id in ["K3J9X2AB", "k3j9-2ab", "k3j9_2ab", "k3j9x2aé"] {
            assert_matches!(id.parse::<Id>(), Err(IdError::InvalidCharacter), "{id:?}");
        }
    }
}
