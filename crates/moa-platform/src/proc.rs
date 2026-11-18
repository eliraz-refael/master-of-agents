//! Process reads, in-process (C22: no `ps`, `pgrep` or `lsof`): a process's argv and a few
//! kept environment entries, its direct children, and when it started. macOS reads them
//! through `sysctl` and libproc, Linux through `/proc`.

use std::time::SystemTime;

use crate::sys;

/// The only environment entries ever kept from a process. The rest can hold secrets.
pub const ENV_KEPT: [&str; 5] = [
    "CLAUDE_CONFIG_DIR",
    "MOA_SESSION",
    "MOA_BRAIN",
    "MOA_TITLE",
    "MOA_GROUP",
];

/// A process's argv and its `ENV_KEPT` entries, as lossy UTF-8.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ProcArgs {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Reads processes' arguments into one buffer, reused from call to call: an argument area
/// can be as big as `KERN_ARGMAX`, so one reader serves a whole inspection.
#[derive(Default)]
pub struct Reader {
    buf: Vec<u8>,
}

/// The buffer holds the last process's whole environment, so it is never printed.
impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reader").finish_non_exhaustive()
    }
}

impl Reader {
    pub fn new() -> Reader {
        Reader::default()
    }

    /// `pid`'s argv and kept environment, or `None` when the process is gone or isn't ours.
    pub fn args(&mut self, pid: u32) -> Option<ProcArgs> {
        sys::read_args(pid, &mut self.buf)
    }
}

/// The pids of `pid`'s direct children; empty when it has none or is gone.
pub fn children(pid: u32) -> Vec<u32> {
    sys::child_pids(pid)
}

/// When `pid` started, or `None` when it is gone.
pub fn started(pid: u32) -> Option<SystemTime> {
    sys::start_time(pid)
}

/// The NUL-terminated strings of `buf`, as lossy UTF-8.
fn strings(buf: &[u8]) -> impl Iterator<Item = String> + '_ {
    buf.split(|&b| b == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
}

/// The `KEY=value` strings whose key is in `ENV_KEPT`; everything else is dropped unread.
fn kept_env(entries: impl Iterator<Item = String>) -> Vec<(String, String)> {
    entries
        .filter_map(|e| {
            let (k, v) = e.split_once('=')?;
            ENV_KEPT
                .contains(&k)
                .then(|| (k.to_string(), v.to_string()))
        })
        .collect()
}

/// macOS's `KERN_PROCARGS2` layout: `argc` (a native-endian `i32`), the exec path, NUL
/// padding, `argc` argv strings, then the environment strings, each NUL-terminated. A
/// process that rewrote its argv (Node's `process.title`) can leave extra NULs before the
/// environment, so they are skipped; the environment ends at the next empty string.
pub fn parse_procargs2(buf: &[u8]) -> Option<ProcArgs> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    let argc = usize::try_from(argc).ok()?;
    let rest = &buf[4..];
    let path_end = rest.iter().position(|&b| b == 0)?;
    let rest = &rest[path_end..];
    let start = rest.iter().position(|&b| b != 0).unwrap_or(rest.len());
    let mut words = strings(&rest[start..]);
    let argv: Vec<String> = words.by_ref().take(argc).collect();
    if argv.len() < argc {
        return None;
    }
    let env = kept_env(
        words
            .skip_while(String::is_empty)
            .take_while(|s| !s.is_empty()),
    );
    Some(ProcArgs { argv, env })
}

/// Linux's `/proc/<pid>/cmdline` and `/proc/<pid>/environ`: NUL-terminated strings. A
/// process that rewrote its argv can pad it with NULs, so trailing empty words are dropped.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_proc_files(cmdline: &[u8], environ: &[u8]) -> ProcArgs {
    let mut argv: Vec<String> = strings(cmdline).collect();
    while argv.last().is_some_and(String::is_empty) {
        argv.pop();
    }
    ProcArgs {
        argv,
        env: kept_env(strings(environ).filter(|s| !s.is_empty())),
    }
}

/// The fields of a `/proc/<pid>/stat` line after `comm`, which is in parentheses and can hold
/// spaces and parentheses itself, so the fields start after the **last** `)`. The first of
/// them is field 3, the state.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn stat_fields(stat: &str) -> Option<std::str::SplitWhitespace<'_>> {
    Some(stat[stat.rfind(')')? + 1..].split_whitespace())
}

