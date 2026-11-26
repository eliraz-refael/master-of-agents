//! A panel follows moa's build (§6.15): when the daemon says hello on another build and
//! names its program, the panel gives the terminal back and starts that program in its own
//! place, in the same pane, with the same arguments.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use moa_proto::HelloOk;

/// Set on a panel started in another's place to follow moa: the daemon's build it went to.
pub const FOLLOWED: &str = "MOA_PANEL_FOLLOWED";
/// Set beside [`FOLLOWED`]: the build the panel left, for `panel.log`'s line.
pub const FOLLOWED_FROM: &str = "MOA_PANEL_FOLLOWED_FROM";

/// What a panel does with the daemon's hello.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Follow {
    /// The same build: it goes on.
    Stay,
    /// Another build, which runs this program: the panel starts it in its own place.
    Exec(PathBuf),
    /// Another build it can't follow: the panel says so and sends nothing.
    Mismatch,
}

/// What `hello` asks of a panel on `own_build`. A panel already started to follow
/// `followed` that still isn't the daemon's build follows no further, so two builds never
/// hand the panel back and forth; nor does one whose daemon names no program (from before
/// live updates).
pub fn follow(own_build: &str, hello: &HelloOk, followed: Option<&str>) -> Follow {
    if hello.build == own_build {
        return Follow::Stay;
    }
    if followed == Some(hello.build.as_str()) {
        return Follow::Mismatch;
    }
    match hello.exe.as_deref().map(Path::new) {
        Some(exe) if exe.is_absolute() => Follow::Exec(exe.to_path_buf()),
        _ => Follow::Mismatch,
    }
}

/// How a panel that followed moa was started: from which build, to which.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Followed {
    /// The build it left.
    pub from: String,
    /// The daemon's build it went to follow.
    pub to: String,
}

impl Followed {
    /// What this process was started with, if it was started to follow moa.
    pub fn from_env() -> Option<Followed> {
        let to = std::env::var(FOLLOWED).ok().filter(|v| !v.is_empty())?;
        let from = std::env::var(FOLLOWED_FROM).unwrap_or_else(|_| "-".to_string());
        Some(Followed { from, to })
    }
}

/// Starts `exe` in this process's place with this panel's own arguments, saying what it
/// follows. It returns only why it couldn't.
pub(super) fn start_in_place(exe: &Path, followed: &Followed) -> io::Error {
    Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env(FOLLOWED, &followed.to)
        .env(FOLLOWED_FROM, &followed.from)
        .exec()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use moa_proto::HelloOk;

    use super::{Follow, follow};

    fn hello(build: &str, exe: Option<&str>) -> HelloOk {
        HelloOk {
            brain: "default".into(),
            build: build.into(),
            rev: 0,
            matched: build == "mine",
            pid: 1,
            exe: exe.map(Into::into),
        }
    }

    /// The same build stays, whatever the daemon's program.
    #[test]
    fn a_matched_build_stays() {
        assert_eq!(
            follow("mine", &hello("mine", Some("/tmp/i/moa")), None),
            Follow::Stay
        );
        assert_eq!(
            follow("mine", &hello("mine", None), Some("mine")),
            Follow::Stay
        );
    }

    /// Another build whose daemon names its program: the panel starts that program.
    #[test]
    fn another_build_with_a_program_is_followed() {
        let want = Follow::Exec(PathBuf::from("/tmp/i/moa"));
        assert_eq!(
            follow("mine", &hello("new", Some("/tmp/i/moa")), None),
            want
        );
        // An earlier follow, to an older build, doesn't stop the next one.
        assert_eq!(
            follow("mine", &hello("new", Some("/tmp/i/moa")), Some("old")),
            want
        );
    }

    /// A daemon from before live updates names no program: nothing to follow.
    #[test]
    fn no_program_is_a_mismatch() {
        assert_eq!(follow("mine", &hello("new", None), None), Follow::Mismatch);
    }

    /// A panel started to follow `new` that still isn't `new` (the program changed again in
    /// between) follows no further: no loop.
    #[test]
    fn a_followed_build_that_still_differs_is_a_mismatch() {
        assert_eq!(
            follow("mine", &hello("new", Some("/tmp/i/moa")), Some("new")),
            Follow::Mismatch
        );
    }
}
