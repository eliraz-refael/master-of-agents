//! moa opens the day (D2, D3, C19): the first time the user is here each local day, never at
//! midnight itself, moa's agent gets one prompt that opens it, carrying what's due, the idle
//! terminals, and whatever else waits. The user's own prompt opens it instead when it comes
//! first.

mod common;

use common::at;
use common::central::{
    Disk, acps, body_of, commits, end_turn, fire, hear_at, opened, play, prompts_sent, sent, toml,
    you_commit, you_row,
};
use common::world::{
    DUE_BLOCK, HOUR, IDLE_BLOCK, MIN, SEC, World, YES_ID, d10_terminals, five_due, nags,
};
use jiff::civil::Date;
use moa_core::UtcMs;
use moa_core::central::channel::AUTO_KEY;
use moa_core::central::opening::BRIEFED_KEY;
use moa_core::central::wire::{AcpCmd, Opened, ThreadHow};
use moa_core::change::Change;
use moa_core::effect::{Effect, Input, TmuxIn, handle_checked};
use moa_core::model::{Task, Terminal, YES_TOLD};
use moa_core::op::{ConnId, Op, ReqId};
use moa_core::panel::View;
use moa_core::state::{CallerInfo, Deps, State};
use moa_core::time::start_of_next_day;
use moa_core::tmux::notify::Notification;
use moa_testkit::brain::TypicalBrain;

/// D10's opening, as moa's agent is told it.
const OPENING: &str = "(moa · the user is here for the first time today, not typed by the user)\n\
                       Open the day: brief the user as a thread's first message does (see \
                       \"How to talk\" in your instructions).";

const GAP: &str = "\n\n---\n\n";

const ATTACH: &str = "moa:hook client-attached /dev/ttys1";

/// How many kinds of presence [`presence`] makes.
const KINDS: usize = 8;

/// A world moa last opened the day before the brain's day.
fn unopened(
    tasks: impl FnOnce(Date) -> Vec<Task>,
    terminals: impl FnOnce(&TypicalBrain) -> Vec<Terminal>,
    settings: &str,
) -> World {
    let yesterday = |today: Date| vec![(BRIEFED_KEY, today.yesterday().unwrap().to_string())];
    World::build(tasks, terminals, settings, yesterday)
}

/// moa's caller, as `moactl` run by moa's agent says it.
fn moa_caller() -> CallerInfo {
    CallerInfo {
        moa_session: Some("moa".into()),
        ..CallerInfo::default()
    }
}

/// Presence of D2's kind `kind` (0 to 7, then again), as an input: a client attached, a
/// client active, a key, a prompt's answer, and the client ops `console.prompt`,
/// `console.command`, `console.page`, and `ui.here` moved by a key or click (R9), from
/// `caller`.
fn presence(kind: usize, caller: CallerInfo) -> Input {
    let message =
        |text: &str| Input::Tmux(TmuxIn::Notification(Notification::Message(text.into())));
    let op = match kind % KINDS {
        0 => return message(ATTACH),
        1 => return message("moa:hook client-active /dev/ttys1"),
        2 => return message("moa:key help - $0 /dev/ttys1"),
        3 => return message("moa:prompt 7 Kite"),
        4 => Op::ConsolePrompt {
            text: "what's up with Kite?".into(),
            wait: false,
        },
        5 => Op::ConsoleCommand {
            line: "/help".into(),
        },
        6 => Op::ConsolePage { before: 10, n: 5 },
        // A key moved the panel's selection: [`panel_up`] said `~/private/lantern` first.
        _ => Op::UiHere {
            folder: Some("~/private/kite".into()),
            by_user: true,
        },
    };
    Input::Client {
        conn: ConnId(1),
        req: ReqId(100 + kind as u64),
        caller,
        op,
    }
}

/// `input` at `ms`, bare: moa's agent hears nothing of what it is asked.
fn input(w: &mut World, ms: i64, input: Input) -> Vec<Effect> {
    handle_checked(&mut w.st, input, at(UtcMs(ms)))
}

/// The prompts of `fx` that open the day.
fn briefs(fx: &[Effect]) -> usize {
    prompts_sent(fx)
        .iter()
        .filter(|t| t.starts_with(OPENING))
        .count()
}

