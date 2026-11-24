//! Recording moa's agent: `acp-tap` records a session, `acp-scrub` makes the file safe to
//! commit, and `stub-acp` replays it, straight (`--replay`) or through the host of moa's agent
//! (the agent dir's replay file). The tap only ever runs in front of `stub-acp`.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use moa_agents::acp::Adapter;
use moa_core::central::wire::{
    AcpIn, AdapterSpec, Caps, Opened, PermKind, PermOption, StopReason, ThreadHow, Update,
};
use moa_testkit::recording::{self, Dir, Entry, Kind, REPLAY_FILE};
use moa_testkit::{StubStep, stub_acp, test_bin, wait_for, write_stub_script};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::mpsc as async_mpsc;

const LIMIT: Duration = Duration::from_secs(10);

/// A folder under `/tmp`.
fn folder() -> TempDir {
    tempfile::Builder::new()
        .prefix("moa-rec")
        .tempdir_in("/tmp")
        .unwrap()
}

/// A JSON-RPC peer on the other end of a child's stdin and stdout, line by line, which keeps
/// every byte the child wrote.
struct Peer {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<Vec<u8>>,
    stderr: JoinHandle<String>,
    /// What the peer saw, in order: what it sent (`In`) and what it got (`Out`).
    seen: Vec<Entry>,
    bytes: Vec<u8>,
}

impl Peer {
    fn spawn(mut command: Command) -> Peer {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start the child");
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            loop {
                let mut line = Vec::new();
                match out.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {
                        if tx.send(line).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        let mut err = child.stderr.take().unwrap();
        let stderr = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = err.read_to_string(&mut text);
            text
        });
        Peer {
            stdin: child.stdin.take(),
            child,
            lines,
            stderr,
            seen: Vec::new(),
            bytes: Vec::new(),
        }
    }

    fn send(&mut self, msg: Value) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
        self.seen.push(Entry { dir: Dir::In, msg });
    }

    /// The next line, or `None` once the child has closed its stdout.
    fn next(&mut self) -> Option<Value> {
        match self.lines.recv_timeout(LIMIT) {
            Ok(line) => {
                self.bytes.extend_from_slice(&line);
                let msg: Value = serde_json::from_slice(&line).expect("a JSON line");
                self.seen.push(Entry {
                    dir: Dir::Out,
                    msg: msg.clone(),
                });
                Some(msg)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => None,
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("nothing from the child in time"),
        }
    }

    /// Sends a request and reads up to and with its answer, which it returns.
    fn ask(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let msg = self.next().expect("an answer before the end");
            if Kind::of(&msg) == Kind::Answer && msg["id"] == json!(id) {
                return msg;
            }
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Closes the child's stdin, reads what's left, and waits for it to exit: its status and
    /// its stderr.
    fn finish(mut self) -> (ExitStatus, String, Vec<Entry>, Vec<u8>) {
        drop(self.stdin.take());
        while self.next().is_some() {}
        let status = wait_for(LIMIT, || self.child.try_wait().unwrap()).expect("the child exits");
        let stderr = self.stderr.join().unwrap();
        (status, stderr, self.seen, self.bytes)
    }
}

fn initialize() -> Value {
    json!({"protocolVersion": 1, "clientCapabilities": {}})
}

fn text_prompt(session: &Value, text: &str) -> Value {
    json!({"sessionId": session, "prompt": [{"type": "text", "text": text}]})
}

/// `acp-tap` in front of `program`, recording to `file`; it gets only its env and `args`.
fn tap(dir: &Path, program: &Path, file: &Path, args: &[&str]) -> Command {
    let mut tap = Command::new(test_bin("acp-tap"));
    tap.current_dir(dir)
        .args(args)
        .env("MOA_ACP_TAP_TARGET", program)
        .env("MOA_ACP_TAP_FILE", file);
    tap
}

/// One scripted session against `command`, as a peer sees it.
fn scripted_session(dir: &Path, mut command: Command) -> (ExitStatus, String, Vec<Entry>, Vec<u8>) {
    command.current_dir(dir);
    let mut peer = Peer::spawn(command);
    peer.ask(0, "initialize", initialize());
    let new = peer.ask(1, "session/new", json!({"cwd": dir, "mcpServers": []}));
    let session = new["result"]["sessionId"].clone();
    let done = peer.ask(2, "session/prompt", text_prompt(&session, "go"));
    assert_eq!(done["result"]["stopReason"], "end_turn");
    peer.finish()
}

#[test]
fn the_tap_records_both_ways() {
    let script = [
        StubStep::Say {
            text: "hi".into(),
            id: Some("m1".into()),
        },
        StubStep::Stderr {
            line: "a line on stderr".into(),
        },
        StubStep::Tool {
            id: "t1".into(),
            title: "Read a file".into(),
            input: Some(json!({"path": "a.txt"})),
        },
        StubStep::ToolDone {
            id: "t1".into(),
            status: "completed".into(),
        },
        StubStep::End { stop: None },
    ];
    let (bare, tapped) = (folder(), folder());
    write_stub_script(bare.path(), &script);
    write_stub_script(tapped.path(), &script);

    let (status, _, _, bare_bytes) = scripted_session(bare.path(), Command::new(stub_acp()));
    assert!(status.success());

    let file = tapped.path().join("tap.jsonl");
    let (status, stderr, seen, bytes) =
        scripted_session(tapped.path(), tap(tapped.path(), &stub_acp(), &file, &[]));
    assert!(status.success(), "{status:?}");
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        String::from_utf8(bare_bytes).unwrap(),
        "the tap changed what the client saw"
    );
    assert!(stderr.contains("a line on stderr"), "{stderr}");

    // Each line once, both ways, in the order the client saw them: it sends a request only
    // once the last one is answered, so no line can pass another in the tap.
    let (header, entries) = recording::read(&file).unwrap();
    assert_eq!(header, None, "the tap writes no header");
    assert_eq!(entries, seen);
}

