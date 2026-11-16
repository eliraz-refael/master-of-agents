//! A typical brain for tests: the spike fixture's Typical scenario, imported into a temp dir,
//! with the core's state loaded from it. Nothing here reads a real brain or the real home.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use moa_agents::claude::find_transcript;
use moa_core::config::{BrainConfig, BrainName, Config};
use moa_core::effect::{handle_checked, reply_of};
use moa_core::op::{Done, Op, OpError};
use moa_core::state::Deps;
use moa_core::{Input, Now, State, UtcMs};
use moa_store::accounts::AccountMap;
use moa_store::import::{IdSource, ImportEnv, import_spike};
use moa_store::{Snapshot, Store};
use tempfile::TempDir;

use crate::{Scenario, SpikeFixture};

pub const TYPICAL_TZ: &str = "EST5EDT,M3.2.0,M11.1.0";
/// Sunday 12:00 in [`TYPICAL_TZ`].
pub const TYPICAL_NOW: &str = "2026-11-15T17:00:00Z";

pub struct TypicalBrain {
    pub dir: TempDir,
    pub home: PathBuf,
    pub db: PathBuf,
}

fn tz() -> TimeZone {
    TimeZone::posix(TYPICAL_TZ).expect("TYPICAL_TZ is a valid POSIX zone")
}

fn typical_now_ms() -> UtcMs {
    let at: Timestamp = TYPICAL_NOW.parse().expect("TYPICAL_NOW is a valid instant");
    UtcMs(at.as_millisecond())
}

impl TypicalBrain {
    /// The Typical spike fixture in `<dir>/home`, imported with derived ids into brain
    /// "default".
    pub fn new() -> TypicalBrain {
        let dir = tempfile::Builder::new()
            .prefix("moa")
            .tempdir_in("/tmp")
            .expect("a temp dir");
        let home = dir.path().join("home");
        fs::create_dir(&home).expect("home");
        let spike = dir.path().join("spike.db");
        SpikeFixture::write(&spike, Scenario::Typical, &home).expect("the fixture writes");
        let db = dir.path().join("moa.db");
        let mut store = Store::open(&db, &dir.path().join("backups")).expect("a new store");
        let env = ImportEnv {
            tz: tz(),
            tz_spec: Some(TYPICAL_TZ.into()),
            now: typical_now_ms(),
            brain: "default".into(),
            accounts: AccountMap::new(home.clone()),
            ids: IdSource::Derived,
            transcript: find_transcript,
        };
        import_spike(&spike, &mut store, &env).expect("the fixture imports");
        TypicalBrain { dir, home, db }
    }

    pub fn store(&self) -> Store {
        Store::open(&self.db, &self.dir.path().join("backups")).expect("the brain opens")
    }

    pub fn snapshot(&self) -> Snapshot {
        self.store().load_snapshot().expect("the snapshot loads")
    }

    /// `brains.default.folders = [<home>/private]`.
    pub fn config(&self) -> Config {
        let mut config = Config::default();
        config.brains.insert(
            BrainName::default_brain(),
            BrainConfig {
                folders: vec![self.home.join("private")],
                ..BrainConfig::default()
            },
        );
        config
    }

    pub fn now(&self) -> Now {
        Now::at(typical_now_ms(), Duration::ZERO, tz())
    }

    /// The core's state with this brain loaded.
    pub fn state(&self) -> State {
        self.state_with(Deps::default())
    }

    /// [`TypicalBrain::state`] with `deps`. The config's folders are canonicalised with
    /// `deps.canonical` before `Startup`, as the daemon's config loader does.
    pub fn state_with(&self, deps: Deps) -> State {
        let mut config = self.config();
        config.canonicalize_with(deps.canonical);
        let mut state = State::new(BrainName::default_brain(), config).with_deps(deps);
        let _ = handle_checked(
            &mut state,
            Input::Startup(Box::new(self.snapshot())),
            self.now(),
        );
        state
    }
}

impl Default for TypicalBrain {
    fn default() -> Self {
        Self::new()
    }
}

/// Runs one op against the state and returns its reply.
pub fn run(state: &mut State, op: Op, now: &Now) -> Result<Done, OpError> {
    use moa_core::op::{ConnId, ReqId};
    use moa_core::state::CallerInfo;
    let input = Input::Client {
        conn: ConnId(1),
        req: ReqId(1),
        caller: CallerInfo::default(),
        op,
    };
    let effects = handle_checked(state, input, now.clone());
    reply_of(&effects)
        .cloned()
        .expect("an op always gets a reply")
}
