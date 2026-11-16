//! `moa service install|uninstall`: register a brain's daemon with launchd (macOS) or
//! `systemd --user` (Linux) so it starts at login. A sandboxed `Paths` writes the file but
//! never registers it.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::paths::{Paths, PathsError};
use crate::spawn::Spawner;
use crate::sys;

#[derive(Debug)]
pub enum ServiceError {
    NixStore,
    NotAbsolute(PathBuf),
    NotAllowed(PathsError),
    Io(io::Error),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NixStore => write!(
                f,
                "that moa is a Nix store path that changes on upgrade; run it from your \
                 profile (~/.nix-profile/bin/moa) or pass --exe"
            ),
            Self::NotAbsolute(p) => write!(f, "--exe must be a full path, not {}", p.display()),
            Self::NotAllowed(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotAllowed(e) => Some(e),
            Self::Io(e) => Some(e),
            Self::NixStore | Self::NotAbsolute(_) => None,
        }
    }
}

impl From<io::Error> for ServiceError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceOutcome {
    pub file: PathBuf,
    pub registered: bool,
    pub message: String,
}

/// The launchd label of a brain's service.
pub fn label(brain: &str) -> String {
    format!("moa.daemon.{brain}")
}

fn unit_name(brain: &str) -> String {
    format!("moa-{brain}.service")
}

/// The moa to register: `given`, else the profile's link (`~/.nix-profile/bin/moa`, not
/// resolved, so an upgrade is picked up), else this program. Never a Nix store path.
pub fn stable_exe(given: Option<&Path>, home: &Path) -> Result<PathBuf, ServiceError> {
    let exe = match given {
        Some(p) if p.is_absolute() => lexical(p),
        Some(p) => return Err(ServiceError::NotAbsolute(p.to_path_buf())),
        None => {
            let profile = home.join(".nix-profile/bin/moa");
            if fs::metadata(&profile).is_ok() {
                profile
            } else {
                std::env::current_exe()?
            }
        }
    };
    if exe.starts_with("/nix/store") {
        return Err(ServiceError::NixStore);
    }
    Ok(exe)
}

/// `p` with `.` and `..` folded away by name alone: a link is never followed, so the
/// profile's stays unresolved.
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn launchd_plist(exe: &Path, brain: &str, paths: &Paths) -> String {
    let s = |v: &str| format!("<string>{}</string>", xml(v));
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ");
    out.push_str("\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
    out.push_str("<plist version=\"1.0\">\n<dict>\n");
    out.push_str(&format!("\t<key>Label</key>\n\t{}\n", s(&label(brain))));
    out.push_str("\t<key>ProgramArguments</key>\n\t<array>\n");
    for a in [&*exe.to_string_lossy(), "daemon", "--brain", brain] {
        out.push_str(&format!("\t\t{}\n", s(a)));
    }
    out.push_str("\t</array>\n");
    // Goes when slice 10 drops the D20 gate: until then the service must pass it too.
    if !paths.is_sandboxed() {
        out.push_str("\t<key>EnvironmentVariables</key>\n\t<dict>\n");
        out.push_str(&format!("\t\t<key>MOA_REAL_HOME</key>\n\t\t{}\n", s("1")));
        out.push_str("\t</dict>\n");
    }
    out.push_str("\t<key>RunAtLoad</key>\n\t<true/>\n");
    out.push_str("\t<key>KeepAlive</key>\n\t<dict>\n");
    out.push_str("\t\t<key>SuccessfulExit</key>\n\t\t<false/>\n\t</dict>\n");
    out.push_str("\t<key>ProcessType</key>\n\t<string>Adaptive</string>\n");
    let err = paths.stderr_log(brain);
    out.push_str(&format!(
        "\t<key>StandardErrorPath</key>\n\t{}\n",
        s(&err.to_string_lossy())
    ));
    out.push_str("</dict>\n</plist>\n");
    out
}

/// A systemd `ExecStart` word in quotes: `"` and `\` escaped, `%` doubled.
fn systemd_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '%' => out.push_str("%%"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn systemd_unit(exe: &Path, brain: &str, paths: &Paths) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "[Unit]\nDescription=moa daemon for the {brain} brain\n\n"
    ));
    out.push_str("[Service]\n");
    out.push_str(&format!(
        "ExecStart={} daemon --brain {brain}\n",
        systemd_quote(&exe.to_string_lossy())
    ));
    // Goes when slice 10 drops the D20 gate: until then the service must pass it too.
    if !paths.is_sandboxed() {
        out.push_str("Environment=MOA_REAL_HOME=1\n");
    }
    out.push_str("Restart=on-failure\n\n[Install]\nWantedBy=default.target\n");
    out
}

