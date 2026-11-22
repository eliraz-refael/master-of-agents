//! How moa's agent is started (`central::launch`) and what it's told (`central::prompt`).

mod common;

use std::io;
use std::path::{Path, PathBuf};

use common::loaded_with;
use moa_core::central::launch::{settings, spec};
use moa_core::central::prompt::{CONSTITUTION, system_text};
use moa_core::effect::{LogLevel, LogLine};
use moa_core::model::NoteKind;
use moa_core::op::{NoParams, Op};
use moa_core::state::{Deps, PanelLaunch, State};
use moa_testkit::brain::{TypicalBrain, run};

const ADAPTER: &str = "/opt/moa/bin/claude-agent-acp";
const LAUNCHER: &str = "/opt/claude/bin/claude";
const EXE: &str = "/opt/moa/bin/moa";
const AGENT_DIR: &str = "/tmp/moa-test-brain/agent";
const SCOPE: &str = "/tmp/moa-test-config/default.md";
const CONSTITUTION_FILE: &str = "/tmp/moa-test-config/constitution.md";

/// `moa.toml` with moa's account `acct-a` in `store`, plus `extra` in its table.
fn toml(store: &str, extra: &str) -> String {
    format!(
        "[brains.default]\nfolders = [\"~/private\", \"~/my code\"]\nmoa_account = \"acct-a\"\n\
         [accounts.acct-a]\nkind = \"claude\"\nstore = \"{store}\"\nlauncher = \"{LAUNCHER}\"\n{extra}"
    )
}

fn deps(b: &TypicalBrain) -> Deps {
    Deps {
        home: b.home.clone(),
        acp_adapter: Some(PathBuf::from(ADAPTER)),
        agent_dir: PathBuf::from(AGENT_DIR),
        scope_file: PathBuf::from(SCOPE),
        constitution_file: PathBuf::from(CONSTITUTION_FILE),
        panel: PanelLaunch {
            exe: PathBuf::from(EXE),
            env: vec![("MOA_ROOT".into(), "/tmp/moa-root".into())],
        },
        ..Deps::default()
    }
}

