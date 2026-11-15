//! `moa adopt-spike` and `moa export-spike`: the offline tools that move a brain between the
//! spike's schema and v1's. Every message names files by file name only and never quotes a
//! record.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use clap::Args;
use jiff::tz::TimeZone;
use moa_agents::claude::find_transcript;
use moa_core::UtcMs;
use moa_store::accounts::AccountMap;
use moa_store::export::{ExportEnv, ExportError, export_spike};
use moa_store::import::{IdSource, ImportEnv, ImportError, import_spike};
use moa_store::{Store, StoreError};

#[derive(Args)]
pub struct AdoptArgs {
    /// The spike's brain.db
    #[arg(long)]
    from: PathBuf,
    /// Where to write the v1 brain. Without it, nothing is written.
    #[arg(long)]
    into: Option<PathBuf>,
    /// Names the brain; its session ids depend on it
    #[arg(long, default_value = "default")]
    brain: String,
    /// An agent account the spike used, NAME=STORE (repeatable)
    #[arg(long = "account", value_name = "NAME=STORE")]
    accounts: Vec<String>,
    /// Your time zone (IANA name or POSIX string); the default is this machine's
    #[arg(long)]
    tz: Option<String>,
    /// Show account names (for your own terminal only)
    #[arg(long)]
    names: bool,
    /// Print the report as JSON
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
pub struct ExportArgs {
    /// The v1 brain
    #[arg(long)]
    from: PathBuf,
    /// Where to write the spike's brain.db
    #[arg(long)]
    into: PathBuf,
    /// An agent account, NAME=STORE (repeatable)
    #[arg(long = "account", value_name = "NAME=STORE")]
    accounts: Vec<String>,
    /// Your time zone (IANA name or POSIX string); the default is this machine's
    #[arg(long)]
    tz: Option<String>,
    /// An account the spike's setup skipped (repeatable); the default comes from the import
    #[arg(long = "setup-skipped", value_name = "NAME")]
    setup_skipped: Vec<String>,
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .unwrap_or_else(|| OsStr::new("?"))
        .to_string_lossy()
        .into_owned()
}

fn zone(tz: Option<&str>) -> Result<TimeZone, String> {
    match tz {
        Some(s) => TimeZone::get(s)
            .or_else(|_| TimeZone::posix(s))
            .map_err(|_| {
                "that isn't a time zone I know; pass --tz with a name like Europe/Paris".to_string()
            }),
        None => {
            TimeZone::try_system().map_err(|_| "can't tell your time zone; pass --tz".to_string())
        }
    }
}

fn accounts(flags: &[String]) -> Result<AccountMap, String> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("can't find your home folder")?);
    let mut map = AccountMap::new(home.clone());
    for flag in flags {
        let (name, store) =
            AccountMap::parse_flag(flag, &home).map_err(|e| format!("--account: {e}"))?;
        map = map.with(name, store);
    }
    Ok(map)
}

/// Refuses a target that exists, or has a leftover SQLite file beside it.
fn refuse_existing(into: &Path) -> Result<(), String> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut s = into.as_os_str().to_owned();
        s.push(suffix);
        let p = PathBuf::from(s);
        if p.symlink_metadata().is_ok() {
            return Err(format!(
                "{} already exists; moa never overwrites a brain",
                file_name(&p)
            ));
        }
    }
    Ok(())
}

fn refuse_same_file(from: &Path, into: &Path) -> Result<(), String> {
    // `into` doesn't exist yet, so canonicalise its folder.
    let parent = into
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let (Ok(a), Ok(dir)) = (from.canonicalize(), parent.canonicalize()) else {
        return Ok(());
    };
    match into.file_name() {
        Some(n) if dir.join(n) == a => Err("the source and the target are the same file".into()),
        _ => Ok(()),
    }
}

fn store_err(e: StoreError) -> String {
    match e {
        StoreError::Missing(p) => format!("no brain at {}", file_name(&p)),
        other => other.to_string(),
    }
}

fn import_err(e: ImportError) -> String {
    match e {
        ImportError::SpikeMissing(p) => format!("no spike brain at {}", file_name(&p)),
        ImportError::Store(e) => store_err(e),
        other => other.to_string(),
    }
}

fn export_err(e: ExportError) -> String {
    match e {
        ExportError::Exists(p) => format!(
            "{} already exists; moa never overwrites a brain",
            file_name(&p)
        ),
        ExportError::Store(e) => store_err(e),
        other => other.to_string(),
    }
}

fn backups_for(db: &Path) -> PathBuf {
    db.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .join("backups")
}

pub fn adopt(args: &AdoptArgs) -> Result<(), String> {
    let tz = zone(args.tz.as_deref())?;
    let accounts = accounts(&args.accounts)?;
    if !args.from.is_file() {
        return Err(format!("no spike brain at {}", file_name(&args.from)));
    }
    if let Some(into) = &args.into {
        refuse_same_file(&args.from, into)?;
        refuse_existing(into)?;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let env = ImportEnv {
        tz,
        now: UtcMs(now),
        brain: args.brain.clone(),
        accounts,
        ids: IdSource::Derived,
        transcript: find_transcript,
    };

    // A dry run imports into a folder that is deleted when this function returns.
    let scratch;
    let db = match &args.into {
        Some(into) => into.clone(),
        None => {
            scratch = tempfile::tempdir().map_err(|e| format!("a file could not be used: {e}"))?;
            scratch.path().join("moa.db")
        }
    };
    // Claim the target exclusively, then undo everything if anything after that fails.
    if let Some(into) = &args.into {
        claim(into)?;
    }
    let imported = Store::open(&db, &backups_for(&db))
        .map_err(store_err)
        .and_then(|mut store| import_spike(&args.from, &mut store, &env).map_err(import_err));
    let report = match imported {
        Ok(r) => r,
        Err(e) => {
            if args.into.is_some() {
                remove_brain(&db);
            }
            return Err(e);
        }
    };

    if args.json {
        println!("{}", report.to_json(args.names));
    } else {
        print!("{}", report.render_text(args.names));
    }
    if args.into.is_none() {
        eprintln!("dry run: nothing written");
    }
    Ok(())
}

/// Creates the target file, failing if anything is already there.
fn claim(into: &Path) -> Result<(), String> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(into)
        .map(drop)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => format!(
                "{} already exists; moa never overwrites a brain",
                file_name(into)
            ),
            _ => "a file could not be created".to_string(),
        })
}

/// Removes a brain this run just created, with the files SQLite keeps beside it.
fn remove_brain(db: &Path) {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut s = db.as_os_str().to_owned();
        s.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(s));
    }
}

pub fn export(args: &ExportArgs) -> Result<(), String> {
    let tz = zone(args.tz.as_deref())?;
    let accounts = accounts(&args.accounts)?;
    if !args.from.is_file() {
        return Err(format!("no v1 brain at {}", file_name(&args.from)));
    }
    refuse_same_file(&args.from, &args.into)?;
    refuse_existing(&args.into)?;
    let store = Store::open_read_only(&args.from).map_err(store_err)?;
    let env = ExportEnv {
        tz,
        accounts,
        setup_skipped: (!args.setup_skipped.is_empty()).then(|| args.setup_skipped.clone()),
    };
    let report = export_spike(&store, &args.into, &env).map_err(export_err)?;
    print!("{}", report.render_text());
    Ok(())
}
