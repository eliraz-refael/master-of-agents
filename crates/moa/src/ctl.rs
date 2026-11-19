//! `moactl <verb>` (also `moa <verb>`), `moa status` and `moa shutdown`: the command line's
//! side of a brain's daemon. Every failure is one plain line on stderr and exit 1.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
use moa_client::{Client, ClientError, ClientOptions, OnMismatch, own_exe};
use moa_core::config::{BrainName, Config, caller_store};
use moa_core::live::DisplayState;
use moa_core::op::Op;
use moa_core::publish::word;
use moa_core::route::{Route, route};
use moa_core::state::CallerInfo;
use moa_daemon::{DaemonError, load_config};
use moa_platform::{Paths, build_id, canonical};
use moa_proto::cli::{Moactl, VerbCtx, VerbError};

#[derive(Args)]
pub struct BrainArgs {
    /// Which brain; the default is the one this folder and account belong to.
    #[arg(long)]
    brain: Option<String>,
}

#[derive(Args)]
pub struct StatusArgs {
    #[command(flatten)]
    brain: BrainArgs,
    /// Print the daemon's whole status as JSON.
    #[arg(long)]
    json: bool,
}

/// Where this command was typed, as the environment and the folder say.
pub(crate) struct Here {
    pub(crate) paths: Paths,
    /// The folder, in its canonical form.
    cwd: PathBuf,
}

impl Here {
    pub(crate) fn new() -> Result<Here, String> {
        let paths = Paths::from_env().map_err(|e| e.to_string())?;
        let cwd = std::env::current_dir().map_err(|_| "can't tell which folder this is")?;
        Ok(Here { paths, cwd })
    }

    pub(crate) fn caller(&self) -> CallerInfo {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        CallerInfo {
            moa_session: var("MOA_SESSION"),
            claude_session: var("CLAUDE_CODE_SESSION_ID"),
            cwd: Some(canonical(&self.cwd)),
        }
    }

    /// The config, for commands that only read: not gated by `MOA_ROOT`.
    pub(crate) fn read_config(&self) -> Result<Config, String> {
        load_config(&self.paths).map_err(|e| e.to_string())
    }

    pub(crate) fn config(&self) -> Result<Config, String> {
        self.paths.check_v1_allowed().map_err(|e| e.to_string())?;
        load_config(&self.paths).map_err(|e| e.to_string())
    }

    pub(crate) fn options(&self, brain: BrainName) -> ClientOptions {
        ClientOptions {
            paths: self.paths.clone(),
            brain,
            client: "moactl",
            caller: self.caller(),
            autostart: None,
            on_mismatch: OnMismatch::Accept,
            build: build_id().into(),
        }
    }
}

/// A brain name from `source` (`MOA_BRAIN`, `--brain`), worded for where it came from.
fn named(source: &str, value: &str) -> Result<BrainName, String> {
    BrainName::parse(value).map_err(|e| {
        let line = e.to_string();
        format!(
            "{source}: {}",
            line.strip_prefix("moa.toml: ").unwrap_or(&line)
        )
    })
}

/// `~/x` for a path under `home`, else the path.
fn with_tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// The brain a command goes to: `flag` (`--brain`), else `MOA_BRAIN`, else where this folder
/// and account route to. `writes` is whether the command changes the brain.
pub(crate) fn pick_brain(
    here: &Here,
    config: &Config,
    flag: Option<&str>,
    writes: bool,
) -> Result<BrainName, String> {
    pick_brain_for(here, config, flag, writes, &canonical(&here.cwd))
}

/// [`pick_brain`], routing by `folder` (canonical) instead of the one the command was typed
/// in.
fn pick_brain_for(
    here: &Here,
    config: &Config,
    flag: Option<&str>,
    writes: bool,
    folder: &Path,
) -> Result<BrainName, String> {
    let env = std::env::var("MOA_BRAIN").ok().filter(|v| !v.is_empty());
    let brain = match (flag, env) {
        (Some(v), _) => named("--brain", v)?,
        (None, Some(v)) => named("MOA_BRAIN", &v)?,
        (None, None) => {
            let home = here.paths.home();
            let claude_dir = std::env::var_os("CLAUDE_CONFIG_DIR");
            let store = caller_store(claude_dir.as_deref(), home);
            let account = config.account_for_store(&canonical(&store));
            if writes && config.refuses_unlisted(account) {
                return Err(format!(
                    "{} isn't an account in moa.toml, so moa won't save anything from it; \
                     add it under [accounts] or set MOA_BRAIN",
                    with_tilde(&store, home)
                ));
            }
            match route(config, account, folder, None) {
                Route::Brain(b) => b,
                Route::NotAvailable => return Err("moa isn't available here".into()),
            }
        }
    };
    if config.brain(&brain).is_none() {
        return Err(DaemonError::NoSuchBrain(brain, here.paths.config_file()).to_string());
    }
    Ok(brain)
}