fn state(b: &TypicalBrain, toml: &str, deps: Deps) -> State {
    loaded_with(b, toml, deps, b.snapshot())
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn words(v: &[&str]) -> Vec<String> {
    v.iter().map(|w| w.to_string()).collect()
}

#[test]
fn the_spec_runs_the_built_in_adapter_with_the_specs_env() {
    let b = TypicalBrain::new();
    let st = state(&b, &toml("~/.claude-acct-a", ""), deps(&b));
    let (account, spec) = spec(&st).unwrap();
    assert_eq!(account.as_str(), "acct-a");
    assert_eq!(spec.program, Path::new(ADAPTER));
    let store = b.home.join(".claude-acct-a").display().to_string();
    assert_eq!(
        spec.env,
        pairs(&[
            ("CLAUDE_CODE_EXECUTABLE", LAUNCHER),
            ("CLAUDE_AGENT_ACP_PROTOCOL", "v1"),
            ("MOA_SESSION", "moa"),
            ("MOA_BRAIN", "default"),
            ("CLAUDE_CONFIG_DIR", &store),
            ("MOA_ROOT", "/tmp/moa-root"),
        ])
    );
    assert_eq!(
        spec.unset,
        words(&[
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_CODE_SESSION_ID",
            "MOA_TITLE",
            "MOA_GROUP",
        ])
    );
    assert_eq!(spec.cwd, Path::new(AGENT_DIR));
    assert_eq!(spec.moactl, Path::new(EXE));
    let folders = [b.home.join("private"), b.home.join("my code")];
    assert_eq!(spec.settings, settings(&folders));
}

#[test]
fn an_accounts_acp_overrides_the_built_in() {
    let b = TypicalBrain::new();
    let st = state(
        &b,
        &toml("~/.claude-acct-a", "acp = \"~/bin/stub-acp\"\n"),
        deps(&b),
    );
    let (_, spec) = spec(&st).unwrap();
    assert_eq!(spec.program, b.home.join("bin/stub-acp"));
}

#[test]
fn the_default_store_unsets_claude_config_dir() {
    let b = TypicalBrain::new();
    let st = state(&b, &toml("~/.claude", ""), deps(&b));
    let (_, spec) = spec(&st).unwrap();
    assert!(
        spec.env.iter().all(|(k, _)| k != "CLAUDE_CONFIG_DIR"),
        "{:?}",
        spec.env
    );
    assert_eq!(
        spec.unset,
        words(&[
            "CLAUDE_CONFIG_DIR",
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_CODE_SESSION_ID",
            "MOA_TITLE",
            "MOA_GROUP",
        ])
    );
}

#[test]
fn no_account_no_launcher_no_adapter_say_so() {
    let b = TypicalBrain::new();
    let err = |toml: &str, deps: Deps| spec(&state(&b, toml, deps)).unwrap_err().0;

    let no_account = "[brains.default]\nfolders = [\"~/private\"]\n";
    assert_eq!(
        err(no_account, deps(&b)),
        "moa has no account yet: set moa_account for the default brain in moa.toml"
    );
    // No `moa.toml` at all: the default brain, with no account either.
    assert_eq!(
        err("", deps(&b)),
        "moa has no account yet: set moa_account for the default brain in moa.toml"
    );

    let no_launcher = "[brains.default]\nmoa_account = \"acct-a\"\n\
                       [accounts.acct-a]\nkind = \"claude\"\nstore = \"~/.claude-acct-a\"\n";
    assert_eq!(
        err(no_launcher, deps(&b)),
        "can't find claude for acct-a: set its launcher in moa.toml"
    );

    let no_adapter = Deps {
        acp_adapter: None,
        ..deps(&b)
    };
    assert_eq!(
        err(&toml("~/.claude-acct-a", ""), no_adapter),
        "moa's agent isn't part of this build: set acp for acct-a in moa.toml"
    );
}

#[test]
fn the_allow_list_is_exact() {
    let folders = [PathBuf::from("/h/private"), PathBuf::from("/h/my code")];
    assert_eq!(
        settings(&folders),
        r#"{
  "permissions": {
    "allow": [
      "Bash(moactl:*)",
      "Bash(git status:*)",
      "Bash(git log:*)",
      "Bash(git diff:*)",
      "Bash(git show:*)",
      "Bash(git branch:*)",
      "Bash(ls:*)",
      "Read(//h/private/**)",
      "Read(//h/my code/**)"
    ]
  }
}
"#
    );
}

/// A folder is spelled by its parts, so a trailing `/` adds nothing; an empty one is skipped.
#[test]
fn the_allow_list_spells_each_folder_once() {
    let folders = [PathBuf::from("/h/kite/"), PathBuf::new()];
    assert_eq!(settings(&folders), settings(&[PathBuf::from("/h/kite")]));
    assert!(settings(&folders).contains("\"Read(//h/kite/**)\""));
    assert_eq!(settings(&folders).matches("Read(").count(), 1);
}

/// The scope file holds "Lantern lives in ~/private/lantern."; the constitution file is
/// missing.
fn scope_only(p: &Path) -> io::Result<Option<String>> {
    Ok((p == Path::new(SCOPE)).then(|| "Lantern lives in ~/private/lantern.\n".to_string()))
}

/// Both files are there.
fn both(p: &Path) -> io::Result<Option<String>> {
    Ok(if p == Path::new(CONSTITUTION_FILE) {
        Some("You are moa, and brief.\n".to_string())
    } else if p == Path::new(SCOPE) {
        Some("Lantern lives in ~/private/lantern.\n".to_string())
    } else {
        None
    })
}

/// Every file is there, and none can be read.
fn unreadable(_: &Path) -> io::Result<Option<String>> {
    Err(io::ErrorKind::PermissionDenied.into())
}

