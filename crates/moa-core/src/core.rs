//! `handle`: the one pure function the daemon runs. State and an input go in; effects come
//! out, in the order they must happen.

use std::collections::BTreeSet;

use crate::central::{self, Waiter};
use crate::change::Change;
use crate::cockpit;
use crate::effect::{Down, Effect, Input, LogLevel, LogLine, Publish, TimerKey, TmuxIn, TmuxToken};
use crate::hooks;
use crate::keys;
use crate::live::{QUIET_AFTER, quiet_candidate};
use crate::model::{ActivityId, ActivityRow, ActivityWho, Event, EventId, EventKind, SessionId};
use crate::op::{ConnId, Done, Op, OpError, ReqId};
use crate::ops;
use crate::ops::term::Tell;
use crate::panel;
use crate::prompt;
use crate::publish::{CoreEvent, Topic};
use crate::reconcile;
use crate::sleep;
use crate::state::{State, Table};
use crate::terminals;
use crate::time::start_of_next_day;
use crate::tmux::cmd::Cmd;
use crate::tmux::focus::mark_seen;
use crate::tmux::ids::PaneId;
use crate::tmux::link::{self, Asked, Link};
use crate::tmux::mailbox::{HookMsg, Mailbox};
use crate::tmux::notify::Notification;
use crate::tmux::panes::parse_panes;
use crate::tmux::status;
use crate::wake;
use crate::{Now, UtcMs};

/// The effects of one input. Persist first, then what people hear about it.
pub fn handle(state: &mut State, input: Input, now: Now) -> Vec<Effect> {
    state.set_now(now.wall);
    let sleep_fired = matches!(input, Input::Timer(TimerKey::Sleep));
    let terminals_fired = matches!(input, Input::Timer(TimerKey::Terminals));
    // A reconcile that was refused or failed is asked again as the next input starts (R10).
    let mut fx: Vec<Effect> = reconcile::ask_if_due(state, &now).into_iter().collect();
    fx.extend(decide(state, input, &now));
    // A restart held for its old agent's vacate opens its new tab, or is refused (R24).
    let released = ops::launch::released(state, &now);
    fold_in(&mut fx, released);
    // Whatever this input changed, a session it left due and eligible goes to sleep (R19).
    sleep::run(state, &now, sleep_fired, &mut fx);
    // So does a terminal's kill or timeout, once moa may act on its tab (T8).
    terminals::run(state, &now, terminals_fired, &mut fx);
    // The terminals are the topology's: a listing's, a terminal's own tag, a session's tag
    // (a terminal running an agent is its session). The views hear of them when they changed.
    fx.extend(
        state
            .publish_changed(Topic::Terms)
            .map(|m| Effect::Publish(Publish::Model(m))),
    );
    // Last, so the status line shows what this input saved, after its `Persist`.
    status::run(state, &now, &mut fx);
    fx
}

