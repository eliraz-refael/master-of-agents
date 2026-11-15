//! Local only: imports a copy of a real spike brain and checks the round trip. It runs only
//! when `MOA_LOCAL_BRAIN_COPY` is set; `MOA_LOCAL_BRAIN_TZ` and `MOA_LOCAL_BRAIN_ACCOUNTS`
//! (`NAME=STORE,...`) say how to read it. It prints counts and reasons, never contents.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use jiff::tz::TimeZone;
use moa_agents::claude::find_transcript;
use moa_core::UtcMs;
use moa_store::Store;
use moa_store::accounts::AccountMap;
use moa_store::export::{ExportEnv, export_spike};
use moa_store::import::{IdSource, ImportEnv, Level, import_spike};

use common::TimeMode;

fn zone(s: &str) -> TimeZone {
    TimeZone::get(s)
        .or_else(|_| TimeZone::posix(s))
        .expect("MOA_LOCAL_BRAIN_TZ is not a time zone")
}

fn accounts(home: &std::path::Path) -> AccountMap {
    let mut map = AccountMap::new(home.to_path_buf());
    let pairs = std::env::var("MOA_LOCAL_BRAIN_ACCOUNTS").unwrap_or_default();
    for pair in pairs.split(',').filter(|p| !p.is_empty()) {
        let (name, store) =
            AccountMap::parse_flag(pair, home).expect("bad MOA_LOCAL_BRAIN_ACCOUNTS");
        map = map.with(name, store);
    }
    map
}

#[test]
fn local_brain_imports_and_round_trips() {
    let Some(copy) = std::env::var_os("MOA_LOCAL_BRAIN_COPY").map(PathBuf::from) else {
        return;
    };
    let tz = zone(&std::env::var("MOA_LOCAL_BRAIN_TZ").expect("set MOA_LOCAL_BRAIN_TZ"));
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("moa.db"), &dir.path().join("backups")).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let env = ImportEnv {
        tz: tz.clone(),
        now: UtcMs(now),
        brain: "default".into(),
        accounts: accounts(&home),
        ids: IdSource::Derived,
        transcript: find_transcript,
    };
    let report = import_spike(&copy, &mut store, &env).expect("the import failed");
    for t in &report.tables {
        println!(
            "{:<14} {} of {} imported, {} with warnings",
            t.table, t.imported, t.source, t.warned
        );
    }
    let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
    for n in report.notes.iter().filter(|n| n.level == Level::Warn) {
        *reasons.entry(n.reason).or_default() += 1;
    }
    for (reason, count) in &reasons {
        println!("warn x{count}: {reason}");
    }

    let out = dir.path().join("out.db");
    let map = accounts(&home);
    let xenv = ExportEnv {
        tz: tz.clone(),
        accounts: accounts(&home),
        setup_skipped: None,
    };
    export_spike(&store, &out, &xenv).expect("the export failed");
    let excluded = common::assert_round_trip_with(
        &copy,
        &out,
        &report.notes,
        &TimeMode::Instant(tz),
        Some(&map),
    );
    println!("round trip ok; {excluded} records left out by the import's own rules");
}
