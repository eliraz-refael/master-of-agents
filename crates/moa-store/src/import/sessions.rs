//! Sessions and the tables that describe them: conversations, spike panes and events.
//!
//! A session's state, activity and "why" follow the spike's own reading (see [`live`]). Its
//! events are read once, first, because the session's times and working directory come from them.

use std::collections::HashMap;

use moa_core::UtcMs;
use moa_core::model::{Activity, Lifecycle, Location, TranscriptCheck};
use moa_core::model::{AgentKind, EventKind, SessionId};
use moa_core::time::spike_local_to_utc;
use rusqlite::{Transaction, params};

use crate::accounts::ResolvedAccount;

use super::live::{self, Latest};
use super::report::*;
use super::rules;
use super::{ImportError, Run, SrcRow, conversation_key, id_key};

/// One spike event, with its time parsed quietly (the events importer reports a bad one).
struct Ev {
    id: i64,
    at: Option<UtcMs>,
    session: Option<String>,
    cwd: Option<String>,
    kind: String,
    row: SrcRow,
}

/// What a session's events say about it.
#[derive(Default)]
struct Facts {
    earliest: Option<UtcMs>,
    /// The cwd of the earliest event that has one, with the key that ordered it.
    first_cwd: Option<((UtcMs, i64), String)>,
    /// Index into the events of the newest prompt, stop or waiting event that isn't a ping.
    latest: Option<usize>,
    last_stop: Option<UtcMs>,
}

fn load_events(run: &Run) -> Result<Vec<Ev>, ImportError> {
    let rows = run.src.rows(
        "events",
        &["id", "ts", "session_id", "account", "cwd", "kind", "text"],
        "id",
    )?;
    let tz = &run.env.tz;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            Some(Ev {
                id: row.int("id")?,
                at: row.text("ts").and_then(|t| spike_local_to_utc(&t, tz).ok()),
                session: row.text("session_id"),
                cwd: row.text("cwd").filter(|c| !c.trim().is_empty()),
                kind: row.text("kind").unwrap_or_default(),
                row,
            })
        })
        .collect())
}

fn facts(events: &[Ev]) -> HashMap<&str, Facts> {
    let mut by: HashMap<&str, Facts> = HashMap::new();
    for (i, e) in events.iter().enumerate() {
        let Some(sid) = e.session.as_deref() else {
            continue;
        };
        let f = by.entry(sid).or_default();
        if let Some(at) = e.at {
            f.earliest = Some(f.earliest.map_or(at, |x| x.min(at)));
            if e.kind == "stop" {
                f.last_stop = Some(f.last_stop.map_or(at, |x| x.max(at)));
            }
        }
        if let Some(cwd) = &e.cwd {
            let key = (e.at.unwrap_or(UtcMs(i64::MAX)), e.id);
            if f.first_cwd.as_ref().is_none_or(|(k, _)| key < *k) {
                f.first_cwd = Some((key, cwd.clone()));
            }
        }
        let counts = matches!(e.kind.as_str(), "prompt" | "stop" | "waiting")
            && !(e.kind == "waiting" && live::is_idle_ping(e.row.text_cow("text").as_deref()));
        if counts {
            f.latest = Some(i);
        }
    }
    by
}

/// `session.started` and friends. `waiting` is the idle ping when its text says so.
fn event_kind(kind: &str, text: Option<&str>) -> Option<&'static str> {
    Some(match kind {
        "start" => EventKind::SESSION_STARTED,
        "prompt" => EventKind::TURN_STARTED,
        "stop" => EventKind::TURN_ENDED,
        "waiting" if live::is_idle_ping(text) => EventKind::IDLE_PING,
        "waiting" => EventKind::NEEDS_PERMISSION,
        "end" => EventKind::SESSION_ENDED,
        "ui" => EventKind::UI,
        _ => return None,
    })
}

pub(super) fn import(run: &mut Run, tx: &Transaction) -> Result<(), ImportError> {
    let events = load_events(run)?;
    sessions(run, tx, &events)?;
    self::events(run, tx, &events)
}

