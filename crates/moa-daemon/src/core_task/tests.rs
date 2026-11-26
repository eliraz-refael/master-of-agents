//! The start drain's signal to the core: what the core steps, in order, as a start runs. How
//! drains are cut around moa's turns, and the link to moa's agent failing.

use std::time::Duration;

use moa_core::config::BrainName;
use moa_core::state::PanelLaunch;
use moa_testkit::brain::{TYPICAL_TZ, TypicalBrain};
use moa_testkit::{
    StubStep, moa_toml_with_adapter, stub_acp, stub_log, wait_for, write_stub_script,
};
use serde_json::json;
use tokio::time::timeout;

use super::*;
use crate::acp::AcpMsg;
use crate::testing::FakeTime;

const PATIENCE: Duration = Duration::from_secs(10);

fn options(brain: &TypicalBrain, time: &Arc<FakeTime>, spool: Option<PathBuf>) -> CoreOptions {
    CoreOptions {
        brain: BrainName::default_brain(),
        config: brain.config(),
        deps: Deps::default(),
        tz: TimeZone::posix(TYPICAL_TZ).unwrap(),
        clock: time.clone(),
        alarm: Box::new(time.alarm()),
        db: brain.db.clone(),
        backups: brain.dir.path().join("backups"),
        events_capacity: 16,
        tmux: None,
        spool,
        facts: None,
    }
}

/// A hook line for a session moa doesn't know: applied as history, or not at all, it is
/// still one `Hook` input.
fn hook_line(seq: u32) -> Vec<u8> {
    let conv = "c0ffee00-0000-4000-8000-000000000001";
    json!({
        "v": 1,
        "agent": "claude",
        "at_ns": 1_700_000_000_000_000_000i64 + i64::from(seq),
        "pid": 100,
        "seq": seq,
        "env": {"claude_session_id": conv},
        "payload": {"session_id": conv, "hook_event_name": "Stop"},
    })
    .to_string()
    .into_bytes()
}

/// The kinds of inputs the core stepped by the time a start has settled, the first `panics`
/// after `Startup` panicking. The link is a channel that is kept open, so `Startup`'s connect
/// adds no input of its own.
async fn stepped_at_start(
    brain: &TypicalBrain,
    spool: Option<PathBuf>,
    panics: u32,
) -> Vec<&'static str> {
    let time = FakeTime::new(brain.now().wall);
    let (link, _commands) = mpsc::unbounded_channel();
    let core = timeout(
        PATIENCE,
        CoreHandle::start_inner(
            options(brain, &time, spool),
            brain.store(),
            vec![],
            Some(link),
            None,
            panics,
        ),
    )
    .await
    .expect("the core starts in time")
    .expect("the core starts");
    timeout(PATIENCE, core.barrier())
        .await
        .expect("the barrier answers");
    let kinds = lock(&core.stats.stepped).clone();
    timeout(PATIENCE, core.stop())
        .await
        .expect("the core stops");
    kinds
}

/// R2: the core hears `StartDrained` once, after every hook the start drain fed it, or
/// right after `Startup` when there is no spool.
#[tokio::test]
async fn start_drained_is_sent_after_the_drain() {
    let brain = TypicalBrain::new();
    let spool = brain.dir.path().join("spool").join("default.jsonl");
    for seq in 1..=3 {
        spool::append(&spool, &hook_line(seq)).unwrap();
    }
    assert_eq!(
        stepped_at_start(&brain, Some(spool.clone()), 0).await,
        ["Startup", "Hook", "Hook", "Hook", "StartDrained"]
    );
    assert!(!spool::draining(&spool).exists(), "the drain was committed");

    let brain = TypicalBrain::new();
    assert_eq!(
        stepped_at_start(&brain, None, 0).await,
        ["Startup", "StartDrained"]
    );
}

/// A `StartDrained` whose step panics is sent again after the reload, before anything that
/// waited behind it: otherwise reconcile would wait for it until the next start.
#[tokio::test]
async fn a_start_drained_that_panics_is_sent_again() {
    let brain = TypicalBrain::new();
    // An empty spool: the panic falls on `StartDrained`.
    let spool = brain.dir.path().join("spool").join("default.jsonl");
    assert_eq!(
        stepped_at_start(&brain, Some(spool), 1).await,
        ["Startup", "StartDrained", "Reload", "StartDrained"]
    );
}

/// A core whose moa runs on `acct-a`, with `program` as its agent (`fake-claude` as its
/// Claude) and `agent_dir` as its folder, under a fresh `/tmp` home; `agent`, when given,
/// stands in for the link to moa's agent.
async fn moa_core(
    brain: &TypicalBrain,
    home: &Path,
    program: &Path,
    agent: Option<mpsc::UnboundedSender<AcpMsg>>,
) -> CoreHandle {
    let toml = moa_toml_with_adapter(program);
    let time = FakeTime::new(brain.now().wall);
    let mut opts = options(brain, &time, None);
    opts.config = Config::from_file_text(Some(&toml), home, Path::to_path_buf).unwrap();
    opts.deps = Deps {
        home: home.to_path_buf(),
        agent_dir: home.join("agent"),
        panel: PanelLaunch {
            exe: stub_acp(),
            env: Vec::new(),
        },
        ..Deps::default()
    };
    let (link, _commands) = mpsc::unbounded_channel();
    timeout(
        PATIENCE,
        CoreHandle::start_inner(opts, brain.store(), vec![], Some(link), agent, 0),
    )
    .await
    .expect("the core starts in time")
    .expect("the core starts")
}

