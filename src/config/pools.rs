use std::collections::HashMap;
use std::fmt;
use std::num::{NonZeroU32, NonZeroU64};
use std::str::FromStr;

use super::document::{Diagnostics, Either, Item, Location, Reason, Table};
use super::github::{Account, Repository};

// Pool names become part of runner and container names (`dayfly-<pool>-<suffix>`).
const MAX_POOL_NAME_LEN: usize = 32;
// The limit of GitHub's generate-jitconfig endpoints.
const MAX_LABELS: usize = 100;
const BACKENDS: [(&str, Backend); 1] = [("docker", Backend::Docker)];
const BYTE_UNITS: [(&str, u32); 4] = [("KiB", 10), ("MiB", 20), ("GiB", 30), ("TiB", 40)];

#[derive(Debug, Clone, PartialEq)]
pub struct Pool {
    name: PoolName,
    scope: Scope,
    labels: Vec<Label>,
    backend: Backend,
    image: Image,
    size: NonZeroU32,
    limits: Limits,
}

impl Pool {
    pub fn name(&self) -> &PoolName {
        &self.name
    }

    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    pub fn labels(&self) -> &[Label] {
        &self.labels
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub fn image(&self) -> &Image {
        &self.image
    }

    pub fn size(&self) -> NonZeroU32 {
        self.size
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }
}

impl fmt::Display for Pool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let labels: Vec<&str> = self.labels.iter().map(Label::as_str).collect();
        writeln!(f, "pool {}", self.name)?;
        writeln!(f, "  scope: {}", self.scope)?;
        writeln!(f, "  labels: {}", labels.join(", "))?;
        writeln!(f, "  backend: {}", self.backend)?;
        writeln!(f, "  image: {}", self.image)?;
        writeln!(f, "  size: {}", self.size)?;
        writeln!(f, "  limits: {}", self.limits)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Scope {
    Repository(Repository),
    Organization(Account),
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repository(repository) => write!(f, "repository {repository}"),
            Self::Organization(organization) => write!(f, "organization {organization}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Backend {
    Docker,
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Docker => f.write_str("docker"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Limits {
    cpus: Option<Cpus>,
    memory: Option<ByteSize>,
    pids: Option<NonZeroU32>,
    disk: Option<ByteSize>,
}

impl Limits {
    pub fn cpus(self) -> Option<Cpus> {
        self.cpus
    }

    pub fn memory(self) -> Option<ByteSize> {
        self.memory
    }

    pub fn pids(self) -> Option<NonZeroU32> {
        self.pids
    }

    pub fn disk(self) -> Option<ByteSize> {
        self.disk
    }
}

impl fmt::Display for Limits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let set: Vec<String> = [
            self.cpus.map(|cpus| format!("cpus {cpus}")),
            self.memory.map(|memory| format!("memory {memory}")),
            self.pids.map(|pids| format!("pids {pids}")),
            self.disk.map(|disk| format!("disk {disk}")),
        ]
        .into_iter()
        .flatten()
        .collect();
        if set.is_empty() {
            f.write_str("none")
        } else {
            f.write_str(&set.join(", "))
        }
    }
}

pub(super) fn parse(item: Item<'_>, diagnostics: &mut Diagnostics<'_>) -> Option<Vec<Pool>> {
    let items = item.non_empty_array("an array of tables (`[[pool]]`)", None, diagnostics)?;
    let mut names = HashMap::new();
    let pools: Vec<Option<Pool>> = items
        .into_iter()
        .map(|item| pool(item, &mut names, diagnostics))
        .collect();
    pools.into_iter().collect()
}

fn pool(
    item: Item<'_>,
    names: &mut HashMap<PoolName, Location>,
    diagnostics: &mut Diagnostics<'_>,
) -> Option<Pool> {
    let mut table = item.table(diagnostics)?;
    let name = table
        .required("name", diagnostics)
        .and_then(|item| unique_name(&item, names, diagnostics));
    let scope = match table.either("repository", "organization", diagnostics) {
        Some(Either::First(item)) => item.parse("repository", diagnostics).map(Scope::Repository),
        Some(Either::Second(item)) => item
            .parse("organization", diagnostics)
            .map(Scope::Organization),
        None => None,
    };
    let labels = table
        .required("labels", diagnostics)
        .and_then(|item| labels(item, diagnostics));
    let backend = table
        .required("backend", diagnostics)
        .and_then(|item| item.choice(&BACKENDS, diagnostics));
    let image = table
        .required("image", diagnostics)
        .and_then(|item| item.parse("image", diagnostics));
    let size = table
        .required("size", diagnostics)
        .and_then(|item| item.positive_u32(diagnostics));
    let limits = match table.optional("limits") {
        Some(item) => item
            .table(diagnostics)
            .map(|table| limits(table, diagnostics)),
        None => Some(Limits::default()),
    };
    table.finish(diagnostics);
    let (
        Some(name),
        Some(scope),
        Some(labels),
        Some(backend),
        Some(image),
        Some(size),
        Some(limits),
    ) = (name, scope, labels, backend, image, size, limits)
    else {
        return None;
    };
    Some(Pool {
        name,
        scope,
        labels,
        backend,
        image,
        size,
        limits,
    })
}

fn unique_name(
    item: &Item<'_>,
    names: &mut HashMap<PoolName, Location>,
    diagnostics: &mut Diagnostics<'_>,
) -> Option<PoolName> {
    let name: PoolName = item.parse("pool name", diagnostics)?;
    if let Some(&first) = names.get(&name) {
        let reason = Reason::Duplicate {
            of: name.to_string(),
            at: first,
        };
        item.report(diagnostics, reason);
        return None;
    }
    names.insert(name.clone(), diagnostics.locate(item.offset()));
    Some(name)
}

fn labels(item: Item<'_>, diagnostics: &mut Diagnostics<'_>) -> Option<Vec<Label>> {
    let items = item.non_empty_array("an array of strings", Some(MAX_LABELS), diagnostics)?;
    let mut seen: HashMap<String, (String, Location)> = HashMap::new();
    let labels: Vec<Option<Label>> = items
        .iter()
        .map(|item| {
            let label: Label = item.parse("label", diagnostics)?;
            let folded = label.as_str().to_lowercase();
            if let Some((first, at)) = seen.get(&folded) {
                let reason = Reason::Duplicate {
                    of: first.clone(),
                    at: *at,
                };
                item.report(diagnostics, reason);
                return None;
            }
            let location = diagnostics.locate(item.offset());
            seen.insert(folded, (label.to_string(), location));
            Some(label)
        })
        .collect();
    labels.into_iter().collect()
}

fn limits(mut table: Table<'_>, diagnostics: &mut Diagnostics<'_>) -> Limits {
    let cpus = table.optional("cpus").and_then(|item| {
        let count = item.number(diagnostics)?;
        item.check("CPU count", count, Cpus::try_from(count), diagnostics)
    });
    let memory = table
        .optional("memory")
        .and_then(|item| item.parse("memory size", diagnostics));
    let pids = table
        .optional("pids")
        .and_then(|item| item.positive_u32(diagnostics));
    let disk = table
        .optional("disk")
        .and_then(|item| item.parse("disk size", diagnostics));
    table.finish(diagnostics);
    Limits {
        cpus,
        memory,
        pids,
        disk,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PoolName(String);

impl PoolName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PoolName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PoolNameError {
    #[error("must not be empty")]
    Empty,
    #[error("must be at most {MAX_POOL_NAME_LEN} characters")]
    TooLong,
    #[error("must contain only lowercase ASCII letters, digits and hyphens")]
    InvalidCharacter,
    #[error("must start and end with a letter or digit")]
    EdgeHyphen,
}

impl FromStr for PoolName {
    type Err = PoolNameError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        if name.is_empty() {
            return Err(PoolNameError::Empty);
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(PoolNameError::InvalidCharacter);
        }
        if name.len() > MAX_POOL_NAME_LEN {
            return Err(PoolNameError::TooLong);
        }
        if name.starts_with('-') || name.ends_with('-') {
            return Err(PoolNameError::EdgeHyphen);
        }
        Ok(Self(name.to_owned()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Label(String);

impl Label {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LabelError {
    #[error("must not be empty")]
    Empty,
    #[error("must not contain whitespace")]
    Whitespace,
    #[error("must not contain commas")]
    Comma,
}

impl FromStr for Label {
    type Err = LabelError;

    fn from_str(label: &str) -> Result<Self, Self::Err> {
        if label.is_empty() {
            return Err(LabelError::Empty);
        }
        if label.contains(char::is_whitespace) {
            return Err(LabelError::Whitespace);
        }
        if label.contains(',') {
            return Err(LabelError::Comma);
        }
        Ok(Self(label.to_owned()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Image(String);

impl Image {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ImageError {
    #[error("must not be empty")]
    Empty,
    #[error("must not contain whitespace")]
    Whitespace,
}

impl FromStr for Image {
    type Err = ImageError;

    fn from_str(image: &str) -> Result<Self, Self::Err> {
        if image.is_empty() {
            return Err(ImageError::Empty);
        }
        if image.contains(char::is_whitespace) {
            return Err(ImageError::Whitespace);
        }
        Ok(Self(image.to_owned()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Cpus(f64);

impl Cpus {
    pub fn get(self) -> f64 {
        self.0
    }
}

impl fmt::Display for Cpus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("must be a finite number greater than 0")]
pub struct CpusError;

impl TryFrom<f64> for Cpus {
    type Error = CpusError;

    fn try_from(count: f64) -> Result<Self, Self::Error> {
        if count.is_finite() && count > 0.0 {
            Ok(Self(count))
        } else {
            Err(CpusError)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteSize(NonZeroU64);

impl ByteSize {
    pub fn bytes(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.bytes();
        match BYTE_UNITS
            .iter()
            .rev()
            .find(|&&(_, shift)| bytes.is_multiple_of(1 << shift))
        {
            Some(&(unit, shift)) => write!(f, "{}{unit}", bytes >> shift),
            None => write!(f, "{bytes}B"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ByteSizeError {
    #[error("expected a whole number followed by KiB, MiB, GiB or TiB, such as `512MiB`")]
    Format,
    #[error("must be greater than 0")]
    Zero,
    #[error("must be less than 16EiB")]
    TooLarge,
}

impl FromStr for ByteSize {
    type Err = ByteSizeError;

    fn from_str(size: &str) -> Result<Self, Self::Err> {
        let (digits, unit) = size.split_at(
            size.find(|c: char| !c.is_ascii_digit())
                .unwrap_or(size.len()),
        );
        let &(_, shift) = BYTE_UNITS
            .iter()
            .find(|&&(name, _)| name == unit)
            .ok_or(ByteSizeError::Format)?;
        if digits.is_empty() {
            return Err(ByteSizeError::Format);
        }
        let bytes = digits
            .parse::<u64>()
            .ok()
            .and_then(|count| count.checked_mul(1 << shift))
            .ok_or(ByteSizeError::TooLarge)?;
        NonZeroU64::new(bytes).map(Self).ok_or(ByteSizeError::Zero)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_names_accept_lowercase_letters_digits_and_hyphens() {
        let name: PoolName = "linux-x64-2".parse().unwrap();

        assert_eq!(name.as_str(), "linux-x64-2");
    }

    #[test]
    fn empty_pool_name_is_rejected() {
        assert_eq!("".parse::<PoolName>(), Err(PoolNameError::Empty));
    }

    #[test]
    fn pool_name_longer_than_32_characters_is_rejected() {
        "a".repeat(32).parse::<PoolName>().unwrap();
        assert_eq!(
            "a".repeat(33).parse::<PoolName>(),
            Err(PoolNameError::TooLong)
        );
    }

    #[test]
    fn pool_name_with_other_characters_is_rejected() {
        for name in ["Linux", "linux_x64", "linux x64", "linux.x64", "línux"] {
            assert_eq!(
                name.parse::<PoolName>(),
                Err(PoolNameError::InvalidCharacter),
                "{name}"
            );
        }
    }

    #[test]
    fn pool_name_starting_or_ending_with_a_hyphen_is_rejected() {
        for name in ["-linux", "linux-", "-"] {
            assert_eq!(
                name.parse::<PoolName>(),
                Err(PoolNameError::EdgeHyphen),
                "{name}"
            );
        }
    }

    #[test]
    fn labels_keep_their_case() {
        let label: Label = "GPU:A100".parse().unwrap();

        assert_eq!(label.as_str(), "GPU:A100");
    }

    #[test]
    fn empty_label_is_rejected() {
        assert_eq!("".parse::<Label>(), Err(LabelError::Empty));
    }

    #[test]
    fn label_with_whitespace_is_rejected() {
        for label in ["two words", " linux", "linux\t"] {
            assert_eq!(
                label.parse::<Label>(),
                Err(LabelError::Whitespace),
                "{label:?}"
            );
        }
    }

    #[test]
    fn label_with_a_comma_is_rejected() {
        assert_eq!("linux,x64".parse::<Label>(), Err(LabelError::Comma));
    }

    #[test]
    fn empty_image_is_rejected() {
        assert_eq!("".parse::<Image>(), Err(ImageError::Empty));
    }

    #[test]
    fn image_with_whitespace_is_rejected() {
        assert_eq!(
            "ghcr.io/octo/runner latest".parse::<Image>(),
            Err(ImageError::Whitespace)
        );
    }

    #[test]
    fn cpus_must_be_finite_and_positive() {
        assert_eq!(Cpus::try_from(0.5).map(Cpus::get), Ok(0.5));
        for count in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(Cpus::try_from(count), Err(CpusError), "{count}");
        }
    }

    #[test]
    fn byte_sizes_accept_binary_units() {
        for (size, bytes) in [
            ("1KiB", 1 << 10),
            ("512MiB", 512 << 20),
            ("4GiB", 4 << 30),
            ("2TiB", 2 << 40),
        ] {
            assert_eq!(
                size.parse::<ByteSize>().map(ByteSize::bytes),
                Ok(bytes),
                "{size}"
            );
        }
    }

    #[test]
    fn byte_sizes_display_in_the_largest_exact_unit() {
        for (size, displayed) in [
            ("1024MiB", "1GiB"),
            ("1536MiB", "1536MiB"),
            ("2TiB", "2TiB"),
        ] {
            assert_eq!(
                size.parse::<ByteSize>().unwrap().to_string(),
                displayed,
                "{size}"
            );
        }
    }

    #[test]
    fn byte_sizes_without_a_whole_number_and_binary_unit_are_rejected() {
        for size in ["4GB", "4g", "4 GiB", "4", "GiB", "1.5GiB", "-1GiB", ""] {
            assert_eq!(
                size.parse::<ByteSize>(),
                Err(ByteSizeError::Format),
                "{size}"
            );
        }
    }

    #[test]
    fn zero_byte_size_is_rejected() {
        assert_eq!("0MiB".parse::<ByteSize>(), Err(ByteSizeError::Zero));
    }

    #[test]
    fn byte_size_over_16_eib_is_rejected() {
        for size in ["16777216TiB", "99999999999999999999KiB"] {
            assert_eq!(
                size.parse::<ByteSize>(),
                Err(ByteSizeError::TooLarge),
                "{size}"
            );
        }
    }
}
