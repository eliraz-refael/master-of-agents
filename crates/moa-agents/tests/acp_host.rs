//! The host of moa's agent, against `stub-acp` in a folder under `/tmp`: what it starts, what
//! it writes, and that everything the agent says reaches the core, in order, as moa's own
//! types.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use moa_agents::acp::Adapter;
use moa_core::central::wire::{
    AcpIn, AdapterSpec, Caps, Opened, PermKind, PermOption, StopReason, ThreadHow, Update,
};
use moa_platform::pid_alive;
use moa_testkit::{StubStep, stub_acp, stub_log, wait_for, write_stub_script};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::mpsc;

const LIMIT: Duration = Duration::from_secs(10);

/// The generation every test starts its adapter with.
const GEN: u64 = 7;

type Events = mpsc::UnboundedReceiver<AcpIn>;

fn say(text: &str, id: Option<&str>) -> StubStep {
    StubStep::Say {
        text: text.into(),
        id: id.map(Into::into),
    }
}

fn end() -> StubStep {
    StubStep::End { stop: None }
}

fn spec(dir: &Path) -> AdapterSpec {
    AdapterSpec {
        program: stub_acp(),
        env: vec![
            ("MOA_SESSION".into(), "moa".into()),
            ("MOA_BRAIN".into(), "default".into()),
            ("CLAUDE_CODE_EXECUTABLE".into(), "/tmp/fake-claude".into()),
            ("CLAUDE_AGENT_ACP_PROTOCOL".into(), "v1".into()),
        ],
        // `CARGO_MANIFEST_DIR` is always in a test's environment, so its absence in the
        // stub's shows that `unset` works.
        unset: vec!["CLAUDE_CONFIG_DIR".into(), "CARGO_MANIFEST_DIR".into()],
        cwd: dir.to_path_buf(),
        settings: r#"{"permissions":{"allow":["Bash(moactl:*)"]}}"#.into(),
        moactl: PathBuf::from("/tmp/moa-test-moactl"),
    }
}

/// A folder under `/tmp` holding `steps` as the stub's script.
fn folder(steps: &[StubStep]) -> TempDir {
    let dir = tempfile::Builder::new()
        .prefix("moa-acp")
        .tempdir_in("/tmp")
        .unwrap();
    write_stub_script(dir.path(), steps);
    dir
}

/// Starts the stub in `dir` and reads its `Started`.
async fn start_in(dir: &Path) -> (Adapter, Events, Caps) {
    let (tx, mut events) = mpsc::unbounded_channel();
    let to_core = Arc::new(move |m: AcpIn| {
        let _ = tx.send(m);
    });
    let adapter = Adapter::start(GEN, &spec(dir), to_core)
        .await
        .expect("the stub starts");
    let AcpIn::Started { generation, caps } = next(&mut events).await else {
        panic!("Started comes first");
    };
    assert_eq!(generation, GEN);
    (adapter, events, caps)
}

async fn next(events: &mut Events) -> AcpIn {
    tokio::time::timeout(LIMIT, events.recv())
        .await
        .expect("a message in time")
        .expect("the host is there")
}

/// Every message up to and with the next `TurnEnded`.
async fn turn(events: &mut Events) -> Vec<AcpIn> {
    let mut got = Vec::new();
    loop {
        let m = next(events).await;
        let ended = matches!(m, AcpIn::TurnEnded { .. });
        got.push(m);
        if ended {
            return got;
        }
    }
}

fn drain(events: &mut Events) -> Vec<AcpIn> {
    std::iter::from_fn(|| events.try_recv().ok()).collect()
}

/// Opens a new thread with the system prompt, and returns its id.
async fn open_new(adapter: &Adapter, events: &mut Events, dir: &Path) -> String {
    let how = ThreadHow::New {
        system_prompt: Some("Be moa.".into()),
        claude_md: None,
    };
    adapter.open(how, dir);
    match next(events).await {
        AcpIn::Opened {
            generation: GEN,
            result: Ok((thread, Opened::New)),
        } => thread,
        other => panic!("{other:?}"),
    }
}