/// The `moa.briefed` day each commit of `fx` saves.
fn briefed(fx: &[Effect]) -> Vec<String> {
    commits(fx)
        .into_iter()
        .flatten()
        .filter_map(|c| match c {
            Change::Meta { key, value } if key == BRIEFED_KEY => value.clone(),
            _ => None,
        })
        .collect()
}

/// The system prompt of the new thread `fx` opens.
fn system_prompt(fx: &[Effect]) -> String {
    let opens: Vec<&String> = acps(fx)
        .into_iter()
        .filter_map(|c| match c {
            AcpCmd::Open {
                how:
                    ThreadHow::New {
                        system_prompt: Some(s),
                        ..
                    },
                ..
            } => Some(s),
            _ => None,
        })
        .collect();
    match opens[..] {
        [s] => s.clone(),
        _ => panic!("{} new threads opened", opens.len()),
    }
}

fn brief_due(st: &State) -> bool {
    st.central().brief_due.is_some()
}

/// Connection `conn` says it is a panel, then its folder `here`, as every panel's connect
/// does (`first_ops`): none of it is the user.
fn panel_up(w: &mut World, ms: i64, conn: u64, here: &str) {
    for op in [
        Op::UiPanel {
            pane: None,
            view: Some(View::Tree),
            placed: false,
            once: false,
        },
        Op::UiHere {
            folder: Some(here.into()),
            by_user: false,
        },
    ] {
        let fx = input(w, ms, client(conn, op));
        assert!(acps(&fx).is_empty(), "{fx:?}");
    }
}

/// `op` from the user on connection `conn`.
fn client(conn: u64, op: Op) -> Input {
    Input::Client {
        conn: ConnId(conn),
        req: ReqId(1),
        caller: CallerInfo::default(),
        op,
    }
}

#[test]
fn the_first_attach_of_the_day_briefs() {
    let mut w = unopened(five_due, d10_terminals, "");
    let today = w.today();
    let ms = w.t(MIN);
    let fx = w.message(ms, ATTACH);
    assert!(brief_due(&w.st));
    let fx = play(&mut w.st, ms, fx);
    let text = sent(&fx);
    assert_eq!(text, [OPENING, DUE_BLOCK, IDLE_BLOCK].join(GAP));
    // A new thread, told today's brief.
    assert!(
        system_prompt(&fx).contains("# Brief (Sun 15 Nov 12:01)"),
        "{}",
        system_prompt(&fx)
    );
    let commit = you_commit(&fx);
    assert!(commit.iter().any(|c| matches!(c, Change::Thread(_))));
    assert_eq!(briefed(&fx), [today.to_string()]);
    assert_eq!(nags(commit).len(), 5);
    let row = you_row(&fx);
    assert!(row.automatic);
    let body = body_of(row);
    assert_eq!(body["opening"], true);
    assert_eq!(body["nags"], 5);
    assert_eq!(body["text"].as_str(), Some(text.as_str()));
    assert!(body.get("carried").is_none(), "{body}");
    assert!(!brief_due(&w.st));
    assert_eq!(w.st.meta(BRIEFED_KEY), Some(today.to_string().as_str()));

    // The next morning's first attach opens a new thread, told that day's brief.
    let ms = w.t(HOUR);
    end_turn(&mut w.st, ms);
    let morning = start_of_next_day(today, &common::tz()).0 + 8 * HOUR;
    let fx = w.message(morning, ATTACH);
    let generation = w.st.central().generation;
    let fx2 = hear_at(
        &mut w.st,
        at(UtcMs(morning)),
        opened(generation, "thread-monday", Opened::New),
    );
    assert!(
        system_prompt(&fx).contains("# Brief (Mon 16 Nov 08:00)"),
        "{}",
        system_prompt(&fx)
    );
    let text = sent(&fx2);
    assert!(
        text.starts_with(&format!("{OPENING}{GAP}Due by tomorrow")),
        "{text}"
    );
    let tomorrow = today.tomorrow().unwrap();
    assert_eq!(briefed(&fx2), [tomorrow.to_string()]);
    assert_eq!(you_row(&fx2).thread_id, "thread-monday");
}

