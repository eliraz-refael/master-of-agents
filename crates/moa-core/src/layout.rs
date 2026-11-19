//! Layouts: the spike's `layouts.json` (L1). A layout is a tree of splits over panels. The
//! file's entries stay raw JSON and are parsed when used, so one broken entry never takes the
//! others down. Pure: the core reads the file through `Deps.read_text` and the daemon writes
//! it (`Effect::WriteFile`).

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::model::UnknownValue;
use crate::panel::View;
use crate::state::State;

/// The meta key holding the brain's current layout (L2): the file is shared by every brain.
pub const CURRENT_KEY: &str = "layout.current";

/// The layout a brain starts with, and falls back to.
pub const DEFAULT_LAYOUT: &str = "cockpit";

/// The presets, in the spike's order.
pub const PRESET_KEYS: [&str; 4] = ["cockpit", "focus", "classic", "triage"];

/// What a layout's leaves can be, in the spike's `PANELS` order.
pub const LEAVES: [&str; 9] = [
    "console", "tree", "today", "activity", "projects", "plan", "status", "picture", "agent",
];

/// How a split lays out its children.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis {
    /// Side by side (`"h"`).
    H,
    /// Stacked (`"v"`).
    V,
}

/// A cell's size: cells, or a share of the window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Size {
    Cells(u16),
    /// 1 to 99.
    Percent(u8),
}

impl Size {
    /// As `resize-pane -x|-y` takes it: `40` or `26%`.
    pub fn arg(&self) -> String {
        match self {
            Size::Cells(n) => n.to_string(),
            Size::Percent(p) => format!("{p}%"),
        }
    }
}

/// What fills a layout's cell: a panel showing a view, or the agent spot.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Leaf {
    Panel(View),
    Agent,
}

impl Leaf {
    /// The leaf as the file spells it.
    pub fn word(self) -> &'static str {
        match self {
            Leaf::Panel(view) => view.word(),
            Leaf::Agent => "agent",
        }
    }
}

impl FromStr for Leaf {
    type Err = UnknownValue;
    fn from_str(s: &str) -> Result<Self, UnknownValue> {
        match s {
            "agent" => Ok(Leaf::Agent),
            s if LEAVES.contains(&s) => s.parse().map(Leaf::Panel),
            _ => Err(UnknownValue { what: "panel" }),
        }
    }
}

impl fmt::Display for Leaf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())
    }
}

/// On the wire, a leaf is its word (`goto`'s `panel`).
impl Serialize for Leaf {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.word())
    }
}

impl<'de> Deserialize<'de> for Leaf {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let word = String::deserialize(d)?;
        word.parse().map_err(|_| {
            serde::de::Error::custom(format!(
                "unknown panel '{word}' (panels: {})",
                LEAVES.join(", ")
            ))
        })
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for Leaf {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Leaf".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string", "enum": LEAVES })
    }
}

/// One layout, parsed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Node {
    Split {
        axis: Axis,
        children: Vec<Node>,
        size: Option<Size>,
    },
    Leaf {
        leaf: Leaf,
        size: Option<Size>,
    },
}

/// Why a layout can't be used or saved, as one line.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum LayoutError {
    /// The spike's validation texts, at a path: `layout`, `layout.children[2]`.
    #[error("{path}: {why}")]
    Invalid { path: String, why: String },
    #[error("no layout '{name}' (have: {have})")]
    NoSuch { name: String, have: String },
    /// The file doesn't parse: nothing is written over it.
    #[error("fix layouts.json first ({0}); nothing was saved")]
    FileBroken(String),
    /// A hand-written entry named `""`.
    #[error("a layout needs a name")]
    NoName,
    /// A name `layout.set` won't save.
    #[error("a layout's name is one word, like mine")]
    BadName,
}

/// A name `layout.set` saves: one word, with no space or control character in it. So no
/// layout it saves is named `""`, the `layouts` topic's key for the file's error.
pub fn check_name(name: &str) -> Result<(), LayoutError> {
    if name.is_empty() || name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(LayoutError::BadName);
    }
    Ok(())
}

/// A layout from its JSON, checked as the spike's `validate` did.
pub fn parse(v: &Value) -> Result<Node, LayoutError> {
    node_at(v, "layout")
}