fn update(thread: &str, update: Update) -> AcpIn {
    AcpIn::Update {
        generation: GEN,
        thread: thread.into(),
        update,
    }
}

fn ended(prompt: u64, result: Result<StopReason, String>) -> AcpIn {
    AcpIn::TurnEnded {
        generation: GEN,
        prompt,
        result,
    }
}

fn logged(dir: &Path, method: &str) -> Vec<Value> {
    stub_log(dir)
        .into_iter()
        .filter(|l| l["method"] == method)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_adapter_starts_with_the_specs_env_and_files() {
    let dir = folder(&[StubStep::Caps {
        load: false,
        resume: true,
        system_prompt: true,
    }]);
    let before = moa_platform::spawn::launches();
    let (adapter, _events, caps) = start_in(dir.path()).await;
    assert_eq!(moa_platform::spawn::launches(), before + 1);
    assert_eq!(
        caps,
        Caps {
            load: false,
            resume: true,
            system_prompt: true
        }
    );

    let init = &logged(dir.path(), "initialize")[0];
    let env = &init["env"];
    assert_eq!(env["MOA_SESSION"], "moa");
    assert_eq!(env["MOA_BRAIN"], "default");
    assert_eq!(env["CLAUDE_CODE_EXECUTABLE"], "/tmp/fake-claude");
    assert_eq!(env["CLAUDE_AGENT_ACP_PROTOCOL"], "v1");
    assert_eq!(env["CLAUDE_CONFIG_DIR"], Value::Null);
    assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
    let names = init["env_names"].as_array().expect("the stub's variables");
    assert!(!names.is_empty());
    for unset in ["CLAUDE_CONFIG_DIR", "CARGO_MANIFEST_DIR"] {
        assert!(!names.contains(&json!(unset)), "{unset} reached the agent");
    }
    let bin = format!("{}/bin:", dir.path().display());
    let path = env["PATH"].as_str().expect("a PATH");
    assert!(path.starts_with(&bin), "{path}");
    let cwd = dir.path().canonicalize().unwrap();
    assert_eq!(init["cwd"], json!(cwd));
    // moa reads and writes no files and runs no terminals for its agent.
    let params = &init["params"];
    assert_eq!(params["clientInfo"]["name"], "moa");
    let caps = &params["clientCapabilities"];
    for off in [
        &caps["fs"]["readTextFile"],
        &caps["fs"]["writeTextFile"],
        &caps["terminal"],
    ] {
        assert_ne!(off, &json!(true), "{caps}");
    }

    let link = std::fs::read_link(dir.path().join("bin/moactl")).unwrap();
    assert_eq!(link, spec(dir.path()).moactl);
    let settings = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
    assert_eq!(settings, spec(dir.path()).settings);
    adapter.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_program_fails_to_start() {
    let dir = folder(&[]);
    let mut spec = spec(dir.path());
    spec.program = dir.path().join("no-such-agent");
    let to_core = Arc::new(|m: AcpIn| panic!("nothing is said: {m:?}"));
    let started = Adapter::start(GEN, &spec, to_core).await;
    assert!(started.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_new_sends_the_system_prompt_or_writes_claude_md() {
    let dir = folder(&[]);
    let (adapter, mut events, _) = start_in(dir.path()).await;
    let claude_md = dir.path().join("CLAUDE.md");

    std::fs::write(&claude_md, "an old one").unwrap();
    let first = open_new(&adapter, &mut events, dir.path()).await;
    assert_eq!(first, "stub-0001");
    assert!(!claude_md.exists(), "the system prompt replaces CLAUDE.md");

    let how = ThreadHow::New {
        system_prompt: None,
        claude_md: Some("# moa\n".into()),
    };
    adapter.open(how, dir.path());
    assert_eq!(
        next(&mut events).await,
        AcpIn::Opened {
            generation: GEN,
            result: Ok(("stub-0002".into(), Opened::New))
        }
    );
    assert_eq!(std::fs::read_to_string(&claude_md).unwrap(), "# moa\n");

    let news = logged(dir.path(), "session/new");
    assert_eq!(
        news[0]["params"]["systemPrompt"],
        json!({"mode": "append", "content": [{"type": "text", "text": "Be moa."}]})
    );
    assert_eq!(news[1]["params"].get("systemPrompt"), None);
    assert_eq!(news[1]["params"]["cwd"], json!(dir.path()));
    adapter.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn updates_reach_the_core_in_order_before_the_turn_ends() {
    let dir = folder(&[
        say("hello", Some("m1")),
        StubStep::Think { text: "hmm".into() },
        StubStep::Tool {
            id: "t1".into(),
            title: "Read a file".into(),
            input: Some(json!({"path": "a.txt"})),
        },
        StubStep::ToolDone {
            id: "t1".into(),
            status: "completed".into(),
        },
        StubStep::End {
            stop: Some("max_tokens".into()),
        },
    ]);
    let (adapter, mut events, _) = start_in(dir.path()).await;
    let thread = open_new(&adapter, &mut events, dir.path()).await;
    adapter.prompt(&thread, 41, "go");
    assert_eq!(
        turn(&mut events).await,
        [
            update(
                &thread,
                Update::Message {
                    id: Some("m1".into()),
                    text: "hello".into()
                }
            ),
            update(
                &thread,
                Update::Thought {
                    id: None,
                    text: "hmm".into()
                }
            ),
            update(
                &thread,
                Update::Tool {
                    id: "t1".into(),
                    title: "Read a file".into(),
                    kind: "other".into(),
                    status: "pending".into(),
                    input: Some(r#"{"path":"a.txt"}"#.into()),
                }
            ),
            update(
                &thread,
                Update::ToolUpdate {
                    id: "t1".into(),
                    title: None,
                    status: Some("completed".into()),
                }
            ),
            ended(41, Ok(StopReason::MaxTokens)),
        ]
    );
    let prompts = logged(dir.path(), "session/prompt");
    assert_eq!(
        prompts[0]["params"]["prompt"],
        json!([{"type": "text", "text": "go"}])
    );
    adapter.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_burst_never_blocks_the_adapter() {
    let n = 5000;
    let dir = folder(&[StubStep::Burst { n }, end()]);
    let (adapter, mut events, _) = start_in(dir.path()).await;
    let thread = open_new(&adapter, &mut events, dir.path()).await;
    adapter.prompt(&thread, 1, "go");
    // Nothing is read from the host until the stub has written every chunk: the host reads
    // the agent whether or not the core keeps up.
    wait_for(LIMIT, || {
        (!logged(dir.path(), "burst.done").is_empty()).then_some(())
    })
    .expect("the stub wrote the whole burst");
    let got = turn(&mut events).await;
    let want: Vec<AcpIn> = (0..n)
        .map(|i| {
            update(
                &thread,
                Update::Message {
                    id: Some("burst".into()),
                    text: format!("{i} "),
                },
            )
        })
        .chain([ended(1, Ok(StopReason::EndTurn))])
        .collect();
    assert_eq!(got.len(), want.len());
    assert!(got == want, "the burst arrived out of order");
    adapter.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_permission_waits_for_its_answer() {
    let ask = |title: &str| StubStep::Ask {
        title: title.into(),
        options: vec![
            ("o-yes".into(), "allow_once".into()),
            ("o-no".into(), "reject_once".into()),
        ],
    };
    let dir = folder(&[ask("Run it"), ask("Run that"), end()]);
    let (adapter, mut events, _) = start_in(dir.path()).await;
    let thread = open_new(&adapter, &mut events, dir.path()).await;
    adapter.prompt(&thread, 3, "go");

    let options = vec![
        PermOption {
            id: "o-yes".into(),
            kind: PermKind::AllowOnce,
        },
        PermOption {
            id: "o-no".into(),
            kind: PermKind::RejectOnce,
        },
    ];
    let AcpIn::Permission {
        generation: GEN,
        req: first,
        thread: asked_in,
        title,
        options: offered,
    } = next(&mut events).await
    else {
        panic!("a permission request");
    };
    assert_eq!(
        (asked_in.as_str(), title.as_str()),
        (thread.as_str(), "Run it")
    );
    assert_eq!(offered, options);
    assert!(
        logged(dir.path(), "permission.outcome").is_empty(),
        "the stub waits for the answer"
    );
    adapter.answer(first, Some("o-no".into()));

    let AcpIn::Permission {
        req: second, title, ..
    } = next(&mut events).await
    else {
        panic!("a second permission request");
    };
    assert_eq!(title, "Run that");
    assert_ne!(first, second);
    adapter.answer(second, None);
    assert_eq!(turn(&mut events).await, [ended(3, Ok(StopReason::EndTurn))]);

    let outcomes: Vec<Value> = logged(dir.path(), "permission.outcome")
        .into_iter()
        .map(|l| l["params"].clone())
        .collect();
    assert_eq!(
        outcomes,
        [
            json!({"outcome": "selected", "optionId": "o-no"}),
            json!({"outcome": "cancelled"})
        ]
    );
    adapter.stop().await;
}

/// The log, as text, for the one test that reads it.
#[derive(Clone, Default)]
struct LogText(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogText {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl LogText {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stderr_lines_reach_the_log_cut_to_500() {
    // The adapter's stderr is read on the runtime's threads, so the subscriber is global; each
    // test is a process of its own.
    let log = LogText::default();
    let writer = log.clone();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .finish(),
    )
    .unwrap();

    let long = "é".repeat(600);
    let dir = folder(&[
        StubStep::Stderr { line: long.clone() },
        StubStep::Stderr {
            line: "a second line".into(),
        },
        end(),
    ]);
    let (adapter, mut events, _) = start_in(dir.path()).await;
    let thread = open_new(&adapter, &mut events, dir.path()).await;
    adapter.prompt(&thread, 1, "go");
    turn(&mut events).await;
    wait_for(LIMIT, || {
        log.text().contains("adapter: a second line").then_some(())
    })
    .expect("both lines are logged");

    let text = log.text();
    let logged: Vec<&str> = text
        .lines()
        .filter_map(|l| l.split_once("adapter: ").map(|(_, line)| line))
        .collect();
    let cut: String = long.chars().take(500).collect();
    assert_eq!(logged, [cut.as_str(), "a second line"]);
    adapter.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dead_adapter_says_so_once() {
    let dir = folder(&[say("last words", None), StubStep::Die]);
    let (adapter, mut events, _) = start_in(dir.path()).await;
    let thread = open_new(&adapter, &mut events, dir.path()).await;
    adapter.prompt(&thread, 5, "go");

    let mut got = Vec::new();
    let (mut exited, mut turn_ended) = (false, false);
    while !(exited && turn_ended) {
        let m = next(&mut events).await;
        exited |= matches!(m, AcpIn::Exited { .. });
        turn_ended |= matches!(m, AcpIn::TurnEnded { .. });
        got.push(m);
    }
    // Stopping waits for every part of the host, so nothing can come after.
    adapter.stop().await;
    got.extend(drain(&mut events));

    let exits: Vec<&AcpIn> = got
        .iter()
        .filter(|m| matches!(m, AcpIn::Exited { .. }))
        .collect();
    assert_eq!(exits.len(), 1, "{got:?}");
    assert!(matches!(
        exits[0],
        AcpIn::Exited {
            generation: GEN,
            ..
        }
    ));
    let turn_end = got
        .iter()
        .find(|m| matches!(m, AcpIn::TurnEnded { .. }))
        .unwrap();
    assert!(
        matches!(
            turn_end,
            AcpIn::TurnEnded {
                generation: GEN,
                prompt: 5,
                result: Err(_)
            }
        ),
        "{turn_end:?}"
    );
}

/// The pid the stub's `Spawn` or `SpawnStubborn` child wrote, once it is there.
fn child_pid(dir: &Path) -> u32 {
    let file = dir.join(".stub-acp.child");
    wait_for(LIMIT, || {
        std::fs::read_to_string(&file).ok()?.trim().parse().ok()
    })
    .expect("the stub started its child")
}

fn gone(pids: &[u32]) {
    wait_for(LIMIT, || pids.iter().all(|&p| !pid_alive(p)).then_some(()))
        .unwrap_or_else(|| panic!("{pids:?} are still there"));
}

/// The stub's children: one that leaves on `SIGTERM`, and one that ignores it.
const CHILDREN: [StubStep; 2] = [StubStep::Spawn, StubStep::SpawnStubborn];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_ends_the_group() {
    for spawn in CHILDREN {
        let dir = folder(&[spawn.clone(), StubStep::HangHard]);
        let (adapter, mut events, _) = start_in(dir.path()).await;
        let thread = open_new(&adapter, &mut events, dir.path()).await;
        adapter.prompt(&thread, 1, "go");
        let child = child_pid(dir.path());
        let stub = adapter.pid();
        assert!(pid_alive(stub) && pid_alive(child), "{spawn:?}");

        tokio::time::timeout(LIMIT, adapter.stop())
            .await
            .expect("stop returns");
        gone(&[stub, child]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dead_agents_group_goes_with_it() {
    for spawn in CHILDREN {
        let gate = StubStep::Wait {
            file: "gate".into(),
        };
        let dir = folder(&[spawn.clone(), gate, StubStep::Die]);
        let (adapter, mut events, _) = start_in(dir.path()).await;
        let thread = open_new(&adapter, &mut events, dir.path()).await;
        adapter.prompt(&thread, 1, "go");
        let child = child_pid(dir.path());
        assert!(pid_alive(child), "{spawn:?}");
        std::fs::write(dir.path().join("gate"), "").unwrap();

        while !matches!(next(&mut events).await, AcpIn::Exited { .. }) {}
        gone(&[child]);
        adapter.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_and_load_report_how_they_opened() {
    let dir = folder(&[say("hi", Some("m1")), end()]);
    let (adapter, mut events, _) = start_in(dir.path()).await;
    let thread = open_new(&adapter, &mut events, dir.path()).await;
    adapter.prompt(&thread, 1, "hello");
    turn(&mut events).await;

    adapter.open(
        ThreadHow::Resume {
            thread: thread.clone(),
        },
        dir.path(),
    );
    assert_eq!(
        next(&mut events).await,
        AcpIn::Opened {
            generation: GEN,
            result: Ok((thread.clone(), Opened::Resumed))
        }
    );

    adapter.open(
        ThreadHow::Resume {
            thread: "stub-0099".into(),
        },
        dir.path(),
    );
    let unknown = next(&mut events).await;
    assert!(
        matches!(
            unknown,
            AcpIn::Opened {
                generation: GEN,
                result: Err(_)
            }
        ),
        "{unknown:?}"
    );

    adapter.open(
        ThreadHow::Load {
            thread: thread.clone(),
        },
        dir.path(),
    );
    let mut got = Vec::new();
    while !matches!(got.last(), Some(AcpIn::Opened { .. })) {
        got.push(next(&mut events).await);
    }
    assert_eq!(
        got,
        [
            update(&thread, Update::UserEcho),
            update(
                &thread,
                Update::Message {
                    id: Some("m1".into()),
                    text: "hi".into()
                }
            ),
            AcpIn::Opened {
                generation: GEN,
                result: Ok((thread.clone(), Opened::Loaded))
            },
        ]
    );
    adapter.stop().await;
}

/// A guard, not a red/green step: no test build is given `MOA_ACP_ADAPTER` (only the flake's
/// `packages.default` is), so no test can reach the real adapter.
#[test]
fn a_test_build_knows_no_adapter() {
    assert_eq!(moa_agents::acp::BUILT_IN_ADAPTER, None);
}
