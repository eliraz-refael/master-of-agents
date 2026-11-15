//! Random spike brains for property tests.
//!
//! `brain_valid` makes well-formed data that should survive an import and an export.
//! `brain_any` also makes the kinds of mess real brains hold. Every free-text, name, path,
//! group and status string is a unique 16-character token, so a check that a report holds no
//! record contents can search for all of them.

use proptest::prelude::*;
use rusqlite::types::Value;

pub use crate::spike_fixture::SpikeBrain;
use crate::spike_fixture::{Row, ids};

const PROJECT_STATUS: [&str; 4] = ["idea", "active", "paused", "done"];
const TASK_STATUS: [&str; 7] = [
    "idea", "todo", "active", "waiting", "blocked", "done", "dropped",
];
const NOTE_KIND: [&str; 5] = [
    "fact",
    "preference",
    "routine",
    "observation",
    "thread-summary",
];
const SUGGESTION_STATUS: [&str; 4] = ["open", "accepted", "rejected", "later"];
const SESSION_STATE: [&str; 6] = [
    "working",
    "idle",
    "waiting",
    "ended",
    "archived",
    "elsewhere",
];
const EVENT_KIND: [&str; 5] = ["start", "prompt", "stop", "waiting", "end"];
const ASK_STATUS: [&str; 6] = [
    "pending", "sent", "received", "answered", "failed", "dropped",
];
const WHO: [&str; 3] = ["you", "agent", "other"];
const PRIORITY: [&str; 3] = ["p0", "p1", "p2"];
/// Stand-ins for the hex letters a-f in "wild" tokens. The mapping is one to one, so wild
/// tokens stay unique and stay 16 characters.
const WILD: [char; 6] = ['日', '\n', '🦀', 'é', ' ', '"'];

/// Everything the generator needs, as plain data so that proptest can shrink it.
#[derive(Clone, Debug)]
struct Shape {
    seed: u32,
    wild: bool,
    old: bool,
    sizes: [usize; 8],
    bytes: Vec<u8>,
}

fn shape(messy: bool) -> impl Strategy<Value = Shape> {
    (
        any::<u32>(),
        any::<bool>(),
        any::<bool>(),
        [0..4usize, 0..6, 0..4, 0..4, 0..5, 0..14, 0..4, 0..4],
        proptest::collection::vec(any::<u8>(), 64..256),
    )
        .prop_map(move |(seed, wild, old, sizes, bytes)| Shape {
            seed,
            wild: messy && wild,
            old: messy && old,
            sizes,
            bytes,
        })
}

struct Gen {
    shape: Shape,
    any: bool,
    pos: usize,
    count: u32,
    tokens: Vec<String>,
}

impl Gen {
    fn byte(&mut self) -> u8 {
        let b = self.shape.bytes[self.pos % self.shape.bytes.len()];
        self.pos += 1;
        b
    }

    fn below(&mut self, n: usize) -> usize {
        usize::from(self.byte()) % n
    }

    fn chance(&mut self, percent: u8) -> bool {
        self.byte() % 100 < percent
    }

    /// A messy choice, only when generating any brain.
    fn messy(&mut self, percent: u8) -> bool {
        let roll = self.chance(percent);
        self.any && roll
    }

    fn raw_token(&mut self) -> String {
        self.count += 1;
        let hex = format!("{:08x}{:08x}", self.shape.seed, self.count);
        if self.shape.wild {
            hex.chars()
                .map(|c| match c {
                    'a'..='f' => WILD[usize::from(c as u8 - b'a')],
                    other => other,
                })
                .collect()
        } else {
            hex
        }
    }

    fn tok(&mut self) -> String {
        let t = self.raw_token();
        self.tokens.push(t.clone());
        t
    }

    fn text(&mut self) -> Value {
        Value::Text(self.tok())
    }

    fn opt_text(&mut self) -> Value {
        if self.chance(40) {
            Value::Null
        } else {
            self.text()
        }
    }

    fn pick(&mut self, xs: &[&str]) -> Value {
        if self.messy(15) {
            return self.text();
        }
        let i = self.below(xs.len());
        Value::Text(xs[i].to_string())
    }

    fn opt_pick(&mut self, xs: &[&str]) -> Value {
        if self.chance(35) {
            Value::Null
        } else {
            self.pick(xs)
        }
    }

    /// A local time that is not near a daylight-saving change.
    fn time(&mut self) -> Value {
        if self.messy(15) {
            return Value::Text(["garbage", "", "2026-13-45 99:00:00"][self.below(3)].to_string());
        }
        let (mo, d, h, mi, sec) = (
            1 + self.below(6),
            1 + self.below(28),
            3 + self.below(19),
            self.below(60),
            self.below(60),
        );
        Value::Text(format!("2026-{mo:02}-{d:02} {h:02}:{mi:02}:{sec:02}"))
    }

    fn opt_time(&mut self) -> Value {
        if self.chance(30) {
            Value::Null
        } else {
            self.time()
        }
    }