/// A fresh home under `/tmp`, with its agent folder made.
fn moa_home() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("moa")
        .tempdir_in("/tmp")
        .unwrap();
    moa_platform::ensure_private_dir(&dir.path().join("agent")).unwrap();
    dir
}

fn prompt_op(text: &str, wait: bool) -> Op {
    Op::ConsolePrompt {
        text: text.into(),
        wait,
    }
}

/// The link to moa's agent panics while moa is answering: whoever waits hears that moa's
/// agent stopped (the live generation's `Exited`), the core says the link was restarted, and
/// the next prompt starts the agent again on the restarted link.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panicking_acp_actor_is_restarted_and_says_so() {
    let brain = TypicalBrain::new();
    let dir = moa_home();
    let agent = dir.path().join("agent");
    let say = |text: &str, id: &str| StubStep::Say {
        text: text.into(),
        id: Some(id.into()),
    };
    let steps = [
        say("thinking", "m1"),
        StubStep::Hang,
        say("back", "m2"),
        StubStep::End { stop: None },
    ];
    write_stub_script(&agent, &steps);
    let core = moa_core(&brain, dir.path(), &stub_acp(), None).await;
    let mut events = core.events();
    let (conn, mut replies) = core.open_conn();
    let prompt = |req, text: &str| {
        core.send(
            conn,
            ReqId(req),
            CallerInfo::default(),
            prompt_op(text, true),
        );
    };

    prompt(1, "wait for it");
    // Its first words are saved: the stub has counted the turn, and hangs.
    let thinking = || {
        let store = Store::open_read_only(&brain.db).ok()?;
        let n: i64 = store
            .conn()
            .query_row("SELECT count(*) FROM feed WHERE kind = 'moa'", [], |r| {
                r.get(0)
            })
            .ok()?;
        (n == 1).then_some(())
    };
    wait_for(PATIENCE, thinking).expect("moa is answering");
    core.acp.send(AcpMsg::Crash).expect("the link takes it");

    let (req, told) = timeout(PATIENCE, replies.recv())
        .await
        .expect("the waiter is told in time")
        .expect("a reply");
    assert_eq!(req, ReqId(1));
    let told = told.expect_err("moa's agent stopped").0;
    assert!(told.starts_with("moa's agent stopped: "), "{told}");
    let said = timeout(PATIENCE, async {
        loop {
            if let Ok(CoreEvent::Error { message }) = events.recv().await
                && message.contains("moa's agent")
                && message.ends_with("stopped and was restarted")
            {
                return message;
            }
        }
    })
    .await
    .expect("the core says the link was restarted");
    assert_eq!(said, "the link to moa's agent stopped and was restarted");

    prompt(2, "again");
    let (req, answered) = timeout(PATIENCE, replies.recv())
        .await
        .expect("answered in time")
        .expect("a reply");
    assert_eq!(req, ReqId(2));
    assert_eq!(answered.expect("moa answers").text, "back");
    let started = stub_log(&agent)
        .iter()
        .filter(|l| l["method"] == "initialize")
        .count();
    assert_eq!(started, 2, "the agent started again");
    timeout(PATIENCE, core.stop())
        .await
        .expect("the core stops");
}

/// The end of moa's turn starts a drain of its own, so a failed commit of the updates before
/// it is stepped again while the turn is still under way.
#[test]
fn the_end_of_moas_turn_starts_a_drain() {
    use moa_core::central::wire::{AcpIn, StopReason, Update};

    let acp = |input| Item::Input(Input::Acp(input));
    let update = || {
        acp(AcpIn::Update {
            generation: 1,
            thread: "t".into(),
            update: Update::Message {
                id: None,
                text: "hi".into(),
            },
        })
    };
    let ended = || {
        acp(AcpIn::TurnEnded {
            generation: 1,
            prompt: 1,
            result: Ok(StopReason::EndTurn),
        })
    };
    let is_end = |item: &Item| matches!(item, Item::Input(Input::Acp(AcpIn::TurnEnded { .. })));
    let (tx, mut inbox) = mpsc::unbounded_channel();
    for item in [update(), update(), ended(), update()] {
        tx.send(item).unwrap();
    }
    let mut carry = VecDeque::new();

    let first = drain::take_drain(&mut carry, &mut inbox);
    assert_eq!(first.len(), 2);
    assert!(!first.iter().any(is_end));
    let second = drain::take_drain(&mut carry, &mut inbox);
    assert_eq!(second.len(), 2);
    assert!(is_end(&second[0]));
}

