//! Slice 7's scenarios, end to end through the real daemon: asks between sessions whose agents
//! (`fake-claude`) run in tabs of a real terminal server and read what moa types there as
//! Claude does, and asks to and from moa (`stub-acp`). Time moves only by `FakeTime::advance`.

mod common;

use std::time::Duration;

use common::create_server;
use common::rig::{Rig, Tab, Up, WAIT, answer, end, gate, refusal, say};
use moa_core::asks::{ASK_ENTER_MS, ASK_RECEIPT_MS, NO_RECEIPT};
use moa_testkit::{FakeStep, StubStep, stub_acp, wait_for};
use serde_json::{Map, Value, json};

/// Harbor's answer: more than one line, as a turn's last message often is.
const RENEWED: &str = "Renewed through May.\n\nThe invoice is in ~/private/Harbor.";

const DOMAIN: &str = "Is the domain renewed?";

/// What moa types into Harbor's tab for ask `n` from `from` (A13).
fn framed(n: i64, from: &str, text: &str) -> String {
    format!(
        "[moa · message from {from} · ask #{n}] {text}\n\n\
         (Your reply this turn goes back to {from} as the answer.)"
    )
}

/// The time from a paste to its Enter.
fn enter() -> Duration {
    Duration::from_millis(u64::try_from(ASK_ENTER_MS).unwrap())
}

/// The agent answers the next ask typed into its tab with `reply`, then sits there.
fn answers(reply: &str) -> Vec<FakeStep> {
    vec![
        FakeStep::Answer {
            reply: reply.into(),
        },
        FakeStep::Hang,
    ]
}

fn hook(event: &str, key: &str, value: &str) -> FakeStep {
    let mut fields = Map::new();
    fields.insert(key.into(), json!(value));
    FakeStep::Hook {
        event: event.into(),
        fields,
    }
}

/// The agent starts a turn of the user's once `busy` opens, ends it once `done` opens, then
/// answers the next ask with `reply`.
fn busy_then_answers(reply: &str) -> Vec<FakeStep> {
    let mut steps = vec![
        FakeStep::Wait {
            file: "busy".into(),
        },
        hook("UserPromptSubmit", "prompt", "light the Lantern"),
        FakeStep::Wait {
            file: "done".into(),
        },
        hook("Stop", "last_assistant_message", "the Lantern is lit"),
    ];
    steps.extend(answers(reply));
    steps
}

/// `steps` after a start: the agent waits for the gate `go`, then fires `SessionStart`, as
/// an agent moa launched does once it is up. Its prompt box is ready by then.
fn started(steps: &[FakeStep]) -> Vec<FakeStep> {
    let mut all = vec![
        FakeStep::Wait { file: "go".into() },
        hook("SessionStart", "source", "startup"),
    ];
    all.extend_from_slice(steps);
    all
}

/// A daemon linked to a real terminal server, where Kite (session 1) and Harbor (session 2)
/// each run their agent in a tab of their own, started ([`started`]).
struct Cockpit {
    rig: Rig,
    up: Up,
    kite: Tab,
    harbor: Tab,
}

impl Cockpit {
    async fn new(moa: &[StubStep], kite: &[FakeStep], harbor: &[FakeStep]) -> Cockpit {
        let rig = Rig::new(moa);
        // A first start writes the server's config, which the server starts with.
        rig.start().await.stop().await;
        create_server(&rig.tb, &rig.paths);
        let (kite, mut rows) = rig.agent_tab(1, "Kite", &started(kite));
        let (harbor, more) = rig.agent_tab(2, "Harbor", &started(harbor));
        rows.extend(more);
        rig.write(&rows);
        let up = rig.start().await;
        let c = Cockpit {
            rig,
            up,
            kite,
            harbor,
        };
        c.link();
        // Each agent starts once moa can hear it: its start is through once its context is
        // back (an empty one makes the file too).
        for tab in [&c.kite, &c.harbor] {
            tab.open("go");
            let context = tab.dir.join(".fake-claude.context");
            wait_for(WAIT, || context.exists().then_some(()))
                .unwrap_or_else(|| panic!("{} never started", tab.dir.display()));
        }
        c
    }