/// Review focus 1: a burst of 20 presence inputs, of every kind, briefs once.
#[test]
fn presence_bursts_brief_once() {
    let mut w = unopened(five_due, |_| vec![], "");
    let ms = w.t(MIN);
    panel_up(&mut w, ms, 1, "~/private/lantern");
    let mut all = Vec::new();
    for i in 0..20 {
        let ms = w.t(MIN + i * SEC);
        let fx = input(&mut w, ms, presence(i as usize, CallerInfo::default()));
        all.extend(play(&mut w.st, ms, fx));
    }
    assert_eq!(briefs(&all), 1, "{:?}", prompts_sent(&all));
    assert_eq!(prompts_sent(&all).len(), 1, "{:?}", prompts_sent(&all));
    assert_eq!(briefed(&all).len(), 1);
    // Its turn ends; the day is open, and more presence does nothing.
    let ms = w.t(2 * MIN);
    let mut all = end_turn(&mut w.st, ms);
    for i in 0..20 {
        let kind = i as usize;
        if kind % KINDS == 4 {
            continue;
        }
        let fx = input(&mut w, ms + i * SEC, presence(kind, CallerInfo::default()));
        all.extend(play(&mut w.st, ms + i * SEC, fx));
        assert!(!brief_due(&w.st));
    }
    assert!(prompts_sent(&all).is_empty(), "{:?}", prompts_sent(&all));
}

/// Each of D2's kinds is presence on its own.
#[test]
fn every_kind_of_presence_counts() {
    for kind in 0..KINDS {
        let mut w = unopened(|_| vec![], |_| vec![], "");
        let ms = w.t(MIN);
        panel_up(&mut w, ms, 1, "~/private/lantern");
        assert!(!brief_due(&w.st));
        let fx = input(&mut w, ms, presence(kind, CallerInfo::default()));
        assert!(brief_due(&w.st), "kind {kind}");
        let fx = play(&mut w.st, ms, fx);
        let text = sent(&fx);
        assert!(text.starts_with(OPENING), "kind {kind}: {text}");
        assert!(!brief_due(&w.st), "kind {kind}");
    }
}

/// Review focus 1: presence while moa answers a wake-up: the brief goes at its turn's end.
#[test]
fn the_brief_waits_for_moa() {
    let mut w = unopened(five_due, |_| vec![], "");
    let text = sent(&w.wake(w.t(MIN)));
    assert!(!text.starts_with(OPENING), "{text}");
    assert!(text.contains(DUE_BLOCK), "{text}");
    let ms = w.t(3 * MIN);
    let fx = w.message(ms, ATTACH);
    assert!(prompts_sent(&fx).is_empty(), "{fx:?}");
    assert!(brief_due(&w.st));
    let fx = end_turn(&mut w.st, ms + MIN);
    // What's due was told with the wake-up: the brief is the opening alone.
    assert_eq!(sent(&fx), OPENING);
    assert_eq!(briefed(&fx), [w.today().to_string()]);
    assert!(!brief_due(&w.st));
}

/// Review focus 1: the daemon started again an hour after the day's brief doesn't brief again.
#[test]
fn a_restart_doesnt_brief_again() {
    let mut w = unopened(|_| vec![], |_| vec![], "");
    let mut disk = Disk::new(&w.b, "");
    // Started before any brief, presence would brief.
    let mut before = {
        let deps = Deps {
            home: w.b.home.clone(),
            ..Deps::default()
        };
        let now = at(UtcMs(w.t(MIN)));
        common::started_at(&w.b, &toml("acct-a"), deps, disk.snapshot(), now).0
    };
    handle_checked(
        &mut before,
        presence(0, CallerInfo::default()),
        at(UtcMs(w.t(MIN))),
    );
    assert!(brief_due(&before));
    // The brief, saved.
    let ms = w.t(MIN);
    let fx = w.message(ms, ATTACH);
    let fx = play(&mut w.st, ms, fx);
    assert_eq!(briefs(&fx), 1);
    for c in commits(&fx) {
        disk.apply(c);
    }
    assert_eq!(disk.meta(BRIEFED_KEY), Some(w.today().to_string()));
    // An hour later the daemon starts again: presence does nothing.
    let later = at(UtcMs(w.t(HOUR)));
    let (mut st, _) = disk.restart(&w.b, later.clone());
    let fx = handle_checked(&mut st, presence(0, CallerInfo::default()), later.clone());
    assert!(!brief_due(&st));
    assert!(acps(&fx).is_empty(), "{fx:?}");
    let fx = handle_checked(&mut st, presence(5, CallerInfo::default()), later);
    assert!(!brief_due(&st));
    assert!(acps(&fx).is_empty(), "{fx:?}");
}

