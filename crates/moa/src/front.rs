//! `moa [BRAIN]` and `moa --sandbox`: the front door. It starts or finds the brain's
//! daemon, creates the terminal server when there is none (T1), and attaches the user.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use moa_client::{Client, ClientOptions, OnMismatch, own_exe};
use moa_core::config::{BrainName, RESERVED_BRAIN_NAMES};
use moa_core::op::Op;
use moa_core::state::CallerInfo;
use moa_daemon::{DaemonError, load_config};
use moa_platform::{
    Paths, PathsError, Spawner, TmuxSocket, build_id, current_uid, ensure_private_dir,
    sandbox_root, which,
};
use moa_tmux::client::version_ok;
use tokio::time::timeout;

use crate::ctl::runtime;

/// How long each answer from the daemon may take (T5).
const ANSWER_WAIT: Duration = Duration::from_secs(10);
/// How long connecting may take in all, a start or a restart included.
const CONNECT_WAIT: Duration = Duration::from_secs(30);
const NOT_SWITCHED_OVER: &str = "moa v1 isn't switched over yet; try moa --sandbox";
const NO_TMUX: &str = "moa needs tmux 3.4 or newer; install it and run moa again";

pub struct FrontArgs {
    pub brain: Option<String>,
    pub sandbox: bool,
}

/// Whether this `moa` runs inside a terminal server, and whose.
#[derive(PartialEq, Eq, Debug)]
pub enum Nesting {
    Outside,
    /// This brain's own server.
    Ours,
    /// Any other server.
    Other,
}

/// How the front door ends: a line to print, or the client to become.
enum Next {
    Say(String),
    Become(Command),
}

pub fn front(args: &FrontArgs) -> ExitCode {
    match open(args) {
        Ok(Next::Say(line)) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Ok(Next::Become(mut cmd)) => {
            // `exec` returns only when it failed.
            let e = cmd.exec();
            eprintln!("moa couldn't open its terminal: {e}");
            ExitCode::from(1)
        }
        Err(line) => {
            eprintln!("{line}");
            ExitCode::from(1)
        }
    }
}

/// The paths `moa` works in: the sandbox's root for `--sandbox`, else the environment's.
fn paths(sandbox: bool) -> Result<Paths, String> {
    if !sandbox {
        return Paths::from_env().map_err(|e| e.to_string());
    }
    // A relative HOME would put the sandbox under the current folder.
    let home = std::env::var_os("HOME")
        .filter(|h| Path::new(h).is_absolute())
        .ok_or_else(|| PathsError::NoHome.to_string())?;
    let root = sandbox_root(Path::new(&home));
    ensure_private_dir(&root).map_err(|e| e.to_string())?;
    Paths::from_vars(
        Some(root.into()),
        Some(home),
        std::env::var_os("TMPDIR"),
        current_uid(),
        false,
    )
    .map_err(|e| e.to_string())
}

/// The brain to open, which must be in `moa.toml`. A command's word (`moa --sandbox
/// status`: after a flag clap takes it as a brain) gets a pointer to the command instead.
fn pick_brain(paths: &Paths, name: Option<&str>) -> Result<BrainName, String> {
    let name = name.unwrap_or("default");
    if RESERVED_BRAIN_NAMES.contains(&name) {
        return Err(format!(
            "moa --sandbox opens a brain; run moa {name} from a moa terminal"
        ));
    }
    let brain = BrainName::parse(name).map_err(|e| {
        let line = e.to_string();
        line.strip_prefix("moa.toml: ").unwrap_or(&line).to_string()
    })?;
    let config = load_config(paths).map_err(|e| e.to_string())?;
    if config.brain(&brain).is_none() {
        return Err(DaemonError::NoSuchBrain(brain, paths.config_file()).to_string());
    }
    Ok(brain)
}