fn decide(state: &mut State, input: Input, now: &Now) -> Vec<Effect> {
    match input {
        Input::Startup(snapshot) => {
            state.refuse_writes = None;
            state.load(*snapshot);
            let mut fx = vec![Effect::Backup { day: now.local_day }, backup_timer(now)];
            fx.push(quiet_timer(state, now.wall));
            // A row's notes sit together, one per reason; the line names no reason, so one
            // line per row.
            let mut rows: Vec<&crate::state::LoadNote> = state.unknown().iter().collect();
            rows.dedup_by(|a, b| a.table == b.table && a.key == b.key);
            fx.extend(rows.into_iter().map(|n| {
                // A group's key is its path: a name, so it stays out of the log.
                let message = if n.table == "groups" {
                    "a row in groups has a value this build doesn't know".to_string()
                } else {
                    format!(
                        "row {} in {} has a value this build doesn't know",
                        n.key, n.table
                    )
                };
                Effect::Log(LogLine {
                    level: LogLevel::Warn,
                    message,
                })
            }));
            // A suggestion whose action no longer parses is flagged and saved, before anyone
            // is told about the state.
            let flags = ops::suggestions::startup_flags(state);
            if !flags.is_empty() {
                fx.extend(flags.iter().map(|s| {
                    Effect::Log(LogLine {
                        level: LogLevel::Warn,
                        message: format!("suggestion #{} has an action that doesn't parse", s.id),
                    })
                }));
                let changes: Vec<Change> = flags.into_iter().map(Change::Suggestion).collect();
                for c in &changes {
                    state.apply(c);
                }
                fx.push(Effect::Persist(changes));
            }
            state.layouts = ops::layout::read(state);
            fx.extend(publish_all(state));
            fx.extend(state.tmux.startup());
            fx
        }
        Input::Reload {
            snapshot,
            refuse_writes,
        } => {
            state.load(*snapshot);
            state.forget_hooks();
            state.refuse_writes = refuse_writes;
            // The timers this input armed may have been lost with it: they are armed again.
            state.sleep_at = None;
            state.terminals_at = None;
            // The closes under way may have been half-changed: they are forgotten.
            state.closing.clear();
            let mut fx = vec![backup_timer(now), quiet_timer(state, now.wall)];
            fx.extend(publish_all(state));
            fx.extend(state.tmux.reload());
            fx.extend(cockpit::run::reload(state));
            fx.extend(ops::launch::lost_opens(state));
            // The reload may have dropped a reconcile's changes: the panes are looked at again.
            fx.extend(reconcile::ask_if_due(state, now));
            // moa's prompt whose row it lost was never sent.
            fx.extend(central::reload(state));
            fx
        }
        Input::Tmux(t) => {
            // Every key goes through one dispatch (L13).
            let mailbox = match &t {
                TmuxIn::Notification(Notification::Message(m)) => Mailbox::parse(m).ok(),
                _ => None,
            };
            if let Some(Mailbox::Key {
                action,
                pane,
                session,
                client,
            }) = &mailbox
            {
                return keys::run(state, *action, *pane, *session, client.clone());
            }
            // So does every answer to a prompt that waits for one (S5); the link logs the
            // rest.
            if let Some(Mailbox::Prompt { token, text }) = &mailbox
                && state.prompts.contains_key(token)
            {
                return prompt::answered(state, now, *token, text);
            }
            // The spot's agent ending by itself leaves the spot empty: the layout is built
            // again (L11). Its tag is read before `pane_gone` deletes the binding.
            let refill = matches!(&mailbox, Some(Mailbox::Hook(HookMsg::PaneExited(pane)))
                if state.tmux.topology.view_of(*pane) == Some(cockpit::AGENT));
            // A pane that exited puts its session to sleep, before the link looks at the tree.
            let mut fx = match link::exited_pane(&t) {
                Some(pane) => {
                    let mut tx = Tx::new(now);
                    let saved = hooks::pane_gone(state, &mut tx, pane);
                    let mut fx = post_commit(state, tx);
                    if !saved {
                        fx.push(Effect::Log(LogLine {
                            level: LogLevel::Warn,
                            message: "a pane exit was not saved; reconcile puts its session to sleep once moa can save again".into(),
                        }));
                    }
                    if !fx.is_empty() {
                        fx.push(quiet_timer(state, now.wall));
                    }
                    fx
                }
                None => Vec::new(),
            };
            // Opens this answers, while the link is as they knew it. Never the same input as
            // a pane exit, so this commit is the input's only one.
            let settled = state.tmux.settle(&t);
            if !settled.is_empty() {
                fx.extend(ops::launch::opened(state, now, settled));
            }
            let answers = state.tmux.answered(&t);
            let replied = matches!(t, TmuxIn::Replies { .. });
            let (connected, no_server) = (
                matches!(t, TmuxIn::Connected { .. }),
                matches!(t, TmuxIn::Down(Down::NoServer)),
            );
            // A new connection, or none: nothing asked on the old one is answered (L17).
            let link_conn = state.tmux.conn();
            let fresh = matches!(t, TmuxIn::Connected { conn, .. } if link_conn != Some(conn));
            let lost = matches!(t, TmuxIn::Down(_)) || (fresh && link_conn.is_some());
            let link_listed = matches!(t, TmuxIn::Replies { conn, token: TmuxToken::Refresh, .. }
                if link_conn == Some(conn));
            // Another server's pane ids name other tabs: the views the old tabs had go, and
            // no panel has its tab any more (L8).
            let another_server = state.tmux.another_server(&t);
            // A prompt's template names the old control client: no answer to it can come.
            if fresh {
                state.prompts.clear();
            }
            let mut unpaned = None;
            // A close under way is for a tab of the link it was asked on: never sent on another.
            if lost || another_server {
                state.closing.clear();
            }
            if another_server {
                state.pane_views.clear();
                state.sheets.clear();
                unpaned = panel::server_changed(state);
            }
            // A user's session leaving a tab, or a terminal detaching, may close a sheet (S9),
            // once the topology has taken the notification in.
            let sheet_news = cockpit::run::sheet_news(state, &t);
            fx.extend(state.tmux.input(t));
            if lost {
                fx.extend(cockpit::run::lost(state));
            }
            // Reconcile reads its listing, a resume, an adopt or an `ls` its own and a
            // terminal its new tab or a key its folder, once the link is as the input left it.
            let (mut reconciles, mut resumes, mut terms) = (Vec::new(), Vec::new(), Vec::new());
            let (mut adopts, mut lists, mut looks) = (Vec::new(), Vec::new(), Vec::new());
            let (mut heres, mut spawns, mut closes) = (Vec::new(), Vec::new(), Vec::new());
            let (mut scenes, mut chains) = (Vec::new(), Vec::new());
            for (asked, answers) in answers {
                // A panel spawn reads each command's answer (L18); the rest read them as one.
                match asked {
                    Asked::Reconcile => reconciles.push(link::all_lines(answers)),
                    Asked::Resume(ask) => resumes.push((ask, link::all_lines(answers))),
                    Asked::Adopt { conn, req, args } => {
                        adopts.push((conn, req, args, link::all_lines(answers)))
                    }
                    Asked::Ls { conn, req, all } => {
                        lists.push((conn, req, all, link::all_lines(answers)))
                    }
                    Asked::Term { tell, name, path } => {
                        terms.push((tell, name, path, link::all_lines(answers)))
                    }
                    Asked::TermClose { tell, id } => {
                        closes.push((tell, id, link::all_lines(answers)))
                    }
                    Asked::Here { client } => heres.push((client, link::all_lines(answers))),
                    Asked::Look { id, events } => {
                        looks.push((id, events, link::all_lines(answers)))
                    }
                    Asked::Spawn { views, seq } => spawns.push((views, seq, answers)),
                    Asked::Scene { seq } => scenes.push((seq, answers)),
                    Asked::Chain { seq } => chains.push((seq, answers)),
                }
            }
            let panes_of = |answer: link::Answer| {
                answer.and_then(|lists| match lists.as_slice() {
                    [info, start] => Ok(parse_panes(info, start)),
                    _ => Err(()),
                })
            };
            let was_reconciled = state.tmux().reconciled();
            let mut more = reconcile::answered(state, now, reconciles, replied);
            let just_reconciled = !was_reconciled && state.tmux().reconciled();
            more.extend(unpaned);
            for (ask, answer) in resumes {
                more.extend(ops::launch::resumed(state, now, ask, panes_of(answer)));
            }
            for (conn, req, args, answer) in adopts {
                more.extend(ops::adopt::adopted(
                    state,
                    now,
                    conn,
                    req,
                    &args,
                    panes_of(answer),
                ));
            }
            for (conn, req, all, answer) in lists {
                more.extend(ops::sessions::listed(
                    state,
                    now,
                    conn,
                    req,
                    all,
                    panes_of(answer),
                ));
            }
            for (tell, name, path, answer) in terms {
                more.extend(ops::term::term_opened(
                    state, now, tell, &name, &path, answer, replied,
                ));
            }
            for (client, answer) in heres {
                more.extend(ops::term::here_answered(state, client, answer, replied));
            }
            for (tell, id, answer) in closes {
                more.extend(ops::term::close_answered(
                    state, now, tell, id, answer, replied,
                ));
            }
            for (id, events, answer) in looks {
                more.extend(wake::looked(state, now, id, &events, panes_of(answer)));
            }
            // The cockpit's operation in flight reads its answers (L17). The panes a spawn
            // started are tagged whatever became of it. A spawn the link lost may have started
            // panels moa knows nothing of: the log says so. Otherwise what the link lost,
            // nothing reads: `lost` ended its operation.
            for (views, seq, answers) in spawns {
                match answers {
                    Some(answers) => {
                        more.extend(cockpit::run::spawn_answered(state, &views, seq, answers))
                    }
                    None => more.push(cockpit::spawn::spawn_lost(&views)),
                }
            }
            for (seq, answers) in scenes {
                if let Some(answers) = answers {
                    more.extend(cockpit::run::scene_answered(state, seq, answers));
                }
            }
            for (seq, answers) in chains {
                if let Some(answers) = answers {
                    more.extend(cockpit::run::chain_answered(state, now, seq, answers));
                }
            }
            if connected {
                more.extend(reconcile::ask_if_due(state, now));
            }
            // A user's first attach, and a new link, build the cockpit if it isn't (L9); a
            // panel that stopped is logged (L12).
            if fresh {
                more.extend(cockpit::run::linked(state, another_server));
            }
            if link_listed {
                // Each panel's view is read again from the tags (L8).
                // Healing waits while a cockpit operation runs (L17): this listing may be
                // older than the chain in flight, and the next listing heals.
                let heal = cockpit::run::idle(state);
                more.extend(panel::listed(state, heal));
                more.extend(cockpit::run::link_listed(state));
            }
            // Each terminal is recorded when moa sees it, once the tabs are reconciled: the
            // end of this connection's reconcile is its first look, against the listing it
            // has. After this input's answers, so a terminal `term` just opened is recorded
            // with its name, not as a stranger, and only once when both happen here.
            if link_listed || just_reconciled {
                more.extend(terminals::register(state, now));
            }
            if link_listed {
                terminals::closes_listed(state);
            }
            match mailbox {
                Some(Mailbox::Hook(HookMsg::ClientAttached(_))) => {
                    more.extend(cockpit::run::queue(state, cockpit::run::CockpitOp::Lazy));
                }
                Some(Mailbox::Hook(HookMsg::PaneDied(pane))) => {
                    more.extend(cockpit::run::died(state, pane));
                }
                _ => {}
            }
            if refill {
                more.extend(cockpit::run::queue(state, cockpit::run::CockpitOp::Refill));
            }
            if let Some(news) = sheet_news {
                more.extend(cockpit::run::sheets_notified(state, news));
            }
            if no_server {
                more.extend(reconcile::no_server(state, now));
            }
            if more.iter().any(|e| matches!(e, Effect::Persist(_))) {
                more.push(quiet_timer(state, now.wall));
            }
            fold_in(&mut fx, more);
            // Whatever moved, a session a user can now see becomes seen.
            mark_seen_into(state, now, &mut fx);
            touched_into(state, now, &mut fx);
            // After every record this input changed: `term ls` tells them as they now are.
            fold_in(&mut fx, ops::term::ls_answers(state, now, link_listed));
            fx
        }
        Input::Hook { reply, envelope } => {
            let mut fx = hooks::run(state, now, reply, &envelope);
            if fx.iter().any(|e| matches!(e, Effect::Persist(_))) {
                fx.push(quiet_timer(state, now.wall));
            }
            fx
        }
        Input::Timer(TimerKey::Backup) => {
            vec![Effect::Backup { day: now.local_day }, backup_timer(now)]
        }
        // The status clock reads the time when `handle` ends.
        Input::Timer(TimerKey::Clock) => {
            state.tmux.clock_fired();
            Vec::new()
        }
        // Sessions that just went quiet show differently: tell the views.
        Input::Timer(TimerKey::Quiet) => {
            let topics = BTreeSet::from([Topic::Sessions, Topic::Status]);
            let mut fx: Vec<Effect> = state
                .publish(&topics)
                .map(|m| Effect::Publish(Publish::Model(m)))
                .into_iter()
                .collect();
            fx.push(quiet_timer(state, now.wall));
            fx
        }
        // The checks at the end of every input do the work.
        Input::Timer(TimerKey::Sleep | TimerKey::Terminals) => Vec::new(),
        // Its waiters are dropped; a launch it asked for goes on, and a terminal's tab still
        // gets its tags (R26). A panel on it goes, and `panels` says so; a popup's going may
        // nudge tmux to reap it.
        Input::ClientGone(conn) => {
            // A panel moa placed keeps its connection for life: once it goes, its program
            // has ended, and a listing shows whether its pane is dead (L12).
            let ended = state
                .panels
                .get(&conn)
                .is_some_and(|r| r.placed && r.pane.is_some());
            state.tmux.client_gone(conn);
            state.term_ls.retain(|(c, _)| *c != conn);
            cockpit::run::client_gone(state, conn);
            let mut fx = panel::gone(state, conn);
            if ended {
                fx.extend(state.tmux.refresh());
            }
            fx
        }
        // Reconcile waits for it (R2): the connection's panes are looked at now, or, when
        // the link found no server, every session that ran in one goes to sleep.
        Input::StartDrained => {
            state.tmux.drained = true;
            let mut fx: Vec<Effect> = reconcile::ask_if_due(state, now).into_iter().collect();
            if state.tmux().link == Link::Down && state.tmux().last_down == Some(Down::NoServer) {
                fx.extend(reconcile::no_server(state, now));
            }
            if fx.iter().any(|e| matches!(e, Effect::Persist(_))) {
                fx.push(quiet_timer(state, now.wall));
            }
            fx
        }
        Input::ZoneChanged => vec![backup_timer(now)],
        Input::ActorFailed { actor, message: _ } => {
            // The message may carry contents; it stays with the executor.
            vec![
                Effect::Publish(Publish::Event(CoreEvent::Error {
                    message: format!("{actor} stopped and was restarted"),
                })),
                Effect::Log(LogLine {
                    level: LogLevel::Error,
                    message: format!("actor {actor} failed and was restarted"),
                }),
            ]
        }
        Input::Client {
            conn,
            req,
            op: Op::TmuxEnsure { tmux },
            ..
        } => state.tmux.ensure(conn, req, tmux),
        // The panel registry is by connection, in memory: nothing is saved.
        Input::Client {
            conn,
            req,
            op:
                Op::UiPanel {
                    pane,
                    view,
                    placed,
                    once,
                },
            ..
        } => panel::register(state, conn, req, pane.as_deref(), view, placed, once),
        Input::Client {
            conn,
            req,
            op: Op::UiHere { folder },
            ..
        } => panel::here(state, conn, req, folder),
        // A sheet's close runs in the cockpit's queue (S9, L17); it is answered at once.
        Input::Client {
            conn,
            req,
            op: Op::UiClose(_),
            ..
        } => cockpit::run::ui_close(state, conn, req),
        // `layouts.json` is read again first, and only the daemon writes it: nothing is saved.
        Input::Client {
            conn,
            req,
            op: op @ (Op::LayoutList(_) | Op::LayoutShow { .. } | Op::LayoutSet { .. }),
            ..
        } => ops::layout::run(state, conn, req, op),
        // Each runs in the cockpit's queue, from a listing of its own (L17), and is answered
        // once its chain is.
        Input::Client {
            conn,
            req,
            op: Op::LayoutApply { name },
            ..
        } => cockpit::run::apply(state, conn, req, name),
        Input::Client {
            conn,
            req,
            op: Op::Goto { panel },
            ..
        } => cockpit::run::goto(state, conn, req, panel),
        Input::Client {
            conn,
            req,
            op: Op::UiSelectView { pane, view },
            ..
        } => cockpit::run::select_view(state, conn, req, &pane, view),
        // Answered once tmux says how the new tab turned out. Every tab opens detached
        // (`-d`); unless `bg`, it is brought to the screen once it is bound.
        Input::Client {
            conn,
            req,
            op:
                Op::SessionOpen {
                    path,
                    account,
                    title,
                    group,
                    prompt,
                    bg,
                },
            ..
        } => {
            let asked = ops::launch::OpenArgs {
                path: &path,
                account: account.as_deref(),
                title: title.as_deref(),
                group: group.as_deref(),
                prompt: prompt.as_deref(),
            };
            ops::launch::open_told(state, now, &asked, Tell::Reply(conn, req), !bg)
        }
        // Answered now, or once the tab it waits for is bound.
        Input::Client {
            conn,
            req,
            op: Op::SessionFocus { q },
            ..
        } => ops::launch::focus(state, now, conn, req, &q),
        // Answered once tmux says which pane the new tab has. It binds nothing, so it
        // needs the link up but not the tabs reconciled.
        Input::Client {
            conn,
            req,
            op: Op::TermOpen { path, title },
            ..
        } => ops::term::term(state, Tell::Reply(conn, req), &path, title.as_deref()),
        // Answered once a fresh listing of the tabs lands, or at once with the link down.
        Input::Client {
            conn,
            req,
            op: Op::TermLs(_),
            ..
        } => ops::term::start_ls(state, now, conn, req),
        // Answered at once, from the topology: nothing is saved.
        Input::Client {
            conn,
            req,
            op: Op::TermShow { pane },
            ..
        } => ops::term::show(state, conn, req, &pane),
        // Answered once tmux says what runs in the terminal's tab, or at once.
        Input::Client {
            conn,
            req,
            op: Op::TermClose { target },
            ..
        } => ops::term::close(state, conn, req, &target),
        // Answered at once, or after one pane query and the new tab (C11).
        Input::Client {
            conn,
            req,
            op: Op::SessionResume { q, bg },
            ..
        } => ops::launch::start_resume(state, now, conn, req, &q, !bg, false),
        // Answered at once, or after one pane query and the new tab (R24).
        Input::Client {
            conn,
            req,
            op: Op::SessionRestart { q },
            ..
        } => ops::launch::start_restart(state, now, conn, req, &q),
        // Answered at once, or after one pane query (R21).
        Input::Client {
            conn,
            req,
            op:
                Op::SessionAdopt {
                    pane,
                    title,
                    group,
                    id,
                },
            ..
        } => ops::adopt::start(state, conn, req, &pane, title, group, id),
        // With the tabs reconciled, answered after one listing of them (R23); otherwise at
        // once, below, without the agents moa doesn't track.
        Input::Client {
            conn,
            req,
            op: Op::SessionLs { all },
            ..
        } if link::ready(state).is_ok() => ops::sessions::start_ls(state, now, conn, req, all),
        // Answered once the prompt's row is saved, or with `wait` when moa's turn ends.
        Input::Client {
            conn,
            req,
            op: Op::ConsolePrompt { text, wait },
            ..
        } => central::send(state, now, text, false, Some(Waiter { conn, req, wait })),
        Input::Client {
            conn,
            req,
            op: Op::ConsoleCancel(_),
            ..
        } => central::cancel(state, conn, req),
        Input::Acp(heard) => central::heard(state, now, heard),
        Input::Client {
            conn,
            req,
            caller,
            op,
        } => {
            let caller = state.caller(&caller);
            let mut tx = Tx::new(now);
            let mut fx = Vec::new();
            let result = ops::run(state, &mut tx, &caller, op, now, &mut fx);
            fx.extend(finish_op(state, tx, conn, req, result));
            fx
        }
    }
}