/// Review focus 1: while writes are refused the brief waits, still due, and goes once they
/// are back; one refused at its commit saves and sends nothing, and goes the same way.
#[test]
fn a_refused_brief_stays_due() {
    // Refused before the presence.
    let mut w = unopened(five_due, |_| vec![], "");
    let ms = w.t(MIN);
    let back = ms + 10 * MIN;
    w.refuse_writes(ms, back);
    let fx = w.message(ms + SEC, ATTACH);
    assert!(acps(&fx).is_empty(), "{fx:?}");
    assert!(brief_due(&w.st));
    let fx = fire(&mut w.st, back);
    let fx = play(&mut w.st, back, fx);
    assert!(sent(&fx).starts_with(&format!("{OPENING}{GAP}{DUE_BLOCK}")));
    assert!(!brief_due(&w.st));

    // Refused while moa's agent starts for it.
    let mut w = unopened(five_due, |_| vec![], "");
    let ms = w.t(MIN);
    let fx = w.message(ms, ATTACH);
    w.refuse_writes(ms, back);
    let fx = play(&mut w.st, ms, fx);
    assert!(prompts_sent(&fx).is_empty(), "{fx:?}");
    assert!(briefed(&fx).is_empty());
    assert!(brief_due(&w.st));
    assert_ne!(w.st.meta(BRIEFED_KEY), Some(w.today().to_string().as_str()));
    let fx = fire(&mut w.st, back);
    let fx = play(&mut w.st, back, fx);
    assert!(sent(&fx).starts_with(&format!("{OPENING}{GAP}{DUE_BLOCK}")));
    assert_eq!(briefed(&fx), [w.today().to_string()]);
    assert!(!brief_due(&w.st));
}

/// C19, U18: crossing midnight, with wake-ups on either side of it, opens nothing; the
/// first presence after it does.
#[test]
fn midnight_itself_does_nothing() {
    let mut w = World::new(five_due, |_| vec![], "");
    let today = w.today();
    let midnight = start_of_next_day(today, &common::tz()).0;
    // A wake-up's turn runs across midnight; another wake-up after it, and the timer once
    // more an hour later.
    let mut all = w.wake(midnight - MIN);
    end_turn(&mut w.st, midnight + MIN);
    all.extend(w.wake(midnight + 10 * MIN));
    assert_eq!(prompts_sent(&all).len(), 2, "{:?}", prompts_sent(&all));
    end_turn(&mut w.st, midnight + HOUR);
    all.extend(fire(&mut w.st, midnight + HOUR));
    assert_eq!(briefs(&all), 0, "{:?}", prompts_sent(&all));
    assert!(briefed(&all).is_empty());
    assert!(!brief_due(&w.st));
    assert_eq!(w.st.meta(BRIEFED_KEY), Some(today.to_string().as_str()));
    // The first presence that morning opens the day.
    let ms = midnight + 8 * HOUR;
    let fx = w.message(ms, ATTACH);
    let fx = play(&mut w.st, ms, fx);
    assert_eq!(briefs(&fx), 1);
    assert_eq!(briefed(&fx), [today.tomorrow().unwrap().to_string()]);
}

