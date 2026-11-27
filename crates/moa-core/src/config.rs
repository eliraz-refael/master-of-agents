//! `moa.toml`: brains, accounts and setup state. Parsing and validation only; the caller
//! reads the file and supplies the home directory and a path canonicalizer, so this stays pure.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::model::AgentKind;
use crate::route::is_under;

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

/// The name of moa's constitution file in moa's config folder, `constitution.md`. A brain
/// can't be called this: its scope file, `<brain>.md` in the same folder, would be that file.
pub const CONSTITUTION_NAME: &str = "constitution";

/// Words a brain can't be called, since `moa <word>` already means a command: `moa`'s
/// subcommands (clap's `help` among them) and every `moactl` verb (T18). Tests in `moa` and
/// `moa-proto` check that each one is here.
pub const RESERVED_BRAIN_NAMES: &[&str] = &[
    "adopt-spike",
    "export-spike",
    "daemon",
    "status",
    "shutdown",
    "upgrade",
    "build-id",
    "sql",
    "service",
    "panel",
    "pick",
    "console",
    "hook",
    "help",
    "ls",
    "find",
    "set",
    "archive",
    "group",
    "note",
    "project",
    "task",
    "suggest",
    "suggestion",
    "brief",
    "attention",
    "open",
    "resume",
    "focus",
    "close",
    "sleep",
    "term",
    "adopt",
    "restart",
    "goto",
    "layout",
    "bar",
    "peers",
    "ask",
    "reply",
    "asks",
    "setup",
    "import-herdr",
];

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
    /// How long an idle session waits before it goes to sleep (`sleep_after_hours`, R17).
    pub sleep_after: SleepAfter,
}

/// How long an idle session waits before it goes to sleep, in milliseconds; `0` is never.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SleepAfter(pub i64);

impl Default for SleepAfter {
    /// Six hours (spec §6.13).
    fn default() -> Self {
        SleepAfter(21_600_000)
    }
}

impl SleepAfter {
    /// The wait, or `None` for never.
    pub fn ms(self) -> Option<i64> {
        (self.0 > 0).then_some(self.0)
    }

    /// `sleep_after_hours`: a number of hours, 0 or more ([`hours_ms`]).
    fn parse(v: &toml::Value) -> Option<SleepAfter> {
        hours_ms(v).map(SleepAfter)
    }
}

/// A TOML number of hours, whole or fractional, 0 or more, as milliseconds (`0` stays `0`: a
/// setting's "never" or "off"). A positive value too small for a millisecond is one. `None`
/// for anything else.
pub fn hours_ms(v: &toml::Value) -> Option<i64> {
    let hours = match v {
        toml::Value::Integer(n) => *n as f64,
        toml::Value::Float(f) => *f,
        _ => return None,
    };
    if !hours.is_finite() || hours < 0.0 {
        return None;
    }
    // `as` saturates: an absurd number of hours is as good as never waking.
    let ms = (hours * 3_600_000.0).round() as i64;
    Some(if hours > 0.0 { ms.max(1) } else { 0 })
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AccountConfig {
    pub kind: AgentKind,
    pub store: PathBuf,
    pub launcher: Option<PathBuf>,
    /// The program that runs moa's agent for this account (`acp`), over the one moa was
    /// built with.
    pub acp: Option<PathBuf>,
    pub brain: Option<BrainName>,
    pub except: Vec<PathBuf>,
    pub only: Vec<PathBuf>,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SetupConfig {
    pub skipped: Vec<AccountName>,
}

/// `[ui]`: how moa's terminal behaves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct UiConfig {
    /// moa's terminal turns the mouse on (V11). With `false` moa says nothing about the
    /// mouse, so the user's own setting stands.
    pub mouse: bool,
    /// The form of the session picker `Ctrl-a /` opens; `s` opens the other one (S8).
    pub picker: Form,
    /// How long a terminal you aren't looking at goes untouched before `term ls` marks it
    /// idle (`terminal_idle_hours`), in milliseconds; `0` turns the mark off.
    pub terminal_idle_ms: i64,
}

impl Default for UiConfig {
    fn default() -> UiConfig {
        UiConfig {
            mouse: true,
            picker: Form::Popup,
            terminal_idle_ms: 2 * 3_600_000,
        }
    }
}

/// A form the picker opens in: a popup over the tab you're on, or a full tab of its own. The
/// `brain` row carries it as `"popup"` or `"tab"`, so help knows which key opens which.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Form {
    #[default]
    Popup,
    Tab,
}

