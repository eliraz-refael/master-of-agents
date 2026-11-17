//! Commands for tmux: the only way moa sends one, with an allow-list.

use std::path::Path;

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
];

/// A command that must not be sent, with the reason.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
#[error("{0}")]
pub struct Refused(pub &'static str);

const NOT_ALLOWED: &str = "that isn't a command moa sends";
const NEEDS_B: &str = "command-prompt needs -b";
const OWN_CONFIG: &str = "moa only sources its own config";
const NEEDS_D_P: &str = "new-window needs -d and -P";
const ENV_NEWLINE: &str = "a new window's environment can't hold a newline";

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cmd {
    name: &'static str,
    args: Vec<String>,
}

impl Cmd {
    pub fn new(name: &'static str) -> Cmd {
        Cmd {
            name,
            args: Vec::new(),
        }
    }

    pub fn arg(mut self, a: impl Into<String>) -> Cmd {
        self.args.push(a.into());
        self
    }

    pub fn args<S: Into<String>>(mut self, a: impl IntoIterator<Item = S>) -> Cmd {
        self.args.extend(a.into_iter().map(Into::into));
        self
    }

    pub fn name(&self) -> &str {
        self.name
    }

    pub fn argv(&self) -> &[String] {
        &self.args
    }

    /// The text to send: the name and the quoted arguments. Never holds a newline.
    pub fn line(&self) -> String {
        let mut line = self.name.to_string();
        for a in &self.args {
            line.push(' ');
            line.push_str(&quote(a));
        }
        line
    }

    /// Whether this command runs other commands, each of which tmux answers with its own
    /// reply block over a control client. Only `source-file` does; the client sends a
    /// sentinel after it to find where its blocks end.
    pub fn nests(&self) -> bool {
        self.name == "source-file"
    }

    /// Whether moa may send this command.
    pub fn check(&self) -> Result<(), Refused> {
        if !ALLOWED.contains(&self.name) {
            return Err(Refused(NOT_ALLOWED));
        }
        match self.name {
            "command-prompt" if !self.args.iter().any(|a| a == "-b") => Err(Refused(NEEDS_B)),
            "source-file" => {
                let mut paths = self.args.iter().filter(|a| !a.starts_with('-')).peekable();
                if paths.peek().is_some() && paths.all(|p| is_own_config(p)) {
                    Ok(())
                } else {
                    Err(Refused(OWN_CONFIG))
                }
            }
            "new-window" => check_new_window(&self.args),
            _ => Ok(()),
        }
    }
}

/// `new-window`'s flags, read up to the command it runs: `-d` (the user's view stays put)
/// and `-P` (it says which pane) are both there, and no `-e` value holds a newline. The
/// command's own arguments are quoted like any other.
fn check_new_window(args: &[String]) -> Result<(), Refused> {
    let (mut detached, mut prints) = (false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-d" => detached = true,
            "-P" => prints = true,
            "-a" | "-b" | "-k" | "-S" => {}
            "-c" | "-F" | "-n" | "-t" => {
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
    if detached && prints {
        Ok(())
    } else {
        Err(Refused(NEEDS_D_P))
    }
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
}
