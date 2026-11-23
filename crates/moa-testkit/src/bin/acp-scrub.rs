//! `acp-scrub <in> <out> [--replace FROM=TO…] [--deny REGEX…]`: makes a recording of moa's
//! agent (`acp-tap`'s file) safe to commit. In every string of every message, keys too, it
//! replaces:
//!
//! 1. each `FROM` with its `TO`, as given;
//! 2. every home folder: each `/Users/<name>` and `/home/<name>` in the file, and `$HOME`,
//!    with `/home/user`;
//! 3. every host name: this machine's, and each one the file's paths reveal (`file://<host>/`,
//!    `<host>.local`), with `host`;
//! 4. every user name: each home folder's name, `$USER` and `$LOGNAME`, with `user`.
//!
//! Steps 2 to 4 match in any case, and names only as whole words. Then each line must be
//! clean: no `--deny` pattern may match it (in any case), and it may hold no email address
//! (but one at a reserved example domain). A line that isn't clean is named, with the pattern
//! it matched but never the text, nothing is written, and the scrubber exits 1. `<out>`
//! starts with the header (`moa_testkit::recording::Header`): the agent's name and version
//! from its own `initialize` answer.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::exit;

use moa_testkit::recording::{Dir, Entry, Header, Line};
use regex::{Regex, RegexBuilder};
use serde_json::{Map, Value};

const HOME: &str = "/home/user";
const USER: &str = "user";
const HOST: &str = "host";

const USAGE: &str = "usage: acp-scrub <in> <out> [--replace FROM=TO…] [--deny REGEX…]";

fn usage(why: impl std::fmt::Display) -> ! {
    eprintln!("acp-scrub: {why}\n{USAGE}");
    exit(2)
}

struct Args {
    input: PathBuf,
    out: PathBuf,
    replace: Vec<(String, String)>,
    deny: Vec<(String, Regex)>,
}

/// `--replace` and `--deny` each take every value up to the next option, and may be given
/// more than once.
fn args() -> Args {
    let (mut files, mut replace, mut deny) = (Vec::new(), Vec::new(), Vec::new());
    let mut option = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--replace" | "--deny" => option = Some(arg),
            _ if arg.starts_with("--") => usage(format!("unknown option {arg}")),
            _ => match option.as_deref() {
                None => files.push(PathBuf::from(arg)),
                Some("--replace") => match arg.split_once('=') {
                    Some((from, to)) if !from.is_empty() => replace.push((from.into(), to.into())),
                    _ => usage("--replace takes FROM=TO"),
                },
                Some(_) => {
                    let pattern = RegexBuilder::new(&arg)
                        .case_insensitive(true)
                        .build()
                        .unwrap_or_else(|e| usage(format!("--deny {arg:?}: {e}")));
                    deny.push((arg, pattern));
                }
            },
        }
    }
    let [input, out] =
        <[PathBuf; 2]>::try_from(files).unwrap_or_else(|_| usage("give <in> and <out>"));
    Args {
        input,
        out,
        replace,
        deny,
    }
}

/// `value` with `f` applied to each of its strings, object keys included.
fn map_strings(value: Value, f: &impl Fn(&str) -> String) -> Value {
    match value {
        Value::String(s) => Value::String(f(&s)),
        Value::Array(items) => Value::Array(items.into_iter().map(|v| map_strings(v, f)).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(k, v)| (f(&k), map_strings(v, f)))
                .collect::<Map<_, _>>(),
        ),
        other => other,
    }
}

/// Every string in `value`, object keys included.
fn strings<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(s) => out.push(s),
        Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
        Value::Object(fields) => {
            for (k, v) in fields {
                out.push(k);
                strings(v, out);
            }
        }
        _ => {}
    }
}

/// `text` in any case; as a whole word at each end that is a word character.
fn word(text: &str) -> Regex {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let edge = |yes: bool| if yes { r"\b" } else { "" };
    let pattern = format!(
        "{}{}{}",
        edge(text.starts_with(is_word)),
        regex::escape(text),
        edge(text.ends_with(is_word))
    );
    RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()
        .expect("an escaped pattern")
}

/// What steps 2 to 4 replace, found in `texts` and this machine's environment.
#[derive(Default)]
struct Found {
    homes: BTreeSet<String>,
    hosts: BTreeSet<String>,
    users: BTreeSet<String>,
}

/// Whether `name` could name someone: not the placeholder, `root`, macOS's `/Users/Shared`
/// or a hidden folder.
fn names_someone(name: &str) -> bool {
    !["", USER, "root", "shared"].contains(&name.to_lowercase().as_str()) && !name.starts_with('.')
}

impl Found {
    fn user(&mut self, name: &str) {
        if names_someone(name) {
            self.users.insert(name.to_string());
        }
    }

    fn host(&mut self, name: &str) {
        let name = name.trim_end_matches('.');
        if !["", HOST, "localhost"].contains(&name.to_lowercase().as_str()) {
            self.hosts.insert(name.to_string());
        }
    }