fn node_at(v: &Value, path: &str) -> Result<Node, LayoutError> {
    let bad = |why: String| LayoutError::Invalid {
        path: path.to_string(),
        why,
    };
    let Some(node) = v.as_object() else {
        return Err(bad("must be an object".into()));
    };
    let size = match node.get("size") {
        None | Some(Value::Null) => None,
        Some(s) => Some(
            size_of(s)
                .ok_or_else(|| bad(r#"size must be a number of cells or like "30%""#.into()))?,
        ),
    };
    if let Some(panel) = node.get("panel") {
        let leaf = panel.as_str().and_then(|w| w.parse().ok()).ok_or_else(|| {
            bad(format!(
                "unknown panel {} (panels: {})",
                quoted(panel),
                LEAVES.join(", ")
            ))
        })?;
        return Ok(Node::Leaf { leaf, size });
    }
    let axis = match node.get("split").and_then(Value::as_str) {
        Some("h") => Axis::H,
        Some("v") => Axis::V,
        _ => {
            return Err(bad(
                r#"needs "panel", or "split": "h" (side by side) / "v" (stacked)"#.into(),
            ));
        }
    };
    let kids = match node.get("children") {
        Some(Value::Array(kids)) if kids.len() >= 2 => kids,
        _ => return Err(bad("a split needs at least 2 children".into())),
    };
    let children = kids
        .iter()
        .enumerate()
        .map(|(i, kid)| node_at(kid, &format!("{path}.children[{i}]")))
        .collect::<Result<Vec<_>, _>>()?;
    let node = Node::Split {
        axis,
        children,
        size,
    };
    let all = leaves(&node);
    let count = |leaf: Leaf| all.iter().filter(|l| **l == leaf).count();
    if count(Leaf::Panel(View::Console)) > 1 || count(Leaf::Agent) > 1 {
        return Err(bad("at most one console and one agent panel".into()));
    }
    Ok(node)
}

/// A positive whole number of cells, or `"N%"` with N from 1 to 99: tmux refuses `0%`.
fn size_of(v: &Value) -> Option<Size> {
    match v {
        Value::Number(n) => n
            .as_u64()
            .filter(|n| *n > 0)
            .and_then(|n| u16::try_from(n).ok())
            .map(Size::Cells),
        Value::String(s) => {
            let digits = s.strip_suffix('%')?;
            if !(1..=2).contains(&digits.len()) || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let p: u8 = digits.parse().ok()?;
            (1..=99).contains(&p).then_some(Size::Percent(p))
        }
        _ => None,
    }
}

/// A value as the spike's `repr` showed it: a string in single quotes, anything else as JSON.
fn quoted(v: &Value) -> String {
    match v {
        Value::String(s) => format!("'{s}'"),
        v => v.to_string(),
    }
}

/// The layout's leaves, in order.
pub fn leaves(n: &Node) -> Vec<Leaf> {
    match n {
        Node::Leaf { leaf, .. } => vec![*leaf],
        Node::Split { children, .. } => children.iter().flat_map(leaves).collect(),
    }
}

/// `layouts.json` as read, merged with the presets.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Layouts {
    /// Every top-level key of the file as read, `layouts` and `current` included.
    pub file: Map<String, Value>,
    /// The presets the file doesn't have, then the file's entries in its order (the spike's
    /// merge, so a deleted preset comes back).
    pub entries: IndexMap<String, Value>,
    /// Why the file doesn't parse. The presets alone are used, and nothing is written over it.
    pub file_error: Option<String>,
}

/// The built-in layouts: the spike's `DEFAULTS`.
pub fn presets() -> Layouts {
    let entries = [
        (
            "cockpit",
            json!({"split": "h", "children": [
                {"panel": "tree", "size": "26%"},
                {"panel": "console"},
                {"panel": "plan", "size": "30%"}]}),
        ),
        (
            "focus",
            json!({"split": "h", "children": [
                {"split": "v", "size": "24%", "children": [
                    {"panel": "tree"},
                    {"panel": "plan", "size": "45%"}]},
                {"split": "v", "children": [
                    {"panel": "agent"},
                    {"panel": "console", "size": "30%"}]}]}),
        ),
        (
            "classic",
            json!({"split": "h", "children": [
                {"panel": "console"},
                {"split": "v", "size": "42%", "children": [
                    {"panel": "plan"},
                    {"panel": "tree", "size": "40%"}]}]}),
        ),
        // Many agents at once: the status panel (what each needs from you, most urgent
        // first) in the tree's place.
        (
            "triage",
            json!({"split": "h", "children": [
                {"panel": "status", "size": "30%"},
                {"panel": "console"},
                {"panel": "plan", "size": "28%"}]}),
        ),
    ];
    Layouts {
        file: Map::new(),
        entries: entries
            .into_iter()
            .map(|(name, v)| (name.to_string(), v))
            .collect(),
        file_error: None,
    }
}

impl Layouts {
    /// The file's text (`None`: there is no file), merged with the presets. A text that
    /// doesn't parse gives the presets alone, with the error.
    pub fn load(text: Option<&str>) -> Layouts {
        let Some(text) = text else {
            return presets();
        };
        merged(text).unwrap_or_else(|why| Layouts {
            file_error: Some(why),
            ..presets()
        })
    }

    /// The layout named `name`, parsed.
    pub fn node(&self, name: &str) -> Result<Node, LayoutError> {
        let entry = self.entry(name)?;
        if name.is_empty() {
            return Err(LayoutError::NoName);
        }
        parse(entry)
    }

    /// The JSON of the layout named `name`.
    pub fn entry(&self, name: &str) -> Result<&Value, LayoutError> {
        self.entries.get(name).ok_or_else(|| LayoutError::NoSuch {
            name: name.to_string(),
            have: self.names().collect::<Vec<_>>().join(", "),
        })
    }

    /// Every layout's name, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// The file's text with `layout` saved as `name`: its own keys and entries as they were,
    /// in its order, never the presets it doesn't hold, pretty-printed with 2 spaces as the
    /// spike's `json.dump(indent=2)`. An `Err` while the file doesn't parse.
    pub fn file_text_with(&self, name: &str, layout: &Value) -> Result<String, LayoutError> {
        if let Some(why) = &self.file_error {
            return Err(LayoutError::FileBroken(why.clone()));
        }
        // `file` keeps the entries' names; `entries` keeps their order.
        let held = self.file.get("layouts").and_then(Value::as_object);
        let mut own: IndexMap<&str, &Value> = self
            .entries
            .iter()
            .filter(|(n, _)| held.is_some_and(|h| h.contains_key(*n)))
            .map(|(n, v)| (n.as_str(), v))
            .collect();
        own.insert(name, layout);
        let mut top: BTreeMap<&str, Part> = self
            .file
            .iter()
            .map(|(k, v)| (k.as_str(), Part::Value(v)))
            .collect();
        top.insert("layouts", Part::Entries(own));
        let mut text =
            serde_json::to_string_pretty(&top).expect("JSON with string keys always serializes");
        text.push('\n');
        Ok(text)
    }
}

/// A top-level value of the file as it is written.
#[derive(Serialize)]
#[serde(untagged)]
enum Part<'a> {
    Value(&'a Value),
    Entries(IndexMap<&'a str, &'a Value>),
}

/// The file's text merged with the presets (the spike's merge), or why it doesn't parse. An
/// empty file, and `"layouts": null`, hold no entries.
fn merged(text: &str) -> Result<Layouts, String> {
    /// The entries, in the file's order (a `Map` sorts them).
    #[derive(Deserialize)]
    struct InOrder {
        #[serde(default)]
        layouts: Option<IndexMap<String, Value>>,
    }
    if text.trim().is_empty() {
        return Ok(presets());
    }
    let file = match serde_json::from_str(text).map_err(|e| e.to_string())? {
        Value::Object(file) => file,
        _ => return Err("it must be one JSON object".into()),
    };
    let InOrder { layouts } = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let own = layouts.unwrap_or_default();
    let mut entries: IndexMap<String, Value> = presets()
        .entries
        .into_iter()
        .filter(|(name, _)| !own.contains_key(name))
        .collect();
    entries.extend(own);
    Ok(Layouts {
        file,
        entries,
        file_error: None,
    })
}

/// After the daemon couldn't write `layouts.json` for a `set`: the file read again, so the
/// state and the model drop the layout that wasn't saved, and the `layouts` model that says
/// so, when it changed.
pub fn reread(state: &mut State) -> Option<crate::effect::Publish> {
    state.layouts = crate::ops::layout::read(state);
    state
        .publish_changed(crate::publish::Topic::Layouts)
        .map(crate::effect::Publish::Model)
}

/// The brain's current layout: the meta value, else `cockpit`. A name the file no longer has,
/// or a broken entry, falls back to `cockpit` too.
pub fn current_layout(state: &State) -> String {
    match state.meta(CURRENT_KEY) {
        Some(name) if state.layouts.node(name).is_ok() => name.to_string(),
        _ => DEFAULT_LAYOUT.to_string(),
    }
}
