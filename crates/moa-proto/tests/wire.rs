use std::collections::BTreeMap;
use std::sync::Arc;

use moa_core::op::{Done, NoParams, Op};
use moa_core::state::CallerInfo;
use moa_proto::*;
use serde_json::{Value, json};

fn rows(items: &[(&str, Value)]) -> Arc<BTreeMap<String, Value>> {
    Arc::new(
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    )
}

fn decode(v: Value) -> Result<Incoming, (Option<Id>, RpcError)> {
    decode_incoming(&v.to_string())
}

#[test]
fn a_task_add_request_decodes() {
    let got = decode(json!({"jsonrpc":"2.0","id":7,"method":"task.add",
        "params":{"project":"Lantern","title":"x","today":true}}))
    .unwrap();
    let Incoming::Request {
        id,
        call: Call::Op(Op::TaskAdd(t)),
    } = got
    else {
        panic!("not a task.add request: {got:?}");
    };
    assert_eq!(id, Id::Num(7));
    assert_eq!(
        (t.project.as_str(), t.title.as_str(), t.today),
        ("Lantern", "x", true)
    );
}

#[test]
fn control_methods_decode() {
    let hello = json!({"jsonrpc":"2.0","id":1,"method":"hello",
        "params":{"client":"cli","build":"b1","caller":{}}});
    let got = decode(hello).unwrap();
    assert_eq!(
        got,
        Incoming::Request {
            id: Id::Num(1),
            call: Call::Hello(Hello {
                client: "cli".into(),
                build: "b1".into(),
                caller: CallerInfo::default()
            })
        }
    );
    let sub = decode(json!({"jsonrpc":"2.0","id":2,"method":"subscribe",
        "params":{"topics":["tasks","status"]}}))
    .unwrap();
    assert_eq!(
        sub,
        Incoming::Request {
            id: Id::Num(2),
            call: Call::Subscribe(Subscribe {
                topics: vec![Topic::Tasks, Topic::Status],
                since: None
            })
        }
    );
    let status = decode(json!({"jsonrpc":"2.0","id":3,"method":"status"})).unwrap();
    assert_eq!(
        status,
        Incoming::Request {
            id: Id::Num(3),
            call: Call::Status
        }
    );
}

#[test]
fn ui_panel_decodes() {
    let got = decode(json!({"jsonrpc":"2.0","id":1,"method":"ui.panel",
        "params":{"view":"tree","pane":"%3"}}))
    .unwrap();
    assert_eq!(
        got,
        Incoming::Request {
            id: Id::Num(1),
            call: Call::Op(Op::UiPanel {
                pane: Some("%3".into()),
                view: Some(moa_core::panel::View::Tree),
            })
        }
    );
}

#[test]
fn an_unknown_view_is_invalid_params() {
    let (id, bad) = decode(json!({"jsonrpc":"2.0","id":2,"method":"ui.panel",
        "params":{"view":"nope"}}))
    .unwrap_err();
    assert_eq!(id, Some(Id::Num(2)));
    assert_eq!(bad.code, code::INVALID_PARAMS);
}

#[test]
fn unit_ops_take_any_empty_params() {
    for params in [None, Some(Value::Null), Some(json!({}))] {
        let mut m = json!({"jsonrpc":"2.0","id":1,"method":"brief"});
        if let Some(p) = params {
            m["params"] = p;
        }
        assert_eq!(
            decode(m).unwrap(),
            Incoming::Request {
                id: Id::Num(1),
                call: Call::Op(Op::Brief(NoParams))
            }
        );
    }
}