/// An op's transaction, then its reply. Handlers commit once, last: a refusal has applied
/// nothing, so there is nothing to persist that the reply would deny, and nothing it queued
/// (an event, a tmux command) is said.
pub(crate) fn finish_op(
    state: &mut State,
    mut tx: Tx,
    conn: ConnId,
    req: ReqId,
    result: Result<Done, OpError>,
) -> Vec<Effect> {
    debug_assert!(
        result.is_ok() || tx.changes.is_empty(),
        "an op returned Err after committing"
    );
    if result.is_err() {
        tx.events.clear();
        tx.cmds.clear();
        tx.vacates.clear();
    }
    let mut fx = post_commit(state, tx);
    fx.push(Effect::Reply { conn, req, result });
    fx
}

/// What a transaction's commits owe the world, in order: the `Persist`, the model it
/// changed, its events, then its tmux commands (R15), then its vacates queued in the
/// cockpit's queue (L17). The caller adds any reply after. The commands go only for a saved
/// change, on the link's connection: with nothing saved or the link down, they are
/// dropped. A vacate of a saved change waits for the link instead.
pub(crate) fn post_commit(state: &mut State, tx: Tx) -> Vec<Effect> {
    let Tx {
        changes,
        events,
        cmds,
        vacates,
        ..
    } = tx;
    let topics = topics_of(&changes);
    let saved = !changes.is_empty();
    let mut fx = Vec::new();
    if saved {
        fx.push(Effect::Persist(changes));
    }
    if let Some(model) = state.publish(&topics) {
        fx.push(Effect::Publish(Publish::Model(model)));
    }
    fx.extend(
        events
            .into_iter()
            .map(|e| Effect::Publish(Publish::Event(e))),
    );
    if !cmds.is_empty() {
        match state.tmux().link {
            Link::Up { conn, .. } if saved => fx.push(Effect::Tmux {
                conn,
                cmds,
                token: None,
            }),
            _ => fx.push(Effect::Log(LogLine {
                level: LogLevel::Debug,
                message: format!(
                    "{} tmux commands dropped: nothing saved, or no link",
                    cmds.len()
                ),
            })),
        }
    }
    if saved {
        for (pane, id) in vacates {
            fx.extend(cockpit::run::queue(
                state,
                cockpit::run::CockpitOp::Vacate { pane, id },
            ));
        }
    }
    fx
}

