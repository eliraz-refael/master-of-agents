//! `moa adopt-spike` and `moa export-spike`: the offline tools that move a brain between the
//! spike's schema and v1's. Every message names files by file name only and never quotes a
//! record.

use std::path::{Path, PathBuf};

use clap::Args;
use jiff::tz::TimeZone;
use moa_agents::claude::find_transcript;
use moa_core::UtcMs;
use moa_store::accounts::AccountMap;
use moa_store::export::{ExportEnv, ExportError, export_spike};
use moa_store::import::{IdSource, ImportEnv, ImportError, import_spike, recorded_zone};
use moa_store::{Store, StoreError, file_name_of, is_sidecar_name, sidecars};

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

/// The zone to use, and the text the user gave for it (`None` for the system zone).
fn zone(tz: Option<&str>) -> Result<(TimeZone, Option<String>), String> {
    match tz {
        Some(s) => TimeZone::get(s)
            .or_else(|_| TimeZone::posix(s))
            .map(|z| (z, Some(s.to_string())))
            .map_err(|_| {
                "that isn't a time zone I know; pass --tz with a name like Europe/Paris".to_string()
            }),
        None => TimeZone::try_system()
            .map(|z| (z, None))
            .map_err(|_| "can't tell your time zone; pass --tz".to_string()),
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

/// Refuses a target named like one of SQLite's own files.
fn refuse_sidecar_name(into: &Path) -> Result<(), String> {
    if is_sidecar_name(into) {
        return Err(format!(
            "{} is a name SQLite keeps for its own files; pick another",
            file_name_of(into)
        ));
    }
    Ok(())
}

/// Refuses a target that exists, has a leftover SQLite file beside it, or is named like one.
fn refuse_existing(into: &Path) -> Result<(), String> {
    refuse_sidecar_name(into)?;
    for p in std::iter::once(into.to_path_buf()).chain(sidecars(into)) {
        if p.symlink_metadata().is_ok() {
            return Err(format!(
                "{} already exists; moa never overwrites a brain",
                file_name_of(&p)
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
        StoreError::Missing(p) => format!("no brain at {}", file_name_of(&p)),
        other => other.to_string(),
    }
}

fn import_err(e: ImportError) -> String {
    match e {
        ImportError::SpikeMissing(p) => format!("no spike brain at {}", file_name_of(&p)),
        ImportError::Store(e) => store_err(e),
        other => other.to_string(),
    }
}

fn export_err(e: ExportError) -> String {
    match e {
        ExportError::Exists(p) => format!(
            "{} already exists; moa never overwrites a brain",
            file_name_of(&p)
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
    let (tz, tz_spec) = zone(args.tz.as_deref())?;
    let accounts = accounts(&args.accounts)?;
    if !args.from.is_file() {
        return Err(format!("no spike brain at {}", file_name_of(&args.from)));
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
        tz_spec,
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
                file_name_of(into)
            ),
            std::io::ErrorKind::NotFound => {
                format!("the folder for {} doesn't exist", file_name_of(into))
            }
            _ => "a file could not be created".to_string(),
        })
}

/// Removes a brain this run just created, with the files SQLite keeps beside it.
fn remove_brain(db: &Path) {
    for p in std::iter::once(db.to_path_buf()).chain(sidecars(db)) {
        let _ = std::fs::remove_file(p);
    }
}

pub fn export(args: &ExportArgs) -> Result<(), String> {
    let given = args.tz.as_deref().map(|s| zone(Some(s))).transpose()?;
    let accounts = accounts(&args.accounts)?;
    if !args.from.is_file() {
        return Err(format!("no v1 brain at {}", file_name_of(&args.from)));
    }
    refuse_same_file(&args.from, &args.into)?;
    // An existing target is `export_spike`'s to refuse (`ExportError::Exists`).
    refuse_sidecar_name(&args.into)?;
    let store = Store::open_read_only(&args.from).map_err(|e| match e {
        StoreError::NotABrain => {
            format!("{} isn't a moa brain", file_name_of(&args.from))
        }
        other => store_err(other),
    })?;
    // The spike's times are read in the zone the import used; only a missing record falls
    // back to this machine's zone.
    let recorded = recorded_zone(&store);
    let tz = match given {
        Some((tz, spec)) => {
            let label = tz.iana_name().map(str::to_owned).or(spec);
            if recorded.is_some_and(|r| Some(r) != label) {
                eprintln!("note: exporting with a different time zone than the import used");
            }
            tz
        }
        None => match recorded.as_deref().map(|r| zone(Some(r))) {
            Some(Ok((tz, _))) => tz,
            Some(Err(_)) => {
                eprintln!(
                    "note: the time zone the import used isn't known here; using this machine's"
                );
                zone(None)?.0
            }
            None => zone(None)?.0,
        },
    };
    let env = ExportEnv {
        tz,
        accounts,
        setup_skipped: (!args.setup_skipped.is_empty()).then(|| args.setup_skipped.clone()),
    };
    let report = export_spike(&store, &args.into, &env).map_err(export_err)?;
    print!("{}", report.render_text());
    Ok(())
}
