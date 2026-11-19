//! `Live` against a scripted server: a socket in a `/tmp` folder that answers `hello` and then
//! does what each test says, so the timing is the test's.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use futures_util::{FutureExt, SinkExt, StreamExt};
use moa_client::{Client, ClientOptions, Live, LiveMsg, OnMismatch};
use moa_core::config::BrainName;
use moa_core::op::{Done, Op, TaskSet};
use moa_core::publish::Topic;
use moa_core::state::CallerInfo;
use moa_platform::{Paths, ensure_private_dir};
use moa_proto::{
    Call, HelloOk, Id, Incoming, Patch, SnapshotMsg, decode_incoming, encode_notice, encode_result,
};
use serde::Serialize;
use serde_json::json;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_util::codec::{Framed, LinesCodec};

const WAIT: Duration = Duration::from_secs(10);

/// The daemon's side of one connection.
struct Server {
    framed: Framed<UnixStream, LinesCodec>,
}

impl Server {
    /// The next request: its id and what it asks.
    async fn request(&mut self) -> (Id, Call) {
        let line = self.framed.next().await.unwrap().unwrap();
        match decode_incoming(&line) {
            Ok(Incoming::Request { id, call }) => (id, call),
            other => panic!("not a request: {line} ({other:?})"),
        }
    }

    async fn answer(&mut self, id: &Id, result: &impl Serialize) {
        self.framed.send(encode_result(id, result)).await.unwrap();
    }

    async fn notice(&mut self, method: &str, params: &impl Serialize) {
        self.framed
            .send(encode_notice(method, params))
            .await
            .unwrap();
    }

    /// Answers a `subscribe` with `snapshot`.
    async fn subscribed(&mut self, snapshot: &SnapshotMsg) {
        let (id, call) = self.request().await;
        assert!(matches!(call, Call::Subscribe(_)), "{call:?}");
        self.answer(&id, snapshot).await;
    }
}

/// A server in a fresh `/tmp` folder that says hello, then runs `script`.
fn scripted<F, Fut>(script: F) -> (tempfile::TempDir, ClientOptions)
where
    F: FnOnce(Server) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    let dir = tempfile::Builder::new()
        .prefix("moa")
        .tempdir_in("/tmp")
        .unwrap();
    let paths = Paths::with_root(dir.path());
    ensure_private_dir(&paths.run_dir()).unwrap();
    let listener = UnixListener::bind(paths.socket("default").unwrap()).unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut server = Server {
            framed: Framed::new(stream, LinesCodec::new()),
        };
        let (id, call) = server.request().await;
        assert!(matches!(call, Call::Hello(_)), "{call:?}");
        let ok = HelloOk {
            brain: "default".into(),
            build: "b".into(),
            rev: 0,
            matched: true,
            pid: std::process::id(),
        };
        server.answer(&id, &ok).await;
        script(server).await;
    });
    let opts = ClientOptions {
        paths,
        brain: BrainName::default_brain(),
        client: "test",
        caller: CallerInfo::default(),
        autostart: None,
        on_mismatch: OnMismatch::Accept,
        build: "b".into(),
    };
    (dir, opts)
}

fn snapshot(rev: u64) -> SnapshotMsg {
    SnapshotMsg {
        rev,
        topics: BTreeMap::from([(Topic::Panels, vec![])]),
    }
}

fn patch(rev: u64) -> Patch {
    Patch {
        rev,
        upsert: BTreeMap::from([(
            Topic::Panels,
            vec![json!({"key": "c1", "pane": "%1", "view": "tree"})],
        )]),
        delete: BTreeMap::new(),
    }
}

fn done(text: &str) -> Done {
    Done {
        text: text.into(),
        data: json!({}),
    }
}

async fn next(live: &mut Live) -> LiveMsg {
    timeout(WAIT, live.recv())
        .await
        .expect("a message within 10 s")
}

#[tokio::test]
async fn first_ops_are_answered_before_the_snapshot() {
    let (_dir, opts) = scripted(|mut s| async move {
        let (id, call) = s.request().await;
        assert!(matches!(call, Call::Op(Op::UiPanel { .. })), "{call:?}");
        s.answer(
            &id,
            &Done {
                text: String::new(),
                data: json!({"key": "c1", "view": "tree"}),
            },
        )
        .await;
        let (id, call) = s.request().await;
        assert!(matches!(call, Call::Op(Op::UiHere { .. })), "{call:?}");
        s.answer(&id, &done("")).await;
        s.subscribed(&snapshot(7)).await;
        // Keep the connection open while the client looks.
        let _ = s.framed.next().await;
    });
    let client = Client::connect(opts).await.unwrap();
    let first = vec![
        Op::UiPanel {
            pane: Some("%1".into()),
            view: None,
        },
        Op::UiHere { folder: None },
    ];
    let (_live, replies, snap) = timeout(WAIT, client.live(&[Topic::Panels], first))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        replies,
        vec![
            Ok(Done {
                text: String::new(),
                data: json!({"key": "c1", "view": "tree"}),
            }),
            Ok(done("")),
        ]
    );
    assert_eq!(snap, snapshot(7));
}