pub(crate) fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("can't start: {e}"))
}

fn finish(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(line) => {
            eprintln!("{line}");
            ExitCode::from(1)
        }
    }
}

/// `moactl <verb>`.
pub fn run(m: Moactl) -> ExitCode {
    finish(verb(&m))
}

fn verb(m: &Moactl) -> Result<(), String> {
    let here = Here::new()?;
    let ctx = VerbCtx {
        cwd: Some(here.cwd.clone()),
        home: here.paths.home().to_path_buf(),
    };
    let mut op = m.verb.to_op(&ctx).map_err(|e: VerbError| e.to_string())?;
    // A terminal opens in a folder that is there, named as the system spells it.
    if let Op::TermOpen { path, .. } = &mut op {
        if !path.is_dir() {
            return Err(format!("no folder {}", path.display()));
        }
        *path = canonical(path);
    }
    let config = here.config()?;
    // `open` goes where the folder it opens belongs, unless MOA_BRAIN says otherwise.
    let brain = match &mut op {
        Op::SessionOpen { path, .. } => {
            // Routed before the folder is made: a refused `open` leaves nothing behind.
            let brain = pick_brain_for(&here, &config, None, true, &canonical(path))?;
            *path = open_folder(path, m.json)?;
            brain
        }
        op => pick_brain(&here, &config, None, op.writes())?,
    };
    let exe = own_exe().map_err(|e| format!("can't find this program: {e}"))?;
    let mut opts = here.options(brain);
    opts.autostart = Some(exe);
    opts.on_mismatch = OnMismatch::Restart;
    runtime()?.block_on(async {
        let mut client = Client::connect(opts).await.map_err(|e| e.to_string())?;
        let done = client.call(op).await.map_err(|e| e.to_string())?;
        if m.json {
            println!("{}", done.data);
        } else {
            println!("{}", done.text);
        }
        Ok(())
    })
}

/// `open`'s folder: made when it's missing, as the spike did, then in its canonical form.
/// With `--json`, stdout holds only the reply's JSON, so the note goes to stderr.
fn open_folder(path: &Path, json: bool) -> Result<PathBuf, String> {
    if !path.is_dir() {
        std::fs::create_dir_all(path).map_err(|e| format!("can't make {}: {e}", path.display()))?;
        let note = format!("created {}", path.display());
        if json {
            eprintln!("{note}");
        } else {
            println!("{note}");
        }
    }
    Ok(canonical(path))
}

/// `moa status`. The terminal server's version and pid are in `--json` only.
pub fn status(args: &StatusArgs) -> ExitCode {
    let json = args.json;
    finish(running(&args.brain, false, |brain, client| async move {
        let mut client = client;
        let s = client.status().await.map_err(|e| e.to_string())?;
        if json {
            let text = serde_json::to_string(&s).map_err(|e| e.to_string())?;
            println!("{text}");
            return Ok(());
        }
        println!(
            "moa {brain}: running, build {}, pid {}, {} launches",
            s.build, s.pid, s.launches
        );
        println!("you are: {}", s.caller);
        let live: Vec<String> = DisplayState::ALL
            .iter()
            .filter_map(|d| {
                let w = word(*d);
                let n = s.sessions.get(&w).copied().unwrap_or(0);
                (n > 0).then(|| format!("{w} {n}"))
            })
            .collect();
        if live.is_empty() {
            println!("sessions: none");
        } else {
            println!("sessions: {}", live.join(", "));
        }
        let terminal = if s.tmux.is_some() {
            "connected"
        } else {
            "not connected"
        };
        println!("terminal: {terminal}");
        Ok(())
    }))
}

/// `moa shutdown`.
pub fn shutdown(args: &BrainArgs) -> ExitCode {
    finish(running(args, true, |brain, client| async move {
        let mut client = client;
        client.shutdown().await.map_err(|e| e.to_string())?;
        println!("moa {brain}: stopped");
        Ok(())
    }))
}

/// Connects to the brain's daemon without starting one and runs `then` on it. Both callers
/// are allowed on a mismatched build. When nothing is running that is an error, unless
/// `fine_if_stopped` (`shutdown`: it is already what was asked), which just says so.
fn running<F, Fut>(args: &BrainArgs, fine_if_stopped: bool, then: F) -> Result<(), String>
where
    F: FnOnce(BrainName, Client) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let here = Here::new()?;
    let config = here.config()?;
    let brain = pick_brain(&here, &config, args.brain.as_deref(), false)?;
    let opts = here.options(brain.clone());
    runtime()?.block_on(async {
        match Client::connect(opts).await {
            Ok(client) => then(brain, client).await,
            Err(e @ ClientError::NotRunning(_)) if fine_if_stopped => {
                println!("{e}");
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    })
}