fn open(args: &FrontArgs) -> Result<Next, String> {
    let paths = paths(args.sandbox)?;
    // T21: until the switch-over only a sandbox's server is ever touched.
    if !paths.is_sandboxed() {
        return Err(NOT_SWITCHED_OVER.into());
    }
    let brain = pick_brain(&paths, args.brain.as_deref())?;
    let b = brain.as_str();
    let tmux = which("tmux", std::env::var_os("PATH").as_deref()).ok_or(NO_TMUX)?;
    let socket = paths.tmux_socket(b).map_err(|e| e.to_string())?;
    let tmux_tmpdir = std::env::var_os("TMUX_TMPDIR").filter(|v| !v.is_empty());
    let socket_path = socket.path(tmux_tmpdir.as_deref().map(Path::new), current_uid());
    let inside = nesting(std::env::var_os("TMUX").as_deref(), &socket_path);
    if inside == Nesting::Other {
        return Ok(Next::Say(elsewhere(&brain, args.sandbox)));
    }

    let rt = runtime()?;
    let log = paths.log(b);
    let mut client = rt.block_on(connect(&paths, brain.clone(), &log))?;
    if !rt.block_on(ensure(&mut client, &tmux, &log))? {
        // The daemon checks the running server's version; a server from an old tmux would
        // outlive an upgrade, so none is made.
        check_version(&tmux)?;
        let created = create(&paths, b, &tmux, &socket, &socket_path)?;
        match rt.block_on(ensure(&mut client, &tmux, &log)) {
            Ok(true) => {}
            result => {
                // Don't leave a server the daemon won't use for the next `moa` to find.
                if created {
                    kill_server(&paths, &tmux, &socket);
                }
                result?;
                return Err(format!(
                    "moa couldn't start its terminal server; see its log ({})",
                    log.display()
                ));
            }
        }
    }
    drop(client);
    drop(rt);

    // The `moa` session is made here when it's missing (its last shell exited), never by
    // the daemon: a session takes its first shell's variables and folder from the client
    // that makes it, and only this one has the user's.
    let home = user_home(&paths);
    let mut cmd = Command::new(&tmux);
    cmd.arg("-u").args(socket.args());
    match inside {
        // Inside its own server a second client would nest; the one there moves instead.
        Nesting::Ours => {
            make_moa_session(&paths, &tmux, &socket, &home)?;
            cmd.args(["switch-client", "-t", "=moa"]);
        }
        _ => {
            cmd.args(moa_session_argv(false, &home));
        }
    };
    cmd.env_clear().envs(front_env(std::env::vars_os(), &paths));
    Ok(Next::Become(cmd))
}

/// The user's home folder, where the `moa` session starts: `HOME`, else the paths' home.
fn user_home(paths: &Paths) -> PathBuf {
    std::env::var_os("HOME")
        .filter(|h| Path::new(h).is_absolute())
        .map_or_else(|| paths.home().to_path_buf(), PathBuf::from)
}

/// The `moa` session in `home`: made in the background (`detached`), or attached, and made
/// first when it's missing.
pub fn moa_session_argv(detached: bool, home: &Path) -> Vec<OsString> {
    let how = if detached { "-d" } else { "-A" };
    let mut argv: Vec<OsString> = ["new-session", how, "-s", "moa", "-n", "moa", "-c"]
        .map(OsString::from)
        .into();
    argv.push(home.into());
    argv
}

/// Makes the `moa` session in the background when it's missing, in the user's environment.
/// One that's there already is fine.
fn make_moa_session(
    paths: &Paths,
    tmux: &Path,
    socket: &TmuxSocket,
    home: &Path,
) -> Result<(), String> {
    let mut cmd = Command::new(tmux);
    cmd.args(socket.args())
        .args(moa_session_argv(true, home))
        .env_clear()
        .envs(front_env(std::env::vars_os(), paths))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let failed = |why: &str| format!("moa couldn't open its terminal: {why}");
    let out = Spawner
        .spawn(&mut cmd)
        .and_then(|c| c.wait_with_output())
        .map_err(|e| failed(&e.to_string()))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if out.status.success() || stderr.starts_with("duplicate session") {
        return Ok(());
    }
    Err(failed(stderr.lines().next().unwrap_or("").trim()))
}