#[test]
fn the_error_table() {
    let fails = |line: &str| decode_incoming(line).unwrap_err();
    let e = |code: i64, message: &str| RpcError {
        code,
        message: message.into(),
    };

    assert_eq!(fails("nope"), (None, e(code::PARSE, "that isn't JSON")));
    assert_eq!(
        fails("[]"),
        (None, e(code::INVALID_REQUEST, "batches aren't supported"))
    );
    let not_rpc = e(code::INVALID_REQUEST, "that isn't a JSON-RPC 2.0 message");
    assert_eq!(
        fails(r#"{"id":4,"method":"status"}"#),
        (Some(Id::Num(4)), not_rpc.clone())
    );
    assert_eq!(
        fails(r#"{"jsonrpc":"1.0","id":"z","method":"status"}"#),
        (Some(Id::Str("z".into())), not_rpc.clone())
    );
    assert_eq!(fails("3"), (None, not_rpc));
    assert_eq!(
        fails(r#"{"jsonrpc":"2.0","id":5,"method":"nope.nope"}"#),
        (
            Some(Id::Num(5)),
            e(
                code::METHOD_NOT_FOUND,
                "moa doesn't know the method nope.nope"
            )
        )
    );
    let (id, bad) = fails(r#"{"jsonrpc":"2.0","id":6,"method":"task.add","params":{"title":"x"}}"#);
    assert_eq!(id, Some(Id::Num(6)));
    assert_eq!(bad.code, code::INVALID_PARAMS);
    assert!(
        bad.message
            .starts_with("the parameters for task.add don't fit: missing field"),
        "{}",
        bad.message
    );
    assert!(!bad.message.contains('\n'));

    // A null id is not a request id.
    let (id, bad) = fails(r#"{"jsonrpc":"2.0","id":null,"method":"status"}"#);
    assert_eq!((id, bad.code), (None, code::INVALID_REQUEST));

    // Notifications other than `hook` are dropped, known verb or not.
    for m in ["status", "task.add", "from.the.future"] {
        let line = json!({"jsonrpc":"2.0","method":m,"params":{}}).to_string();
        assert_eq!(decode_incoming(&line).unwrap(), Incoming::Ignored, "{m}");
    }
}

#[test]
fn string_ids_round_trip() {
    let got = decode(json!({"jsonrpc":"2.0","id":"a","method":"status"})).unwrap();
    let Incoming::Request { id, .. } = got else {
        panic!("not a request")
    };
    assert_eq!(id, Id::Str("a".into()));
    let reply = encode_result(&id, &json!({"ok": true}));
    assert_eq!(
        decode_server_line(&reply).unwrap(),
        ServerLine::Result {
            id: Id::Str("a".into()),
            value: json!({"ok": true})
        }
    );
    let failed = encode_error(
        Some(&id),
        &RpcError {
            code: code::FAILED,
            message: "no".into(),
        },
    );
    assert_eq!(
        decode_server_line(&failed).unwrap(),
        ServerLine::Error {
            id: Some(Id::Str("a".into())),
            error: RpcError {
                code: code::FAILED,
                message: "no".into()
            }
        }
    );
    let anon = encode_error(
        None,
        &RpcError {
            code: code::PARSE,
            message: "x".into(),
        },
    );
    assert!(matches!(
        decode_server_line(&anon).unwrap(),
        ServerLine::Error { id: None, .. }
    ));
    let notice = encode_notice("event", &CoreEvent::Ui { text: "hi".into() });
    assert_eq!(
        decode_server_line(&notice).unwrap(),
        ServerLine::Notice {
            method: "event".into(),
            params: json!({"kind": "ui", "text": "hi"})
        }
    );
}

#[test]
fn hook_is_a_notification() {
    let got = decode(json!({"jsonrpc":"2.0","method":"hook","params":{"event":"Stop"}})).unwrap();
    assert_eq!(
        got,
        Incoming::Hook {
            id: None,
            params: json!({"event": "Stop"})
        }
    );
}

#[test]
fn a_hook_request_keeps_its_id() {
    let got = decode(
        json!({"jsonrpc":"2.0","id":"h1","method":"hook","params":{"event":"SessionStart"}}),
    )
    .unwrap();
    assert_eq!(
        got,
        Incoming::Hook {
            id: Some(Id::Str("h1".into())),
            params: json!({"event": "SessionStart"})
        }
    );
}

#[test]
fn mismatch_allows_only_the_four() {
    let hello = Call::Hello(Hello {
        client: "c".into(),
        build: "b".into(),
        caller: CallerInfo::default(),
    });
    for c in [&hello, &Call::Status, &Call::Upgrade, &Call::Shutdown] {
        assert!(allowed_on_mismatch(c), "{c:?}");
    }
    let sub = Call::Subscribe(Subscribe {
        topics: vec![],
        since: None,
    });
    let op = Call::Op(Op::Brief(NoParams));
    for c in [&sub, &op] {
        assert!(!allowed_on_mismatch(c), "{c:?}");
    }
}

#[test]
fn patch_between_models() {
    let a = json!({"key":"1","title":"a"});
    let b = json!({"key":"2","title":"b"});
    let b2 = json!({"key":"2","title":"b!"});
    let c = json!({"key":"3","title":"c"});
    let mut old = Model {
        rev: 4,
        ..Model::default()
    };
    old.topics
        .insert(Topic::Tasks, rows(&[("1", a.clone()), ("2", b)]));
    old.topics
        .insert(Topic::Groups, rows(&[("g", json!({"key":"g"}))]));
    let mut new = old.clone();
    new.rev = 5;
    new.topics
        .insert(Topic::Tasks, rows(&[("2", b2.clone()), ("3", c.clone())]));
    let topics = [Topic::Tasks, Topic::Groups];

    let p = Patch::between(&old, &new, &topics).unwrap();
    assert_eq!(p.rev, 5);
    assert_eq!(p.upsert, BTreeMap::from([(Topic::Tasks, vec![b2, c])]));
    assert_eq!(
        p.delete,
        BTreeMap::from([(Topic::Tasks, vec!["1".to_string()])])
    );
    assert!(!p.upsert.contains_key(&Topic::Groups));

    assert_eq!(Patch::between(&old, &old, &topics), None);
    let mut same = old.clone();
    same.rev = 9;
    assert_eq!(Patch::between(&old, &same, &topics), None);
    // A topic nobody subscribed to doesn't count.
    assert_eq!(Patch::between(&old, &new, &[Topic::Groups]), None);

    let snap = snapshot_of(&new, &[Topic::Tasks, Topic::Asks]);
    assert_eq!(snap.rev, 5);
    assert_eq!(snap.topics[&Topic::Tasks].len(), 2);
    assert!(snap.topics[&Topic::Asks].is_empty());
}

#[test]
fn encoded_lines_have_no_newline() {
    let done = Done {
        text: "one\ntwo\n".into(),
        data: json!({"a": "x\ny"}),
    };
    let l = encode_result(&Id::Num(1), &done);
    assert!(!l.contains('\n'));
    let ServerLine::Result { value, .. } = decode_server_line(&l).unwrap() else {
        panic!("not a result")
    };
    assert_eq!(serde_json::from_value::<Done>(value).unwrap(), done);
    assert!(!encode_notice("event", &json!({"t": "a\nb"})).contains('\n'));
}