/// Field 4 of a `/proc/<pid>/stat` line: the parent's pid.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn stat_ppid(stat: &str) -> Option<u32> {
    stat_fields(stat)?.nth(1)?.parse().ok()
}

/// Field 22 of a `/proc/<pid>/stat` line: the start time, in clock ticks since boot.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn stat_start_ticks(stat: &str) -> Option<u64> {
    stat_fields(stat)?.nth(19)?.parse().ok()
}

/// The `btime` line of `/proc/stat`: the boot time, in seconds since the epoch.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn boot_time(proc_stat: &str) -> Option<u64> {
    proc_stat
        .lines()
        .find_map(|l| l.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()
}

/// The pids in a `/proc/<pid>/task/<tid>/children` file: space-separated numbers.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn pid_list(text: &str) -> impl Iterator<Item = u32> + '_ {
    text.split_whitespace().filter_map(|w| w.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn procargs2(argc: i32, words: &[&str]) -> Vec<u8> {
        let mut buf = argc.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/opt/bin/claude\0\0\0\0");
        for w in words {
            buf.extend_from_slice(w.as_bytes());
            buf.push(0);
        }
        buf
    }

    #[test]
    fn procargs2_layout_parses() {
        let buf = procargs2(
            2,
            &[
                "claude",
                "--resume",
                "SECRET=s",
                "MOA_TITLE=t",
                "CLAUDE_CONFIG_DIR=/s",
                "",
                "executable_path=/opt/bin/claude",
            ],
        );
        let args = parse_procargs2(&buf).unwrap();
        assert_eq!(args.argv, ["claude", "--resume"]);
        assert_eq!(
            args.env,
            [
                ("MOA_TITLE".to_string(), "t".to_string()),
                ("CLAUDE_CONFIG_DIR".to_string(), "/s".to_string())
            ]
        );
    }

    #[test]
    fn a_retitled_argv_still_finds_the_env() {
        // `claude --resume X` rewritten to `claude` and NULs, then the environment.
        let mut buf = procargs2(3, &["claude", "", "", "", "", "", "", "", "", ""]);
        buf.extend_from_slice(b"MOA_SESSION=m\0\0");
        let args = parse_procargs2(&buf).unwrap();
        assert_eq!(args.argv, ["claude", "", ""]);
        assert_eq!(args.env, [("MOA_SESSION".to_string(), "m".to_string())]);
    }

    #[test]
    fn a_short_or_broken_area_is_none() {
        assert_eq!(parse_procargs2(&[]), None);
        assert_eq!(parse_procargs2(&[1, 0]), None);
        assert_eq!(parse_procargs2(&procargs2(-1, &["claude"])), None);
        assert_eq!(parse_procargs2(&procargs2(3, &["claude"])), None);
        // An exec path with no NUL after it.
        assert_eq!(parse_procargs2(&1i32.to_ne_bytes()), None);
    }

    #[test]
    fn proc_files_parse() {
        let args = parse_proc_files(b"claude\0--resume\0X\0\0\0", b"SECRET=s\0MOA_SESSION=m\0\0");
        assert_eq!(args.argv, ["claude", "--resume", "X"]);
        assert_eq!(args.env, [("MOA_SESSION".to_string(), "m".to_string())]);
        assert_eq!(parse_proc_files(b"", b""), ProcArgs::default());
    }

    #[test]
    fn stat_ppid_reads_after_the_last_paren() {
        let stat = "4242 (a) (b) S 17 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 \
                    98765 1000 50 18446744073709551615";
        assert_eq!(stat_ppid(stat), Some(17));
        assert_eq!(stat_start_ticks(stat), Some(98765));
        assert_eq!(stat_ppid("4242 (sleep"), None);
        assert_eq!(stat_start_ticks("4242 (sleep) S 1"), None);
    }

    #[test]
    fn boot_time_and_child_lists_parse() {
        assert_eq!(
            boot_time("cpu 1 2 3\nintr 5\nbtime 1700000000\nprocesses 9\n"),
            Some(1_700_000_000)
        );
        assert_eq!(boot_time("cpu 1\n"), None);
        assert_eq!(pid_list("12 345 \n").collect::<Vec<_>>(), [12, 345]);
        assert_eq!(pid_list("").count(), 0);
    }
}