/// The line for a `moa` run inside another terminal server: the command that opens this
/// brain from a terminal of its own, `--sandbox` included when it was given.
fn elsewhere(brain: &BrainName, sandbox: bool) -> String {
    let mut open_it = String::from("moa");
    if sandbox {
        open_it.push_str(" --sandbox");
    }
    if *brain != BrainName::default_brain() {
        open_it.push(' ');
        open_it.push_str(brain.as_str());
    }
    format!(
        "the {} moa is running; open it from a new terminal tab: {open_it}",
        brain.as_str()
    )
}

fn no_answer(within: &str, log: &Path) -> String {
    format!(
        "moa didn't answer within {within}; see its log ({})",
        log.display()
    )
}

/// Connects, starting the daemon when none answers. The client bounds a start and a
/// restart itself; this bounds a daemon that takes the connection and never says hello.
async fn connect(paths: &Paths, brain: BrainName, log: &Path) -> Result<Client, String> {
    let exe = own_exe().map_err(|e| format!("can't find this program: {e}"))?;
    let opts = ClientOptions {
        paths: paths.clone(),
        brain,
        client: "moa",
        caller: CallerInfo::default(),
        autostart: Some(exe),
        on_mismatch: OnMismatch::Restart,
        build: build_id().into(),
    };
    timeout(CONNECT_WAIT, Client::connect(opts))
        .await
        .map_err(|_| no_answer("30 s", log))?
        .map_err(|e| e.to_string())
}

/// `tmux -V`, which must say 3.4 or newer, in the daemon's words when it doesn't.
fn check_version(tmux: &Path) -> Result<(), String> {
    let mut cmd = Command::new(tmux);
    cmd.arg("-V")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // A tmux that can't say its version is as good as unknown.
    let out = Spawner
        .spawn(&mut cmd)
        .and_then(|c| c.wait_with_output())
        .ok()
        .filter(|o| o.status.success());
    let text = out.map_or(String::new(), |o| {
        String::from_utf8_lossy(&o.stdout).into_owned()
    });
    let version = text.trim().strip_prefix("tmux ").unwrap_or(text.trim());
    if version_ok(version) {
        return Ok(());
    }
    let version = if version.is_empty() {
        "unknown"
    } else {
        version
    };
    Err(format!(
        "moa needs tmux 3.4 or newer; this one is {version}"
    ))
}

/// Stops the server this `moa` just created. Best effort: the error being reported matters
/// more.
fn kill_server(paths: &Paths, tmux: &Path, socket: &TmuxSocket) {
    let mut cmd = Command::new(tmux);
    cmd.args(socket.args())
        .arg("kill-server")
        .env_clear()
        .envs(front_env(std::env::vars_os(), paths))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(mut child) = Spawner.spawn(&mut cmd) {
        let _ = child.wait();
    }
}

/// `tmux.ensure`: whether the server is there, with the daemon linked to it.
async fn ensure(client: &mut Client, tmux: &Path, log: &Path) -> Result<bool, String> {
    let op = Op::TmuxEnsure {
        tmux: Some(tmux.to_path_buf()),
    };
    let done = timeout(ANSWER_WAIT, client.call(op))
        .await
        .map_err(|_| no_answer("10 s", log))?
        .map_err(|e| e.to_string())?;
    Ok(done.data["running"].as_bool().unwrap_or(false))
}