#[tokio::test]
async fn a_slow_reply_doesnt_hold_back_pushes() {
    // Op 1's reply is held until the client has seen what came after it.
    let (seen, held) = oneshot::channel::<()>();
    let (_dir, opts) = scripted(|mut s| async move {
        s.subscribed(&snapshot(1)).await;
        let (one, _) = s.request().await;
        let (two, _) = s.request().await;
        s.notice("patch", &patch(2)).await;
        s.answer(&two, &done("two")).await;
        held.await.unwrap();
        s.answer(&one, &done("one")).await;
        let _ = s.framed.next().await;
    });
    let client = Client::connect(opts).await.unwrap();
    let (mut live, _, _) = client.live(&[Topic::Panels], vec![]).await.unwrap();
    assert_eq!(live.send(Op::SessionFocus { q: "s1".into() }), 1);
    assert_eq!(live.send(Op::UiHere { folder: None }), 2);
    assert_eq!(next(&mut live).await, LiveMsg::Patch(patch(2)));
    assert_eq!(
        next(&mut live).await,
        LiveMsg::Reply {
            tag: 2,
            result: Ok(done("two")),
        }
    );
    seen.send(()).unwrap();
    assert_eq!(
        next(&mut live).await,
        LiveMsg::Reply {
            tag: 1,
            result: Ok(done("one")),
        }
    );
}

/// The daemon reads nothing while an op is in flight. Ops queued behind a slow one fill the
/// socket, and the daemon keeps pushing: the pushes still arrive, and dropping the `Live`
/// still hangs up. Sizes are well past any socket buffer (8 KiB on macOS, ~200 KiB on Linux).
#[tokio::test]
async fn a_write_that_waits_doesnt_stop_the_reading() {
    const OPS: usize = 256;
    const PUSHES: u64 = 256;
    let big = "x".repeat(4096);
    let push = move |rev: u64| Patch {
        rev,
        upsert: BTreeMap::from([(Topic::Panels, vec![json!({"key": "c1", "pad": big})])]),
        delete: BTreeMap::new(),
    };
    let pushed = push.clone();
    let (hung_up, eof) = oneshot::channel::<()>();
    let (_dir, opts) = scripted(move |mut s| async move {
        s.subscribed(&snapshot(1)).await;
        // The slow op: never answered, and nothing more is read while it is in flight.
        let _ = s.request().await;
        for rev in 2..2 + PUSHES {
            s.notice("patch", &pushed(rev)).await;
        }
        // Now read to the end: the client must hang up.
        while let Some(Ok(_)) = s.framed.next().await {}
        hung_up.send(()).unwrap();
    });
    let client = Client::connect(opts).await.unwrap();
    let (mut live, _, _) = client.live(&[Topic::Panels], vec![]).await.unwrap();
    live.send(Op::SessionFocus { q: "s1".into() });
    for _ in 0..OPS {
        live.send(Op::UiHere {
            folder: Some(format!("/{}", "y".repeat(4096))),
        });
    }
    for rev in 2..2 + PUSHES {
        assert_eq!(next(&mut live).await, LiveMsg::Patch(push(rev)));
    }
    drop(live);
    timeout(WAIT, eof)
        .await
        .expect("the server sees the client hang up within 10 s")
        .unwrap();
}

/// Review Focus 4: a write the daemon read but never answered may have been saved.
#[tokio::test]
async fn live_says_closed_when_the_daemon_stops() {
    let (_dir, opts) = scripted(|mut s| async move {
        s.subscribed(&snapshot(1)).await;
        let (_, call) = s.request().await;
        assert!(matches!(call, Call::Op(Op::TaskSet(_))), "{call:?}");
        // Hangs up without answering.
    });
    let client = Client::connect(opts).await.unwrap();
    let (mut live, _, _) = client.live(&[Topic::Panels], vec![]).await.unwrap();
    let op = Op::TaskSet(TaskSet {
        id: 1,
        status: Some("done".into()),
        ..TaskSet::default()
    });
    assert!(op.writes());
    assert_eq!(live.send(op), 1);
    assert_eq!(
        next(&mut live).await,
        LiveMsg::Reply {
            tag: 1,
            result: Err(
                "moa stopped before answering; it may have saved that, so check before trying again"
                    .into()
            ),
        }
    );
    assert_eq!(next(&mut live).await, LiveMsg::Closed);
    // Nothing after: the next receive never completes.
    assert_eq!(live.recv().now_or_never(), None);
}
