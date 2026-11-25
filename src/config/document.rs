use std::error::Error;
use std::fmt;
use std::num::NonZeroU32;
use std::str::FromStr;

use toml::Spanned;
use toml::de::{DeTable, DeValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Location {
    line: usize,
    column: usize,
}

impl Location {
    pub fn line(self) -> usize {
        self.line
    }

    pub fn column(self) -> usize {
        self.column
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

#[derive(Debug)]
pub struct Problem {
    location: Location,
    key: Option<String>,
    reason: Reason,
}

impl Problem {
    pub fn location(&self) -> Location {
        self.location
    }

    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    pub fn reason(&self) -> &Reason {
        &self.reason
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.location)?;
        if let Some(key) = &self.key {
            write!(f, "`{key}`: ")?;
        }
        write!(f, "{}", self.reason)
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Reason {
    #[error("{0}")]
    Syntax(String),
    #[error("unknown key, expected {}", alternatives(.expected))]
    UnknownKey { expected: Vec<&'static str> },
    #[error("required but not set")]
    Missing,
    #[error("set `{0}` or `{1}`")]
    MissingEither(&'static str, &'static str),
    #[error("cannot be combined with `{0}`")]
    Conflict(&'static str),
    #[error("expected {expected}, found {found}")]
    WrongType {
        expected: &'static str,
        found: &'static str,
    },
    #[error("unsupported value `{value}`, expected {}", alternatives(.expected))]
    Unsupported {
        value: String,
        expected: Vec<&'static str>,
    },
    #[error("must not be empty")]
    Empty,
    #[error("must have at most {0} entries")]
    TooMany(usize),
    #[error("must be from {min} to {max}")]
    OutOfRange { min: u64, max: u64 },
    #[error("duplicates `{of}` at {at}")]
    Duplicate { of: String, at: Location },
    #[error("invalid {what} `{value}`: {cause}")]
    Invalid {
        what: &'static str,
        value: String,
        cause: Box<dyn Error + Send + Sync>,
    },
    #[error(
        "must name an environment variable: letters, digits and underscores, not starting with a digit"
    )]
    EnvName,
    #[error("looks like a credential; name the environment variable or file that holds it instead")]
    Credential,
}

fn alternatives(values: &[&str]) -> String {
    match values {
        [only] => format!("`{only}`"),
        [first, second] => format!("`{first}` or `{second}`"),
        _ => {
            let quoted: Vec<String> = values.iter().map(|value| format!("`{value}`")).collect();
            format!("one of {}", quoted.join(", "))
        }
    }
}

#[derive(Debug)]
pub(super) struct Diagnostics<'s> {
    source: &'s str,
    problems: Vec<Problem>,
}

impl<'s> Diagnostics<'s> {
    pub(super) fn new(source: &'s str) -> Self {
        Self {
            source,
            problems: Vec::new(),
        }
    }

    pub(super) fn report(&mut self, offset: usize, key: Option<String>, reason: Reason) {
        let location = self.locate(offset);
        self.problems.push(Problem {
            location,
            key,
            reason,
        });
    }

    pub(super) fn locate(&self, offset: usize) -> Location {
        let before = self.source.get(..offset).unwrap_or(self.source);
        let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
        Location {
            line: before.matches('\n').count() + 1,
            column: before[line_start..].chars().count() + 1,
        }
    }

    pub(super) fn into_result<T>(self, value: Option<T>) -> Result<T, Vec<Problem>> {
        match value {
            Some(value) if self.problems.is_empty() => Ok(value),
            _ => {
                let mut problems = self.problems;
                debug_assert!(
                    !problems.is_empty(),
                    "a value was rejected without a problem"
                );
                problems.sort_by_key(Problem::location);
                Err(problems)
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct Table<'i> {
    key: Option<String>,
    offset: usize,
    entries: DeTable<'i>,
    known: Vec<&'static str>,
}

#[derive(Debug)]
pub(super) enum Either<'i> {
    First(Item<'i>),
    Second(Item<'i>),
}

impl<'i> Table<'i> {
    pub(super) fn root(document: Spanned<DeTable<'i>>) -> Self {
        Self {
            key: None,
            offset: document.span().start,
            entries: document.into_inner(),
            known: Vec::new(),
        }
    }

    pub(super) fn optional(&mut self, key: &'static str) -> Option<Item<'i>> {
        self.known.push(key);
        let (_, value) = self.entries.remove_entry(key)?;
        Some(Item::new(child(self.key.as_deref(), key), value))
    }

    pub(super) fn required(
        &mut self,
        key: &'static str,
        diagnostics: &mut Diagnostics<'_>,
    ) -> Option<Item<'i>> {
        let item = self.optional(key);
        if item.is_none() {
            diagnostics.report(
                self.offset,
                Some(child(self.key.as_deref(), key)),
                Reason::Missing,
            );
        }
        item
    }

    pub(super) fn either(
        &mut self,
        first: &'static str,
        second: &'static str,
        diagnostics: &mut Diagnostics<'_>,
    ) -> Option<Either<'i>> {
        match (self.optional(first), self.optional(second)) {
            (Some(item), None) => Some(Either::First(item)),
            (None, Some(item)) => Some(Either::Second(item)),
            (Some(first_item), Some(second_item)) => {
                if first_item.offset < second_item.offset {
                    second_item.report(diagnostics, Reason::Conflict(first));
                } else {
                    first_item.report(diagnostics, Reason::Conflict(second));
                }
                None
            }
            (None, None) => {
                diagnostics.report(
                    self.offset,
                    self.key.clone(),
                    Reason::MissingEither(first, second),
                );
                None
            }
        }
    }

    pub(super) fn finish(self, diagnostics: &mut Diagnostics<'_>) {
        let Self {
            key: parent,
            entries,
            known,
            ..
        } = self;
        for (key, _) in entries {
            diagnostics.report(
                key.span().start,
                Some(child(parent.as_deref(), key.get_ref())),
                Reason::UnknownKey {
                    expected: known.clone(),
                },
            );
        }
    }
}

fn child(parent: Option<&str>, key: &str) -> String {
    match parent {
        Some(parent) => format!("{parent}.{key}"),
        None => key.to_owned(),
    }
}

#[derive(Debug)]
pub(super) struct Item<'i> {
    key: String,
    offset: usize,
    value: DeValue<'i>,
}

impl<'i> Item<'i> {
    fn new(key: String, value: Spanned<DeValue<'i>>) -> Self {
        Self {
            key,
            offset: value.span().start,
            value: value.into_inner(),
        }
    }

    pub(super) fn offset(&self) -> usize {
        self.offset
    }

    pub(super) fn report(&self, diagnostics: &mut Diagnostics<'_>, reason: Reason) {
        diagnostics.report(self.offset, Some(self.key.clone()), reason);
    }

    fn wrong_type(&self, expected: &'static str, diagnostics: &mut Diagnostics<'_>) {
        let found = match self.value {
            DeValue::String(_) => "a string",
            DeValue::Integer(_) => "an integer",
            DeValue::Float(_) => "a float",
            DeValue::Boolean(_) => "a boolean",
            DeValue::Datetime(_) => "a datetime",
            DeValue::Array(_) => "an array",
            DeValue::Table(_) => "a table",
        };
        self.report(diagnostics, Reason::WrongType { expected, found });
    }

    pub(super) fn table(self, diagnostics: &mut Diagnostics<'_>) -> Option<Table<'i>> {
        let DeValue::Table(entries) = self.value else {
            self.wrong_type("a table", diagnostics);
            return None;
        };
        Some(Table {
            key: Some(self.key),
            offset: self.offset,
            entries,
            known: Vec::new(),
        })
    }

    pub(super) fn non_empty_array(
        self,
        expected: &'static str,
        max_len: Option<usize>,
        diagnostics: &mut Diagnostics<'_>,
    ) -> Option<Vec<Item<'i>>> {
        let DeValue::Array(values) = self.value else {
            self.wrong_type(expected, diagnostics);
            return None;
        };
        let len_problem = if values.is_empty() {
            Some(Reason::Empty)
        } else {
            max_len
                .filter(|&max_len| values.len() > max_len)
                .map(Reason::TooMany)
        };
        if let Some(reason) = len_problem {
            diagnostics.report(self.offset, Some(self.key.clone()), reason);
        }
        let items = values
            .into_iter()
            .enumerate()
            .map(|(index, value)| Item::new(format!("{}[{index}]", self.key), value))
            .collect();
        Some(items)
    }

    pub(super) fn str(&self, diagnostics: &mut Diagnostics<'_>) -> Option<&str> {
        if let DeValue::String(text) = &self.value {
            Some(text)
        } else {
            self.wrong_type("a string", diagnostics);
            None
        }
    }

    pub(super) fn non_empty_str(&self, diagnostics: &mut Diagnostics<'_>) -> Option<&str> {
        let text = self.str(diagnostics)?;
        if text.is_empty() {
            self.report(diagnostics, Reason::Empty);
            return None;
        }
        Some(text)
    }

    pub(super) fn parse<T>(
        &self,
        what: &'static str,
        diagnostics: &mut Diagnostics<'_>,
    ) -> Option<T>
    where
        T: FromStr,
        T::Err: Error + Send + Sync + 'static,
    {
        let text = self.str(diagnostics)?;
        self.check(what, text, text.parse(), diagnostics)
    }

    pub(super) fn check<T, E>(
        &self,
        what: &'static str,
        value: impl fmt::Display,
        result: Result<T, E>,
        diagnostics: &mut Diagnostics<'_>,
    ) -> Option<T>
    where
        E: Error + Send + Sync + 'static,
    {
        match result {
            Ok(parsed) => Some(parsed),
            Err(cause) => {
                let reason = Reason::Invalid {
                    what,
                    value: value.to_string(),
                    cause: Box::new(cause),
                };
                self.report(diagnostics, reason);
                None
            }
        }
    }

    pub(super) fn choice<T: Copy>(
        &self,
        options: &[(&'static str, T)],
        diagnostics: &mut Diagnostics<'_>,
    ) -> Option<T> {
        let text = self.str(diagnostics)?;
        let chosen = options
            .iter()
            .find(|(name, _)| *name == text)
            .map(|&(_, value)| value);
        if chosen.is_none() {
            let reason = Reason::Unsupported {
                value: text.to_owned(),
                expected: options.iter().map(|&(name, _)| name).collect(),
            };
            self.report(diagnostics, reason);
        }
        chosen
    }

    pub(super) fn positive_u32(&self, diagnostics: &mut Diagnostics<'_>) -> Option<NonZeroU32> {
        let DeValue::Integer(integer) = &self.value else {
            self.wrong_type("an integer", diagnostics);
            return None;
        };
        let value = i128::from_str_radix(integer.as_str(), integer.radix())
            .ok()
            .and_then(|value| u32::try_from(value).ok())
            .and_then(NonZeroU32::new);
        if value.is_none() {
            let reason = Reason::OutOfRange {
                min: 1,
                max: u32::MAX.into(),
            };
            self.report(diagnostics, reason);
        }
        value
    }

    pub(super) fn number(&self, diagnostics: &mut Diagnostics<'_>) -> Option<f64> {
        let text = match &self.value {
            DeValue::Integer(integer) if integer.radix() == 10 => integer.as_str(),
            DeValue::Float(float) => float.as_str(),
            _ => {
                self.wrong_type("a decimal number", diagnostics);
                return None;
            }
        };
        self.check("number", text, text.parse::<f64>(), diagnostics)
    }
}