fn sessions(run: &mut Run, tx: &Transaction, events: &[Ev]) -> Result<(), ImportError> {
    let rows = run.src.rows(
        "sessions",
        &[
            "rowid",
            "session_id",
            "account",
            "cwd",
            "project_id",
            "title",
            "state",
            "last_prompt",
            "first_seen",
            "last_seen",
            "pane",
            "tmux",
            "grp",
            "asleep_at",
        ],
        "rowid",
    )?;
    let by = facts(events);
    let (now, tz) = (run.env.now, run.env.tz.clone());
    let mut gen_ids = run.env.ids.generator();
    let (mut imported, mut warned) = (0, 0);
    let mut panes = 0;
    let mut conversations = 0;
    let mut conversations_warned = 0;
    let none = Facts::default();

    for r in &rows {
        let Some(conv) = r.text("session_id") else {
            run.warn(
                "sessions",
                &id_key(r.int("rowid").unwrap_or(0)),
                R_SESSION_ID_NULL,
            );
            warned += run.end_record();
            continue;
        };
        let key = conversation_key(&conv);
        run.check_text("sessions", &key, r);
        if run.session_ids.contains_key(&conv) {
            run.warn("sessions", &key, R_SESSION_ID_REPEATED);
            warned += run.end_record();
            continue;
        }
        let f = by.get(conv.as_str()).unwrap_or(&none);
        let latest = f.latest.map(|i| &events[i]);

        // Account: who the session belongs to and which agent it is.
        let resolved = match r.text("account").filter(|a| !a.trim().is_empty()) {
            Some(a) => Some(run.resolve_account(&a)),
            None => {
                run.warn("sessions", &key, R_SESSION_ACCOUNT_NULL);
                None
            }
        };
        let kind = resolved.as_ref().map_or(AgentKind::Claude, |a| a.kind);
        let account = resolved.as_ref().map(|a| a.name.clone());

        // Times. A session with no first_seen starts when its first event did.
        let last_seen = run.rule(
            "sessions",
            &key,
            rules::instant(r.text("last_seen").as_deref(), &tz, R_SESSION_LAST_SEEN),
        );
        let first_seen = rules::instant(r.text("first_seen").as_deref(), &tz, R_SESSION_CREATED).0;
        // `id_time` is what the derived id is built from. It must be the same in every run,
        // so a session with no time at all (created_at falls back to now) uses time 0.
        let (created, id_time) = match first_seen {
            Some(t) => (t, t),
            None => {
                run.warn("sessions", &key, R_SESSION_CREATED);
                match f.earliest.or(last_seen) {
                    Some(t) => (t, t),
                    None => (now, UtcMs(0)),
                }
            }
        };
        let id = gen_ids.next_id(&run.env.brain, &conv, id_time);
        run.session_ids.insert(conv.clone(), id);

        // State, following the spike's reading.
        let state = r.text("state");
        let latest_kind = latest.map(|e| e.kind.as_str());
        let (lifecycle, activity, location) = match state.as_deref() {
            Some("working") => (Lifecycle::Live, Activity::Working, Location::Here),
            Some("waiting") if latest_kind == Some("waiting") => {
                (Lifecycle::Live, Activity::NeedsPermission, Location::Here)
            }
            Some("waiting" | "idle") => (Lifecycle::Live, Activity::Idle, Location::Here),
            Some("ended" | "archived") => (Lifecycle::Ended, Activity::Idle, Location::Here),
            Some("elsewhere") => (Lifecycle::Ended, Activity::Idle, Location::Elsewhere),
            _ => {
                run.warn("sessions", &key, R_SESSION_STATE);
                (Lifecycle::Ended, Activity::Idle, Location::Here)
            }
        };
        let archived_at =
            (state.as_deref() == Some("archived")).then(|| last_seen.unwrap_or(created));

        let asleep_text = r.text("asleep_at").filter(|t| !t.trim().is_empty());
        let asleep_at = if state.as_deref() == Some("ended") {
            run.rule(
                "sessions",
                &key,
                rules::instant(asleep_text.as_deref(), &tz, R_SESSION_ASLEEP),
            )
        } else {
            if asleep_text.is_some() {
                run.info("sessions", &key, R_SESSION_ASLEEP_DROPPED);
            }
            None
        };

        let title = r.text("title");
        let cwd = r.text("cwd");
        let start_cwd = f.first_cwd.as_ref().map(|(_, c)| c.clone()).or(cwd.clone());
        let project = run.project_ref("sessions", &key, r, R_SESSION_PROJECT);
        // Task 6 already warned about a group path that needed cleaning.
        let group = r
            .text("grp")
            .map(|g| rules::group_path(&g).0)
            .filter(|g| !g.is_empty());
        let last_prompt = r.text("last_prompt");
        let latest_text = latest.and_then(|e| e.row.text_cow("text"));
        let why = live::activity_why(
            state.as_deref(),
            last_prompt.as_deref(),
            latest
                .map(|e| Latest {
                    kind: &e.kind,
                    text: latest_text.as_deref(),
                })
                .as_ref(),
        );
        let activity_at = latest.and_then(|e| e.at).or(last_seen);

        tx.execute(
            "INSERT INTO sessions (id, agent_kind, account, cwd, start_cwd, project_id, title, \
             title_locked, group_path, lifecycle, activity, activity_at, activity_why, last_prompt, \
             last_turn_end_at, seen_at, asleep_at, archived_at, location, resume_failed, \
             created_at, last_seen_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, \
             ?18, ?19, 0, ?20, ?21)",
            params![
                id.to_string(),
                kind.as_str(),
                account,
                cwd,
                start_cwd,
                project,
                title,
                i64::from(title.as_deref().is_some_and(|t| !t.is_empty())),
                group,
                lifecycle.as_str(),
                activity.as_str(),
                activity_at.map(|t| t.0),
                why,
                last_prompt,
                f.last_stop.map(|t| t.0),
                now.0,
                asleep_at.map(|t| t.0),
                archived_at.map(|t| t.0),
                location.as_str(),
                created.0,
                last_seen.map(|t| t.0),
            ],
        )?;

        warned += run.end_record();

        conversation(
            run,
            tx,
            &Conv {
                key: &key,
                conv: &conv,
                id,
                kind,
                resolved: resolved.as_ref(),
                raw_account: r.text("account"),
                first_seen: created,
                last_seen: last_seen.unwrap_or(created),
            },
        )?;
        conversations += 1;

        if let Some(pane) = r.text("pane") {
            tx.execute(
                "INSERT INTO spike_panes (session_id, pane_id, tmux, imported_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![id.to_string(), pane, r.text("tmux"), now.0],
            )?;
            panes += 1;
            if lifecycle == Lifecycle::Live {
                run.info("sessions", &key, R_PANE_UNVERIFIED);
            }
        }

        imported += 1;
        conversations_warned += run.end_record();
    }
    run.finish("sessions", rows.len(), imported, warned);
    run.finish(
        "conversations",
        rows.len(),
        conversations,
        conversations_warned,
    );
    run.finish(
        "spike_panes",
        rows.iter().filter(|r| r.text("pane").is_some()).count(),
        panes,
        0,
    );
    Ok(())
}

