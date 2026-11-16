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
];

/// A command that must not be sent, with the reason.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
#[error("{0}")]
pub struct Refused(pub &'static str);

const NOT_ALLOWED: &str = "that isn't a command moa sends";
const NEEDS_B: &str = "command-prompt needs -b";
const OWN_CONFIG: &str = "moa only sources its own config";

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
            _ => Ok(()),
        }
    }
}

/// An absolute path to a file named exactly `moa.tmux`, with no glob characters.
fn is_own_config(p: &str) -> bool {
    let path = Path::new(p);
    path.is_absolute()
        && !p.contains(['*', '?', '['])
        && path.file_name().is_some_and(|f| f == "moa.tmux")
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