#[test]
fn the_tap_passes_its_args_and_the_agents_exit_code() {
    let dir = folder();
    let file = dir.path().join("tap.jsonl");
    let synthetic = recording::synthetic();
    let replay = ["--replay", synthetic.to_str().unwrap()];
    let mut peer = Peer::spawn(tap(dir.path(), &stub_acp(), &file, &replay));
    peer.ask(0, "initialize", initialize());
    peer.send(json!({"jsonrpc": "2.0", "id": 1, "method": "session/load",
                     "params": {"sessionId": "s", "cwd": "/tmp", "mcpServers": []}}));
    let (status, stderr, _, _) = peer.finish();
    assert_eq!(status.code(), Some(1));
    assert!(
        stderr.contains("replay expected session/new, got session/load"),
        "{stderr}"
    );
}

#[test]
fn a_replay_out_of_step_says_so() {
    let dir = folder();
    // `--replay` wins over a replay file in the agent dir: this empty one would end the
    // replay at `initialize`.
    std::fs::write(dir.path().join(REPLAY_FILE), "").unwrap();
    let mut stub = Command::new(stub_acp());
    stub.current_dir(dir.path())
        .arg("--replay")
        .arg(recording::synthetic());
    let mut peer = Peer::spawn(stub);

    // Ids are the client's own, whatever the recording's were.
    let init = peer.ask(7, "initialize", initialize());
    assert_eq!(init["result"]["agentInfo"]["version"], "0.100.0");
    let new = peer.ask(8, "session/new", json!({"cwd": "/tmp", "mcpServers": []}));
    let session = new["result"]["sessionId"].clone();
    assert_eq!(session, "5f0c7d2e-3b9a-4e61-8c47-2a9d1e6b0f38");

    peer.send(json!({"jsonrpc": "2.0", "id": 9, "method": "session/load",
                     "params": {"sessionId": session, "cwd": "/tmp", "mcpServers": []}}));
    let (status, stderr, seen, _) = peer.finish();
    assert_eq!(status.code(), Some(1));
    assert!(
        stderr.contains("replay expected session/prompt, got session/load"),
        "{stderr}"
    );
    let answers = seen
        .iter()
        .filter(|e| e.dir == Dir::Out && Kind::of(&e.msg) == Kind::Answer)
        .map(|e| e.msg["id"].clone())
        .collect::<Vec<_>>();
    assert_eq!(answers, [json!(7), json!(8)], "the load is never answered");
}

