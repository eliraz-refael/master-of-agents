//! `layout.list`, `layout.show` and `layout.set` (L3). Each reads `layouts.json` again first,
//! so a hand edit counts at the next layout op with no watcher, and publishes `layouts` when
//! that changed it. Nothing is saved in the brain: `set` writes the file through the daemon.

use serde_json::{Value, json};

use crate::effect::{Effect, Publish};
use crate::layout::{Layouts, check_name, current_layout, parse, presets};
use crate::op::{ConnId, Done, Op, OpError, ReqId};
use crate::ops::term::with_tilde;
use crate::publish::Topic;
use crate::state::State;

/// The file as it is now. One that can't be read counts as broken: the presets work, and
/// nothing is written over it.
pub(crate) fn read(state: &State) -> Layouts {
    let deps = state.deps();
    match (deps.read_text)(&deps.layouts_file) {
        Ok(text) => Layouts::load(text.as_deref()),
        Err(e) => Layouts {
            file_error: Some(format!("it can't be read ({e})")),
            ..presets()
        },
    }
}

/// Runs a layout op: the file read again, then the op, then `layouts` if it changed, then
/// the reply.
pub(crate) fn run(state: &mut State, conn: ConnId, req: ReqId, op: Op) -> Vec<Effect> {
    state.layouts = read(state);
    let mut fx = Vec::new();
    let result = match op {
        Op::LayoutList(_) => Ok(list(state)),
        Op::LayoutShow { name } => show(state, &name),
        Op::LayoutSet { name, layout } => set(state, (conn, req), &name, &layout, &mut fx),
        _ => Err(OpError("moa can't answer that here".into())),
    };
    fx.extend(
        state
            .publish_changed(Topic::Layouts)
            .map(|m| Effect::Publish(Publish::Model(m))),
    );
    fx.push(Effect::Reply { conn, req, result });
    fx
}

/// `▸ cockpit: tree · console · plan` per layout (`▸` for the current one), then the
/// file's error if it has one, then where the file is.
fn list(state: &State) -> Done {
    let rows = state.layout_rows();
    let mut lines: Vec<String> = rows
        .iter()
        .map(|r| {
            let mark = if r.current { '▸' } else { ' ' };
            match &r.error {
                Some(e) => format!("{mark} {}: has an error: {e}", r.name),
                None => format!("{mark} {}: {}", r.name, r.leaves.join(" · ")),
            }
        })
        .collect();
    let file_error = &state.layouts.file_error;
    if let Some(e) = file_error {
        lines.push(format!("layouts.json has an error: {e}"));
    }
    let deps = state.deps();
    let config = with_tilde(
        &deps.layouts_file.display().to_string(),
        &deps.home,
        deps.canonical,
    );
    lines.push(format!("(config: {config})"));
    Done {
        text: lines.join("\n"),
        data: json!({
            "layouts": rows,
            "current": current_layout(state),
            "file_error": file_error,
            "config": config,
        }),
    }
}

/// The entry's JSON, pretty.
fn show(state: &State, name: &str) -> Result<Done, OpError> {
    let entry = state
        .layouts
        .entry(name)
        .map_err(|e| OpError(e.to_string()))?;
    Ok(Done {
        text: pretty(entry),
        data: json!({"name": name, "layout": entry}),
    })
}

/// Validates `layout`, then has the daemon write the file with it (`Effect::WriteFile`),
/// which answers for `req` when the write fails.
fn set(
    state: &mut State,
    req: (ConnId, ReqId),
    name: &str,
    layout: &Value,
    fx: &mut Vec<Effect>,
) -> Result<Done, OpError> {
    let refused = |e: crate::layout::LayoutError| OpError(e.to_string());
    check_name(name).map_err(refused)?;
    parse(layout).map_err(refused)?;
    let text = state
        .layouts
        .file_text_with(name, layout)
        .map_err(refused)?;
    state.layouts = Layouts::load(Some(&text));
    fx.push(Effect::WriteFile {
        path: state.deps().layouts_file.clone(),
        text,
        req: Some(req),
    });
    Ok(Done {
        text: format!("saved layout {name}; apply it with: moa layout apply {name}"),
        data: json!({ "name": name }),
    })
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}
