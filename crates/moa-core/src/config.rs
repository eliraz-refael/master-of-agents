//! `moa.toml`: brains, accounts and setup state. Parsing and validation only; the caller
//! reads the file and supplies the home directory and a path canonicalizer, so this stays pure.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::model::AgentKind;

/// A problem with `moa.toml`, as one plain line that always starts with `moa.toml`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

fn problem(msg: impl fmt::Display) -> ConfigError {
    ConfigError(format!("moa.toml: {msg}"))
}

macro_rules! name_type {
    ($name:ident, $max:expr, $ok_char:expr, $what:expr, $hint:expr) => {
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
        pub struct $name(String);

        impl $name {
            pub fn parse(s: &str) -> Result<Self, ConfigError> {
                let ok_char: fn(char) -> bool = $ok_char;
                let starts_ok = s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric());
                if s.chars().count() > $max || !starts_ok || !s.chars().all(ok_char) {
                    return Err(problem(format_args!(
                        "\"{s}\" isn't {} name (use {})",
                        $what, $hint
                    )));
                }
                Ok(Self(s.to_string()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Self::parse(&s).map_err(|e| serde::de::Error::custom(e.0))
            }
        }
    };
}

name_type!(
    BrainName,
    32,
    |c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_',
    "a brain",
    "lowercase letters, digits, - and _"
);
name_type!(
    AccountName,
    64,
    |c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_',
    "an account",
    "letters, digits, ., - and _"
);

impl BrainName {
    pub fn default_brain() -> Self {
        Self("default".to_string())
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct BrainConfig {
    pub folders: Vec<PathBuf>,
    pub moa_account: Option<AccountName>,
    pub open_account: Option<AccountName>,
    pub spike_db: Option<PathBuf>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AccountConfig {
    pub kind: AgentKind,
    pub store: PathBuf,
    pub launcher: Option<PathBuf>,
    pub brain: Option<BrainName>,
    pub except: Vec<PathBuf>,
    pub only: Vec<PathBuf>,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SetupConfig {
    pub skipped: Vec<AccountName>,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Config {
    pub brains: BTreeMap<BrainName, BrainConfig>,
    pub accounts: BTreeMap<AccountName, AccountConfig>,
    pub setup: SetupConfig,
    /// Top-level tables moa doesn't know, for the caller to log as a warning.
    pub skipped_tables: Vec<String>,
}

#[derive(Deserialize, Default)]
struct RawConfig {
    #[serde(default)]
    brains: BTreeMap<String, RawBrain>,
    #[serde(default)]
    accounts: BTreeMap<String, RawAccount>,
    #[serde(default)]
    setup: RawSetup,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawBrain {
    #[serde(default)]
    folders: Vec<String>,
    moa_account: Option<String>,
    open_account: Option<String>,
    spike_db: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAccount {
    kind: Option<String>,
    store: Option<String>,
    launcher: Option<String>,
    brain: Option<String>,
    #[serde(default)]
    except: Vec<String>,
    only: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawSetup {
    #[serde(default)]
    skipped: Vec<String>,
}

fn toml_problem(text: &str, e: &toml::de::Error) -> ConfigError {
    let msg = e.message().split_whitespace().collect::<Vec<_>>().join(" ");
    match e.span() {
        Some(span) => {
            let line = 1 + text[..span.start.min(text.len())].matches('\n').count();
            ConfigError(format!("moa.toml line {line}: {msg}"))
        }
        None => problem(msg),
    }
}

/// Expands `~` and `~/x` and insists on an absolute path. An empty entry stays empty.
fn expand(s: &str, what: &str, place: &str, home: &Path) -> Result<PathBuf, ConfigError> {
    let path = expand_unchecked(s, what, place, home)?;
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(problem(format_args!(
            "the {what} in {place} can't contain .."
        )));
    }
    Ok(path)
}

fn expand_unchecked(s: &str, what: &str, place: &str, home: &Path) -> Result<PathBuf, ConfigError> {
    if s.is_empty() {
        return Ok(PathBuf::new());
    }
    if s == "~" {
        return Ok(home.to_path_buf());
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return Ok(home.join(rest));
    }
    if s.starts_with('~') {
        return Err(problem(format_args!("{s}: only ~ or ~/… is understood")));
    }
    if !s.starts_with('/') {
        return Err(problem(format_args!(
            "the {what} {s} in {place} must start with / or ~"
        )));
    }
    Ok(PathBuf::from(s))
}

/// Like `expand`, but for a single path where an empty value is an error.
fn expand_one(s: &str, what: &str, place: &str, home: &Path) -> Result<PathBuf, ConfigError> {
    if s.is_empty() {
        return Err(problem(format_args!(
            "the {what} in {place} is empty; it must start with / or ~"
        )));
    }
    expand(s, what, place, home)
}

const TABLES: [&str; 3] = ["brains", "accounts", "setup"];

/// Levenshtein distance, for catching a misspelt table name.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

fn expand_all(
    v: &[String],
    what: &str,
    place: &str,
    home: &Path,
) -> Result<Vec<PathBuf>, ConfigError> {
    v.iter().map(|s| expand(s, what, place, home)).collect()
}

impl Config {
    /// Parses and validates `moa.toml` text, expanding `~` against `home`.
    pub fn parse(text: &str, home: &Path) -> Result<Config, ConfigError> {
        let table: toml::Table = toml::from_str(text).map_err(|e| toml_problem(text, &e))?;
        let mut skipped_tables = Vec::new();
        for key in table.keys().filter(|k| !TABLES.contains(&k.as_str())) {
            if let Some(known) = TABLES.iter().find(|t| distance(key, t) <= 2) {
                return Err(problem(format_args!(
                    "[{key}] isn't a table moa knows; did you mean [{known}]?"
                )));
            }
            skipped_tables.push(key.clone());
        }
        let raw: RawConfig = toml::from_str(text).map_err(|e| toml_problem(text, &e))?;

        let mut config = Config {
            skipped_tables,
            ..Config::default()
        };
        for (name, b) in &raw.brains {
            let name = BrainName::parse(name)?;
            let place = format!("brains.{name}");
            config.brains.insert(
                name,
                BrainConfig {
                    folders: expand_all(&b.folders, "folder", &place, home)?,
                    moa_account: b
                        .moa_account
                        .as_deref()
                        .map(AccountName::parse)
                        .transpose()?,
                    open_account: b
                        .open_account
                        .as_deref()
                        .map(AccountName::parse)
                        .transpose()?,
                    spike_db: b
                        .spike_db
                        .as_deref()
                        .map(|s| expand_one(s, "spike_db", &place, home))
                        .transpose()?,
                },
            );
        }
        for (name, a) in &raw.accounts {
            let name = AccountName::parse(name)?;
            let place = format!("accounts.{name}");
            let kind = a
                .kind
                .as_deref()
                .and_then(|k| k.parse::<AgentKind>().ok())
                .ok_or_else(|| problem(format_args!("{name}'s kind must be claude or codex")))?;
            let store = a
                .store
                .as_deref()
                .ok_or_else(|| problem(format_args!("{name} needs a store")))?;
            let brain = a.brain.as_deref().map(BrainName::parse).transpose()?;
            if !a.except.is_empty() && brain.is_none() {
                return Err(problem(format_args!(
                    "{name} has except folders but no brain to send the rest to"
                )));
            }
            if a.only.as_ref().is_some_and(Vec::is_empty) {
                return Err(problem(format_args!(
                    "only in accounts.{name} is empty; remove it to let the account work everywhere"
                )));
            }
            if brain.is_some() && a.only.is_some() {
                return Err(problem(format_args!(
                    "{name} can't both go to a brain and be limited to folders"
                )));
            }
            config.accounts.insert(
                name,
                AccountConfig {
                    kind,
                    store: expand_one(store, "store", &place, home)?,
                    launcher: a
                        .launcher
                        .as_deref()
                        .map(|s| expand_one(s, "launcher", &place, home))
                        .transpose()?,
                    brain,
                    except: expand_all(&a.except, "folder", &format!("{place}.except"), home)?,
                    only: expand_all(
                        a.only.as_deref().unwrap_or_default(),
                        "folder",
                        &format!("{place}.only"),
                        home,
                    )?,
                },
            );
        }
        config.setup.skipped = raw
            .setup
            .skipped
            .iter()
            .map(|s| AccountName::parse(s))
            .collect::<Result<_, _>>()?;

        for (name, a) in &config.accounts {
            if let Some(b) = &a.brain
                && *b != BrainName::default_brain()
                && !config.brains.contains_key(b)
            {
                return Err(problem(format_args!(
                    "{name} sends its sessions to the {b} brain, which isn't in moa.toml"
                )));
            }
        }
        config.check_distinct_stores()?;
        for (name, b) in &config.brains {
            for (key, acct) in [
                ("moa_account", &b.moa_account),
                ("open_account", &b.open_account),
            ] {
                if let Some(acct) = acct
                    && !config.accounts.contains_key(acct)
                {
                    return Err(problem(format_args!(
                        "the {name} brain's {key} {acct} isn't in [accounts]"
                    )));
                }
            }
        }
        Ok(config)
    }

    /// Rewrites every folder, `except`, `only` and `store` path. Empty entries stay empty.
    pub fn canonicalize_with(&mut self, f: impl Fn(&Path) -> PathBuf) {
        let one = |p: &mut PathBuf| {
            if !p.as_os_str().is_empty() {
                *p = f(p);
            }
        };
        for b in self.brains.values_mut() {
            b.folders.iter_mut().for_each(one);
        }
        for a in self.accounts.values_mut() {
            one(&mut a.store);
            a.except.iter_mut().for_each(one);
            a.only.iter_mut().for_each(one);
        }
    }

    /// The one loader: no file means the default config, else parse then canonicalize.
    pub fn from_file_text(
        text: Option<&str>,
        home: &Path,
        canonical: impl Fn(&Path) -> PathBuf,
    ) -> Result<Config, ConfigError> {
        let Some(text) = text else {
            return Ok(Config::default());
        };
        let mut config = Config::parse(text, home)?;
        config.canonicalize_with(canonical);
        // Two spellings of one folder (a symlink, say) only show up once canonical.
        config.check_distinct_stores()?;
        Ok(config)
    }

    fn check_distinct_stores(&self) -> Result<(), ConfigError> {
        let mut stores: BTreeMap<&Path, &AccountName> = BTreeMap::new();
        for (name, a) in &self.accounts {
            if let Some(other) = stores.insert(&a.store, name) {
                return Err(problem(format_args!(
                    "{other} and {name} have the same store {}",
                    a.store.display()
                )));
            }
        }
        Ok(())
    }

    /// The configured brain, or an empty one for `default` when it isn't configured.
    pub fn brain(&self, b: &BrainName) -> Option<BrainConfig> {
        match self.brains.get(b) {
            Some(c) => Some(c.clone()),
            None => (*b == BrainName::default_brain()).then(BrainConfig::default),
        }
    }

    pub fn account_for_store(&self, store: &Path) -> Option<&AccountName> {
        self.accounts
            .iter()
            .find(|(_, a)| a.store == store)
            .map(|(n, _)| n)
    }
}

/// The agent store of whoever is calling: `CLAUDE_CONFIG_DIR`, else `<home>/.claude`.
pub fn caller_store(claude_config_dir: Option<&OsStr>, home: &Path) -> PathBuf {
    match claude_config_dir {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => home.join(".claude"),
    }
}
