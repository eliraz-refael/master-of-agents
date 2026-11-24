//! How moa's agent is started (A3): its program, environment and folder, and the allow-list
//! that lets most of its calls through without asking.

use std::path::PathBuf;

use serde_json::json;

use super::wire::AdapterSpec;
use crate::config::AccountName;
use crate::op::OpError;
use crate::ops::launch::{CONFIG_DIR_MODE, launcher_of, store_var};
use crate::state::State;

/// What moa's agent may run without asking, besides reading the brain's folders.
const ALLOWED: [&str; 7] = [
    "Bash(moactl:*)",
    "Bash(git status:*)",
    "Bash(git log:*)",
    "Bash(git diff:*)",
    "Bash(git show:*)",
    "Bash(git branch:*)",
    "Bash(ls:*)",
];

/// Taken out of the agent's environment: whatever says it runs inside a Claude session or
/// a moa tab.
const UNSET: [&str; 5] = [
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "MOA_TITLE",
    "MOA_GROUP",
];

/// The `meta` key `/account` sets: the account moa's agent runs on, over moa.toml's
/// `moa_account`. moa.toml stays yours.
pub const ACCOUNT_KEY: &str = "moa.account";

/// The account moa's agent runs on: the one `/account` chose while moa.toml still has it,
/// else moa.toml's ([`configured`]).
pub fn account(state: &State) -> Option<AccountName> {
    let chosen = state
        .meta(ACCOUNT_KEY)
        .and_then(|n| AccountName::parse(n).ok())
        .filter(|n| state.config().accounts.contains_key(n));
    chosen.or_else(|| configured(state))
}

/// moa.toml's `moa_account` for this brain: the account `/account` goes back to.
pub fn configured(state: &State) -> Option<AccountName> {
    let config = state.config();
    config.brain(state.brain()).unwrap_or_default().moa_account
}

/// The accounts moa.toml has that moa's agent can run on ([`spec_on`]), by name.
pub fn runnable(state: &State) -> Vec<AccountName> {
    let names = state.config().accounts.keys();
    names
        .filter(|n| spec_on(state, n).is_ok())
        .cloned()
        .collect()
}

/// What moa says when it has no account to run on.
pub(crate) fn no_account(state: &State) -> OpError {
    OpError(format!(
        "moa has no account yet: set moa_account for the {} brain in moa.toml",
        state.brain()
    ))
}

/// How to start moa's agent for this brain, and the account it runs on ([`account`]).
pub fn spec(state: &State) -> Result<(AccountName, AdapterSpec), OpError> {
    let name = account(state).ok_or_else(|| no_account(state))?;
    let spec = spec_on(state, &name)?;
    Ok((name, spec))
}

/// How to start moa's agent on account `name`: refused when moa.toml doesn't have it, or it
/// has no launcher or program for moa's agent.
pub fn spec_on(state: &State, name: &AccountName) -> Result<AdapterSpec, OpError> {
    let brain = state.brain();
    let config = state.config();
    let deps = state.deps();
    let ours = config.brain(brain).unwrap_or_default();
    let acct = config.accounts.get(name).ok_or_else(|| no_account(state))?;
    let launcher = launcher_of(name.as_str(), acct)?;
    let program = acct
        .acp
        .clone()
        .or_else(|| deps.acp_adapter.clone())
        .ok_or_else(|| {
            OpError(format!(
                "moa's agent isn't part of this build: set acp for {name} in moa.toml"
            ))
        })?;

    let var = |k: &str, v: String| (k.to_string(), v);
    let mut env = vec![
        var("CLAUDE_CODE_EXECUTABLE", launcher.display().to_string()),
        var("CLAUDE_AGENT_ACP_PROTOCOL", "v1".into()),
        var("MOA_SESSION", "moa".into()),
        var("MOA_BRAIN", brain.to_string()),
    ];
    let mut unset = Vec::new();
    match store_var(&acct.store, &deps.home, CONFIG_DIR_MODE, deps.canonical) {
        Some(store) => env.push(var("CLAUDE_CONFIG_DIR", store)),
        None => unset.push("CLAUDE_CONFIG_DIR".to_string()),
    }
    // In a sandbox, a store outside it is a real account's, whose login Claude finds through
    // the real home, so moa's agent gets that home and nothing else of moa does.
    if let Some(h) = &deps.agent_home {
        let outside = !(deps.canonical)(&acct.store).starts_with((deps.canonical)(&deps.home));
        if outside {
            env.push(var("HOME", h.display().to_string()));
        }
    }
    env.extend(deps.panel.env.iter().cloned());
    unset.extend(UNSET.map(str::to_string));

    let spec = AdapterSpec {
        program,
        env,
        unset,
        cwd: deps.agent_dir.clone(),
        settings: settings(&ours.folders),
        moactl: deps.panel.exe.clone(),
    };
    Ok(spec)
}

/// The text of the agent's `.claude/settings.json`: [`ALLOWED`], and reading anything under
/// `folders`, which are full paths (an empty one is skipped). Claude spells a full path in a
/// rule with `//`.
pub fn settings(folders: &[PathBuf]) -> String {
    let reads = folders
        .iter()
        .filter(|f| !f.as_os_str().is_empty())
        .map(|f| {
            format!(
                "Read(/{}/**)",
                f.components().collect::<PathBuf>().display()
            )
        });
    let allow: Vec<String> = ALLOWED.iter().map(|a| a.to_string()).chain(reads).collect();
    let text = serde_json::to_string_pretty(&json!({ "permissions": { "allow": allow } }))
        .expect("a list of strings is JSON");
    text + "\n"
}