    fn date(&mut self) -> Value {
        if self.messy(15) {
            return Value::Text(
                ["garbage", "", "next week", "2026-02-31"][self.below(4)].to_string(),
            );
        }
        Value::Text(format!(
            "2026-{:02}-{:02}",
            1 + self.below(12),
            1 + self.below(28)
        ))
    }

    fn opt_date(&mut self) -> Value {
        if self.chance(40) {
            Value::Null
        } else {
            self.date()
        }
    }

    /// An existing id, or none, or (when messy) one that is not there.
    fn reference(&mut self, ids: &[i64]) -> Value {
        if self.messy(10) {
            return Value::Integer(9999);
        }
        if ids.is_empty() || self.chance(30) {
            return Value::Null;
        }
        Value::Integer(ids[self.below(ids.len())])
    }

    fn session_ref(&mut self, sids: &[String]) -> Value {
        if self.messy(10) {
            return Value::Text(ids::AWK_GHOST.to_string());
        }
        if sids.is_empty() || self.chance(30) {
            return Value::Null;
        }
        Value::Text(sids[self.below(sids.len())].clone())
    }
}

fn int_ids(n: usize) -> Vec<i64> {
    (1..=n as i64).collect()
}

fn build(shape: Shape, any: bool) -> SpikeBrain {
    let mut g = Gen {
        shape: shape.clone(),
        any,
        pos: 0,
        count: 0,
        tokens: Vec::new(),
    };
    let [
        n_proj,
        n_task,
        n_note,
        n_sugg,
        n_sess,
        n_event,
        n_group,
        n_ask,
    ] = shape.sizes;
    let mut rows: Vec<(&'static str, Row)> = Vec::new();

    for id in 1..=n_proj as i64 {
        let area = if g.messy(20) {
            g.text()
        } else {
            Value::Text("private".into())
        };
        let path = if g.chance(30) {
            Value::Null
        } else {
            Value::Text(format!("{{home}}/private/{}", g.tok()))
        };
        rows.push((
            "projects",
            vec![
                ("id", Value::Integer(id)),
                ("name", g.text()),
                ("path", path),
                ("area", area),
                ("status", g.pick(&PROJECT_STATUS)),
                ("summary", g.opt_text()),
                ("last_activity", g.opt_date()),
                ("created_at", g.time()),
                ("updated_at", g.time()),
            ],
        ));
    }
    let project_ids = int_ids(n_proj);

    // Sessions come before tasks and events, which refer to them.
    let mut sids: Vec<String> = Vec::new();
    let mut transcripts = Vec::new();
    let mut groups: Vec<String> = Vec::new();
    for _ in 0..n_group {
        let t = g.tok();
        let path = match (g.messy(30), g.below(3)) {
            (true, 0) => format!("{t}/"),
            (true, 1) => format!("{t}//{}", g.tok()),
            _ => t,
        };
        groups.push(path.clone());
        rows.push((
            "groups",
            vec![("path", Value::Text(path)), ("created_at", g.opt_time())],
        ));
    }
    for i in 0..n_sess {
        let prefix = (i as u32 + 1).wrapping_mul(0x0100_0193) ^ shape.seed;
        let mut sid = format!("{prefix:08x}-0000-4000-8000-{:012x}", i + 1);
        if g.messy(8) && !sids.iter().any(|s| s == ids::AWK_PATH) {
            sid = ids::AWK_PATH.to_string();
        }
        let (account, store) = match g.below(if any { 5 } else { 3 }) {
            0 => (Value::Text("{home}/.acct-a".into()), Some(".acct-a")),
            1 => (Value::Text("{home}/.acct-b".into()), Some(".acct-b")),
            2 => (Value::Text("codex".into()), None),
            3 => (Value::Null, None),
            _ => (g.text(), None),
        };
        if let Some(store) = store
            && g.chance(70)
            && sid != ids::AWK_PATH
        {
            transcripts.push((store.to_string(), sid.clone()));
        }
        let cwd = if g.chance(20) {
            Value::Null
        } else {
            Value::Text(format!("{{home}}/private/{}", g.tok()))
        };
        let grp = if !groups.is_empty() && g.chance(40) {
            Value::Text(groups[g.below(groups.len())].clone())
        } else if g.chance(30) {
            g.text()
        } else {
            Value::Null
        };
        let tmux = if g.chance(50) { Value::Null } else { g.text() };
        rows.push((
            "sessions",
            vec![
                ("session_id", Value::Text(sid.clone())),
                ("account", account),
                ("cwd", cwd),
                ("project_id", g.reference(&project_ids)),
                ("title", g.opt_text()),
                ("state", g.opt_pick(&SESSION_STATE)),
                ("last_prompt", g.opt_text()),
                ("first_seen", g.opt_time()),
                ("last_seen", g.opt_time()),
                (
                    "pane",
                    if g.chance(50) {
                        Value::Null
                    } else {
                        Value::Text(format!("%{}", g.below(40)))
                    },
                ),
                ("tmux", tmux),
                ("grp", grp),
                ("asleep_at", g.opt_time()),
            ],
        ));
        sids.push(sid);
    }

    for id in 1..=n_task as i64 {
        let blocked = g.opt_text();
        let session = g.session_ref(&sids);
        rows.push((
            "tasks",
            vec![
                ("id", Value::Integer(id)),
                ("project_id", g.reference(&project_ids)),
                ("title", g.text()),
                ("status", g.pick(&TASK_STATUS)),
                ("blocked_on", blocked),
                ("today", g.opt_date()),
                ("session_id", session),
                ("note", g.opt_text()),
                ("who", g.opt_pick(&WHO)),
                ("due", g.opt_date()),
                ("priority", g.opt_pick(&PRIORITY)),
                ("created_at", g.time()),
                ("updated_at", g.time()),
            ],
        ));
    }

    for id in 1..=n_note as i64 {
        rows.push((
            "notes",
            vec![
                ("id", Value::Integer(id)),
                ("project_id", g.reference(&project_ids)),
                ("kind", g.pick(&NOTE_KIND)),
                ("text", g.text()),
                ("created_at", g.time()),
            ],
        ));
    }

    for id in 1..=n_sugg as i64 {
        let action = if g.chance(40) {
            Value::Null
        } else if g.messy(20) {
            Value::Text("not json".into())
        } else {
            let arg = format!("note {}", g.tok());
            Value::Text(serde_json::to_string(&[arg]).expect("json"))
        };
        rows.push((
            "suggestions",
            vec![
                ("id", Value::Integer(id)),
                ("text", g.text()),
                ("status", g.pick(&SUGGESTION_STATUS)),
                ("action", action),
                ("created_at", g.time()),
                ("resolved_at", g.opt_time()),
            ],
        ));
    }

    let mut last_event = 0;
    for id in 1..=n_event as i64 {
        let ui = g.chance(20);
        let kind = if ui {
            Value::Text("ui".into())
        } else {
            g.pick(&EVENT_KIND)
        };
        // Valid events carry the cwd and text the activity triggers read.
        let cwd = if ui || g.messy(15) {
            Value::Null
        } else {
            Value::Text(format!("{{home}}/private/{}", g.tok()))
        };
        let text = if g.messy(15) { Value::Null } else { g.text() };
        let sid = if ui {
            Value::Null
        } else if sids.is_empty() {
            Value::Text(ids::AWK_GHOST.to_string())
        } else {
            Value::Text(sids[g.below(sids.len())].clone())
        };
        last_event = id;
        rows.push((
            "events",
            vec![
                ("id", Value::Integer(id)),
                ("ts", g.time()),
                ("session_id", sid),
                ("account", Value::Null),
                ("cwd", cwd),
                ("kind", kind),
                ("text", text),
            ],
        ));
    }

    for id in 1..=n_ask as i64 {
        let from = g.session_ref(&sids);
        let to = if !sids.is_empty() && g.chance(70) {
            Value::Text(sids[g.below(sids.len())].clone())
        } else if g.messy(30) {
            g.text()
        } else {
            Value::Text("moa".into())
        };
        rows.push((
            "asks",
            vec![
                ("id", Value::Integer(id)),
                ("ts", g.time()),
                ("from_sid", from),
                ("from_name", g.opt_text()),
                ("to_sid", to),
                ("text", g.text()),
                ("status", g.pick(&ASK_STATUS)),
                ("reply", g.opt_text()),
                ("answered_at", g.opt_time()),
            ],
        ));
    }

    if last_event > 0 {
        let cursor = if g.messy(15) {
            "garbage".to_string()
        } else {
            last_event.to_string()
        };
        rows.push((
            "meta",
            vec![
                ("key", Value::Text("cursor".into())),
                ("value", Value::Text(cursor)),
            ],
        ));
    }
    if g.chance(60) {
        let thread = if g.messy(20) {
            "{".to_string()
        } else {
            let sid = sids
                .first()
                .cloned()
                .unwrap_or_else(|| ids::AWK_GHOST.to_string());
            format!(
                "{{\"sid\":\"{sid}\",\"date\":\"2026-02-03\",\"account\":\"{{home}}/.acct-b\"}}"
            )
        };
        rows.push((
            "meta",
            vec![
                ("key", Value::Text("console_thread".into())),
                ("value", Value::Text(thread)),
            ],
        ));
    }
    if g.chance(50) {
        rows.push((
            "meta",
            vec![
                ("key", Value::Text("setup_skipped".into())),
                ("value", Value::Text("[\"acct-c\"]".into())),
            ],
        ));
    }

    SpikeBrain {
        old: shape.old,
        rows,
        transcripts,
        tokens: g.tokens,
    }
}

/// Well-formed data that should round-trip through import and export.
pub fn brain_valid() -> impl Strategy<Value = SpikeBrain> {
    shape(false).prop_map(|s| build(s, false))
}

/// Anything a real brain might hold, including data that does not fit v1's types.
pub fn brain_any() -> impl Strategy<Value = SpikeBrain> {
    shape(true).prop_map(|s| build(s, true))
}