#[test]
fn the_tap_exits_as_a_signalled_agent_does() {
    let dir = folder();
    let file = dir.path().join("tap.jsonl");
    let mut peer = Peer::spawn(tap(dir.path(), &stub_acp(), &file, &[]));
    peer.ask(0, "initialize", initialize());
    let agent = wait_for(LIMIT, || {
        moa_platform::proc::children(peer.pid())
            .ok()
            .and_then(|c| c.first().copied())
    })
    .expect("the tap's agent");
    let agent = rustix::process::Pid::from_raw(agent as i32).expect("a pid");
    rustix::process::kill_process(agent, rustix::process::Signal::KILL).unwrap();
    let (status, _, _, _) = peer.finish();
    assert_eq!(status.code(), Some(128 + 9));
}

/// `stub-acp --replay` of `lines`, written to a file in `dir`.
fn replay_of(dir: &Path, lines: &[Value]) -> Peer {
    let file = dir.join("recording.jsonl");
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&file, text).unwrap();
    let mut stub = Command::new(stub_acp());
    stub.current_dir(dir).arg("--replay").arg(&file);
    Peer::spawn(stub)
}

#[test]
fn a_replay_past_its_end_or_a_wrong_answer_says_so() {
    let dir = folder();
    let initialize_lines = [
        json!({"dir": "in", "msg": {"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}}}),
        json!({"dir": "out", "msg": {"jsonrpc": "2.0", "id": 0, "result": {}}}),
    ];

    let mut peer = replay_of(dir.path(), &initialize_lines);
    peer.ask(5, "initialize", initialize());
    peer.send(json!({"jsonrpc": "2.0", "id": 6, "method": "session/new", "params": {}}));
    let (status, stderr, _, _) = peer.finish();
    assert_eq!(status.code(), Some(1));
    assert!(
        stderr.contains("replay expected the end, got session/new"),
        "{stderr}"
    );

    let ask = json!({"dir": "out", "msg": {"jsonrpc": "2.0", "id": 3,
        "method": "session/request_permission", "params": {}}});
    let answer = json!({"dir": "in", "msg": {"jsonrpc": "2.0", "id": 3, "result": {}}});
    let lines: Vec<Value> = initialize_lines.into_iter().chain([ask, answer]).collect();
    let mut peer = replay_of(dir.path(), &lines);
    peer.ask(5, "initialize", initialize());
    let request = peer.next().expect("the permission request");
    assert_eq!(request["id"], 3);
    peer.send(json!({"jsonrpc": "2.0", "id": 4, "result": {}}));
    let (status, stderr, _, _) = peer.finish();
    assert_eq!(status.code(), Some(1));
    assert!(
        stderr.contains("replay expected an answer to 3, got an answer to 4"),
        "{stderr}"
    );
}

/// The generation the host's agent starts with.
const GEN: u64 = 3;

fn spec(dir: &Path) -> AdapterSpec {
    AdapterSpec {
        program: stub_acp(),
        env: Vec::new(),
        unset: Vec::new(),
        cwd: dir.to_path_buf(),
        settings: "{}".into(),
        moactl: PathBuf::from("/tmp/moa-test-moactl"),
    }
}

async fn next(events: &mut async_mpsc::UnboundedReceiver<AcpIn>) -> AcpIn {
    tokio::time::timeout(LIMIT, events.recv())
        .await
        .expect("a message in time")
        .expect("the host is there")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_answers_in_order() {
    let dir = folder();
    std::fs::copy(recording::synthetic(), dir.path().join(REPLAY_FILE)).unwrap();
    // A script beside the replay file is ignored: the replay wins.
    write_stub_script(
        dir.path(),
        &[
            StubStep::Say {
                text: "from the script".into(),
                id: None,
            },
            StubStep::End { stop: None },
        ],
    );
    let (tx, mut events) = async_mpsc::unbounded_channel();
    let to_core = Arc::new(move |m: AcpIn| {
        let _ = tx.send(m);
    });
    let adapter = Adapter::start(GEN, &spec(dir.path()), to_core)
        .await
        .expect("the replay starts");

    let mut got = vec![next(&mut events).await];
    let how = ThreadHow::New {
        system_prompt: Some("Be moa.".into()),
        claude_md: None,
    };
    adapter.open(how, dir.path());
    got.push(next(&mut events).await);
    let Some(AcpIn::Opened {
        result: Ok((thread, _)),
        ..
    }) = got.last().cloned()
    else {
        panic!("{got:?}");
    };
    for (prompt, said) in [
        (1, "hello"),
        (2, "what's running?"),
        (3, "is Harbor's tree clean?"),
    ] {
        adapter.prompt(&thread, prompt, said);
        loop {
            let m = next(&mut events).await;
            if let AcpIn::Permission { req, options, .. } = &m {
                let no = options.iter().find(|o| o.kind == PermKind::RejectOnce);
                adapter.answer(*req, no.map(|o| o.id.clone()));
            }
            let ended = matches!(m, AcpIn::TurnEnded { .. });
            got.push(m);
            if ended {
                break;
            }
        }
    }
    adapter.stop().await;

    let s = |x: &str| x.to_string();
    let update = |update| AcpIn::Update {
        generation: GEN,
        thread: thread.clone(),
        update,
    };
    let message = |id: &str, text: &str| {
        update(Update::Message {
            id: Some(s(id)),
            text: s(text),
        })
    };
    let ended = |prompt| AcpIn::TurnEnded {
        generation: GEN,
        prompt,
        result: Ok(StopReason::EndTurn),
    };
    let tool = |id: &str, title: &str, input: Value| {
        update(Update::Tool {
            id: s(id),
            title: s(title),
            kind: s("execute"),
            status: s("pending"),
            input: Some(input.to_string()),
        })
    };
    let tool_done = |id: &str, status: &str| {
        update(Update::ToolUpdate {
            id: s(id),
            title: None,
            status: Some(s(status)),
            input: None,
        })
    };
    let usage = || update(Update::Other("usage_update"));
    let (ls, git) = (
        "toolu_01Bv6XkR2mPq9LsT4nWc7YhD",
        "toolu_01Cw8YmS3nRr0MtU5oXd8ZjF",
    );
    let git_title = "`git -C ~/private/harbor status --short`";
    let want = vec![
        AcpIn::Started {
            generation: GEN,
            caps: Caps {
                load: true,
                resume: true,
                system_prompt: true,
            },
        },
        AcpIn::Opened {
            generation: GEN,
            result: Ok((s("5f0c7d2e-3b9a-4e61-8c47-2a9d1e6b0f38"), Opened::New)),
        },
        update(Update::Other("available_commands_update")),
        message("msg_01HqT5n8VkLw3RbYc2Xe9PzA", "Hello! I'm moa. "),
        message(
            "msg_01HqT5n8VkLw3RbYc2Xe9PzA",
            "Ask me about your sessions, or tell me what to start.",
        ),
        usage(),
        ended(1),
        update(Update::Thought {
            id: Some(s("msg_01Jd7WcQm4ZsN6vKp8TfRb2E")),
            text: s("moactl ls lists the sessions."),
        }),
        tool(
            ls,
            "`moactl ls`",
            json!({"command": "moactl ls", "description": "List moa's sessions"}),
        ),
        tool_done(ls, "completed"),
        message(
            "msg_01Kf3RtY8bNw5QzLm2Vc6HxJ",
            "Two sessions: **Lantern** is working on the login fix, and **Kite** is idle.",
        ),
        usage(),
        ended(2),
        tool(
            git,
            git_title,
            json!({"command": "git -C ~/private/harbor status --short",
                   "description": "Show Harbor's uncommitted changes"}),
        ),
        AcpIn::Permission {
            generation: GEN,
            req: 1,
            thread: thread.clone(),
            title: s(git_title),
            options: [
                ("allow-once", PermKind::AllowOnce),
                ("allow-with-updates", PermKind::AllowAlways),
                ("reject", PermKind::RejectOnce),
            ]
            .map(|(id, kind)| PermOption { id: s(id), kind })
            .to_vec(),
        },
        tool_done(git, "failed"),
        message(
            "msg_01Lg4SuZ9cPx6RaMn3Wd7JyK",
            "I wasn't allowed to look at Harbor's tree. Run `git status` there, or let me.",
        ),
        usage(),
        ended(3),
    ];
    assert_eq!(got, want);

    // Every update the recording's agent sent reached the core, and nothing else did.
    let (_, entries) = recording::read(&recording::synthetic()).unwrap();
    let sent = entries
        .iter()
        .filter(|e| e.dir == Dir::Out && Kind::of(&e.msg) == Kind::Notification("session/update"))
        .count();
    let updates = got
        .iter()
        .filter(|m| matches!(m, AcpIn::Update { .. }))
        .count();
    assert_eq!(updates, sent);
}

/// The host name and full name `acp-scrub` takes for this machine's in every test.
const TEST_HOST: &str = "scrub-test-host";
const TEST_FULL_NAME: &str = "Ada Lantern";

/// `acp-scrub <in> <out> args…` with no home, user or login name in its environment, unless
/// `home` is given, and the test's host and full name, not the machine's.
fn scrub(input: &Path, out: &Path, home: Option<&str>, args: &[&str]) -> (Option<i32>, String) {
    let mut scrub = Command::new(test_bin("acp-scrub"));
    scrub
        .arg(input)
        .arg(out)
        .args(args)
        .env_remove("HOME")
        .env_remove("USER")
        .env_remove("LOGNAME")
        .env("MOA_SCRUB_HOST", TEST_HOST)
        .env("MOA_SCRUB_FULL_NAME", TEST_FULL_NAME);
    if let Some(home) = home {
        scrub.env("HOME", home);
    }
    let done = scrub.output().expect("run acp-scrub");
    (done.status.code(), String::from_utf8(done.stderr).unwrap())
}

/// Writes `lines` as a tap file in `dir`, after an `initialize` answer from the adapter.
fn tap_file(dir: &Path, lines: &[Value]) -> PathBuf {
    let input = dir.join("tap.jsonl");
    let agent = json!({"dir": "out", "msg": {"jsonrpc": "2.0", "id": 0, "result": {"agentInfo":
        {"name": "@agentclientprotocol/claude-agent-acp", "version": "0.100.0"}}}});
    let text: String = std::iter::once(&agent)
        .chain(lines)
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&input, text).unwrap();
    input
}

/// A `session/update` notification from the agent with `params`.
fn update_with(params: Value) -> Value {
    json!({"dir": "out", "msg": {"jsonrpc": "2.0", "method": "session/update", "params": params}})
}

/// The scrubbed file's lines.
fn scrubbed(out: &Path) -> Vec<Value> {
    std::fs::read_to_string(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn scrub_replaces_and_denies() {
    let dir = folder();
    let input = tap_file(
        dir.path(),
        &[
            json!({"dir": "in", "msg": {"jsonrpc": "2.0", "id": 1, "method": "session/new",
                "params": {"cwd": "/Users/beacon/private/kite", "notes": "/var/lib/lantern-svc/notes"}}}),
            update_with(
                json!({"text": "Beacon's projects: /Users/beacon/.claude/projects/-Users-beacon-private-kite on file://beacon-mbp.local/x"}),
            ),
            update_with(json!({"text": "mail beacon@harbor.lan about Harbor Labs"})),
            update_with(
                json!({"text": "/Users/Shared/notes and .claude/settings.local.json, signed Ada Lantern on scrub-test-host"}),
            ),
        ],
    );
    let home = Some("/var/lib/lantern-svc");
    let replace = [
        "--replace",
        "beacon@harbor.lan=EMAIL",
        "Harbor Labs=Org",
        "--deny",
        "harbor",
        "beacon",
    ];

    let out = dir.path().join("clean.jsonl");
    let (code, stderr) = scrub(&input, &out, home, &replace);
    assert_eq!(code, Some(0), "{stderr}");
    let clean = scrubbed(&out);
    assert_eq!(
        clean[0],
        json!({"adapter": "claude-agent-acp", "version": "0.100.0"}),
        "the header comes first"
    );
    assert_eq!(clean.len(), 6);
    assert_eq!(
        clean[2]["msg"]["params"],
        json!({"cwd": "/home/user/private/kite", "notes": "/home/user/notes"})
    );
    assert_eq!(
        clean[3]["msg"]["params"]["text"],
        "user's projects: /home/user/.claude/projects/-home-user-private-kite on file://host/x"
    );
    assert_eq!(clean[4]["msg"]["params"]["text"], "mail EMAIL about Org");
    // `/Users/Shared` names no one, and `settings.local.json` is no host.
    assert_eq!(
        clean[5]["msg"]["params"]["text"],
        "/Users/Shared/notes and .claude/settings.local.json, signed user on host"
    );

    // A deny pattern that still matches (in any case) fails the line; each pattern is named
    // with its line, never what matched, and nothing is written.
    let out = dir.path().join("denied.jsonl");
    let (code, stderr) = scrub(
        &input,
        &out,
        home,
        &[
            "--replace",
            "beacon@harbor.lan=EMAIL",
            "--deny",
            "nowhere",
            "harbor",
            "kite",
        ],
    );
    assert_eq!(code, Some(1), "{stderr}");
    for want in [
        r#"line 4: matches --deny "harbor""#,
        r#"line 2: matches --deny "kite""#,
        r#"line 3: matches --deny "kite""#,
    ] {
        assert!(stderr.contains(want), "{want}: {stderr}");
    }
    assert!(!stderr.contains("nowhere"), "{stderr}");
    assert!(!stderr.contains("Harbor"), "{stderr}");
    assert!(!out.exists());

    // So does an email address.
    let (code, stderr) = scrub(&input, &out, home, &["--replace", "Harbor Labs=Org"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("line 4: holds an email address"),
        "{stderr}"
    );
    assert!(!stderr.contains("harbor.lan"), "{stderr}");
    assert!(!out.exists());
}

#[test]
fn scrub_redacts_the_account() {
    let dir = folder();
    let id = "00000000-0000-0000-0000-000000000000";
    let input = tap_file(
        dir.path(),
        &[
            json!({"dir": "out", "msg": {"jsonrpc": "2.0", "method": "_auth/status_update",
                "params": {"authStatus": {"kind": "account", "label": "Claude Max",
                    "detail": "gw.harbor.internal", "vendor": {"region": "harbor-east"},
                    "account": {"plan": "max", "email": "beacon@harbor.lan",
                        "organization": "Harbor Labs", "seat": "harbor-17"}}}}}),
            update_with(json!({"accountUuid": "8c1f-harbor", "orgId": 42,
                "nested": [{"orgName": "Harbor Labs", "emailAddress": "beacon@harbor.lan"}],
                "org_name": "Harbor Labs", "Email": "beacon@harbor.lan",
                "organizationID": "harbor-17", "email-address": "beacon@harbor.lan"})),
        ],
    );
    let out = dir.path().join("clean.jsonl");
    let (code, stderr) = scrub(&input, &out, None, &[]);
    assert_eq!(code, Some(0), "{stderr}");
    let clean = scrubbed(&out);
    assert_eq!(
        clean[2]["msg"]["params"]["authStatus"],
        json!({"kind": "account", "label": "Claude Max", "detail": "host",
               "account": {"plan": "max", "email": "user@example.com", "organization": "Org"}})
    );
    assert_eq!(
        clean[3]["msg"]["params"],
        json!({"accountUuid": id, "orgId": 0,
               "nested": [{"orgName": "Org", "emailAddress": "user@example.com"}],
               "org_name": "Org", "Email": "user@example.com",
               "organizationID": id, "email-address": "user@example.com"})
    );
}

#[test]
fn scrub_fails_closed() {
    let dir = folder();
    let input = tap_file(
        dir.path(),
        &[
            update_with(
                json!({"text": "HARBOR LABS ships from /Users/ada.lantern/x (-Users-ada-lantern-x)"}),
            ),
            update_with(
                json!({"text": "see https://wiki.harbor.internal/x and build-01.harbor.io, not github.com or api.anthropic.com"}),
            ),
        ],
    );
    let out = dir.path().join("out.jsonl");
    let hosts = ["wiki.harbor.internal=host", "build-01.harbor.io=host"];
    let run = |extra: &[&str]| {
        let args: Vec<&str> = ["--replace", "Harbor Labs=Org"]
            .iter()
            .chain(&hosts)
            .chain(extra)
            .copied()
            .collect();
        scrub(&input, &out, None, &args)
    };

    // A FROM is still there in another case.
    let (code, stderr) = run(&[]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("line 2: still holds a --replace FROM"),
        "{stderr}"
    );
    assert!(!stderr.contains("line 3"), "{stderr}");

    // Once it's gone, both spellings of the found home are replaced.
    let (code, stderr) = run(&["HARBOR LABS=Org"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        scrubbed(&out)[2]["msg"]["params"]["text"],
        "Org ships from /home/user/x (-home-user-x)"
    );

    // A host outside the allowed ones fails its line until it is replaced.
    let (code, stderr) = scrub(
        &input,
        &out,
        None,
        &["--replace", "Harbor Labs=Org", "HARBOR LABS=Org"],
    );
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("line 3: names a host outside the allowed ones"),
        "{stderr}"
    );
    assert!(!stderr.contains("harbor.i"), "{stderr}");

    // Scrubbing may not change the protocol: a method, or the header.
    let (code, stderr) = run(&["HARBOR LABS=Org", "session=sitting"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("line 2: its method changed")
            && stderr.contains("line 3: its method changed"),
        "{stderr}"
    );
    let (code, stderr) = run(&["HARBOR LABS=Org", "claude=x"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("the header's name or version changed"),
        "{stderr}"
    );

    // Lookalikes of allowed hosts, and IPv4 addresses but loopback and any, fail their lines,
    // never echoed; so does an email or host at a domain that isn't reserved.
    let dir = folder();
    let input = tap_file(
        dir.path(),
        &[
            update_with(json!({"text": "see evil-anthropic.com"})),
            update_with(json!({"text": "see https://anthropic.com.internal/x"})),
            update_with(json!({"text": "ssh 10.20.30.40"})),
            update_with(json!({"text": "listen on 0.0.0.0 or 127.0.0.1"})),
            update_with(json!({"text": "mail a@foo.example.internal or a@example.io"})),
            update_with(json!({"text": "fine: a@docs.example.edu, b@x.invalid, https://x.host/"})),
        ],
    );
    let (code, stderr) = scrub(&input, &dir.path().join("out.jsonl"), None, &[]);
    assert_eq!(code, Some(1), "{stderr}");
    for want in [
        "line 2: names a host",
        "line 3: names a host",
        "line 4: holds an IP address",
        "line 6: holds an email address",
        "line 7: names a host",
    ] {
        assert!(stderr.contains(want), "{want}: {stderr}");
    }
    assert!(!stderr.contains("line 5"), "{stderr}");
    assert!(!stderr.contains("line 7: holds"), "{stderr}");
    assert!(!stderr.contains("10.20"), "{stderr}");
}

#[test]
fn scrub_finds_ordinary_email_addresses() {
    let dir = folder();
    let input = dir.path().join("tap.jsonl");
    let agent = json!({"dir": "out", "msg": {"jsonrpc": "2.0", "id": 0, "result":
        {"agentInfo": {"name": "claude-agent-acp", "version": "0.100.0"}}}});
    let said = |text: &str| json!({"dir": "out", "msg": {"text": text}});
    let lines = [
        agent,
        said("write to first.last+moa@mail.harbor.lan"),
        said("KITE_99@Harbor-Labs.co.lan said so"),
        said("<a-b@c-d.io>"),
        said("an npm name: @scope/pkg@1.2.3, and user@example.com"),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&input, text).unwrap();
    let (code, stderr) = scrub(&input, &dir.path().join("out.jsonl"), None, &[]);
    assert_eq!(code, Some(1), "{stderr}");
    for n in 2..=4 {
        assert!(stderr.contains(&format!("line {n}:")), "line {n}: {stderr}");
    }
    assert!(!stderr.contains("line 5:"), "{stderr}");
}

#[test]
fn the_synthetic_recording_scrubs_to_itself() {
    let dir = folder();
    let out = dir.path().join("out.jsonl");
    let (code, stderr) = scrub(&recording::synthetic(), &out, None, &[]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        std::fs::read_to_string(recording::synthetic()).unwrap()
    );
}