/// The link panics while moa's agent starts, before the agent has said anything: the link's
/// own `Exited` is the only word of it, and whoever waits hears it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_link_that_fails_while_moas_agent_starts_says_so() {
    use std::os::unix::fs::PermissionsExt as _;

    let brain = TypicalBrain::new();
    let dir = moa_home();
    // An agent that never answers `initialize`: the stub in its child mode, which only sleeps.
    let silent = dir.path().join("silent-agent");
    let script = format!("#!/bin/sh\nexec '{}' --child\n", stub_acp().display());
    std::fs::write(&silent, script).unwrap();
    std::fs::set_permissions(&silent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let core = moa_core(&brain, dir.path(), &silent, None).await;
    let (conn, mut replies) = core.open_conn();
    let launched = moa_platform::spawn::launches();

    core.send(
        conn,
        ReqId(1),
        CallerInfo::default(),
        prompt_op("hello", true),
    );
    wait_for(PATIENCE, || {
        (moa_platform::spawn::launches() > launched).then_some(())
    })
    .expect("the agent is starting");
    core.acp.send(AcpMsg::Crash).expect("the link takes it");

    let (req, told) = timeout(PATIENCE, replies.recv())
        .await
        .expect("the waiter is told in time")
        .expect("a reply");
    assert_eq!(req, ReqId(1));
    assert_eq!(
        told.expect_err("the start failed").0,
        "moa's agent couldn't start: the link to moa's agent failed"
    );
    timeout(PATIENCE, core.stop())
        .await
        .expect("the core stops");
}

/// A cancel stepped in the same drain as the prompt it cancels, which waits for the drain's
/// commit, reaches moa's agent after that prompt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_in_its_prompts_drain_follows_the_prompt() {
    use moa_core::central::wire::{AcpCmd, AcpIn, Caps, Opened, StopReason, Update};
    use moa_core::op::NoParams;
    use moa_store::actor::Fault;

    let brain = TypicalBrain::new();
    let dir = moa_home();
    let (agent, mut sent) = mpsc::unbounded_channel();
    let core = moa_core(&brain, dir.path(), &stub_acp(), Some(agent)).await;
    let (conn, mut replies) = core.open_conn();
    let heard = |input| {
        let _ = core.inbox.send(Item::Input(Input::Acp(input)));
    };
    let mut next = async || match timeout(PATIENCE, sent.recv()).await {
        Ok(Some(AcpMsg::Cmd(cmd))) => cmd,
        _ => panic!("a command for moa's agent in time"),
    };

    // The first prompt starts moa's agent and opens its thread; moa is then answering.
    core.send(
        conn,
        ReqId(1),
        CallerInfo::default(),
        prompt_op("one", false),
    );
    assert!(matches!(next().await, AcpCmd::Start { generation: 1, .. }));
    let caps = Caps {
        load: true,
        resume: true,
        system_prompt: true,
    };
    heard(AcpIn::Started {
        generation: 1,
        caps,
    });
    assert!(matches!(next().await, AcpCmd::Open { .. }));
    heard(AcpIn::Opened {
        generation: 1,
        result: Ok(("t1".into(), Opened::New)),
    });
    assert!(matches!(next().await, AcpCmd::Prompt { prompt: 1, .. }));

    // A piece of the answer whose commit waits: what comes meanwhile makes one drain, the
    // turn's end first (it starts a drain of its own), then the next prompt and its cancel.
    let (resume, paused) = std::sync::mpsc::channel();
    core.store_fault(Fault::PauseNextCommit(paused));
    heard(AcpIn::Update {
        generation: 1,
        thread: "t1".into(),
        update: Update::Message {
            id: Some("m1".into()),
            text: "hi".into(),
        },
    });
    heard(AcpIn::TurnEnded {
        generation: 1,
        prompt: 1,
        result: Ok(StopReason::EndTurn),
    });
    core.send(
        conn,
        ReqId(2),
        CallerInfo::default(),
        prompt_op("two", false),
    );
    core.send(
        conn,
        ReqId(3),
        CallerInfo::default(),
        Op::ConsoleCancel(NoParams),
    );
    resume.send(()).unwrap();

    assert!(matches!(next().await, AcpCmd::Prompt { prompt: 2, .. }));
    assert!(matches!(next().await, AcpCmd::Cancel { .. }));
    // Each was answered, the cancel as taken.
    let mut answers = Vec::new();
    while answers.len() < 3 {
        let (req, result) = timeout(PATIENCE, replies.recv())
            .await
            .expect("answered in time")
            .expect("a reply");
        answers.push((req, result.map(|d| d.text)));
    }
    assert_eq!(
        answers,
        [
            (ReqId(1), Ok("moa has it".to_string())),
            (ReqId(2), Ok("moa has it".to_string())),
            (ReqId(3), Ok("cancelling moa's turn".to_string())),
        ]
    );
    drop(sent);
    timeout(PATIENCE, core.stop())
        .await
        .expect("the core stops");
}