    /// Links the daemon, and waits until it sees both sessions live: `peers` lists them.
    fn link(&self) {
        self.up.link();
        let mut moa = self.up.moa_client();
        wait_for(WAIT, || {
            let peers = moa.call("peers", json!({}));
            let labels: Vec<&str> = peers["result"]["data"]
                .as_array()?
                .iter()
                .filter_map(|p| p["label"].as_str())
                .collect();
            (labels == ["Kite", "Harbor"] || labels == ["Harbor", "Kite"]).then_some(())
        })
        .unwrap_or_else(|| {
            let peers = moa.call("peers", json!({}));
            panic!("Kite and Harbor aren't both live: {peers}")
        });
    }

    /// Stops the daemon and starts it again, linked.
    async fn restart(self) -> Cockpit {
        let Cockpit {
            rig,
            up,
            kite,
            harbor,
        } = self;
        up.stop().await;
        let up = rig.start().await;
        let c = Cockpit {
            rig,
            up,
            kite,
            harbor,
        };
        c.link();
        c
    }

    /// Ask `id`'s `(status, reply)` as saved.
    fn ask(&self, id: i64) -> Option<(String, Option<String>)> {
        self.rig
            .store()
            .conn()
            .query_row("SELECT status, reply FROM asks WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .ok()
    }

    /// Waits until ask `id` is saved as `status`.
    fn wait_ask(&self, id: i64, status: &str) {
        wait_for(WAIT, || (self.ask(id)?.0 == status).then_some(()))
            .unwrap_or_else(|| panic!("ask #{id} never became {status}: {:?}", self.ask(id)));
    }

    /// How many asks are saved.
    fn asks(&self) -> i64 {
        self.rig
            .store()
            .conn()
            .query_row("SELECT count(*) FROM asks", [], |r| r.get(0))
            .unwrap()
    }

    /// Session `tab`'s `(activity, last_prompt)` as saved.
    fn session(&self, tab: &Tab) -> (String, Option<String>) {
        self.rig
            .store()
            .conn()
            .query_row(
                "SELECT activity, last_prompt FROM sessions WHERE id = ?1",
                [tab.id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }

    /// Waits until session `tab`'s activity is saved as `activity`.
    fn wait_activity(&self, tab: &Tab, activity: &str) {
        wait_for(WAIT, || (self.session(tab).0 == activity).then_some(()))
            .unwrap_or_else(|| panic!("never {activity}: {:?}", self.session(tab)));
    }

    /// Starts a turn of the user's in Harbor's tab ([`busy_then_answers`]).
    fn harbor_busy(&self) {
        self.harbor.open("busy");
        self.wait_activity(&self.harbor, "working");
    }

    /// The daemon's launches, as `status` says them.
    fn launches(&self) -> u64 {
        let status = self.up.client().call("status", json!({}));
        status["result"]["launches"]
            .as_u64()
            .unwrap_or_else(|| panic!("launches: {status}"))
    }
}

fn ask(to: &str, text: &str) -> Value {
    json!({"to": to, "text": text})
}

/// The roadmap's "done when", in the daemon: Kite asks Harbor; moa types the ask into
/// Harbor's tab, Harbor answers it in a turn, and Kite's `ask` returns the whole answer. The
/// Stop that answered queues no report for moa (A7).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kite_asks_harbor_and_gets_the_answer() {
    let c = Cockpit::new(&[], &[FakeStep::Hang], &answers(RENEWED)).await;
    let mut kite = c.up.session_client(c.kite.id);
    let req = kite.send("ask", ask("Harbor", DOMAIN));
    c.wait_ask(1, "sent");
    c.rig.time.advance(enter());

    let reply = kite.reply(req);
    assert_eq!(answer(&reply), RENEWED);
    assert_eq!(
        reply["result"]["data"],
        json!({"id": 1, "from": "Kite", "to": "Harbor", "status": "answered"})
    );
    assert_eq!(c.ask(1), Some(("answered".into(), Some(RENEWED.into()))));
    // Harbor's prompt was the ask exactly as moa typed it.
    assert_eq!(
        c.session(&c.harbor),
        ("idle".into(), Some(framed(1, "Kite", DOMAIN)))
    );
    c.up.core.barrier().await;
    assert_eq!(c.up.moa_state()["queued"], 0, "{}", c.up.moa_state());
    c.up.stop().await;
}

/// An ask to a peer in the middle of a turn waits, untyped, until the turn ends; then it is
/// typed and answered. The turn's own Stop queues its report as usual.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ask_waits_for_a_busy_peer() {
    let c = Cockpit::new(&[], &[FakeStep::Hang], &busy_then_answers(RENEWED)).await;
    c.harbor_busy();
    let mut kite = c.up.session_client(c.kite.id);
    let req = kite.send("ask", ask("Harbor", DOMAIN));
    c.wait_ask(1, "pending");
    c.rig.time.advance(enter());
    c.up.core.barrier().await;
    assert_eq!(c.ask(1).unwrap().0, "pending", "typed into a busy tab");

    c.harbor.open("done");
    c.wait_ask(1, "sent");
    c.rig.time.advance(enter());
    assert_eq!(answer(&kite.reply(req)), RENEWED);
    c.up.wait_queued(1);
    c.up.stop().await;
}

/// A tab in copy mode takes the paste but not its Enter: no receipt comes, and 20 s after it
/// was typed the ask fails, saying why.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_receipt_fails_the_ask() {
    let c = Cockpit::new(&[], &[FakeStep::Hang], &answers(RENEWED)).await;
    c.rig.tmux(&["copy-mode", "-t", &c.harbor.pane]);
    let mut kite = c.up.session_client(c.kite.id);
    let req = kite.send("ask", ask("Harbor", DOMAIN));
    c.wait_ask(1, "sent");
    c.rig.time.advance(enter());
    c.up.core.barrier().await;
    assert_eq!(c.ask(1).unwrap().0, "sent");

