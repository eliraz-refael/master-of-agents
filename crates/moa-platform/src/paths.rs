//! Where moa keeps things. `MOA_ROOT` moves everything; nothing here creates
//! directories except [`ensure_private_dir`].

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

use crate::build_id::fnv1a64;
use crate::sys;

/// The longest socket path `sockaddr_un` holds on macOS (104 with the NUL).
const SOCKET_LIMIT: usize = 103;

/// Which tmux server: a name under tmux's own socket folder, or a socket path.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TmuxSocket {
    Name(String),
    Path(PathBuf),
}

impl TmuxSocket {
    /// `["-L", name]` or `["-S", path]`.
    pub fn args(&self) -> [OsString; 2] {
        match self {
            TmuxSocket::Name(n) => ["-L".into(), n.into()],
            TmuxSocket::Path(p) => ["-S".into(), p.clone().into_os_string()],
        }
    }

    /// The socket's file. A name lives in `<tmux_tmpdir or /tmp>/tmux-<uid>/<name>`.
    pub fn path(&self, tmux_tmpdir: Option<&Path>, uid: u32) -> PathBuf {
        match self {
            TmuxSocket::Name(n) => tmux_tmpdir
                .unwrap_or(Path::new("/tmp"))
                .join(format!("tmux-{uid}"))
                .join(n),
            TmuxSocket::Path(p) => p.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    home: PathBuf,
    sandboxed: bool,
    real_home_ok: bool,
    tmpdir: PathBuf,
    uid: u32,
}

#[derive(Debug)]
pub enum PathsError {
    NoHome,
    RootNotAbsolute,
    SocketTooLong,
    NotSwitchedOver,
    UnsafeDir(PathBuf, &'static str),
    Io(io::Error),
}

impl fmt::Display for PathsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHome => write!(f, "can't tell your home folder: HOME isn't set"),
            Self::RootNotAbsolute => write!(f, "MOA_ROOT must be a full path"),
            Self::SocketTooLong => write!(
                f,
                "the socket path is too long even under TMPDIR; set a shorter TMPDIR"
            ),
            Self::NotSwitchedOver => write!(
                f,
                "moa v1 isn't switched over yet; set MOA_ROOT to try it (see the try-it)"
            ),
            Self::UnsafeDir(dir, reason) => {
                let d = dir.display();
                write!(f, "{d} {reason}; moa won't use it")?;
                if *reason == "is open to other users" {
                    write!(f, " (chmod 700 {d})")?;
                }
                Ok(())
            }
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PathsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for PathsError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl Paths {
    /// Reads `MOA_ROOT`, `HOME`, `TMPDIR`, `MOA_REAL_HOME` and the uid.
    pub fn from_env() -> Result<Paths, PathsError> {
        let real = std::env::var_os("MOA_REAL_HOME").is_some_and(|v| v == "1");
        Self::from_vars(
            std::env::var_os("MOA_ROOT"),
            std::env::var_os("HOME"),
            std::env::var_os("TMPDIR"),
            sys::current_uid(),
            real,
        )
    }

    pub fn from_vars(
        moa_root: Option<OsString>,
        home: Option<OsString>,
        tmpdir: Option<OsString>,
        uid: u32,
        real_home_ok: bool,
    ) -> Result<Paths, PathsError> {
        let (home, sandboxed) = match moa_root {
            Some(root) => {
                let root = PathBuf::from(root);
                if !root.is_absolute() {
                    return Err(PathsError::RootNotAbsolute);
                }
                (root, true)
            }
            // An empty HOME would put every path under the current folder.
            None => (
                PathBuf::from(home.filter(|h| !h.is_empty()).ok_or(PathsError::NoHome)?),
                false,
            ),
        };
        // An empty or relative TMPDIR would put the socket under the current folder.
        let tmpdir = tmpdir
            .map(PathBuf::from)
            .filter(|t| t.is_absolute())
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        Ok(Paths {
            home,
            sandboxed,
            real_home_ok,
            tmpdir,
            uid,
        })
    }

    /// A sandboxed `Paths` rooted at `root` (for tests and `--sandbox`).
    pub fn with_root(root: &Path) -> Paths {
        Paths {
            home: root.to_path_buf(),
            sandboxed: true,
            real_home_ok: false,
            tmpdir: PathBuf::from("/tmp"),
            uid: sys::current_uid(),
        }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Where the fallback socket lives. A client gives its daemon this as `TMPDIR`, so both
    /// sides compute the same socket path.
    pub fn tmpdir(&self) -> &Path {
        &self.tmpdir
    }

    pub fn is_sandboxed(&self) -> bool {
        self.sandboxed
    }

    /// `Ok` when sandboxed or `MOA_REAL_HOME=1`; v1 may not touch the real home yet.
    pub fn check_v1_allowed(&self) -> Result<(), PathsError> {
        if self.sandboxed || self.real_home_ok {
            Ok(())
        } else {
            Err(PathsError::NotSwitchedOver)
        }
    }

    /// The config file's text, or `None` when it doesn't exist.
    pub fn read_config(&self) -> io::Result<Option<String>> {
        match std::fs::read_to_string(self.config_file()) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn config_dir(&self) -> PathBuf {
        self.home.join(".config/moa")
    }
    pub fn config_file(&self) -> PathBuf {
        self.config_dir().join("moa.toml")
    }
    pub fn data_dir(&self) -> PathBuf {
        self.home.join(".local/share/moa")
    }
    pub fn brain_dir(&self, b: &str) -> PathBuf {
        self.data_dir().join(b)
    }
    pub fn db(&self, b: &str) -> PathBuf {
        self.brain_dir(b).join("moa.db")
    }
    pub fn backups(&self, b: &str) -> PathBuf {
        self.brain_dir(b).join("backups")
    }
    pub fn state_dir(&self) -> PathBuf {
        self.home.join(".local/state/moa")
    }
    pub fn run_dir(&self) -> PathBuf {
        self.state_dir().join("run")
    }
    pub fn lock(&self, b: &str) -> PathBuf {
        self.run_dir().join(format!("{b}.lock"))
    }
    pub fn start_lock(&self, b: &str) -> PathBuf {
        self.run_dir().join(format!("{b}.start"))
    }

    /// `run/<b>.sock`, or, when that is too long for a unix socket,
    /// `<tmpdir>/moa-<uid>/<hash of the intended path>-<b>.sock`.
    pub fn socket(&self, b: &str) -> Result<PathBuf, PathsError> {
        self.short_socket(&format!("{b}.sock"))
    }

    /// `run/<file>` when it fits a unix socket, else
    /// `<tmpdir>/moa-<uid>/<hash of the intended path>-<file>`.
    fn short_socket(&self, file: &str) -> Result<PathBuf, PathsError> {
        let intended = self.run_dir().join(file);
        if intended.as_os_str().len() <= SOCKET_LIMIT {
            return Ok(intended);
        }
        let hash = fnv1a64(intended.as_os_str().as_bytes());
        let fallback = self
            .tmpdir
            .join(format!("moa-{}", self.uid))
            .join(format!("{:08x}-{file}", hash & 0xffff_ffff));
        if fallback.as_os_str().len() <= SOCKET_LIMIT {
            Ok(fallback)
        } else {
            Err(PathsError::SocketTooLong)
        }
    }

    /// Sandboxed brains use a socket path (`run/tmux-<b>`, with `socket`'s length rule and
    /// fallback), so they never reach a real server. Real brains use the names `moa` and
    /// `moa-<b>`.
    pub fn tmux_socket(&self, b: &str) -> Result<TmuxSocket, PathsError> {
        if !self.sandboxed {
            return Ok(TmuxSocket::Name(if b == "default" {
                "moa".to_string()
            } else {
                format!("moa-{b}")
            }));
        }
        self.short_socket(&format!("tmux-{b}"))
            .map(TmuxSocket::Path)
    }

    /// The `-f` file moa gives tmux.
    pub fn tmux_conf(&self, b: &str) -> PathBuf {
        self.brain_dir(b).join("tmux.conf")
    }

    /// moa's own options, keys and menu, which the daemon sources on every connect.
    pub fn moa_tmux(&self, b: &str) -> PathBuf {
        self.brain_dir(b).join("moa.tmux")
    }

    /// The Claude Code plugin the daemon writes, which sessions are started with.
    pub fn session_plugin(&self) -> PathBuf {
        self.data_dir().join("plugin").join("session")
    }

    pub fn spool(&self, b: &str) -> PathBuf {
        self.state_dir().join("spool").join(format!("{b}.jsonl"))
    }
    pub fn log_dir(&self) -> PathBuf {
        self.state_dir().join("logs")
    }
    pub fn log(&self, b: &str) -> PathBuf {
        self.log_dir().join(format!("{b}.log"))
    }
    pub fn stderr_log(&self, b: &str) -> PathBuf {
        self.log_dir().join(format!("{b}.stderr.log"))
    }
    pub fn launch_agents_dir(&self) -> PathBuf {
        self.home.join("Library/LaunchAgents")
    }
    pub fn systemd_user_dir(&self) -> PathBuf {
        self.home.join(".config/systemd/user")
    }
}

/// Where `moa --sandbox` keeps its moa: `<home>/.local/state/moa-sandbox`, used as
/// `MOA_ROOT`.
pub fn sandbox_root(home: &Path) -> PathBuf {
    home.join(".local/state/moa-sandbox")
}

/// Creates `dir` (and missing parents) with mode 0700, then refuses it unless
/// it is a real directory of ours that no one else can reach. Never follows a
/// link: the socket fallback lives in a shared `$TMPDIR`.
pub fn ensure_private_dir(dir: &Path) -> Result<(), PathsError> {
    // A trailing slash makes lstat follow a link, so drop it first.
    let dir: PathBuf = dir.components().collect();
    let dir = dir.as_path();
    let with_path =
        |e: io::Error| PathsError::Io(io::Error::new(e.kind(), format!("{}: {e}", dir.display())));
    // `mkdir` never follows the last component, so a link (even a dangling one) or a file
    // already there is left alone, and the look below refuses it.
    match std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
    {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(with_path(e)),
        _ => {}
    }
    let meta = std::fs::symlink_metadata(dir).map_err(with_path)?;
    if meta.file_type().is_symlink() {
        return Err(PathsError::UnsafeDir(dir.to_path_buf(), "is a symlink"));
    }
    if !meta.is_dir() {
        return Err(PathsError::UnsafeDir(dir.to_path_buf(), "isn't a folder"));
    }
    if meta.uid() != sys::current_uid() {
        return Err(PathsError::UnsafeDir(dir.to_path_buf(), "isn't yours"));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(PathsError::UnsafeDir(
            dir.to_path_buf(),
            "is open to other users",
        ));
    }
    Ok(())
}

/// The first `dir/name` in `path` that is a regular file with an execute bit. Only
/// absolute entries count: an empty or relative one would mean the current folder.
pub fn which(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.mode() & 0o111 != 0))
}