/// One command to run, and whether its failure matters.
#[derive(Debug, PartialEq, Eq)]
struct Step {
    args: Vec<String>,
    ignore_failure: bool,
    /// How many times to try it (a failure is only final after the last).
    tries: u32,
}

/// `launchctl bootstrap` right after a `bootout` can fail while the old job tears down.
const BOOTSTRAP_TRIES: u32 = 5;
const RETRY_DELAY: Duration = Duration::from_millis(200);

fn step(args: &[&str], ignore_failure: bool) -> Step {
    Step {
        args: args.iter().map(|a| a.to_string()).collect(),
        ignore_failure,
        tries: 1,
    }
}

fn install_steps(mac: bool, uid: u32, brain: &str, file: &Path) -> Vec<Step> {
    if mac {
        let target = format!("gui/{uid}/{}", label(brain));
        let domain = format!("gui/{uid}");
        let file = file.to_string_lossy();
        vec![
            step(&["launchctl", "bootout", &target], true),
            Step {
                tries: BOOTSTRAP_TRIES,
                ..step(&["launchctl", "bootstrap", &domain, &file], false)
            },
        ]
    } else {
        let unit = unit_name(brain);
        vec![
            step(&["systemctl", "--user", "daemon-reload"], false),
            step(&["systemctl", "--user", "enable", "--now", &unit], false),
        ]
    }
}

/// What to run to register: nothing when the file was already as wanted.
fn install_plan(same: bool, mac: bool, uid: u32, brain: &str, file: &Path) -> Vec<Step> {
    if same {
        Vec::new()
    } else {
        install_steps(mac, uid, brain, file)
    }
}

fn uninstall_steps(mac: bool, uid: u32, brain: &str) -> Vec<Step> {
    if mac {
        let target = format!("gui/{uid}/{}", label(brain));
        vec![step(&["launchctl", "bootout", &target], true)]
    } else {
        let unit = unit_name(brain);
        vec![step(
            &["systemctl", "--user", "disable", "--now", &unit],
            false,
        )]
    }
}

fn run_steps(steps: &[Step]) -> Result<(), ServiceError> {
    for s in steps {
        let name = s.args[..s.args.len().min(3)].join(" ");
        let mut last = None;
        for attempt in 0..s.tries.max(1) {
            if attempt > 0 {
                std::thread::sleep(RETRY_DELAY);
            }
            let mut cmd = Command::new(&s.args[0]);
            cmd.args(&s.args[1..]);
            match Spawner.spawn(&mut cmd).and_then(|mut c| c.wait()) {
                Ok(status) if status.success() => {
                    last = None;
                    break;
                }
                Ok(status) => last = Some(io::Error::other(format!("{name} failed ({status})"))),
                Err(e) => last = Some(io::Error::new(e.kind(), format!("{name}: {e}"))),
            }
        }
        if let Some(e) = last
            && !s.ignore_failure
        {
            return Err(e.into());
        }
    }
    Ok(())
}

fn service_file(paths: &Paths, brain: &str, mac: bool) -> PathBuf {
    if mac {
        paths
            .launch_agents_dir()
            .join(format!("{}.plist", label(brain)))
    } else {
        paths.systemd_user_dir().join(unit_name(brain))
    }
}

/// Writes `text` to `file` through a temp file in the same folder, mode 0644.
fn write_atomic(file: &Path, text: &str) -> io::Result<()> {
    let dir = file.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.tmp{}", std::process::id()));
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.set_permissions(fs::Permissions::from_mode(0o644))?;
        f.sync_all()?;
        fs::rename(&tmp, file)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn manager(mac: bool) -> &'static str {
    if mac { "launchd" } else { "systemd" }
}

