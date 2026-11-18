//! Every pane on the server, as reconcile, adopt and `ls` read them: one tokened query of two
//! `list-panes -a`, and the test of whether a binding still holds its pane (R4).

use std::collections::BTreeMap;

use super::cmd::Cmd;
use super::ids::{Generation, PaneId};
use crate::model::{PaneBinding, SessionId};
use crate::state::State;

/// One line per pane: its id, process, whether it is dead, moa's tags on it, and its folder.
/// The folder is last, so a tab in it stays in it.
pub const PANE_INFO: &str = "I\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{@moa_term}\t#{@moa_adopted}\t#{@moa_session}\t#{pane_current_path}";

/// One line per pane: its id and the command it was started with, as tmux quotes it. On a
/// line of its own, since the folder could hold anything.
pub const PANE_START: &str = "S\t#{pane_id}\t#{pane_start_command}";

/// The session a pane is tagged with (`@moa_session`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Tag {
    Unset,
    Session(SessionId),
    /// Set, but not a session id: it counts as another session's.
    Other,
}

/// One listed pane.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PaneInfo {
    pub pane: PaneId,
    pub pid: i64,
    pub dead: bool,
    /// A terminal moa opened (`@moa_term`).
    pub term: bool,
    /// An agent moa adopted (`@moa_adopted`).
    pub adopted: bool,
    pub tag: Tag,
    pub cwd: String,
    /// The command the pane was started with, one word per argument; empty when unknown.
    pub start: Vec<String>,
}

/// The two listings, in the order [`parse_panes`] takes their answers.
pub fn list_panes() -> Vec<Cmd> {
    vec![
        Cmd::new("list-panes").args(["-a", "-F", PANE_INFO]),
        Cmd::new("list-panes").args(["-a", "-F", PANE_START]),
    ]
}

/// The panes in `info` ([`PANE_INFO`] lines), each with its start command from `start`
/// ([`PANE_START`] lines). Lines that don't parse are skipped; a pane whose start line is
/// missing or doesn't parse has an empty `start`.
///
/// tmux prints a folder raw, so a folder holding a newline can forge a line of its own. A
/// pane id listed more than once is left out entirely: neither line can be trusted, and an
/// unlisted pane is one no binding holds (R4). A forged line for an id the server doesn't
/// have can only keep a binding of a pane that is already gone, and tmux never reuses a pane
/// id within one server's life, so nothing moa sends to that id reaches a pane.
pub fn parse_panes(info: &[String], start: &[String]) -> Vec<PaneInfo> {
    let mut panes: Vec<PaneInfo> = info.iter().filter_map(|l| parse_info(l)).collect();
    let mut seen = BTreeMap::<PaneId, usize>::new();
    for p in &panes {
        *seen.entry(p.pane).or_default() += 1;
    }
    panes.retain(|p| seen[&p.pane] == 1);
    for line in start {
        let mut f = line.splitn(3, '\t');
        let (Some("S"), Some(pane), Some(cmd)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let Ok(pane) = pane.parse::<PaneId>() else {
            continue;
        };
        if let Some(p) = panes.iter_mut().find(|p| p.pane == pane) {
            p.start = split_start_command(cmd);
        }
    }
    panes
}

fn parse_info(line: &str) -> Option<PaneInfo> {
    let f: Vec<&str> = line.splitn(8, '\t').collect();
    let [kind, pane, pid, dead, term, adopted, tag, cwd] = f.as_slice() else {
        return None;
    };
    if *kind != "I" || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // An unset option prints as an empty field.
    let tag = match *tag {
        "" => Tag::Unset,
        t => t.parse().map_or(Tag::Other, Tag::Session),
    };
    Some(PaneInfo {
        pane: pane.parse().ok()?,
        pid: pid.parse().ok()?,
        dead: *dead == "1",
        term: !term.is_empty(),
        adopted: !adopted.is_empty(),
        tag,
        cwd: cwd.to_string(),
        start: Vec::new(),
    })
}

/// The arguments of a start command as tmux 3.4+ prints it (`cmd_stringify_argv`): each
/// argument through `args_escape`, joined by spaces. `args_escape` quotes with `"` or `'`
/// and escapes with `vis` (`VIS_OCTAL | VIS_CSTYLE | VIS_TAB | VIS_NL`), so the line never
/// holds a newline and a backslash is always an escape. A line tmux couldn't have printed
/// (an unbalanced quote, a trailing backslash) gives no words.
pub fn split_start_command(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let mut words = Vec::new();
    let mut word: Vec<u8> = Vec::new();
    let mut in_word = false;
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        i += 1;
        match c {
            b' ' if quote.is_none() => {
                if in_word {
                    words.push(String::from_utf8_lossy(&word).into_owned());
                    word.clear();
                    in_word = false;
                }
            }
            b'\'' | b'"' if quote.is_none() => {
                quote = Some(c);
                in_word = true;
            }
            c if Some(c) == quote => quote = None,
            b'\\' => {
                let Some(&e) = b.get(i) else {
                    return Vec::new();
                };
                i += 1;
                let byte = match e {
                    b'n' => b'\n',
                    b't' => b'\t',
                    b'r' => b'\r',
                    b'b' => 0x08,
                    b'a' => 0x07,
                    b'v' => 0x0b,
                    b'f' => 0x0c,
                    b'0'..=b'7' => {
                        // Up to three octal digits.
                        let mut n = u32::from(e - b'0');
                        for _ in 0..2 {
                            match b.get(i) {
                                Some(&d @ b'0'..=b'7') => {
                                    n = n * 8 + u32::from(d - b'0');
                                    i += 1;
                                }
                                _ => break,
                            }
                        }
                        (n & 0xff) as u8
                    }
                    other => other,
                };
                word.push(byte);
                in_word = true;
            }
            c => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return Vec::new();
    }
    if in_word {
        words.push(String::from_utf8_lossy(&word).into_owned());
    }
    words
}

/// Whether `b` still holds its pane (R4): it was bound on this server (`generation`), and
/// its pane is listed (`info`), alive, and tagged with its session or not tagged at all.
pub fn holds(b: &PaneBinding, generation: Generation, info: Option<&PaneInfo>) -> bool {
    let Some(info) = info else {
        return false;
    };
    of_server(b, generation)
        && b.pane_id.parse() == Ok(info.pane)
        && !info.dead
        && match info.tag {
            Tag::Unset => true,
            Tag::Session(id) => id == b.session_id,
            Tag::Other => false,
        }
}

/// Whether `b` was bound on the server at `generation`.
pub(crate) fn of_server(b: &PaneBinding, generation: Generation) -> bool {
    b.server_pid == generation.pid && b.server_start == generation.start
}

/// The pane `b` names, as `panes` lists it.
pub fn listed<'p>(b: &PaneBinding, panes: &'p [PaneInfo]) -> Option<&'p PaneInfo> {
    let pane: PaneId = b.pane_id.parse().ok()?;
    panes.iter().find(|p| p.pane == pane)
}

/// The bindings that still hold their pane in `panes`, a listing of the server at
/// `generation` ([`holds`]).
pub(crate) fn kept_bindings<'s>(
    state: &'s State,
    generation: Generation,
    panes: &[PaneInfo],
) -> Vec<&'s PaneBinding> {
    state
        .pane_bindings()
        .filter(|b| holds(b, generation, listed(b, panes)))
        .collect()
}