#[test]
fn the_system_text_is_constitution_scope_and_brief() {
    let b = TypicalBrain::new();
    let now = b.now();
    let constitution = CONSTITUTION.trim_end();

    let with_scope = Deps {
        read_text: scope_only,
        ..deps(&b)
    };
    let mut st = state(&b, &toml("~/.claude-acct-a", ""), with_scope);
    let brief = run(&mut st, Op::Brief(NoParams), &now).unwrap().text;
    let text = system_text(&st, &now);
    assert_eq!(text.brief, brief);
    assert_eq!(
        text.claude_md,
        format!("{constitution}\n\n# This brain: default\n\nLantern lives in ~/private/lantern.")
    );
    assert_eq!(text.full, format!("{}\n\n{brief}", text.claude_md));
    assert_eq!(text.warnings, vec![]);

    // `Deps::default` reads no file.
    let st = state(&b, &toml("~/.claude-acct-a", ""), deps(&b));
    let text = system_text(&st, &now);
    assert_eq!(
        text.claude_md,
        format!("{constitution}\n\n# This brain: default\n\n(no scope file)")
    );
    assert_eq!(text.full, format!("{}\n\n{brief}", text.claude_md));
    // A missing file says nothing.
    assert_eq!(text.warnings, vec![]);
}

/// A file that is there but can't be read is left out as a missing one is, and the log says
/// which and why, so an override is never ignored without a word.
#[test]
fn an_unreadable_file_falls_back_and_says_so() {
    let b = TypicalBrain::new();
    let deps = Deps {
        read_text: unreadable,
        constitution_file: b.home.join(".config/moa/constitution.md"),
        ..deps(&b)
    };
    let st = state(&b, &toml("~/.claude-acct-a", ""), deps);
    let text = system_text(&st, &b.now());
    assert_eq!(
        text.claude_md,
        format!(
            "{}\n\n# This brain: default\n\n(no scope file)",
            CONSTITUTION.trim_end()
        )
    );
    let warn = |message: &str| LogLine {
        level: LogLevel::Warn,
        message: message.into(),
    };
    assert_eq!(
        text.warnings,
        vec![
            warn(
                "~/.config/moa/constitution.md can't be read (permission denied); \
                 moa's agent is told the built-in constitution"
            ),
            warn(
                "/tmp/moa-test-config/default.md can't be read (permission denied); \
                 moa's agent is told this brain has no scope file"
            ),
        ]
    );
}

#[test]
fn a_constitution_file_replaces_the_embedded_one() {
    let b = TypicalBrain::new();
    let deps = Deps {
        read_text: both,
        ..deps(&b)
    };
    let st = state(&b, &toml("~/.claude-acct-a", ""), deps);
    let text = system_text(&st, &b.now());
    assert_eq!(
        text.claude_md,
        "You are moa, and brief.\n\n# This brain: default\n\nLantern lives in ~/private/lantern."
    );
    assert_eq!(text.warnings, vec![]);
}

#[test]
fn the_embedded_constitution_is_short_and_anonymous() {
    assert!(CONSTITUTION.len() <= 10_240, "{} bytes", CONSTITUTION.len());
    for word in ["/Users/", "/home/", "@", ".claude-"] {
        assert!(!CONSTITUTION.contains(word), "it holds {word}");
    }
    assert!(CONSTITUTION.starts_with("# You are moa"));
}

/// `note` saves any kind it's given, so moa is told the ones the brief reads back, and the
/// one it writes at wrap-up.
#[test]
fn the_constitution_names_the_note_kinds() {
    for kind in [
        NoteKind::FACT,
        NoteKind::PREFERENCE,
        NoteKind::ROUTINE,
        NoteKind::OBSERVATION,
        NoteKind::THREAD_SUMMARY,
    ] {
        assert!(CONSTITUTION.contains(&format!("`{kind}`")), "{kind}");
    }
}