pub fn install(paths: &Paths, brain: &str, exe: &Path) -> Result<ServiceOutcome, ServiceError> {
    paths.check_v1_allowed().map_err(ServiceError::NotAllowed)?;
    let mac = cfg!(target_os = "macos");
    let file = service_file(paths, brain, mac);
    let text = if mac {
        launchd_plist(exe, brain, paths)
    } else {
        systemd_unit(exe, brain, paths)
    };
    let old = fs::read_to_string(&file).ok();
    let same = old.as_deref() == Some(text.as_str());
    if !same {
        write_atomic(&file, &text)?;
    }
    if same {
        // Nothing changed, so nothing is restarted or re-registered.
        return Ok(ServiceOutcome {
            message: format!("already installed: {}", file.display()),
            file,
            registered: false,
        });
    }
    if paths.is_sandboxed() {
        let message = format!(
            "sandbox: wrote {}; not registered with {}",
            file.display(),
            manager(mac)
        );
        return Ok(ServiceOutcome {
            file,
            registered: false,
            message,
        });
    }
    if let Err(e) = run_steps(&install_plan(same, mac, sys::current_uid(), brain, &file)) {
        // Put the file back as it was, so running install again registers it rather than
        // finding it unchanged and saying "already installed".
        let _ = match &old {
            Some(old) => write_atomic(&file, old),
            None => fs::remove_file(&file),
        };
        return Err(e);
    }
    let message = format!(
        "installed: {}; registered with {}",
        file.display(),
        manager(mac)
    );
    Ok(ServiceOutcome {
        file,
        registered: true,
        message,
    })
}

pub fn uninstall(paths: &Paths, brain: &str) -> Result<ServiceOutcome, ServiceError> {
    paths.check_v1_allowed().map_err(ServiceError::NotAllowed)?;
    let mac = cfg!(target_os = "macos");
    let file = service_file(paths, brain, mac);
    if !file.exists() {
        return Ok(ServiceOutcome {
            file,
            registered: false,
            message: "not installed".into(),
        });
    }
    let real = !paths.is_sandboxed();
    if real {
        run_steps(&uninstall_steps(mac, sys::current_uid(), brain))?;
    }
    fs::remove_file(&file)?;
    let message = if real {
        format!(
            "removed {}; unregistered from {}",
            file.display(),
            manager(mac)
        )
    } else {
        format!(
            "sandbox: removed {}; nothing to unregister from {}",
            file.display(),
            manager(mac)
        )
    };
    Ok(ServiceOutcome {
        file,
        registered: false,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registration_command_lines() {
        let f = Path::new("/h/Library/LaunchAgents/moa.daemon.work.plist");
        assert_eq!(
            install_steps(true, 501, "work", f),
            vec![
                step(&["launchctl", "bootout", "gui/501/moa.daemon.work"], true),
                Step {
                    tries: BOOTSTRAP_TRIES,
                    ..step(
                        &["launchctl", "bootstrap", "gui/501", &f.to_string_lossy()],
                        false
                    )
                },
            ]
        );
        assert_eq!(
            install_steps(false, 501, "work", f),
            vec![
                step(&["systemctl", "--user", "daemon-reload"], false),
                step(
                    &["systemctl", "--user", "enable", "--now", "moa-work.service"],
                    false
                ),
            ]
        );
        // Unchanged content: nothing to run at all. A changed file retries the bootstrap.
        assert!(install_plan(true, true, 501, "work", f).is_empty());
        assert!(install_plan(true, false, 501, "work", f).is_empty());
        let plan = install_plan(false, true, 501, "work", f);
        assert_eq!(plan, install_steps(true, 501, "work", f));
        assert_eq!(plan[0].tries, 1);
        assert_eq!(plan[1].tries, BOOTSTRAP_TRIES);
        assert_eq!(BOOTSTRAP_TRIES, 5);
        assert_eq!(RETRY_DELAY, Duration::from_millis(200));
        assert_eq!(
            uninstall_steps(true, 501, "work"),
            vec![step(
                &["launchctl", "bootout", "gui/501/moa.daemon.work"],
                true
            )]
        );
        assert_eq!(
            uninstall_steps(false, 501, "work"),
            vec![step(
                &[
                    "systemctl",
                    "--user",
                    "disable",
                    "--now",
                    "moa-work.service"
                ],
                false
            )]
        );
    }
}
