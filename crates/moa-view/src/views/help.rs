//! Help (S10, the spike's `help.txt`): every key moa has, written from moa's own tables (the
//! `Ctrl-a` key table, each view's key hints, the layouts, the display states), so it can't
//! drift from what the keys do. Nothing on it is selectable; keys scroll it, as activity's do
//! (V17).

use moa_core::config::Form;
use moa_core::keymap::ctrl_a;
use moa_core::live::DisplayState;
use moa_core::panel::View;

use crate::ctx::Ctx;
use crate::data::Data;
use crate::keys;
use crate::line::{Line, Span, Tone};
use crate::text::{self, clean, cut, wrap};
use crate::views::{animates, live, state_glyph, status};

/// The prefix section's heading: the user's tmux prefix may not be Ctrl-b.
pub const PREFIX: &str = "tmux's prefix (Ctrl-b unless you changed it)";

/// The views whose own keys help lists, by their key hints, in this order.
const VIEWS: [View; 7] = [
    View::Tree,
    View::Plan,
    View::Status,
    View::Today,
    View::Projects,
    View::Activity,
    View::Picture,
];

/// Help's lines, none wider than `width` cells. The `Ctrl-a` keys are in the form the
/// `brain` row's `picker` names (the popup until the row comes).
pub fn build(data: &Data, ctx: &Ctx, width: u16) -> Vec<Line> {
    let width = usize::from(width);
    let form = data.brain.as_ref().map_or(Form::default(), |b| b.picker);
    let mut out = Vec::new();

    out.push(heading("Ctrl-a keys", width));
    let bindings: Vec<_> = ctrl_a(form).into_iter().flatten().collect();
    let key_w = bindings
        .iter()
        .map(|b| text::width(b.key))
        .max()
        .unwrap_or(0);
    for b in &bindings {
        let pad = key_w - text::width(b.key);
        let lead = Span::new(format!("  {}{}  ", b.key, " ".repeat(pad)), Tone::Head);
        out.extend(entry(lead, b.help, Tone::Text, width));
    }

    out.push(blank());
    out.push(heading(PREFIX, width));
    out.extend(said("Space menu · m back to moa · C repair", width));

    out.push(blank());
    out.push(heading("In every panel", width));
    out.extend(said(
        "j/k ↑↓ move · g/G top/bottom · PgUp/PgDn · ⏎ or a click acts · the wheel scrolls \
         · Ctrl-c cancels a prompt",
        width,
    ));
    out.extend(said("hold Shift to select text", width));
    for view in VIEWS {
        let lead = Span::new(format!("  {}: ", view.label()), Tone::Head);
        out.extend(entry(lead, keys::hint(view, false), Tone::Text, width));
    }

    out.push(blank());
    out.push(heading("Layouts", width));
    if let Some(error) = &data.layouts_error {
        out.extend(entry(indent(), &clean(error), Tone::Warn, width));
    }
    let mut layouts: Vec<_> = data.layouts.values().collect();
    layouts.sort_by_key(|l| l.index);
    for l in layouts {
        let mark = if l.current { "▸" } else { " " };
        let lead = Span::new(
            format!(
                "  {mark} {} {}: ",
                l.index.saturating_add(1),
                clean(&l.name)
            ),
            Tone::Head,
        );
        match &l.error {
            Some(error) => out.extend(entry(lead, &clean(error), Tone::Warn, width)),
            None => out.extend(entry(
                lead,
                &clean(&l.leaves.join(" · ")),
                Tone::Text,
                width,
            )),
        }
    }
    out.extend(said("change one: moa layout set NAME JSON", width));

    out.push(blank());
    out.push(heading("Agent states", width));
    // `DisplayState`'s order is the urgency order, as the status view groups them.
    for &state in DisplayState::ALL.iter().filter(|s| live(**s)) {
        let (tone, bold) = status::look(state);
        let shown = format!("  {} {}", state_glyph(state, ctx.tick), status::word(state));
        let mut line = Line::new(vec![Span::styled(cut(&shown, width), tone, bold)]);
        line.animated = animates(state);
        out.push(line);
    }
    out
}

fn heading(words: &str, width: usize) -> Line {
    Line::new(vec![Span::bold(cut(words, width), Tone::Head)])
}

fn blank() -> Line {
    Line::plain("", Tone::Dim)
}

fn indent() -> Span {
    Span::new("  ", Tone::Text)
}

/// A line of plain words under a heading, wrapped.
fn said(words: &str, width: usize) -> Vec<Line> {
    entry(indent(), words, Tone::Text, width)
}

/// `lead` then `words` in `tone`, wrapped under the words' first column; at a width too
/// narrow for that, under a fixed indent, and every line cut to `width`.
fn entry(lead: Span, words: &str, tone: Tone, width: usize) -> Vec<Line> {
    let lead_w = text::width(&lead.text);
    let hang = if width >= lead_w + 20 { lead_w } else { 4 };
    let room = width.saturating_sub(hang).max(10);
    let first_room = width.saturating_sub(lead_w).max(10);
    let mut rows = wrap(words, first_room).into_iter();
    let first = rows.next().unwrap_or_default();
    let rest = wrap(&rows.collect::<Vec<_>>().join(" "), room);
    let mut lines = vec![fitted(vec![lead, Span::new(first, tone)], width)];
    for row in rest.into_iter().filter(|r| !r.is_empty()) {
        let spans = vec![Span::new(" ".repeat(hang), tone), Span::new(row, tone)];
        lines.push(fitted(spans, width));
    }
    lines
}

/// The line of `spans`, cut to `width` cells.
fn fitted(spans: Vec<Span>, width: usize) -> Line {
    let mut left = width;
    let mut kept = Vec::new();
    for mut span in spans {
        span.text = cut(&span.text, left);
        left -= text::width(&span.text);
        kept.push(span);
    }
    Line::new(kept)
}