/// D3: your prompt opens the day when it comes before the brief: on its own, or after
/// presence while the brief waits (here for writes refused until a moment ago).
#[test]
fn your_first_prompt_opens_the_day() {
    let mut w = unopened(five_due, d10_terminals, "");
    let fx = w.prompt(w.t(MIN), "what's up with Kite?");
    let text = sent(&fx);
    let carried = [OPENING, DUE_BLOCK, IDLE_BLOCK].join(GAP);
    assert_eq!(text, format!("{carried}{GAP}what's up with Kite?"));
    let row = you_row(&fx);
    assert!(!row.automatic);
    let body = body_of(row);
    assert_eq!(body["opening"], true);
    assert_eq!(body["carried"].as_str(), Some(carried.as_str()));
    assert_eq!(body["text"], "what's up with Kite?");
    assert_eq!(nags(you_commit(&fx)).len(), 5);
    assert_eq!(briefed(&fx), [w.today().to_string()]);
    assert!(!brief_due(&w.st));

    let mut w = unopened(five_due, |_| vec![], "");
    let ms = w.t(MIN);
    let back = ms + 10 * MIN;
    w.refuse_writes(ms, back);
    w.message(ms + SEC, ATTACH);
    assert!(brief_due(&w.st));
    let fx = w.prompt(back + SEC, "hi");
    assert_eq!(sent(&fx), format!("{OPENING}{GAP}{DUE_BLOCK}{GAP}hi"));
    assert_eq!(briefed(&fx), [w.today().to_string()]);
    assert!(!brief_due(&w.st));
    // No brief of its own after it.
    end_turn(&mut w.st, back + MIN);
    let fx = fire(&mut w.st, back + 2 * MIN);
    assert!(prompts_sent(&fx).is_empty(), "{fx:?}");
}

/// D3: `/auto` isn't one of the brief's gates. With it off, the session reports wait for
/// your prompt (ruling R3).
#[test]
fn the_brief_goes_with_auto_off() {
    let meta = |today: Date| {
        vec![
            (BRIEFED_KEY, today.yesterday().unwrap().to_string()),
            (AUTO_KEY, "off".to_string()),
        ]
    };
    let mut w = World::build(five_due, |_| vec![], "", meta);
    let ms = w.t(MIN);
    w.report(ms);
    let fx = w.message(ms + SEC, ATTACH);
    let fx = play(&mut w.st, ms + SEC, fx);
    assert_eq!(sent(&fx), [OPENING, DUE_BLOCK].join(GAP));
    assert!(body_of(you_row(&fx)).get("reports").is_none());
    assert_eq!(w.st.reports().count(), 1);
}

/// D2: what moa's own agent does is never presence.
#[test]
fn moas_own_ops_arent_presence() {
    let mut w = unopened(five_due, |_| vec![], "");
    let ms = w.t(MIN);
    panel_up(&mut w, ms, 1, "~/private/lantern");
    for kind in [5, 6, 7] {
        let fx = input(&mut w, ms, presence(kind, moa_caller()));
        assert!(!brief_due(&w.st), "kind {kind}");
        assert!(acps(&fx).is_empty(), "kind {kind}: {fx:?}");
    }
    // A wake-up's `moactl focus`.
    let focus = Op::SessionFocus { q: "kite".into() };
    let fx = w.op_from(ms, moa_caller(), focus);
    assert!(!brief_due(&w.st));
    assert!(acps(&fx).is_empty(), "{fx:?}");
}