/// Runs `mark_seen` and adds what it changed to `fx` ([`fold_in`]).
pub(crate) fn mark_seen_into(state: &mut State, now: &Now, fx: &mut Vec<Effect>) {
    let mut tx = Tx::new(now);
    mark_seen(state, &mut tx, now);
    if tx.changes.is_empty() {
        return;
    }
    let more = post_commit(state, tx);
    fold_in(fx, more);
}

/// Runs `terminals::touched` and adds what it changed to `fx` ([`fold_in`]).
fn touched_into(state: &mut State, now: &Now, fx: &mut Vec<Effect>) {
    let before = state.looked_at.clone();
    let changes = terminals::touched(state, now.wall);
    if changes.is_empty() {
        return;
    }
    let mut tx = Tx::new(now);
    if tx.commit(state, changes).is_ok() {
        let more = post_commit(state, tx);
        fold_in(fx, more);
    } else {
        // Not saved: the next input stamps it again.
        state.looked_at = before;
    }
}

/// Adds a later commit's effects to `fx`: each `Persist` after any `Persist` already there
/// (the store applies them in the order they were made) and before any reply or view, and
/// the rest after.
pub(crate) fn fold_in(fx: &mut Vec<Effect>, more: Vec<Effect>) {
    let mut at = fx.iter().position(Effect::is_heard).unwrap_or(fx.len());
    for e in more {
        if matches!(e, Effect::Persist(_)) {
            fx.insert(at, e);
            at += 1;
        } else {
            fx.push(e);
        }
    }
}