/// `[moa]`: how moa's agent is woken and watched, in milliseconds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MoaConfig {
    /// How long a session report waits for the session to go quiet before moa is woken
    /// (`wake_settle_seconds`); `0` wakes at once.
    pub wake_settle_ms: i64,
    /// The least time between two wake-ups (`wake_gap_seconds`); `0` has no gap.
    pub wake_gap_ms: i64,
    /// The most sessions one wake-up carries (`wake_batch`), 1 to 100.
    pub wake_batch: usize,
    /// How long a turn of moa's agent can go quiet before it's cancelled
    /// (`watchdog_seconds`), 1 or more.
    pub watchdog_ms: i64,
    /// How long a cancelled turn gets to stop before the agent is killed
    /// (`watchdog_kill_seconds`), 1 or more.
    pub watchdog_kill_ms: i64,
}

impl Default for MoaConfig {
    fn default() -> MoaConfig {
        MoaConfig {
            wake_settle_ms: 45_000,
            wake_gap_ms: 180_000,
            wake_batch: 8,
            watchdog_ms: 300_000,
            watchdog_kill_ms: 30_000,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Config {
    pub brains: BTreeMap<BrainName, BrainConfig>,
    pub accounts: BTreeMap<AccountName, AccountConfig>,
    pub setup: SetupConfig,
    pub ui: UiConfig,
    pub moa: MoaConfig,
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
    #[serde(default)]
    ui: RawUi,
    #[serde(default)]
    moa: RawMoa,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawMoa {
    /// Each is checked by hand, so any value gets the one plain line.
    wake_settle_seconds: Option<toml::Value>,
    wake_gap_seconds: Option<toml::Value>,
    wake_batch: Option<toml::Value>,
    watchdog_seconds: Option<toml::Value>,
    watchdog_kill_seconds: Option<toml::Value>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawUi {
    /// Checked by hand, so any value gets the one plain line.
    mouse: Option<toml::Value>,
    /// The same.
    picker: Option<toml::Value>,
    /// The same.
    terminal_idle_hours: Option<toml::Value>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawBrain {
    #[serde(default)]
    folders: Vec<String>,
    moa_account: Option<String>,
    open_account: Option<String>,
    spike_db: Option<String>,
    /// Checked by hand, so any value gets the one plain line.
    sleep_after_hours: Option<toml::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAccount {
    kind: Option<String>,
    store: Option<String>,
    launcher: Option<String>,
    acp: Option<String>,
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

/// A `[moa]` whole number of seconds, at least `min`, as milliseconds. `None` when unset.
fn seconds_ms(v: &Option<toml::Value>, key: &str, min: i64) -> Result<Option<i64>, ConfigError> {
    match v {
        None => Ok(None),
        Some(toml::Value::Integer(n)) if *n >= min => Ok(Some(n.saturating_mul(1000))),
        Some(_) => Err(problem(format_args!(
            "{key} in [moa] must be a whole number of seconds, {min} or more"
        ))),
    }
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

const TABLES: [&str; 5] = ["brains", "accounts", "setup", "ui", "moa"];

/// Edit distance (insert, delete, change, or swap two neighbours), for catching a misspelt
/// table name: `mao` is one edit from `moa`.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut rows = vec![(0..=b.len()).collect::<Vec<usize>>(); a.len() + 1];
    for (i, row) in rows.iter_mut().enumerate() {
        row[0] = i;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let same = usize::from(a[i - 1] != b[j - 1]);
            let mut d = (rows[i - 1][j - 1] + same)
                .min(rows[i - 1][j] + 1)
                .min(rows[i][j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d = d.min(rows[i - 2][j - 2] + 1);
            }
            rows[i][j] = d;
        }
    }
    rows[a.len()][b.len()]
}

/// Whether `key` is a misspelling of the table `known`: two edits for a long name; for a
/// short one, one edit that keeps the first letter, so a short table of its own (`[api]`,
/// `[tui]`, `[x]`) isn't taken for `[ui]`. Case is ignored.
fn near_miss(key: &str, known: &str) -> bool {
    // A difference only in case is no edit at all: `[UI]` is `[ui]` misspelt.
    let key = key.to_lowercase();
    let d = distance(&key, known);
    if known.chars().count() <= 4 {
        d <= 1 && key.chars().next() == known.chars().next()
    } else {
        d <= 2
    }
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
            if let Some(known) = TABLES.iter().find(|t| near_miss(key, t)) {
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
            if name.as_str() == CONSTITUTION_NAME {
                return Err(problem(format_args!(
                    "a brain can't be called {name}; moa's constitution file has that name"
                )));
            }
            if RESERVED_BRAIN_NAMES.contains(&name.as_str()) {
                return Err(problem(format_args!(
                    "a brain can't be called {name}; moa uses that word for a command"
                )));
            }
            let place = format!("brains.{name}");
            let sleep_after = match &b.sleep_after_hours {
                None => SleepAfter::default(),
                Some(v) => SleepAfter::parse(v).ok_or_else(|| {
                    problem(format_args!(
                        "sleep_after_hours in [{place}] must be a number of hours, 0 or more"
                    ))
                })?,
            };
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
                    sleep_after,
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
                    acp: a
                        .acp
                        .as_deref()
                        .map(|s| expand_one(s, "acp", &place, home))
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
        config.ui.mouse = match &raw.ui.mouse {
            None => UiConfig::default().mouse,
            Some(toml::Value::Boolean(on)) => *on,
            Some(_) => return Err(problem("mouse in [ui] must be true or false")),
        };
        config.ui.picker = match &raw.ui.picker {
            None => UiConfig::default().picker,
            Some(toml::Value::String(s)) if s == "popup" => Form::Popup,
            Some(toml::Value::String(s)) if s == "tab" => Form::Tab,
            Some(_) => return Err(problem(r#"picker in [ui] must be "popup" or "tab""#)),
        };
        config.ui.terminal_idle_ms = match &raw.ui.terminal_idle_hours {
            None => UiConfig::default().terminal_idle_ms,
            Some(v) => hours_ms(v).ok_or_else(|| {
                problem("terminal_idle_hours in [ui] must be a number of hours, 0 or more")
            })?,
        };
        let moa = &raw.moa;
        let default = MoaConfig::default();
        config.moa = MoaConfig {
            wake_settle_ms: seconds_ms(&moa.wake_settle_seconds, "wake_settle_seconds", 0)?
                .unwrap_or(default.wake_settle_ms),
            wake_gap_ms: seconds_ms(&moa.wake_gap_seconds, "wake_gap_seconds", 0)?
                .unwrap_or(default.wake_gap_ms),
            wake_batch: match &moa.wake_batch {
                None => default.wake_batch,
                Some(toml::Value::Integer(n)) if (1..=100).contains(n) => *n as usize,
                Some(_) => {
                    return Err(problem(
                        "wake_batch in [moa] must be a whole number from 1 to 100",
                    ));
                }
            },
            watchdog_ms: seconds_ms(&moa.watchdog_seconds, "watchdog_seconds", 1)?
                .unwrap_or(default.watchdog_ms),
            watchdog_kill_ms: seconds_ms(&moa.watchdog_kill_seconds, "watchdog_kill_seconds", 1)?
                .unwrap_or(default.watchdog_kill_ms),
        };
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
        config.check_distinct_folders()?;
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
        config.check_distinct_folders()?;
        Ok(config)
    }

    /// One folder in two brains would leave its owner undecided. Nesting is fine: the most
    /// specific folder wins (`brain_for_folder`).
    fn check_distinct_folders(&self) -> Result<(), ConfigError> {
        let mut owners: BTreeMap<&Path, &BrainName> = BTreeMap::new();
        for (name, b) in &self.brains {
            for f in b.folders.iter().filter(|f| !f.as_os_str().is_empty()) {
                if let Some(other) = owners.insert(f, name)
                    && other != name
                {
                    return Err(problem(format_args!(
                        "{} is a folder of both brains.{other} and brains.{name}; a folder belongs to one brain",
                        f.components().collect::<PathBuf>().display()
                    )));
                }
            }
        }
        Ok(())
    }

    /// The brain that owns `path`: the one whose configured folder is the longest
    /// whole-component prefix of it, so `~/work` beats `~`. `None` when no folder holds it,
    /// or when `path` is relative or contains `..` (`is_under` refuses those). Pass a
    /// canonical path; `from_file_text` canonicalises the folders.
    pub fn brain_for_folder(&self, path: &Path) -> Option<BrainName> {
        self.brains
            .iter()
            .flat_map(|(name, b)| b.folders.iter().map(move |f| (name, f)))
            .filter(|(_, f)| is_under(path, f))
            .max_by_key(|(_, f)| f.components().count())
            .map(|(name, _)| name.clone())
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

    /// Whether a `moactl` write from `account` is refused: `moa.toml` lists accounts and this
    /// caller isn't one of them. With no accounts listed nothing is refused.
    pub fn refuses_unlisted(&self, account: Option<&AccountName>) -> bool {
        !self.accounts.is_empty() && account.is_none()
    }

    pub fn account_for_store(&self, store: &Path) -> Option<&AccountName> {
        self.accounts
            .iter()
            .find(|(_, a)| a.store == store)
            .map(|(n, _)| n)
    }
}

/// The log's warning about the top-level tables of `moa.toml` moa doesn't know and skips,
/// said at start and at each reload; `None` when there are none.
pub fn skipped_warning(config: &Config) -> Option<String> {
    (!config.skipped_tables.is_empty()).then(|| {
        format!(
            "moa.toml has tables moa doesn't know, which it skips: {}",
            config.skipped_tables.join(", ")
        )
    })
}

/// The agent store of whoever is calling: `CLAUDE_CONFIG_DIR`, else `<home>/.claude`.
pub fn caller_store(claude_config_dir: Option<&OsStr>, home: &Path) -> PathBuf {
    match claude_config_dir {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => home.join(".claude"),
    }
}