    let rest = u64::try_from(ASK_RECEIPT_MS - ASK_ENTER_MS).unwrap();
    c.rig.time.advance(Duration::from_millis(rest));
    assert_eq!(
        refusal(&kite.reply(req)),
        format!("ask #1 failed: {NO_RECEIPT}")
    );
    assert_eq!(c.ask(1), Some(("failed".into(), Some(NO_RECEIPT.into()))));
    c.up.stop().await;
}

/// An ask still `pending` when its wait runs out is dropped. One already typed when its wait
/// runs out stays open: `ask` says "no reply yet", and `reply` waits again for the answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pending_ask_is_dropped_and_a_sent_one_waits_again() {
    let c = Cockpit::new(&[], &[FakeStep::Hang], &busy_then_answers(RENEWED)).await;
    c.harbor_busy();
    let mut kite = c.up.session_client(c.kite.id);
    let req = kite.send(
        "ask",
        json!({"to": "Harbor", "text": DOMAIN, "timeout_s": 60}),
    );
    c.wait_ask(1, "pending");
    c.rig.time.advance(Duration::from_secs(60));
    assert_eq!(
        refusal(&kite.reply(req)),
        "Harbor stayed busy for 1m; ask #1 was dropped"
    );
    assert_eq!(
        c.ask(1),
        Some(("dropped".into(), Some("stayed busy".into())))
    );

    c.harbor.open("done");
    c.wait_activity(&c.harbor, "idle");
    let req = kite.send(
        "ask",
        json!({"to": "Harbor", "text": DOMAIN, "timeout_s": 1}),
    );
    c.wait_ask(2, "sent");
    // The Enter goes, and the wait runs out, before Harbor answers.
    c.rig.time.advance(Duration::from_secs(1));
    let reply = kite.reply(req);
    assert_eq!(
        answer(&reply),
        "no reply yet from Harbor; `moactl reply 2` waits again"
    );
    assert_eq!(reply["result"]["data"]["id"], 2, "{reply}");

    let again = kite.call("reply", json!({"id": 2}));
    assert_eq!(answer(&again), RENEWED);
    assert_eq!(c.ask(2), Some(("answered".into(), Some(RENEWED.into()))));
    c.up.stop().await;
}