/// The topics a set of changes touches.
fn topics_of(changes: &[Change]) -> BTreeSet<Topic> {
    changes
        .iter()
        .flat_map(|c| c.topics().iter().copied())
        .collect()
}

fn backup_timer(now: &Now) -> Effect {
    Effect::SetTimer {
        key: TimerKey::Backup,
        at: start_of_next_day(now.local_day, &now.tz),
    }
}

/// The one deadline for sessions turning quiet: the earliest `activity_at + QUIET_AFTER`
/// still ahead among sessions that are working, or no timer when there is none.
pub(crate) fn quiet_timer(state: &State, now: UtcMs) -> Effect {
    let next = state
        .sessions()
        .filter(|s| quiet_candidate(s))
        .filter_map(|s| s.activity_at)
        .map(|t| UtcMs(t.0 + QUIET_AFTER))
        .filter(|at| *at > now)
        .min();
    match next {
        Some(at) => Effect::SetTimer {
            key: TimerKey::Quiet,
            at,
        },
        None => Effect::CancelTimer(TimerKey::Quiet),
    }
}

fn publish_all(state: &mut State) -> Option<Effect> {
    let all: BTreeSet<Topic> = Topic::ALL.iter().copied().collect();
    state
        .publish(&all)
        .map(|m| Effect::Publish(Publish::Model(m)))
}