    fn search(texts: &[&str]) -> Found {
        let home = Regex::new(r"/(?:Users|home)/([A-Za-z0-9._-]+)").expect("a pattern");
        let file_host = Regex::new(r#"file://([^/\s"'<>]+)/"#).expect("a pattern");
        let local_host =
            Regex::new(r"([A-Za-z0-9][A-Za-z0-9-]*)\.local(?:[^A-Za-z0-9.]|$)").expect("a pattern");
        let mut found = Found::default();
        for text in texts {
            for m in home.captures_iter(text) {
                if names_someone(&m[1]) {
                    found.homes.insert(m[0].to_string());
                    found.user(&m[1]);
                }
            }
            for m in file_host.captures_iter(text) {
                found.host(&m[1]);
            }
            for m in local_host.captures_iter(text) {
                found.host(&format!("{}.local", &m[1]));
                found.host(&m[1]);
            }
        }
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        if let Some(home) = env("HOME").map(|h| h.trim_end_matches('/').to_string())
            && home.starts_with('/')
            && home != HOME
        {
            if let Some(name) = home.rsplit('/').next() {
                found.user(name);
            }
            found.homes.insert(home);
        }
        for name in ["USER", "LOGNAME"].into_iter().filter_map(env) {
            found.user(&name);
        }
        let uname = rustix::system::uname();
        let machine = uname.nodename().to_string_lossy();
        found.host(&machine);
        if let Some((short, _)) = machine.split_once('.') {
            found.host(short);
        }
        found
    }

    /// Each found text's pattern and what it becomes, in the order they apply: homes, then
    /// hosts (which often hold the user name), then user names; the longest of each first.
    fn rules(&self) -> Vec<(Regex, &'static str)> {
        let longest_first = |set: &BTreeSet<String>| {
            let mut texts: Vec<&String> = set.iter().collect();
            texts.sort_by_key(|t| std::cmp::Reverse(t.len()));
            texts.into_iter().map(|t| word(t)).collect::<Vec<_>>()
        };
        [
            (&self.homes, HOME),
            (&self.hosts, HOST),
            (&self.users, USER),
        ]
        .into_iter()
        .flat_map(|(set, to)| longest_first(set).into_iter().map(move |re| (re, to)))
        .collect()
    }
}

/// The agent's name and version, from its `initialize` answer.
fn header(entries: &[(usize, Entry)]) -> Option<Header> {
    entries.iter().find_map(|(_, e)| {
        let info = &e.msg["result"]["agentInfo"];
        let (name, version) = (info["name"].as_str()?, info["version"].as_str()?);
        (e.dir == Dir::Out).then(|| Header {
            adapter: name.rsplit('/').next().unwrap_or(name).to_string(),
            version: version.to_string(),
        })
    })
}

/// Whether `text` holds an email address, other than one at a reserved example domain.
fn has_email(email: &Regex, text: &str) -> bool {
    email.captures_iter(text).any(|m| {
        let domain = m[1].to_lowercase();
        let example = ["example.com", "example.net", "example.org"]
            .iter()
            .any(|d| domain == *d || domain.ends_with(&format!(".{d}")));
        !(example || domain.ends_with(".example"))
    })
}

fn main() {
    let args = args();
    let text = std::fs::read_to_string(&args.input)
        .unwrap_or_else(|e| usage(format!("{}: {e}", args.input.display())));

    // Read, and replace each FROM. A header already there is written again below.
    let mut entries = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match Line::parse(line) {
            Ok(Line::Header(_)) => {}
            Ok(Line::Entry(Entry { dir, msg })) => {
                let msg = map_strings(msg, &|s| {
                    args.replace
                        .iter()
                        .fold(s.to_string(), |s, (from, to)| s.replace(from, to))
                });
                entries.push((n + 1, Entry { dir, msg }));
            }
            Err(why) => {
                eprintln!("acp-scrub: line {}: {why}", n + 1);
                exit(1)
            }
        }
    }

    // Find the homes, hosts and users, and replace them.
    let mut texts = Vec::new();
    for (_, e) in &entries {
        strings(&e.msg, &mut texts);
    }
    let found = Found::search(&texts);
    let rules = found.rules();
    let entries: Vec<(usize, Entry)> = entries
        .into_iter()
        .map(|(n, Entry { dir, msg })| {
            let msg = map_strings(msg, &|s| {
                rules.iter().fold(s.to_string(), |s, (re, to)| {
                    re.replace_all(&s, *to).into_owned()
                })
            });
            (n, Entry { dir, msg })
        })
        .collect();

    // Check every line, the header's too.
    let mut problems = Vec::new();
    let header = header(&entries);
    if header.is_none() {
        problems.push("no initialize answer gives the agent's name and version".to_string());
    }
    let email = Regex::new(r"(?i)[a-z0-9._%+-]+@([a-z0-9-]+(?:\.[a-z0-9-]+)*\.[a-z]{2,})\b")
        .expect("a pattern");
    let mut lines = Vec::new();
    if let Some(header) = &header {
        let value = serde_json::to_value(header).expect("a header is JSON");
        lines.push(("the header".to_string(), value));
    }
    lines.extend(entries.into_iter().map(|(n, e)| {
        (
            format!("line {n}"),
            serde_json::to_value(e).expect("an entry is JSON"),
        )
    }));
    let mut out = String::new();
    for (name, value) in &lines {
        let line = value.to_string();
        let mut texts = vec![line.as_str()];
        strings(value, &mut texts);
        for (pattern, re) in &args.deny {
            if texts.iter().any(|t| re.is_match(t)) {
                problems.push(format!("{name}: matches --deny {pattern:?}"));
            }
        }
        if texts.iter().any(|t| has_email(&email, t)) {
            problems.push(format!("{name}: holds an email address"));
        }
        out.push_str(&line);
        out.push('\n');
    }
    if !problems.is_empty() {
        for problem in &problems {
            eprintln!("acp-scrub: {problem}");
        }
        eprintln!("acp-scrub: nothing written");
        exit(1)
    }
    std::fs::write(&args.out, out)
        .unwrap_or_else(|e| usage(format!("{}: {e}", args.out.display())));
    eprintln!(
        "acp-scrub: wrote {} ({} messages); replaced {} home folder(s), {} host name(s), {} user name(s)",
        args.out.display(),
        lines.len() - 1,
        found.homes.len(),
        found.hosts.len(),
        found.users.len()
    );
}