struct Conv<'a> {
    key: &'a str,
    conv: &'a str,
    id: SessionId,
    kind: AgentKind,
    resolved: Option<&'a ResolvedAccount>,
    raw_account: Option<String>,
    first_seen: UtcMs,
    last_seen: UtcMs,
}

/// One conversation row per session, with a look for its transcript file (never its contents).
fn conversation(run: &mut Run, tx: &Transaction, c: &Conv) -> Result<(), ImportError> {
    let store = c.resolved.and_then(|a| a.store.clone());
    let check = match (&store, c.kind) {
        (Some(store), AgentKind::Claude) => (run.env.transcript)(store, c.conv),
        _ => TranscriptCheck::NotChecked,
    };
    let now = run.env.now;
    let (present, path, checked) = match check {
        TranscriptCheck::Present(p) => {
            (Some(1), Some(p.to_string_lossy().into_owned()), Some(now.0))
        }
        TranscriptCheck::Missing => {
            let n = c.resolved.map_or(0, |a| run.env.accounts.index_of(&a.name));
            run.warn("conversations", c.key, r_transcript_missing(n));
            (Some(0), None, Some(now.0))
        }
        TranscriptCheck::UnsafeId => {
            run.warn("conversations", c.key, R_CONV_UNSAFE);
            (None, None, None)
        }
        TranscriptCheck::NotChecked => {
            run.info("conversations", c.key, R_CONV_UNCHECKED);
            (None, None, None)
        }
    };
    tx.execute(
        "INSERT INTO conversations (agent_kind, conversation_id, session_id, store_dir, \
         spike_account, reason, first_seen_at, last_seen_at, transcript_path, \
         transcript_present, transcript_checked_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'import', ?6, ?7, ?8, ?9, ?10)",
        params![
            c.kind.as_str(),
            c.conv,
            c.id.to_string(),
            store.map(|s| s.to_string_lossy().into_owned()),
            c.raw_account,
            c.first_seen.0,
            c.last_seen.0,
            path,
            present,
            checked,
        ],
    )?;
    Ok(())
}

fn events(run: &mut Run, tx: &Transaction, events: &[Ev]) -> Result<(), ImportError> {
    let (now, tz) = (run.env.now, run.env.tz.clone());
    let (mut imported, mut warned) = (0, 0);
    for e in events {
        let key = id_key(e.id);
        run.check_text("events", &key, &e.row);
        let at = run.rule(
            "events",
            &key,
            rules::instant_or(e.row.text("ts").as_deref(), &tz, now, R_EVENT_TS),
        );
        let session = e
            .session
            .as_ref()
            .and_then(|s| run.session_ids.get(s))
            .map(|id| id.to_string());
        let account = e
            .row
            .text("account")
            .filter(|a| !a.trim().is_empty())
            .map(|a| run.resolve_account(&a).name);
        let text = e.row.text_cow("text");
        let kind = match event_kind(&e.kind, text.as_deref()) {
            Some(k) => k.to_string(),
            None => {
                if e.row.text("kind").is_none() {
                    run.warn("events", &key, R_EVENT_KIND_NULL);
                } else {
                    run.warn("events", &key, R_EVENT_KIND_UNKNOWN);
                }
                e.kind.clone()
            }
        };
        tx.execute(
            "INSERT INTO events (id, at, session_id, conversation_id, account, cwd, kind, text) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                e.id,
                at.0,
                session,
                e.session,
                account,
                e.row.text("cwd"),
                kind,
                text
            ],
        )?;
        imported += 1;
        warned += run.end_record();
    }
    run.finish("events", events.len(), imported, warned);
    Ok(())
}
