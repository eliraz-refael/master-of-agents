//! The default keymap: which actions a key may mean in a view, and each view's key hints.

use moa_core::panel::View;

/// A key press, as a panel reads it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Enter,
    Esc,
    Backspace,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
}

/// What a key asks a panel to do.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Action {
    Down,
    Up,
    Top,
    Bottom,
    PageUp,
    PageDown,
    Activate,
    NextTab,
    PrevTab,
    /// The picture's tab by the digit pressed, from 1.
    Tab(u8),
    Quit,
    NewSession,
    ChooseAccount,
    NewGroup,
    Move,
    Rename,
    DeleteGroup,
    Archive,
    Close,
    /// The terminals view's `n` and `t`: a note, and a timeout.
    Note,
    Timeout,
    Restart,
    Done,
    Drop,
    Yes,
    No,
}

/// Keys that move through a list, the same in every view; Ctrl-c quits (V15), though
/// [`Panel::interrupt`](crate::panel::Panel::interrupt) takes it first (L14).
fn moving(key: Key) -> &'static [Action] {
    match key {
        Key::Char('j') | Key::Down => &[Action::Down],
        Key::Char('k') | Key::Up => &[Action::Up],
        Key::Char('g') | Key::Home => &[Action::Top],
        Key::Char('G') | Key::End => &[Action::Bottom],
        Key::PageUp => &[Action::PageUp],
        Key::PageDown => &[Action::PageDown],
        Key::Enter | Key::Char(' ') => &[Action::Activate],
        Key::Ctrl('c') => &[Action::Quit],
        _ => &[],
    }
}

/// The picture's own keys: its tabs, and `q`, which quits only the picture.
fn picture(key: Key) -> &'static [Action] {
    match key {
        Key::Tab | Key::Right | Key::Char('l') => &[Action::NextTab],
        Key::BackTab | Key::Left | Key::Char('h') => &[Action::PrevTab],
        Key::Char('1') => &[Action::Tab(1)],
        Key::Char('2') => &[Action::Tab(2)],
        Key::Char('3') => &[Action::Tab(3)],
        Key::Char('4') => &[Action::Tab(4)],
        Key::Char('5') => &[Action::Tab(5)],
        Key::Char('6') => &[Action::Tab(6)],
        Key::Char('q') => &[Action::Quit],
        _ => &[],
    }
}

/// The keys on a view's rows: the tree's own (V7), the plan's tasks, and elsewhere `a` and
/// `x` on a session row, `n` and `N` on a row with a project (V21). The plan's `x` only
/// drops; where suggestions are listed, `n` is No first.
fn rows(view: View, key: Key) -> &'static [Action] {
    match (view, key) {
        (View::Tree, Key::Char('n')) => &[Action::NewSession],
        (View::Tree, Key::Char('N')) => &[Action::ChooseAccount],
        (View::Tree, Key::Char('+')) => &[Action::NewGroup],
        (View::Tree, Key::Char('m')) => &[Action::Move],
        (View::Tree, Key::Char('r')) => &[Action::Rename],
        (View::Tree, Key::Char('D')) => &[Action::DeleteGroup],
        (View::Tree, Key::Char('a')) => &[Action::Archive],
        (View::Tree, Key::Char('x')) => &[Action::Close],
        (View::Tree, Key::Char('R')) => &[Action::Restart],
        (View::Plan, Key::Char('d')) => &[Action::Done],
        (View::Plan, Key::Char('x')) => &[Action::Drop],
        (View::Today, Key::Char('y')) => &[Action::Yes],
        (View::Today, Key::Char('n')) => &[Action::No, Action::NewSession],
        (View::Status | View::Projects | View::Today, Key::Char('x')) => &[Action::Close],
        (View::Plan | View::Status | View::Projects | View::Today, Key::Char('a')) => {
            &[Action::Archive]
        }
        (View::Plan | View::Status | View::Projects, Key::Char('n')) => &[Action::NewSession],
        (View::Plan | View::Status | View::Projects | View::Today, Key::Char('N')) => {
            &[Action::ChooseAccount]
        }
        // The terminals view's own keys (5d): close, name, note, timeout.
        (View::Terms, Key::Char('x')) => &[Action::Close],
        (View::Terms, Key::Char('r')) => &[Action::Rename],
        (View::Terms, Key::Char('n')) => &[Action::Note],
        (View::Terms, Key::Char('t')) => &[Action::Timeout],
        // Help and the terminals view close, as Ctrl-c closes them (S10).
        (View::Help | View::Terms, Key::Char('q') | Key::Esc) => &[Action::Quit],
        _ => &[],
    }
}

/// What `key` may mean in `view`, in order: the panel takes the first that applies to the
/// selected item (V7, V21). `tab` is the tab the picture shows (`None` for every other
/// view): its rows follow that tab's keys, beside the picture's own tab keys and `q`.
pub fn actions(view: View, tab: Option<View>, key: Key) -> &'static [Action] {
    let moving = moving(key);
    if !moving.is_empty() {
        return moving;
    }
    if view != View::Picture {
        return rows(view, key);
    }
    match picture(key) {
        [] => tab
            .filter(|t| *t != View::Picture)
            .map_or(&[], |t| rows(t, key)),
        own => own,
    }
}

macro_rules! tree_hint {
    () => {
        "⏎/click open · n new · + group · m move · r rename · D del group · a archive · x close"
    };
}

/// The key hints at the foot of a view (the spike's), with the keys V21 adds on session rows
/// and rows with a project; the tree's gain `R` while an adopted session is selected. A view
/// without a list has none.
pub fn hint(view: View, adopted_selected: bool) -> &'static str {
    match view {
        View::Tree if adopted_selected => concat!(tree_hint!(), " · R restart under moa"),
        View::Tree => tree_hint!(),
        View::Plan => "⏎/click jump · d done · x drop · a archive · n/N new",
        View::Status => "⏎/click jump · j/k · a archive · x close · n/N new · Ctrl-a / find",
        View::Today => "j/k · ⏎ open · y/n suggestion · a archive · x close · n/N new",
        View::Projects => "⏎ expand · a archive · x close · n new session · N choose account",
        View::Activity => "j/k scroll",
        View::Picture => {
            "1-6/tab · j/k · ⏎ open · n/N new · a archive · x close · y/n suggestion · q quit"
        }
        View::Picker => "↵ jump · ↑↓ move · esc close",
        View::Terms => "↵ jump · x close · r name · n note · t timeout · esc close",
        View::Help => "j/k scroll · q close",
        View::Console | View::EmptySlot | View::Setup => "",
    }
}

/// The picture's key hints for the tab it shows: the sessions and status tabs keep their
/// views' own hints, as the spike's did; the others name the tab keys, the keys that work on
/// that tab's rows, and `q`.
pub fn picture_hint(tab: View, adopted_selected: bool) -> &'static str {
    match tab {
        View::Tree | View::Status => hint(tab, adopted_selected),
        View::Plan => "1-6/tab · j/k · ⏎ jump · d done · x drop · a archive · n/N new · q quit",
        View::Today => {
            "1-6/tab · j/k · ⏎ jump · y/n suggestion · a archive · x close · n/N new · q quit"
        }
        View::Projects => "1-6/tab · j/k · ⏎ expand/jump · a archive · x close · n/N new · q quit",
        View::Activity => "1-6/tab · j/k scroll · q quit",
        View::Picture
        | View::Console
        | View::Picker
        | View::Terms
        | View::EmptySlot
        | View::Help
        | View::Setup => hint(View::Picture, adopted_selected),
    }
}
