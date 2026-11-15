//! Which agent store a spike `account` value means, and the stable `#n` numbers reports use
//! in place of account names.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

use moa_core::model::AgentKind;

/// An account as moa knows it. `store` is the agent's data directory when there is one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ResolvedAccount {
    pub name: String,
    pub store: Option<PathBuf>,
    pub kind: AgentKind,
}

/// The accounts the user named, the home directory to look in, and the order in which
/// accounts were first mentioned.
#[derive(Debug)]
pub struct AccountMap {
    home: PathBuf,
    explicit: Vec<(String, PathBuf)>,
    seen: RefCell<Vec<String>>,
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b
        || match (fs::canonicalize(a), fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
}

impl AccountMap {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            explicit: Vec::new(),
            seen: RefCell::new(Vec::new()),
        }
    }

    /// Adds an explicit `name` for the agent store at `store`.
    pub fn with(mut self, name: impl Into<String>, store: PathBuf) -> Self {
        self.explicit.push((name.into(), store));
        self
    }

    /// Parses a `NAME=STORE` flag, expanding a leading `~`. The error never echoes the input.
    pub fn parse_flag(s: &str, home: &Path) -> Result<(String, PathBuf), String> {
        let Some((name, store)) = s.split_once('=') else {
            return Err("an account is written NAME=STORE".to_string());
        };
        if name.is_empty() || store.is_empty() {
            return Err("an account needs both a name and a store".to_string());
        }
        Ok((name.to_string(), expand_tilde(store, home)))
    }

    fn explicit_owning(&self, dir: &Path) -> Option<&(String, PathBuf)> {
        self.explicit.iter().find(|(_, s)| same_path(s, dir))
    }

    /// Works out which account a spike `account` value means.
    pub fn resolve(&self, spike_account: &str) -> ResolvedAccount {
        let claude = |name: &str, store: Option<PathBuf>| ResolvedAccount {
            name: name.to_string(),
            store,
            kind: AgentKind::Claude,
        };
        if spike_account == "codex" {
            return ResolvedAccount {
                name: "codex".into(),
                store: None,
                kind: AgentKind::Codex,
            };
        }
        if let Some((name, store)) = self.explicit.iter().find(|(n, _)| n == spike_account) {
            return claude(name, Some(store.clone()));
        }
        if spike_account.starts_with('/') || spike_account.starts_with('~') {
            let path = expand_tilde(spike_account, &self.home);
            if let Some((name, store)) = self.explicit_owning(&path) {
                return claude(name, Some(store.clone()));
            }
            let base = path
                .file_name()
                .map(|b| b.to_string_lossy().into_owned())
                .unwrap_or_default();
            let name = base.strip_prefix('.').unwrap_or(&base);
            let store = path.is_dir().then_some(path.clone());
            return claude(name, store);
        }
        let dotted = self.home.join(format!(".{spike_account}"));
        if dotted.is_dir() {
            if let Some((name, store)) = self.explicit_owning(&dotted) {
                return claude(name, Some(store.clone()));
            }
            return claude(spike_account, Some(dotted));
        }
        claude(spike_account, None)
    }

    /// The agent store of an account name: an explicit entry, else `<home>/.<name>` if it
    /// is a directory.
    pub fn store_for(&self, name: &str) -> Option<PathBuf> {
        if let Some((_, s)) = self.explicit.iter().find(|(n, _)| n == name) {
            return Some(s.clone());
        }
        let dotted = self.home.join(format!(".{name}"));
        dotted.is_dir().then_some(dotted)
    }

    /// The 1-based number reports use for an account name, in first-seen order.
    pub fn index_of(&self, name: &str) -> usize {
        let mut seen = self.seen.borrow_mut();
        match seen.iter().position(|n| n == name) {
            Some(i) => i + 1,
            None => {
                seen.push(name.to_string());
                seen.len()
            }
        }
    }

    /// The account names given a number so far, in order.
    pub fn seen(&self) -> Vec<String> {
        self.seen.borrow().clone()
    }
}

fn expand_tilde(s: &str, home: &Path) -> PathBuf {
    if s == "~" {
        home.to_path_buf()
    } else if let Some(rest) = s.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(s)
    }
}
