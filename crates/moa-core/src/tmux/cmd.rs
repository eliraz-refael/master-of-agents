//! Commands for tmux: the only way moa sends one, with an allow-list.

use std::path::Path;

use super::ids::ClientName;

/// The commands moa sends, by canonical name (an alias like `set` is refused).
pub const ALLOWED: &[&str] = &[
    "display-message",
    "set-option",
    "set-hook",
    "show-options",
    "list-sessions",
    "list-panes",
    "list-clients",
    "new-session",
    "source-file",
    "command-prompt",
    "new-window",
    "kill-pane",
    "select-window",
    "select-pane",
    "swap-pane",
    "join-pane",
    "break-pane",
    "resize-pane",
    "respawn-pane",
    "rename-window",
    "resize-window",
    "run-shell",
];

/// A command that must not be sent, with the reason.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
#[error("{0}")]
pub struct Refused(pub &'static str);

const NOT_ALLOWED: &str = "that isn't a command moa sends";
const NEEDS_B: &str = "command-prompt needs -b";
const PROMPT: &str = "command-prompt only sends moa:prompt back to moa";
const OWN_CONFIG: &str = "moa only sources its own config";
const NEEDS_D_P: &str = "new-window needs -d and -P";
const NEW_WINDOW_TARGET: &str = "new-window needs -t and =moa: or =_moa:";
const ENV_NEWLINE: &str = "a new window's environment can't hold a newline";
const ONE_PANE: &str = "kill-pane needs exactly one tab id";
const NEEDS_T: &str = "select-window and select-pane need -t and a tab id";
const SWAP: &str = "swap-pane needs -d and two tab ids";
const JOIN: &str = "join-pane needs -d, -h or -v, and two tab ids";
const BREAK: &str = "break-pane needs -d, a tab id, and =moa: or =_moa:";
const RESIZE: &str = "resize-pane needs a tab id, and -x or -y with a size";
const RESPAWN: &str = "respawn-pane needs -k, a tab id and a command";
const RESPAWN_ENV: &str = "a tab's environment can't hold a newline";
const RENAME: &str = "rename-window needs a tab id, then -- and a name";
const RENAME_FORMAT: &str = "rename-window's name must keep # literal (##)";
const RESIZE_WINDOW: &str = "resize-window needs -A and a tab id";
const WINDOW_SIZE: &str = "window-size is only unset, on a tab id's window";
const REAP: &str = "run-shell only runs true";
const EMPTY_CHAIN: &str = "a chain needs at least one command";

/// What a chain is called where a name is asked for (a log line). It is no tmux command.
const CHAIN: &str = "chain";

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cmd {
    name: &'static str,
    args: Vec<String>,
    /// A chain's commands, run in order on one line; `None` for a plain command.
    parts: Option<Vec<Cmd>>,
}

impl Cmd {
    pub fn new(name: &'static str) -> Cmd {
        Cmd {
            name,
            args: Vec::new(),
            parts: None,
        }
    }

    /// The commands as one line, `a ; b ; …`. tmux runs them in order and skips the rest
    /// after one fails, so a later command never acts on a move that didn't happen. It is
    /// answered as one: `Ok` with no lines, or the first error's text. A chain inside a chain
    /// is its parts. Each part is checked, and an empty chain is refused.
    pub fn chain(cmds: Vec<Cmd>) -> Cmd {
        let mut parts = Vec::with_capacity(cmds.len());
        for c in cmds {
            match c.parts {
                Some(inner) => parts.extend(inner),
                None => parts.push(c),
            }
        }
        Cmd {
            name: CHAIN,
            args: Vec::new(),
            parts: Some(parts),
        }
    }

    /// The one `command-prompt` moa sends (S5): `label` in `client`'s status line, prefilled
    /// with `prefill`, whose answer comes back to the control client `ctl` as
    /// `moa:prompt TOKEN TEXT`. `-b` keeps the control client from waiting for the answer.
    /// tmux expands `label` and `prefill` as formats and with strftime, and splits `-I` on
    /// `,`: the caller passes no `#`, `%` or `,` in them. The template is in double quotes,
    /// because `%%%` escapes the answer for those (a `'` in it would be lost in single ones).
    pub fn prompt(
        client: &ClientName,
        label: &str,
        prefill: &str,
        ctl: &ClientName,
        token: u64,
    ) -> Cmd {
        Cmd::new("command-prompt")
            .args(["-b", "-t", client.0.as_str(), "-p", label, "-I", prefill])
            .arg(format!(
                "display-message -c {} -l \"moa:prompt {token} %%%\"",
                ctl.0
            ))
    }

    /// A chain's commands; a plain command is its own one part.
    pub fn parts(&self) -> &[Cmd] {
        match &self.parts {
            Some(parts) => parts,
            None => std::slice::from_ref(self),
        }
    }

    /// Panics on a chain: its line is its parts', so an argument would be silently lost.
    pub fn arg(mut self, a: impl Into<String>) -> Cmd {
        assert!(self.parts.is_none(), "a chain takes no arguments");
        self.args.push(a.into());
        self
    }

    /// Panics on a chain, like [`Cmd::arg`].
    pub fn args<S: Into<String>>(mut self, a: impl IntoIterator<Item = S>) -> Cmd {
        assert!(self.parts.is_none(), "a chain takes no arguments");
        self.args.extend(a.into_iter().map(Into::into));
        self
    }

    pub fn name(&self) -> &str {
        self.name
    }

    pub fn argv(&self) -> &[String] {
        &self.args
    }

    /// The text to send: the name and the quoted arguments, or a chain's parts joined by
    /// ` ; `. Never holds a newline. A `;` in an argument is quoted, so it never splits one.
    pub fn line(&self) -> String {
        if let Some(parts) = &self.parts {
            let lines: Vec<String> = parts.iter().map(Cmd::line).collect();
            return lines.join(" ; ");
        }
        let mut line = self.name.to_string();
        for a in &self.args {
            line.push(' ');
            line.push_str(&quote(a));
        }
        line
    }

    /// Whether this command runs other commands, each of which tmux answers with its own
    /// reply block over a control client. `source-file` and a chain do; the client sends a
    /// sentinel after it to find where its blocks end.
    pub fn nests(&self) -> bool {
        self.parts.is_some() || self.name == "source-file"
    }

    /// Whether moa may send this command: for a chain, whether it may send every part.
    pub fn check(&self) -> Result<(), Refused> {
        if let Some(parts) = &self.parts {
            if parts.is_empty() {
                return Err(Refused(EMPTY_CHAIN));
            }
            return parts.iter().try_for_each(Cmd::check);
        }
        if !ALLOWED.contains(&self.name) {
            return Err(Refused(NOT_ALLOWED));
        }
        match self.name {
            "command-prompt" => check_prompt(&self.args),
            "source-file" => {
                let mut paths = self.args.iter().filter(|a| !a.starts_with('-')).peekable();
                if paths.peek().is_some() && paths.all(|p| is_own_config(p)) {
                    Ok(())
                } else {
                    Err(Refused(OWN_CONFIG))
                }
            }
            "new-window" => check_new_window(&self.args),
            // One pane, named by its id: never `-a`, never a target tmux would resolve.
            "kill-pane" => match self.args.as_slice() {
                [t, pane] if t == "-t" && is_pane_id(pane) => Ok(()),
                _ => Err(Refused(ONE_PANE)),
            },
            // The pane, by its id: its window, or the pane in it.
            "select-window" | "select-pane" => match self.args.as_slice() {
                [t, pane] if t == "-t" && is_pane_id(pane) => Ok(()),
                _ => Err(Refused(NEEDS_T)),
            },
            // The commands that move panes (L4) name panes by id and sessions exactly, so a
            // prefix never matches a stranger, and `-d` keeps the user's view where it is.
            "swap-pane" => match self.args.as_slice() {
                [d, s, a, t, b] if d == "-d" && s == "-s" && t == "-t" && panes(a, b) => Ok(()),
                _ => Err(Refused(SWAP)),
            },
            "join-pane" => match self.args.as_slice() {
                [d, hv, s, a, t, b]
                    if d == "-d"
                        && (hv == "-h" || hv == "-v")
                        && s == "-s"
                        && t == "-t"
                        && panes(a, b) =>
                {
                    Ok(())
                }
                _ => Err(Refused(JOIN)),
            },
            // Only to moa's own two sessions, by exact name; `-n` names the window literally.
            "break-pane" => match self.args.as_slice() {
                [d, s, a, t, to, rest @ ..]
                    if d == "-d"
                        && s == "-s"
                        && is_pane_id(a)
                        && t == "-t"
                        && (to == "=moa:" || to == "=_moa:")
                        && (rest.is_empty() || matches!(rest, [n, _] if n == "-n")) =>
                {
                    Ok(())
                }
                _ => Err(Refused(BREAK)),
            },
            "resize-pane" => match self.args.as_slice() {
                [t, a, xy, size]
                    if t == "-t"
                        && is_pane_id(a)
                        && (xy == "-x" || xy == "-y")
                        && is_size(size) =>
                {
                    Ok(())
                }
                _ => Err(Refused(RESIZE)),
            },
            "respawn-pane" => check_respawn(&self.args),
            // A rebuilt cockpit takes its session's size (L6): `-A`, the largest client's.
            "resize-window" => match self.args.as_slice() {
                [a, t, pane] if a == "-A" && t == "-t" && is_pane_id(pane) => Ok(()),
                _ => Err(Refused(RESIZE_WINDOW)),
            },
            // `resize-window` leaves the window's size fixed; unsetting it hands the window
            // back to the user's terminals. moa never sets it otherwise.
            "set-option" if option_name(&self.args) == Some("window-size") => {
                match self.args.as_slice() {
                    [w, u, t, pane, name]
                        if w == "-w"
                            && u == "-u"
                            && t == "-t"
                            && is_pane_id(pane)
                            && name == "window-size" =>
                    {
                        Ok(())
                    }
                    _ => Err(Refused(WINDOW_SIZE)),
                }
            }
            // A name is free text (an agent's title): after `--`, one that starts with `-` is
            // still a name, never flags. tmux expands it as a format, so it must have been
            // through [`literal_format`]: a lone `#` (`#(…)` runs a shell) is refused.
            "rename-window" => match self.args.as_slice() {
                [t, a, dashes, name] if t == "-t" && is_pane_id(a) && dashes == "--" => {
                    if name.replace("##", "").contains('#') {
                        Err(Refused(RENAME_FORMAT))
                    } else {
                        Ok(())
                    }
                }
                _ => Err(Refused(RENAME)),
            },
            // tmux 3.4 and 3.5 built with utempter can lose a pane's exit, and with it the
            // dead pane's line and `pane-died` (moa-tmux's `every_hook_reaches_the_mailbox`).
            // A job ending makes tmux reap: `true` is that job, and the only one moa runs.
            "run-shell" => match self.args.as_slice() {
                [t] if t == "true" => Ok(()),
                _ => Err(Refused(REAP)),
            },
            _ => Ok(()),
        }
    }
}

/// The option a `set-option` names: its first word that is no flag and no `-t`'s target. A
/// value after it (an agent's title) is never taken for it.
fn option_name(args: &[String]) -> Option<&str> {
    let mut words = args.iter();
    while let Some(w) = words.next() {
        match w.as_str() {
            "-t" => {
                words.next();
            }
            w if w.starts_with('-') => {}
            w => return Some(w),
        }
    }
    None
}

/// Both are pane ids.
fn panes(a: &str, b: &str) -> bool {
    is_pane_id(a) && is_pane_id(b)
}

/// A size `resize-pane` takes: cells (`12`) or a share of the window (`30%`).
fn is_size(s: &str) -> bool {
    let (digits, percent) = match s.strip_suffix('%') {
        Some(digits) => (digits, true),
        None => (s, false),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    // L1's sizes: cells from 1, a share of the window from 1 to 99 %.
    match digits.parse::<u16>() {
        Ok(n) if percent => (1..=99).contains(&n),
        Ok(n) => n >= 1,
        Err(_) => false,
    }
}

/// `respawn-pane -k -t %A [-c DIR] [-e K=V]… CMD…`: it kills what the pane runs (`-k`), so
/// the pane is named by its id, and it always says what runs next. No `-e` value holds a
/// newline, and the command's first word isn't one tmux would read as a flag.
fn check_respawn(args: &[String]) -> Result<(), Refused> {
    let [k, t, pane, rest @ ..] = args else {
        return Err(Refused(RESPAWN));
    };
    if k != "-k" || t != "-t" || !is_pane_id(pane) {
        return Err(Refused(RESPAWN));
    }
    let mut it = rest.iter();
    let first = loop {
        match it.next().map(String::as_str) {
            Some("-c") => {
                if it.next().is_none() {
                    return Err(Refused(RESPAWN));
                }
            }
            Some("-e") => match it.next() {
                Some(v) if v.contains(['\n', '\r']) => return Err(Refused(RESPAWN_ENV)),
                Some(_) => {}
                None => return Err(Refused(RESPAWN)),
            },
            other => break other,
        }
    };
    match first {
        Some(word) if !word.starts_with('-') => Ok(()),
        _ => Err(Refused(RESPAWN)),
    }
}

/// `new-window`'s flags, read up to the command it runs: `-d` (the user's view stays put)
/// and `-P` (it says which pane) are both there, every `-t` names one of moa's sessions
/// exactly (`=moa:` or `=_moa:`, so a prefix never matches a stranger's), and no `-e` value
/// holds a newline. The command's own arguments are quoted like any other. With no command
/// the window runs the user's shell: a terminal.
fn check_new_window(args: &[String]) -> Result<(), Refused> {
    let (mut detached, mut prints, mut target) = (false, false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-d" => detached = true,
            "-P" => prints = true,
            "-a" | "-b" | "-k" | "-S" => {}
            "-t" => match it.next().map(String::as_str) {
                Some("=moa:" | "=_moa:") => target = true,
                _ => return Err(Refused(NEW_WINDOW_TARGET)),
            },
            "-c" | "-F" | "-n" => {
                it.next();
            }
            "-e" => {
                if it.next().is_some_and(|v| v.contains(['\n', '\r'])) {
                    return Err(Refused(ENV_NEWLINE));
                }
            }
            // The command it runs.
            _ => break,
        }
    }
    if !(detached && prints) {
        return Err(Refused(NEEDS_D_P));
    }
    if !target {
        return Err(Refused(NEW_WINDOW_TARGET));
    }
    Ok(())
}

/// `command-prompt` only as [`Cmd::prompt`] builds it: `-b`, and `-t`, `-p` and `-I` each
/// with its value, each once, then one template that sends the answer back to a control
/// client as `moa:prompt`. A `-b` that is another flag's value doesn't count: a prompt
/// without one would freeze the control client until it is answered.
fn check_prompt(args: &[String]) -> Result<(), Refused> {
    let mut flags: Vec<&str> = Vec::new();
    let mut rest = args;
    loop {
        match rest {
            [b, tail @ ..] if b == "-b" => {
                flags.push("-b");
                rest = tail;
            }
            [f, _, tail @ ..] if matches!(f.as_str(), "-t" | "-p" | "-I") => {
                flags.push(f.as_str());
                rest = tail;
            }
            _ => break,
        }
    }
    if !flags.contains(&"-b") {
        return Err(Refused(NEEDS_B));
    }
    // Each flag once: a repeat leaves the sorted list longer than this.
    flags.sort_unstable();
    match rest {
        [template] if flags == ["-I", "-b", "-p", "-t"] && is_answer_template(template) => Ok(()),
        _ => Err(Refused(PROMPT)),
    }
}

/// `display-message -c NAME -l "moa:prompt DIGITS %%%"`, with a client name tmux's parser
/// reads as one bare word. The name holds no `%`: tmux puts the answer, unescaped, in place
/// of every `%1` and the first `%%` of a template, so a `%` there could run what is typed.
fn is_answer_template(t: &str) -> bool {
    let Some((name, rest)) = t
        .strip_prefix("display-message -c ")
        .and_then(|t| t.split_once(" -l \"moa:prompt "))
    else {
        return false;
    };
    let Some(token) = rest.strip_suffix(" %%%\"") else {
        return false;
    };
    !name.is_empty()
        && name.chars().all(|c| c != '%' && is_bare(c))
        && !token.is_empty()
        && token.bytes().all(|b| b.is_ascii_digit())
}

/// A pane id as tmux prints it: `%` and digits.
fn is_pane_id(s: &str) -> bool {
    s.strip_prefix('%')
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

/// An absolute path to a file named exactly `moa.tmux`, with no glob characters.
fn is_own_config(p: &str) -> bool {
    let path = Path::new(p);
    path.is_absolute()
        && !p.contains(['*', '?', '['])
        && path.file_name().is_some_and(|f| f == "moa.tmux")
}

/// A value tmux expands as a format after parsing (a `new-window` `-n` name or `-c`
/// folder), kept literal: every `#` doubled, so `#S`, `#{…}` and `#(…)` are just text.
/// [`quote`] guards only the command parser. moa-tmux's `conf.rs` doubles `#` the same way.
pub fn literal_format(s: &str) -> String {
    s.replace('#', "##")
}

fn is_bare(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_@%:=.,/+-".contains(c)
}

/// Quote one argument for tmux's command parser. Safe words go bare. Anything else is
/// single-quoted, `'` is written `'\''`, and a newline or CR is a double-quoted piece.
pub fn quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.chars().all(is_bare) {
        return s.to_string();
    }
    let mut out = String::new();
    let mut open = false;
    for c in s.chars() {
        match c {
            '\n' | '\r' => {
                if open {
                    out.push('\'');
                    open = false;
                }
                out.push_str(if c == '\n' { r#""\n""# } else { r#""\r""# });
            }
            _ => {
                if !open {
                    out.push('\'');
                    open = true;
                }
                if c == '\'' {
                    out.push_str(r"'\''");
                } else {
                    out.push(c);
                }
            }
        }
    }
    if open {
        out.push('\'');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_window(flags: &[&str]) -> Cmd {
        Cmd::new("new-window")
            .args(flags.iter().copied())
            .args(["-n", "x", "-c", "/w", "-t", "=moa:"])
            .args(["/opt/claude", "--name", "-d", "-P"])
    }

    #[test]
    fn literal_format_doubles_every_hash() {
        assert_eq!(literal_format("plain"), "plain");
        assert_eq!(literal_format("#S #(x) #{a} ##"), "##S ##(x) ##{a} ####");
    }

    #[test]
    fn new_window_needs_d_and_p() {
        assert_eq!(new_window(&["-d", "-P"]).check(), Ok(()));
        assert_eq!(new_window(&["-P", "-F", "x", "-d"]).check(), Ok(()));
        // `-d` and `-P` after the command it runs are the command's own words.
        for flags in [&["-d"][..], &["-P"], &[]] {
            assert_eq!(
                new_window(flags).check(),
                Err(Refused("new-window needs -d and -P")),
                "{flags:?}"
            );
        }
        for value in ["A=b\nc", "A=b\rc"] {
            assert_eq!(
                new_window(&["-d", "-P", "-e", value]).check(),
                Err(Refused("a new window's environment can't hold a newline"))
            );
        }
        // A newline in the command's own arguments is quoted, as any argument is.
        let cmd = new_window(&["-d", "-P"]).arg("two\nlines");
        assert_eq!(cmd.check(), Ok(()));
        assert!(
            cmd.line().ends_with(r#"'two'"\n"'lines'"#),
            "{}",
            cmd.line()
        );
    }

    #[test]
    fn kill_pane_needs_one_pane_id() {
        let kill = |args: &[&str]| Cmd::new("kill-pane").args(args.iter().copied()).check();
        assert_eq!(kill(&["-t", "%3"]), Ok(()));
        for args in [
            &[][..],
            &["-t", "moa"],
            &["-a", "-t", "%3"],
            &["-t", "%3x"],
            &["-t", "%"],
            &["-t", "%3", "-a"],
        ] {
            assert_eq!(
                kill(args),
                Err(Refused("kill-pane needs exactly one tab id")),
                "{args:?}"
            );
        }
    }

    #[test]
    fn select_needs_a_target() {
        for name in ["select-window", "select-pane"] {
            assert_eq!(Cmd::new(name).args(["-t", "%3"]).check(), Ok(()), "{name}");
            for args in [&[][..], &["-t"], &["-Z"]] {
                assert_eq!(
                    Cmd::new(name).args(args.iter().copied()).check(),
                    Err(Refused(
                        "select-window and select-pane need -t and a tab id"
                    )),
                    "{name} {args:?}"
                );
            }
        }
    }

    /// A chain's line is its parts', so an argument given to the chain itself would be lost:
    /// that is a bug in the caller, in every build.
    #[test]
    #[should_panic(expected = "a chain takes no arguments")]
    fn a_chain_takes_no_argument() {
        let _ = Cmd::chain(vec![Cmd::new("select-pane").args(["-t", "%1"])]).arg("-Z");
    }

    #[test]
    #[should_panic(expected = "a chain takes no arguments")]
    fn a_chain_takes_no_arguments() {
        let _ = Cmd::chain(vec![Cmd::new("select-pane").args(["-t", "%1"])]).args(["-Z"]);
    }

    #[test]
    fn a_terminal_window_has_no_command() {
        let cmd = Cmd::new("new-window").args(["-d", "-P", "-F", "x", "-n", "t", "-t", "=_moa:"]);
        assert_eq!(cmd.check(), Ok(()));
        let cmd = Cmd::new("new-window").args(["-d", "-P", "-c", "/w", "-t", "=moa:"]);
        assert_eq!(cmd.check(), Ok(()));
    }
}