/// One op's edits. Handlers validate, build their changes, then call [`Tx::commit`] once: a
/// handler that returns `Err` has applied nothing.
pub(crate) struct Tx {
    pub(crate) wall: UtcMs,
    /// The local date of `wall`.
    pub(crate) today: jiff::civil::Date,
    /// Everything committed so far, in order.
    pub(crate) changes: Vec<Change>,
    pub(crate) events: Vec<CoreEvent>,
    /// tmux commands for after the commit (R15): see [`Tx::tmux`].
    pub(crate) cmds: Vec<Cmd>,
    /// Agents' tabs taken down through the cockpit's queue after the commit: see
    /// [`Tx::vacate`].
    pub(crate) vacates: Vec<(PaneId, SessionId)>,
    /// The last id handed out per table, for ids asked for before the commit.
    issued: Vec<(Table, i64)>,
}

impl Tx {
    pub(crate) fn new(now: &Now) -> Tx {
        Tx {
            wall: now.wall,
            today: now.local_day,
            changes: Vec::new(),
            events: Vec::new(),
            cmds: Vec::new(),
            vacates: Vec::new(),
            issued: Vec::new(),
        }
    }

    /// Queues a tmux command for after this transaction's commit, on the link's connection
    /// ([`post_commit`]). Only after a successful [`Tx::commit`]: a kill must never go out
    /// for a change the store refused.
    pub(crate) fn tmux(&mut self, cmd: Cmd) {
        debug_assert!(!self.changes.is_empty(), "a tmux command before any commit");
        self.cmds.push(cmd);
    }