/// Starts the brain's terminal server with T1's command, in the user's environment. A
/// failure is fine when a server is there afterwards: another `moa` started it meanwhile.
/// Whether this call is the one that started it.
fn create(
    paths: &Paths,
    b: &str,
    tmux: &Path,
    socket: &TmuxSocket,
    socket_path: &Path,
) -> Result<bool, String> {
    if let Some(dir) = socket_path.parent() {
        ensure_private_dir(dir).map_err(|e| e.to_string())?;
    }
    let mut cmd = Command::new(tmux);
    cmd.args(create_argv(socket, &paths.tmux_conf(b), &user_home(paths)))
        .env_clear()
        .envs(front_env(std::env::vars_os(), paths))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let failed = |why: &str| format!("moa couldn't start its terminal server: {why}");
    let child = Spawner
        .spawn(&mut cmd)
        .map_err(|e| failed(&e.to_string()))?;
    // The server it forks puts its stdio on /dev/null, so this returns once the client
    // exits.
    let out = child
        .wait_with_output()
        .map_err(|e| failed(&e.to_string()))?;
    if out.status.success() {
        return Ok(true);
    }
    if UnixStream::connect(socket_path).is_ok() {
        return Ok(false);
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let first = stderr.lines().next().unwrap_or("it stopped at once").trim();
    Err(failed(first))
}

/// The environment a terminal server and its client get (T1): the user's, minus moa's and
/// the agents' variables and `TMUX`/`TMUX_PANE`, plus `MOA_ROOT` when sandboxed. `SHELL`
/// stays: a server without it takes the passwd entry's shell for every pane.
pub fn front_env(
    parent: impl Iterator<Item = (OsString, OsString)>,
    paths: &Paths,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = parent
        .filter(|(k, _)| {
            let k = k.as_bytes();
            !(k.starts_with(b"MOA_")
                || k.starts_with(b"CLAUDE_CODE_")
                || matches!(
                    k,
                    b"CLAUDECODE" | b"CLAUDE_CONFIG_DIR" | b"TMUX" | b"TMUX_PANE"
                ))
        })
        .collect();
    if paths.is_sandboxed() {
        env.push(("MOA_ROOT".into(), paths.home().into()));
    }
    env
}

/// A socket path with its folder made canonical: the socket itself may not exist yet.
fn canonical_socket(p: &Path) -> PathBuf {
    match (p.parent(), p.file_name()) {
        (Some(dir), Some(name)) => std::fs::canonicalize(dir)
            .unwrap_or_else(|_| dir.to_path_buf())
            .join(name),
        _ => p.to_path_buf(),
    }
}

/// `$TMUX`'s socket (before its first `,`) against `ours`.
pub fn nesting(tmux_var: Option<&OsStr>, ours: &Path) -> Nesting {
    let Some(var) = tmux_var.filter(|v| !v.is_empty()) else {
        return Nesting::Outside;
    };
    let bytes = var.as_bytes();
    let theirs = bytes.split(|c| *c == b',').next().unwrap_or(bytes);
    let theirs = Path::new(OsStr::from_bytes(theirs));
    if canonical_socket(theirs) == canonical_socket(ours) {
        Nesting::Ours
    } else {
        Nesting::Other
    }
}

/// T1's command: the server with its config, the `_moa` session the daemon's link
/// attaches to, and the `moa` session the user does, in `home`.
pub fn create_argv(socket: &TmuxSocket, conf: &Path, home: &Path) -> Vec<OsString> {
    let mut argv: Vec<OsString> = socket.args().into();
    argv.push("-f".into());
    argv.push(conf.into());
    argv.extend(
        [
            "new-session",
            "-d",
            "-s",
            "_moa",
            "-x",
            "10",
            "-y",
            "3",
            "/bin/sh",
            ";",
        ]
        .map(OsString::from),
    );
    argv.extend(moa_session_argv(true, home));
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(env: Vec<(OsString, OsString)>) -> Vec<String> {
        let mut v: Vec<String> = env
            .into_iter()
            .map(|(k, v)| format!("{}={}", k.to_string_lossy(), v.to_string_lossy()))
            .collect();
        v.sort();
        v
    }

    fn parent() -> impl Iterator<Item = (OsString, OsString)> {
        [
            ("MOA_SESSION", "7"),
            ("MOA_BRAIN", "work"),
            ("MOA_ROOT", "/elsewhere"),
            ("MOA_REAL_HOME", "1"),
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_ENTRYPOINT", "cli"),
            ("CLAUDE_CONFIG_DIR", "/x"),
            ("TMUX", "/t,1,0"),
            ("TMUX_PANE", "%1"),
            ("PATH", "/bin"),
            ("HOME", "/home/u"),
            ("SHELL", "/bin/zsh"),
            ("TERM", "xterm-ghostty"),
            ("SSH_AUTH_SOCK", "/a.sock"),
            ("TMUX_TMPDIR", "/run/t"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
    }

    #[test]
    fn front_env_drops_agent_and_moa_variables() {
        let kept = [
            "HOME=/home/u",
            "PATH=/bin",
            "SHELL=/bin/zsh",
            "SSH_AUTH_SOCK=/a.sock",
            "TERM=xterm-ghostty",
            "TMUX_TMPDIR=/run/t",
        ];
        let sandboxed = Paths::with_root(Path::new("/r"));
        let mut want: Vec<&str> = kept.to_vec();
        want.push("MOA_ROOT=/r");
        want.sort();
        assert_eq!(sorted(front_env(parent(), &sandboxed)), want);

        let real = Paths::from_vars(None, Some("/home/u".into()), None, 501, true).unwrap();
        assert_eq!(sorted(front_env(parent(), &real)), kept);
    }

    #[test]
    fn nesting_compares_socket_paths() {
        let dir = tempfile::Builder::new()
            .prefix("moa")
            .tempdir_in("/tmp")
            .unwrap();
        // The socket needn't exist yet: only its folder is made canonical.
        let ours = dir.path().join("tmux-default");
        let var = |p: &Path| {
            let mut v = p.as_os_str().to_owned();
            v.push(",123,0");
            v
        };
        assert_eq!(nesting(Some(&var(&ours)), &ours), Nesting::Ours);
        let other = dir.path().join("tmux-work");
        assert_eq!(nesting(Some(&var(&other)), &ours), Nesting::Other);
        assert_eq!(
            nesting(Some(OsStr::new("/tmp/elsewhere/default,1,0")), &ours),
            Nesting::Other
        );
        // A path with a `..` in it is the same socket.
        let around = dir.path().join("x/../tmux-default");
        std::fs::create_dir(dir.path().join("x")).unwrap();
        assert_eq!(nesting(Some(&var(&around)), &ours), Nesting::Ours);
        assert_eq!(nesting(None, &ours), Nesting::Outside);
        assert_eq!(nesting(Some(OsStr::new("")), &ours), Nesting::Outside);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn nesting_sees_through_the_tmp_link() {
        assert_eq!(
            nesting(Some(OsStr::new("/tmp/x,1,0")), Path::new("/private/tmp/x")),
            Nesting::Ours
        );
        assert_eq!(
            nesting(Some(OsStr::new("/private/tmp/x,1,0")), Path::new("/tmp/x")),
            Nesting::Ours
        );
    }

    #[test]
    fn a_command_word_as_the_brain_points_to_the_command() {
        let paths = Paths::with_root(Path::new("/nonexistent"));
        assert_eq!(
            pick_brain(&paths, Some("status")).unwrap_err(),
            "moa --sandbox opens a brain; run moa status from a moa terminal"
        );
    }

    #[test]
    fn the_line_elsewhere_names_the_command_that_opens_this_brain() {
        let work = BrainName::parse("work").unwrap();
        let default = BrainName::default_brain();
        let line = |b: &BrainName, sandbox| {
            elsewhere(b, sandbox)
                .rsplit_once(": ")
                .unwrap()
                .1
                .to_string()
        };
        assert_eq!(line(&default, false), "moa");
        assert_eq!(line(&work, false), "moa work");
        assert_eq!(line(&default, true), "moa --sandbox");
        assert_eq!(line(&work, true), "moa --sandbox work");
    }

    #[test]
    fn the_moa_session_is_attached_or_made_in_home() {
        let text = |detached| {
            moa_session_argv(detached, Path::new("/home/u"))
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(text(false), "new-session -A -s moa -n moa -c /home/u");
        assert_eq!(text(true), "new-session -d -s moa -n moa -c /home/u");
    }

    #[test]
    fn create_argv_is_t1s_command() {
        let argv = create_argv(
            &TmuxSocket::Path("/r/run/tmux-default".into()),
            Path::new("/r/default/tmux.conf"),
            Path::new("/home/u"),
        );
        let argv: Vec<String> = argv
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            argv.join(" "),
            "-S /r/run/tmux-default -f /r/default/tmux.conf \
             new-session -d -s _moa -x 10 -y 3 /bin/sh ; \
             new-session -d -s moa -n moa -c /home/u"
        );
        let named = create_argv(
            &TmuxSocket::Name("moa".into()),
            Path::new("/c"),
            Path::new("/h"),
        );
        assert_eq!(named[..2], [OsString::from("-L"), OsString::from("moa")]);
    }
}