/// D3: the brief carries what waits: your yes and the session reports, as well as the
/// opening, and its one commit marks them all.
#[test]
fn the_brief_carries_reports_and_yeses() {
    let mut w = unopened(five_due, d10_terminals, "");
    // A wake-up told what's due and the idle terminals already.
    let text = sent(&w.wake(w.t(MIN)));
    assert!(text.contains(IDLE_BLOCK), "{text}");
    // While moa answers it: a report, your yes, and you.
    let ms = w.t(2 * MIN);
    w.report(ms);
    let fx = w.yes(ms);
    assert!(prompts_sent(&fx).is_empty(), "{fx:?}");
    w.message(ms + SEC, ATTACH);
    let fx = end_turn(&mut w.st, ms + 2 * SEC);
    let text = sent(&fx);
    let parts: Vec<&str> = text.split(GAP).collect();
    assert_eq!(parts.len(), 4, "{text}");
    assert_eq!(parts[0], OPENING);
    assert!(parts[1].starts_with("(moa · the user said yes"), "{text}");
    assert!(parts[2].starts_with("(moa · a wake-up, not typed by the user) 1 session report"));
    // The brief tells every idle terminal, not only those since the last wake-up.
    assert_eq!(parts[3], IDLE_BLOCK);
    let commit = you_commit(&fx);
    assert!(commit.iter().any(|c| matches!(c, Change::DeleteReport(_))));
    let told = |c: &&Change| {
        matches!(c, Change::Suggestion(s)
            if s.id.0 == YES_ID && s.result.as_deref() == Some(YES_TOLD))
    };
    assert!(commit.iter().any(|c| told(&c)));
    assert!(commit.iter().any(|c| matches!(c,
        Change::Meta { key, .. } if key == BRIEFED_KEY)));
    let body = body_of(you_row(&fx));
    assert_eq!(body["opening"], true);
    assert_eq!(body["reports"], 1);
    assert_eq!(body["yeses"], serde_json::json!([YES_ID]));
    assert_eq!(w.st.reports().count(), 0);
    // Nothing is left for a wake-up or a yes.
    let fx = fire(&mut w.st, ms + 5 * MIN);
    assert!(prompts_sent(&fx).is_empty(), "{fx:?}");
}

/// R9: a panel says its folder by itself: on every connect (a daemon restart in the night
/// brings each panel back), and whenever data moves its selection (its first snapshot picks a
/// row, a row it had vanishes). None of it is the user; a key or click moving it is.
#[test]
fn only_a_key_or_click_in_a_panel_is_the_user() {
    let mut w = unopened(five_due, |_| vec![], "");
    let ms = w.t(MIN);
    let here = |f: Option<&str>, by_user: bool| Op::UiHere {
        folder: f.map(String::from),
        by_user,
    };
    // A panel connects with no row picked yet; its first snapshot picks one; a row vanishes
    // and the selection shifts to the next.
    panel_up(&mut w, ms, 1, "~/private/lantern");
    for folder in [None, Some("~/private/lantern"), Some("~/private/harbor")] {
        let fx = input(&mut w, ms, client(1, here(folder, false)));
        assert!(!brief_due(&w.st), "{folder:?}");
        assert!(acps(&fx).is_empty(), "{fx:?}");
    }
    // It connects again on a new connection.
    input(&mut w, ms, Input::ClientGone(ConnId(1)));
    panel_up(&mut w, ms, 2, "~/private/beacon");
    assert!(!brief_due(&w.st));
    // A view picked for a panel isn't the user either: nothing of moa's sends it on a key.
    let view = Op::UiSelectView {
        pane: "%3".into(),
        view: View::Console,
    };
    input(&mut w, ms, client(2, view));
    assert!(!brief_due(&w.st));
    // A key moves the selection.
    let fx = input(&mut w, ms, client(2, here(Some("~/private/kite"), true)));
    assert!(brief_due(&w.st));
    let fx = play(&mut w.st, ms, fx);
    assert_eq!(briefs(&fx), 1);
}

/// R8: presence at 22:00 while writes are refused until 03:00 opens nothing at 03:00: the
/// day it was due has passed. The next presence opens the new day.
#[test]
fn a_brief_due_yesterday_is_dropped() {
    let mut w = unopened(five_due, |_| vec![], "");
    let today = w.today();
    let ten_pm = w.t(10 * HOUR);
    let three_am = w.t(15 * HOUR);
    w.refuse_writes(ten_pm, three_am);
    let fx = w.message(ten_pm + SEC, ATTACH);
    assert!(acps(&fx).is_empty(), "{fx:?}");
    assert_eq!(w.st.central().brief_due, Some(today));
    let fx = fire(&mut w.st, three_am);
    let fx = play(&mut w.st, three_am, fx);
    assert!(prompts_sent(&fx).is_empty(), "{fx:?}");
    assert!(briefed(&fx).is_empty());
    assert!(!brief_due(&w.st));
    // The user that morning: the new day's opening.
    let eight_am = w.t(20 * HOUR);
    let fx = w.message(eight_am, ATTACH);
    let fx = play(&mut w.st, eight_am, fx);
    assert_eq!(briefs(&fx), 1);
    assert_eq!(briefed(&fx), [today.tomorrow().unwrap().to_string()]);
}