    /// Queues session `id`'s agent in `pane` to be taken down by the cockpit's queue once
    /// this transaction is saved (L17), as [`Tx::tmux`] would kill it: only after a
    /// successful [`Tx::commit`].
    pub(crate) fn vacate(&mut self, pane: PaneId, id: SessionId) {
        debug_assert!(!self.changes.is_empty(), "a vacate before any commit");
        self.vacates.push((pane, id));
    }

    /// Checks every change, then applies all of them to `state` and records them. A refusal
    /// applies none.
    pub(crate) fn commit(
        &mut self,
        state: &mut State,
        changes: Vec<Change>,
    ) -> Result<(), OpError> {
        if let Some(r) = state.refuse_writes()
            && self.wall < r.until
        {
            return Err(OpError(format!(
                "moa can't save right now ({}); nothing was changed",
                r.reason
            )));
        }
        // Most brains have no unknown rows; then no key needs building.
        let checked = if state.unknown().is_empty() {
            &[][..]
        } else {
            &changes[..]
        };
        for c in checked {
            let Some((table, key)) = c.row() else {
                continue;
            };
            if state.is_unknown(table, &key) {
                return Err(OpError(unknown_message(state, table, &key)));
            }
        }
        for c in &changes {
            state.apply(c);
        }
        self.changes.extend(changes);
        Ok(())
    }