/// Harbor can't ask Kite while Kite waits on Harbor's answer: both would wait out their whole
/// wait. Nothing is saved.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_deadlock_is_refused() {
    let c = Cockpit::new(&[], &[FakeStep::Hang], &[FakeStep::Hang]).await;
    let mut kite = c.up.session_client(c.kite.id);
    kite.send("ask", ask("Harbor", DOMAIN));
    c.wait_ask(1, "sent");

    let mut harbor = c.up.session_client(c.harbor.id);
    let back = harbor.call("ask", ask("Kite", "Which domain?"));
    assert_eq!(
        refusal(&back),
        "Kite is waiting on your answer to ask #1; answer that first"
    );
    assert_eq!(c.asks(), 1);
    c.up.stop().await;
}

/// Harbor's agent dies with the ask typed into its tab: the ask fails at once, and Kite hears
/// why.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_peer_that_dies_fails_the_ask() {
    let harbor = [FakeStep::Wait { file: "die".into() }, FakeStep::Die];
    let c = Cockpit::new(&[], &[FakeStep::Hang], &harbor).await;
    let mut kite = c.up.session_client(c.kite.id);
    let req = kite.send("ask", ask("Harbor", DOMAIN));
    c.wait_ask(1, "sent");

    c.harbor.open("die");
    assert_eq!(refusal(&kite.reply(req)), "ask #1 failed: Harbor ended");
    assert_eq!(
        c.ask(1),
        Some(("failed".into(), Some("Harbor ended".into())))
    );
    c.up.stop().await;
}

/// Kite asks moa during a turn of the user's: the ask waits for that turn, then goes to moa
/// as a hidden prompt of its own, with a card in the console, and moa's reply is Kite's
/// answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kite_asks_moa_between_its_turns() {
    let moa = [
        say("noted", "m1"),
        gate("g1"),
        end(),
        say("Renewed through May.", "m2"),
        end(),
    ];
    let c = Cockpit::new(&moa, &[FakeStep::Hang], &[FakeStep::Hang]).await;
    let mut you = c.up.client();
    let turn = you.send(
        "console.prompt",
        json!({"text": "light the Lantern", "wait": true}),
    );
    c.rig.wait_saved("moa", "noted");

    let mut kite = c.up.session_client(c.kite.id);
    let req = kite.send("ask", ask("moa", DOMAIN));
    c.wait_ask(1, "pending");
    c.up.core.barrier().await;
    assert_eq!(c.rig.logged("session/prompt").len(), 1, "sent mid-turn");

    c.rig.open("g1");
    assert_eq!(answer(&you.reply(turn)), "noted");
    let reply = kite.reply(req);
    assert_eq!(answer(&reply), "Renewed through May.");
    assert_eq!(
        reply["result"]["data"],
        json!({"id": 1, "from": "Kite", "to": "moa", "status": "answered"})
    );
    let prompts: Vec<Value> = c
        .rig
        .logged("session/prompt")
        .iter()
        .map(|l| l["params"]["prompt"][0]["text"].clone())
        .collect();
    assert_eq!(
        prompts,
        [
            json!("light the Lantern"),
            json!(format!(
                "(moa · Kite asks you, ask #1, not typed by the user)\n{DOMAIN}\n\n\
                 Your reply this turn goes back to Kite as the answer."
            ))
        ]
    );
    let card = c
        .rig
        .rows()
        .into_iter()
        .find(|(kind, _, _)| kind == "card")
        .map(|(_, body, _)| body);
    assert_eq!(
        card,
        Some(json!({"what": "ask", "text": format!("Kite · #1: {DOMAIN}")}))
    );
    c.up.stop().await;
}

