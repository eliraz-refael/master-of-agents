//! Layouts: the spike's `layouts.json` (its format, presets, merge and messages), and the
//! `layout.list`, `layout.show` and `layout.set` ops that read it again each time (L1–L3).

mod common;

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};

use moa_core::config::BrainName;
use moa_core::effect::{Effect, Input, Publish, handle_checked};
use moa_core::layout::{
    Axis, LEAVES, Layouts, Leaf, Node, PRESET_KEYS, Size, leaves, parse, presets,
};
use moa_core::op::{ConnId, NoParams, Op, ReqId};
use moa_core::panel::View;
use moa_core::publish::Topic;
use moa_core::state::{Deps, State};
use moa_testkit::brain::TypicalBrain;
use serde_json::{Value, json};

use common::*;

thread_local! {
    /// What `layouts.json` holds for the test's `read_text`; `None` is no file.
    static FILE: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn read_text(_: &Path) -> io::Result<Option<String>> {
    Ok(FILE.with(|f| f.borrow().clone()))
}

fn file_holds(text: Option<&str>) {
    FILE.with(|f| *f.borrow_mut() = text.map(str::to_string));
}

fn layouts_file(b: &TypicalBrain) -> PathBuf {
    b.home.join(".config/moa/layouts.json")
}

/// The typical brain with `layouts.json` holding `text`, and the meta values `meta`.
fn started(text: Option<&str>, meta: &[(&str, &str)]) -> (TypicalBrain, State) {
    file_holds(text);
    let b = TypicalBrain::new();
    let deps = Deps {
        home: b.home.clone(),
        read_text,
        layouts_file: layouts_file(&b),
        ..Deps::default()
    };
    let mut snapshot = b.snapshot();
    snapshot
        .meta
        .extend(meta.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    let mut st = State::new(BrainName::default_brain(), b.config()).with_deps(deps);
    handle_checked(&mut st, Input::Startup(Box::new(snapshot)), b.now());
    (b, st)
}

fn text_of(fx: &[Effect]) -> String {
    match &replies(fx)[0].1 {
        Ok(done) => done.text.clone(),
        Err(e) => panic!("refused: {}", e.0),
    }
}

fn refusal(fx: &[Effect]) -> String {
    match &replies(fx)[0].1 {
        Err(e) => e.0.clone(),
        Ok(done) => panic!("expected a refusal, got {}", done.text),
    }
}

fn writes(fx: &[Effect]) -> Vec<&Effect> {
    fx.iter()
        .filter(|e| matches!(e, Effect::WriteFile { .. }))
        .collect()
}

/// The `layouts` rows the last publish in `fx` carries.
fn layouts_topic(fx: &[Effect]) -> Option<Value> {
    fx.iter().rev().find_map(|e| match e {
        Effect::Publish(Publish::Model(m)) => m
            .topics
            .get(&Topic::Layouts)
            .map(|rows| serde_json::to_value(&**rows).unwrap()),
        _ => None,
    })
}

fn list() -> Op {
    Op::LayoutList(NoParams)
}

fn set(name: &str, layout: Value) -> Op {
    Op::LayoutSet {
        name: name.into(),
        layout,
    }
}

fn leaf(view: View, size: Option<Size>) -> Node {
    Node::Leaf {
        leaf: Leaf::Panel(view),
        size,
    }
}

fn split(axis: Axis, size: Option<Size>, children: Vec<Node>) -> Node {
    Node::Split {
        axis,
        children,
        size,
    }
}

const PANELS: &str =
    "(panels: console, tree, today, activity, projects, plan, status, picture, agent)";

#[test]
fn presets_are_the_spikes() {
    use Axis::{H, V};
    use Size::Percent as P;
    let p = presets();
    assert_eq!(p.names().collect::<Vec<_>>(), PRESET_KEYS);
    assert_eq!(PRESET_KEYS, ["cockpit", "focus", "classic", "triage"]);
    assert_eq!(p.file_error, None);
    assert!(p.file.is_empty());
    let agent = Node::Leaf {
        leaf: Leaf::Agent,
        size: None,
    };
    let want = [
        split(
            H,
            None,
            vec![
                leaf(View::Tree, Some(P(26))),
                leaf(View::Console, None),
                leaf(View::Plan, Some(P(30))),
            ],
        ),
        split(
            H,
            None,
            vec![
                split(
                    V,
                    Some(P(24)),
                    vec![leaf(View::Tree, None), leaf(View::Plan, Some(P(45)))],
                ),
                split(V, None, vec![agent, leaf(View::Console, Some(P(30)))]),
            ],
        ),
        split(
            H,
            None,
            vec![
                leaf(View::Console, None),
                split(
                    V,
                    Some(P(42)),
                    vec![leaf(View::Plan, None), leaf(View::Tree, Some(P(40)))],
                ),
            ],
        ),
        split(
            H,
            None,
            vec![
                leaf(View::Status, Some(P(30))),
                leaf(View::Console, None),
                leaf(View::Plan, Some(P(28))),
            ],
        ),
    ];
    for (name, want) in PRESET_KEYS.iter().zip(want) {
        assert_eq!(p.node(name).unwrap(), want, "{name}");
    }
}

#[test]
fn leaves_and_sizes_read_as_the_spike_wrote_them() {
    assert_eq!(
        LEAVES,
        [
            "console", "tree", "today", "activity", "projects", "plan", "status", "picture",
            "agent"
        ]
    );
    for word in LEAVES {
        let leaf: Leaf = word.parse().unwrap();
        assert_eq!(leaf.to_string(), word);
    }
    assert!("empty-slot".parse::<Leaf>().is_err());
    let focus = presets().node("focus").unwrap();
    let words: Vec<String> = leaves(&focus).iter().map(Leaf::to_string).collect();
    assert_eq!(words, ["tree", "plan", "agent", "console"]);
    assert_eq!(Size::Percent(26).arg(), "26%");
    assert_eq!(Size::Cells(40).arg(), "40");
}

#[test]
fn a_file_keeps_its_order_and_gets_missing_presets_first() {
    let l = Layouts::load(Some(
        r#"{"layouts": {"mine": {"panel": "tree"}, "cockpit": {"panel": "console"}}}"#,
    ));
    assert_eq!(
        l.names().collect::<Vec<_>>(),
        ["focus", "classic", "triage", "mine", "cockpit"]
    );
    // The file's own entry wins over the preset of the same name.
    assert_eq!(l.node("cockpit").unwrap(), leaf(View::Console, None));
    assert_eq!(l.file_error, None);
    // No file: the presets alone.
    assert_eq!(Layouts::load(None).names().collect::<Vec<_>>(), PRESET_KEYS);
}

#[test]
fn validation_says_what_the_spike_said() {
    let err = |v: Value| parse(&v).unwrap_err().to_string();
    assert_eq!(
        err(json!({"panel": "tre"})),
        format!("layout: unknown panel 'tre' {PANELS}")
    );
    assert_eq!(
        err(json!({"split": "h", "children": [{"panel": "tree"}, {"panel": "tre"}]})),
        format!("layout.children[1]: unknown panel 'tre' {PANELS}")
    );
    assert_eq!(
        err(json!({"split": "h", "children": [{"panel": "tree"}]})),
        "layout: a split needs at least 2 children"
    );
    assert_eq!(
        err(json!({"split": "x", "children": [{"panel": "tree"}, {"panel": "plan"}]})),
        r#"layout: needs "panel", or "split": "h" (side by side) / "v" (stacked)"#
    );
    assert_eq!(
        err(json!({"split": "h", "children": [{"panel": "console"}, {"panel": "console"}]})),
        "layout: at most one console and one agent panel"
    );
    for size in [json!("0%"), json!(0), json!(30.0), json!(true)] {
        assert_eq!(
            err(json!({"split": "h", "children": [
                {"panel": "tree", "size": size},
                {"panel": "plan"}
            ]})),
            r#"layout.children[0]: size must be a number of cells or like "30%""#,
            "{size}"
        );
    }
    // The edges that are fine.
    let ok = json!({"split": "v", "children": [
        {"panel": "tree", "size": 1},
        {"panel": "plan", "size": "99%"}
    ]});
    assert_eq!(
        parse(&ok).unwrap(),
        split(
            Axis::V,
            None,
            vec![
                leaf(View::Tree, Some(Size::Cells(1))),
                leaf(View::Plan, Some(Size::Percent(99)))
            ]
        )
    );
}

#[test]
fn a_broken_entry_doesnt_take_the_others_down() {
    let l = Layouts::load(Some(r#"{"layouts": {"bad": {"panel": "tre"}}}"#));
    assert!(l.names().any(|n| n == "bad"));
    assert_eq!(
        l.node("bad").unwrap_err().to_string(),
        format!("layout: unknown panel 'tre' {PANELS}")
    );
    assert!(l.node("cockpit").is_ok());
    assert_eq!(
        l.node("nope").unwrap_err().to_string(),
        "no layout 'nope' (have: cockpit, focus, classic, triage, bad)"
    );
}

#[test]
fn a_broken_file_keeps_the_presets_and_is_never_overwritten() {
    let text = r#"{"layouts": {"#;
    let l = Layouts::load(Some(text));
    assert_eq!(l.names().collect::<Vec<_>>(), PRESET_KEYS);
    let error = l.file_error.clone().expect("a file error");
    assert!(error.contains("line 1"), "{error}");
    assert!(l.file_text_with("mine", &json!({"panel": "tree"})).is_err());

    let (b, mut st) = started(Some(text), &[]);
    let fx = send(&b, &mut st, 1, list());
    let shown = text_of(&fx);
    assert!(
        shown.starts_with("▸ cockpit: tree · console · plan\n"),
        "{shown}"
    );
    assert!(
        shown
            .lines()
            .any(|l| l == format!("layouts.json has an error: {error}")),
        "{shown}"
    );
    assert_eq!(
        st.model().topics[&Topic::Layouts]["file"],
        json!({"key": "file", "error": error})
    );

    let fx = send(&b, &mut st, 2, set("mine", json!({"panel": "tree"})));
    assert_eq!(
        refusal(&fx),
        format!("fix layouts.json first ({error}); nothing was saved")
    );
    assert!(writes(&fx).is_empty());
}

#[test]
fn layout_list_marks_the_current_one() {
    let (b, mut st) = started(None, &[("layout.current", "focus")]);
    let fx = send(&b, &mut st, 1, list());
    assert_eq!(
        text_of(&fx),
        "  cockpit: tree · console · plan\n\
         ▸ focus: tree · plan · agent · console\n  \
         classic: console · plan · tree\n  \
         triage: status · console · plan\n\
         (config: ~/.config/moa/layouts.json)"
    );
    let rows = &st.model().topics[&Topic::Layouts];
    assert_eq!(
        rows["focus"],
        json!({
            "key": "focus",
            "name": "focus",
            "index": 1,
            "leaves": ["tree", "plan", "agent", "console"],
            "current": true,
            "error": null
        })
    );
    assert_eq!(rows["cockpit"]["current"], json!(false));
}

#[test]
fn a_current_layout_that_is_gone_or_broken_falls_back_to_cockpit() {
    for current in ["gone", "bad"] {
        let (b, mut st) = started(
            Some(r#"{"layouts": {"bad": {"panel": "tre"}}}"#),
            &[("layout.current", current)],
        );
        assert_eq!(moa_core::layout::current_layout(&st), "cockpit");
        let fx = send(&b, &mut st, 1, list());
        assert!(text_of(&fx).starts_with("▸ cockpit: "), "{current}");
    }
}

#[test]
fn layout_show_prints_the_entry() {
    let (b, mut st) = started(Some(r#"{"layouts": {"mine": {"panel": "tree"}}}"#), &[]);
    let fx = send(
        &b,
        &mut st,
        1,
        Op::LayoutShow {
            name: "mine".into(),
        },
    );
    assert_eq!(text_of(&fx), "{\n  \"panel\": \"tree\"\n}");
    let fx = send(
        &b,
        &mut st,
        2,
        Op::LayoutShow {
            name: "nope".into(),
        },
    );
    assert_eq!(
        refusal(&fx),
        "no layout 'nope' (have: cockpit, focus, classic, triage, mine)"
    );
}

#[test]
fn layout_set_validates_writes_and_publishes() {
    let file = r#"{
        "theme": {"x": 1},
        "current": "classic",
        "layouts": {
            "zeta": {"panel": "tree"},
            "cockpit": {"split": "v", "children": [{"panel": "console"}, {"panel": "plan"}]}
        }
    }"#;
    let (b, mut st) = started(Some(file), &[]);
    let mine =
        json!({"split": "h", "children": [{"panel": "tree", "size": 40}, {"panel": "agent"}]});
    let fx = send(&b, &mut st, 1, set("mine", mine));
    let heard: Vec<&Effect> = fx
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::WriteFile { .. } | Effect::Publish(_) | Effect::Reply { .. }
            )
        })
        .collect();
    assert_eq!(heard.len(), 3, "{heard:?}");
    // The file's other keys and entries as they were (the presets it doesn't hold stay out),
    // then `mine`, with two spaces.
    let want = r#"{
  "current": "classic",
  "layouts": {
    "zeta": {
      "panel": "tree"
    },
    "cockpit": {
      "children": [
        {
          "panel": "console"
        },
        {
          "panel": "plan"
        }
      ],
      "split": "v"
    },
    "mine": {
      "children": [
        {
          "panel": "tree",
          "size": 40
        },
        {
          "panel": "agent"
        }
      ],
      "split": "h"
    }
  },
  "theme": {
    "x": 1
  }
}
"#;
    assert_eq!(
        heard[0],
        &Effect::WriteFile {
            path: layouts_file(&b),
            text: want.into(),
            req: Some((ConnId(1), ReqId(1))),
        }
    );
    let rows = layouts_topic(&fx[..]).expect("layouts published");
    assert_eq!(rows["mine"]["leaves"], json!(["tree", "agent"]));
    assert!(matches!(heard[1], Effect::Publish(Publish::Model(_))));
    assert_eq!(
        text_of(&fx),
        "saved layout mine; apply it with: moa layout apply mine"
    );
    assert!(matches!(heard[2], Effect::Reply { .. }));

    let fx = send(&b, &mut st, 2, set("bad", json!({"panel": "tre"})));
    assert_eq!(
        refusal(&fx),
        format!("layout: unknown panel 'tre' {PANELS}")
    );
    assert!(writes(&fx).is_empty());
}

#[test]
fn layout_ops_read_the_file_again() {
    let (b, mut st) = started(None, &[]);
    let fx = send(&b, &mut st, 1, list());
    assert!(!text_of(&fx).contains("mine"));
    file_holds(Some(r#"{"layouts": {"mine": {"panel": "tree"}}}"#));
    let fx = send(&b, &mut st, 2, list());
    assert!(
        text_of(&fx).lines().any(|l| l == "  mine: tree"),
        "{}",
        text_of(&fx)
    );
    let rows = layouts_topic(&fx).expect("the new entry is published");
    assert_eq!(rows["mine"]["leaves"], json!(["tree"]));
    // The rows are by name; `index` gives the file's order back (after the presets it lacks).
    let order: Vec<(u64, &str)> = rows
        .as_object()
        .unwrap()
        .values()
        .map(|r| (r["index"].as_u64().unwrap(), r["name"].as_str().unwrap()))
        .collect::<std::collections::BTreeMap<_, _>>()
        .into_iter()
        .collect();
    assert_eq!(
        order,
        [
            (0, "cockpit"),
            (1, "focus"),
            (2, "classic"),
            (3, "triage"),
            (4, "mine")
        ]
    );
}

/// An empty file, and `"layouts": null`, hold no entries: the presets alone, no error, and
/// `set` writes the file.
#[test]
fn an_empty_file_or_null_layouts_is_no_entries() {
    for text in ["", "  \n", r#"{"layouts": null}"#] {
        let l = Layouts::load(Some(text));
        assert_eq!(l.file_error, None, "{text:?}");
        assert_eq!(l.names().collect::<Vec<_>>(), PRESET_KEYS, "{text:?}");

        let (b, mut st) = started(Some(text), &[]);
        let fx = send(&b, &mut st, 1, set("mine", json!({"panel": "tree"})));
        assert_eq!(
            text_of(&fx),
            "saved layout mine; apply it with: moa layout apply mine"
        );
        let Some(Effect::WriteFile { text: written, .. }) = writes(&fx).first().copied() else {
            panic!("no write for {text:?}");
        };
        let saved: Value = serde_json::from_str(written).unwrap();
        assert_eq!(
            saved,
            json!({"layouts": {"mine": {"panel": "tree"}}}),
            "{text:?}"
        );
    }
}