    /// The id the next row of `table` takes, counting ids already handed out by this
    /// transaction.
    pub(crate) fn next_id(&mut self, state: &State, table: Table) -> i64 {
        let want = state.next_id(table);
        match self.issued.iter_mut().find(|(t, _)| *t == table) {
            Some((_, last)) => {
                *last = (*last + 1).max(want);
                *last
            }
            None => {
                self.issued.push((table, want));
                want
            }
        }
    }

    /// Commits `changes` together with a `ui` event and its activity row (`who = you`, the
    /// same text), as the spike's `log_ui` and `a_event` trigger did, and queues the
    /// `Ui` event for the views.
    pub(crate) fn ui(
        &mut self,
        state: &mut State,
        changes: Vec<Change>,
        text: String,
    ) -> Result<(), OpError> {
        self.ui_each(state, changes, vec![text])
    }

    /// An activity row stamped with this transaction's time, for the caller to commit.
    /// `of` is the `(ref_kind, ref_id)` the row points at.
    pub(crate) fn activity_row(
        &mut self,
        state: &State,
        who: ActivityWho,
        text: String,
        of: Option<(&str, String)>,
    ) -> Change {
        let (ref_kind, ref_id) = match of {
            Some((kind, id)) => (Some(kind.to_string()), Some(id)),
            None => (None, None),
        };
        Change::Activity(ActivityRow {
            id: ActivityId(self.next_id(state, Table::Activity)),
            at: self.wall,
            who,
            text,
            ref_kind,
            ref_id,
        })
    }

    /// [`Tx::ui`] with one `ui` event and activity row per text, all in one commit.
    pub(crate) fn ui_each(
        &mut self,
        state: &mut State,
        changes: Vec<Change>,
        texts: Vec<String>,
    ) -> Result<(), OpError> {
        self.ui_by(state, changes, texts, ActivityWho::You)
    }

    /// [`Tx::ui_each`] with the activity rows written by `who`.
    pub(crate) fn ui_by(
        &mut self,
        state: &mut State,
        mut changes: Vec<Change>,
        texts: Vec<String>,
        who: ActivityWho,
    ) -> Result<(), OpError> {
        for text in &texts {
            changes.push(Change::Event(Event {
                id: EventId(self.next_id(state, Table::Event)),
                at: self.wall,
                session_id: None,
                conversation_id: None,
                account: None,
                cwd: None,
                kind: EventKind(EventKind::UI.to_string()),
                text: Some(text.clone()),
                detail: None,
            }));
            changes.push(self.activity_row(state, who, text.clone(), None));
        }
        self.commit(state, changes)?;
        self.events
            .extend(texts.into_iter().map(|text| CoreEvent::Ui { text }));
        Ok(())
    }
}

/// `<table word> <key> has a value this build doesn't know; update moa`.
pub(crate) fn unknown_message(state: &State, table: &str, key: &str) -> String {
    let word = match table {
        "projects" => "project",
        "tasks" => "task",
        "notes" => "note",
        "suggestions" => "suggestion",
        "sessions" => "session",
        "conversations" => "conversation",
        "groups" => "group",
        "asks" => "ask",
        "moa_threads" => "thread",
        "nags" => "nag",
        "terminals" => "terminal",
        "pane_bindings" | "spike_panes" => "pane",
        other => other,
    };
    let key = match table {
        "sessions" => key
            .parse()
            .map(|id| state.handle_of(id))
            .unwrap_or_else(|_| key.to_string()),
        _ => key.to_string(),
    };
    format!("{word} {key} has a value this build doesn't know; update moa")
}

#[cfg(test)]
mod tests;