/// moa asks Kite from its turn, as its agent runs `moactl ask` from a tool call: moa types
/// the ask into Kite's tab, and the command prints Kite's answer to moa.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn moa_asks_kite() {
    const GREEN: &str = "Green: 212 tests pass.";
    let c = Cockpit::new(&[], &answers(GREEN), &[FakeStep::Hang]).await;
    let sock = c.rig.paths.socket("default").expect("the socket's path");
    let params = json!({"to": "Kite", "text": "Is the build green?"}).to_string();
    let argv = vec![
        stub_acp().to_string_lossy().into_owned(),
        "--call".into(),
        sock.to_string_lossy().into_owned(),
        moa_platform::build_id().into(),
        "ask".into(),
        params,
    ];
    moa_testkit::write_stub_script(
        &c.rig.agent,
        &[
            StubStep::Run { argv },
            say("Kite says it's green", "m1"),
            end(),
        ],
    );
    let mut you = c.up.client();
    let turn = you.send(
        "console.prompt",
        json!({"text": "is Kite's build green?", "wait": true}),
    );
    c.wait_ask(1, "sent");
    c.rig.time.advance(enter());

    assert_eq!(answer(&you.reply(turn)), "Kite says it's green");
    let ran = c.rig.wait_logged("run.done");
    assert_eq!(ran["code"], 0, "{ran}");
    assert_eq!(ran["stdout"], format!("{GREEN}\n"), "{ran}");
    assert_eq!(
        c.session(&c.kite).1,
        Some(framed(1, "moa", "Is the build green?"))
    );
    c.up.stop().await;
}

/// A restart keeps what the rows say: an ask still `pending` is typed once its peer is free;
/// one typed whose Enter the restart lost fails 20 s after it was typed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn asks_survive_a_restart() {
    let c = Cockpit::new(&[], &answers("Green."), &busy_then_answers(RENEWED)).await;
    c.harbor_busy();
    let mut moa = c.up.moa_client();
    moa.send("ask", ask("Harbor", DOMAIN));
    c.wait_ask(1, "pending");
    // A connection takes one request at a time, as `moactl` makes one: a second ask is a
    // second `moactl`.
    let mut moa_again = c.up.moa_client();
    moa_again.send("ask", ask("Kite", "Is the build green?"));
    c.wait_ask(2, "sent");
    drop((moa, moa_again));

    let c = c.restart().await;
    c.up.core.barrier().await;
    assert_eq!(
        c.ask(2).unwrap().0,
        "sent",
        "the restart lost only the Enter"
    );
    c.rig.time.advance(Duration::from_millis(
        u64::try_from(ASK_RECEIPT_MS).unwrap(),
    ));
    c.wait_ask(2, "failed");
    assert_eq!(c.ask(2).unwrap().1.as_deref(), Some(NO_RECEIPT));

    c.harbor.open("done");
    c.wait_ask(1, "sent");
    c.rig.time.advance(enter());
    let mut moa = c.up.moa_client();
    assert_eq!(answer(&moa.call("reply", json!({"id": 1}))), RENEWED);
    assert_eq!(
        refusal(&moa.call("reply", json!({"id": 2}))),
        format!("ask #2 failed: {NO_RECEIPT}")
    );
    c.up.stop().await;
}

/// An ask, its typing and its answer start no process: moa types through its link to the
/// terminal server.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_ask_launches_nothing() {
    let c = Cockpit::new(&[], &[FakeStep::Hang], &answers(RENEWED)).await;
    let before = c.launches();
    let mut kite = c.up.session_client(c.kite.id);
    let req = kite.send("ask", ask("Harbor", DOMAIN));
    c.wait_ask(1, "sent");
    c.rig.time.advance(enter());
    assert_eq!(answer(&kite.reply(req)), RENEWED);
    assert_eq!(c.launches(), before);
    c.up.stop().await;
}
