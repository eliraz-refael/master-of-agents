//! `acp-scrub <in> <out> [--replace FROM=TO…] [--deny REGEX…]`: makes a recording of moa's
//! agent (`acp-tap`'s file) safe to commit. It fails closed: what it can't clean, it refuses.
//!
//! First it redacts who the account is, whatever the flags: each `_auth/status_update` keeps
//! only its `kind`, `label` and `account.plan` (its `account.email`, `account.organization`
//! and `detail` become placeholders), and every identity key anywhere ([`IDENTITY_KEYS`])
//! gets a placeholder of its value's type. Then, in every string of every message, keys too,
//! it replaces:
//!
//! 1. each `FROM` with its `TO`, as given;
//! 2. every home folder: each `/Users/<name>` and `/home/<name>` in the file, and `$HOME`,
//!    with `/home/user`;
//! 3. every host name: this machine's, and each one the file's paths reveal (`file://<host>/`,
//!    `<host>.local`), with `host`;
//! 4. every user name: each home folder's name, `$USER`, `$LOGNAME`, and the user's full name
//!    (`git config user.name`, the OS's), with `user`.
//!
//! Steps 2 to 4 match in any case, names only as whole words, and each also in the spelling
//! Claude gives a path in its project folders (`/Users/a.b` as `-Users-a-b`). Then each line
//! must be clean. It may not match a `--deny` pattern (in any case), still hold a `FROM` (in
//! any case) or anything steps 2 to 4 found, hold an email address (but one at a reserved
//! domain) or an IPv4 address (but `127.0.0.1` and `0.0.0.0`), or name a host outside
//! [`ALLOWED_HOSTS`] (`--replace` one to pass). Nor may scrubbing have changed a message's
//! `method` or the header. A line that isn't clean is named, with what it matched but never
//! the text; nothing is written, and the scrubber exits with 1. `<out>` starts with the header (`moa_testkit::recording::Header`): the
//! agent's name and version from its own `initialize` answer.
//!
//! `MOA_SCRUB_HOST` and `MOA_SCRUB_FULL_NAME`, when set, stand for this machine's host name
//! and the user's full name, so a test doesn't depend on the machine it runs on.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::{Command, exit};

use moa_testkit::recording::{Dir, Entry, Header, Line};
use regex::{Regex, RegexBuilder};
use serde_json::{Map, Value};

const HOME: &str = "/home/user";
const USER: &str = "user";
const HOST: &str = "host";
const EMAIL: &str = "user@example.com";
const ORG: &str = "Org";
const ID: &str = "00000000-0000-0000-0000-000000000000";

/// The adapter's notification of who the account is.
const AUTH_STATUS: &str = "_auth/status_update";

/// Keys that say who the account is, wherever they are, and the text each becomes. From the
/// adapter's auth status, `claude auth status --json`, and Claude's account info. Written as
/// [`key_name`] spells a key, so `org_name`, `Email` and `organizationID` match too.
const IDENTITY_KEYS: &[(&str, &str)] = &[
    ("email", EMAIL),
    ("emailaddress", EMAIL),
    ("useremail", EMAIL),
    ("organization", ORG),
    ("organizationname", ORG),
    ("orgname", ORG),
    ("orgid", ID),
    ("orguuid", ID),
    ("organizationid", ID),
    ("organizationuuid", ID),
    ("accountid", ID),
    ("accountuuid", ID),
    ("userid", ID),
];

/// A key as [`IDENTITY_KEYS`] spells it: lower case, without `_` or `-`.
fn key_name(key: &str) -> String {
    key.chars()
        .filter(|c| !matches!(c, '_' | '-'))
        .flat_map(char::to_lowercase)
        .collect()
}

/// Hosts a line may name, with their subdomains (but the `host` placeholder only whole), and
/// the reserved ones ([`is_reserved`]).
const ALLOWED_HOSTS: &[&str] = &[
    "github.com",
    "anthropic.com",
    "claude.ai",
    "claude.com",
    "agentclientprotocol.com",
    "localhost",
    HOST,
];

/// The last labels that make a bare `a.b.tld` a host. Not file extensions (`.sh`, `.md`,
/// `.app`, `.info`), so file names don't pass for hosts.
const HOST_TLDS: &[&str] = &[
    "com",
    "net",
    "org",
    "edu",
    "gov",
    "mil",
    "io",
    "dev",
    "ai",
    "co",
    "cloud",
    "internal",
    "corp",
    "lan",
    "local",
    "localdomain",
    "intra",
    "intranet",
    "home",
    "site",
    "tech",
    "biz",
    "me",
    "us",
    "uk",
    "de",
    "fr",
    "nl",
    "il",
    "eu",
    "ca",
    "au",
    "jp",
    "cn",
    "ru",
    "ch",
    "se",
    "xyz",
    "ly",
    "gg",
    "tv",
];

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

/// A stand-in for `value` of the same type: `text` for a string, else the type's empty value.
fn placeholder(value: &Value, text: &str) -> Value {
    match value {
        Value::String(_) => Value::String(text.into()),
        Value::Number(_) => Value::from(0),
        Value::Bool(_) => Value::Bool(false),
        Value::Array(_) => Value::Array(Vec::new()),
        Value::Object(_) => Value::Object(Map::new()),
        Value::Null => Value::Null,
    }
}

/// An auth status that says only how the account signs in: `kind`, `label` and
/// `account.plan`, with placeholders for `account.email`, `account.organization` and
/// `detail` (a gateway's host). Everything else, `vendor` included, goes.
fn auth_status(status: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (key, value) in status {
        match key.as_str() {
            "kind" | "label" => {
                out.insert(key.clone(), value.clone());
            }
            "detail" => {
                out.insert(key.clone(), placeholder(value, HOST));
            }
            "account" => {
                let mut account = Map::new();
                for (key, value) in value.as_object().into_iter().flatten() {
                    let value = match key.as_str() {
                        "plan" => value.clone(),
                        "email" => placeholder(value, EMAIL),
                        "organization" => placeholder(value, ORG),
                        _ => continue,
                    };
                    account.insert(key.clone(), value);
                }
                out.insert(key.clone(), Value::Object(account));
            }
            _ => {}
        }
    }
    out
}

/// `msg` with its identity redacted: its auth status, if it is one, and every identity key.
fn redact(mut msg: Value) -> Value {
    if msg["method"] == AUTH_STATUS
        && let Some(status) = msg["params"]["authStatus"].as_object()
    {
        msg["params"]["authStatus"] = Value::Object(auth_status(status));
    }
    redact_keys(msg)
}

fn redact_keys(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(redact_keys).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(k, v)| {
                    let name = key_name(&k);
                    let v = match IDENTITY_KEYS.iter().find(|(key, _)| *key == name) {
                        Some((_, text)) => placeholder(&v, text),
                        None => redact_keys(v),
                    };
                    (k, v)
                })
                .collect::<Map<_, _>>(),
        ),
        other => other,
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

/// `text` as Claude spells a path in its project folders' names: `/Users/a.b` is `-Users-a-b`.
fn claude_spelling(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Whether `name` could name someone: not the placeholder, `root`, macOS's `/Users/Shared`
/// or a hidden folder.
fn names_someone(name: &str) -> bool {
    !["", USER, "root", "shared"].contains(&name.to_lowercase().as_str()) && !name.starts_with('.')
}

/// A command's first line of output, if it ran and said something.
fn output(program: &str, args: &[&str]) -> Option<String> {
    let done = Command::new(program).args(args).output().ok()?;
    let text = String::from_utf8(done.stdout).ok()?;
    let line = text.lines().next()?.trim().to_string();
    (done.status.success() && !line.is_empty()).then_some(line)
}

/// This machine's host name and its user's full names, or the stand-ins a test set.
fn machine() -> (String, Vec<String>) {
    let env = |name: &str| std::env::var(name).ok();
    let host = env("MOA_SCRUB_HOST")
        .unwrap_or_else(|| rustix::system::uname().nodename().to_string_lossy().into());
    if let Some(name) = env("MOA_SCRUB_FULL_NAME") {
        return (host, vec![name]);
    }
    let os_name = output("id", &["-F"]).or_else(|| {
        let uid = output("id", &["-u"])?;
        let entry = output("getent", &["passwd", &uid])?;
        let gecos = entry
            .split(':')
            .nth(4)?
            .split(',')
            .next()?
            .trim()
            .to_string();
        Some(gecos)
    });
    let names = [output("git", &["config", "user.name"]), os_name];
    (host, names.into_iter().flatten().collect())
}

/// What steps 2 to 4 replace, found in `texts`, this machine and its environment.
#[derive(Default)]
struct Found {
    homes: BTreeSet<String>,
    hosts: BTreeSet<String>,
    users: BTreeSet<String>,
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
        let (machine, full_names) = machine();
        found.host(&machine);
        if let Some((short, _)) = machine.split_once('.') {
            found.host(short);
        }
        for name in &full_names {
            found.user(name);
        }
        found
    }

    /// Each kind of found text, with the placeholders it and its Claude spelling become.
    fn kinds(&self) -> [(&BTreeSet<String>, &'static str, &'static str, &'static str); 3] {
        [
            (&self.homes, HOME, "-home-user", "a home folder"),
            (&self.hosts, HOST, HOST, "a host name"),
            (&self.users, USER, USER, "a user name"),
        ]
    }

    /// Each found text's pattern and what it becomes, in the order they apply: homes, then
    /// hosts (which often hold the user name), then user names; the longest of each first.
    fn rules(&self) -> Vec<(Regex, &'static str)> {
        let mut rules = Vec::new();
        for (set, to, spelled_to, _) in self.kinds() {
            let mut texts: Vec<(String, &str)> = set
                .iter()
                .flat_map(|t| [(t.clone(), to), (claude_spelling(t), spelled_to)])
                .collect();
            texts.sort_by_key(|(t, _)| std::cmp::Reverse(t.len()));
            texts.dedup_by(|a, b| a.0 == b.0);
            rules.extend(texts.into_iter().map(|(t, to)| (word(&t), to)));
        }
        rules
    }

    /// Patterns that must not match after scrubbing: each found text, in both spellings, and
    /// what each names.
    fn denied(&self) -> Vec<(Regex, &'static str)> {
        self.kinds()
            .into_iter()
            .flat_map(|(set, _, _, what)| {
                set.iter()
                    .flat_map(|t| [word(t), word(&claude_spelling(t))])
                    .map(move |re| (re, what))
            })
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

/// Whether `domain` is reserved, so naming it gives nothing away: `example.com`, `.org`,
/// `.net` or `.edu` or a subdomain of one, or a name under `.example`, `.test` or `.invalid`.
fn is_reserved(domain: &str) -> bool {
    let domain = domain.trim_end_matches('.').to_lowercase();
    let under = |parent: &str| domain == parent || domain.ends_with(&format!(".{parent}"));
    ["example.com", "example.org", "example.net", "example.edu"]
        .into_iter()
        .any(under)
        || [".example", ".test", ".invalid"]
            .iter()
            .any(|tld| domain.ends_with(tld))
}

/// Whether `text` holds an email address, other than one at a reserved domain.
fn has_email(email: &Regex, text: &str) -> bool {
    email.captures_iter(text).any(|m| !is_reserved(&m[1]))
}

/// Whether a line may name `host`: an allowed one or its subdomain, a reserved domain, or a
/// loopback address.
fn allowed_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_lowercase();
    let host = host.trim_start_matches('[').trim_end_matches(']');
    ALLOWED_HOSTS
        .iter()
        .any(|a| host == *a || (*a != HOST && host.ends_with(&format!(".{a}"))))
        || is_reserved(host)
        || ["127.0.0.1", "::1"].contains(&host)
}

/// Whether `text` holds an IPv4 address other than `127.0.0.1` or `0.0.0.0`.
fn has_ip(ipv4: &Regex, text: &str) -> bool {
    ipv4.find_iter(text).any(|m| {
        let address = m.as_str();
        let octets = address.split('.').all(|o| o.parse::<u8>().is_ok());
        octets && !["127.0.0.1", "0.0.0.0"].contains(&address)
    })
}

/// Finds the hosts a line names: each URL's, and each bare `a.b.tld` with a [`HOST_TLDS`] tld.
struct Hosts {
    url: Regex,
    bare: Regex,
}

impl Hosts {
    fn new() -> Hosts {
        Hosts {
            url: Regex::new(
                r#"(?i)\b[a-z][a-z0-9+.-]*://(?:[^/\s"'<>@]*@)?(\[[^\]]*\]|[^/\s"'<>:?#\\]*)"#,
            )
            .expect("a pattern"),
            bare: Regex::new(r"(?i)\b(?:[a-z0-9-]+\.)+([a-z]{2,})\b").expect("a pattern"),
        }
    }

    /// Whether `text` names a host that isn't allowed.
    fn any_unknown(&self, text: &str) -> bool {
        let urls = self
            .url
            .captures_iter(text)
            .map(|m| m[1].to_string())
            .filter(|h| !h.is_empty());
        let bare = self
            .bare
            .captures_iter(text)
            .filter(|m| HOST_TLDS.contains(&m[1].to_lowercase().as_str()))
            .map(|m| m[0].to_string());
        urls.chain(bare).any(|h| !allowed_host(&h))
    }
}

fn main() {
    let args = args();
    let text = std::fs::read_to_string(&args.input)
        .unwrap_or_else(|e| usage(format!("{}: {e}", args.input.display())));

    // Read. A header already there is written again below.
    let mut entries = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match Line::parse(line) {
            Ok(Line::Header(_)) => {}
            Ok(Line::Entry(entry)) => entries.push((n + 1, entry)),
            Err(why) => {
                eprintln!("acp-scrub: line {}: {why}", n + 1);
                exit(1)
            }
        }
    }
    // What scrubbing must leave alone.
    let original_header = header(&entries);
    let methods: Vec<Value> = entries
        .iter()
        .map(|(_, e)| e.msg["method"].clone())
        .collect();

    // Redact the identity, and replace each FROM.
    let entries: Vec<(usize, Entry)> = entries
        .into_iter()
        .map(|(n, Entry { dir, msg })| {
            let msg = map_strings(redact(msg), &|s| {
                args.replace
                    .iter()
                    .fold(s.to_string(), |s, (from, to)| s.replace(from, to))
            });
            (n, Entry { dir, msg })
        })
        .collect();

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
    match (&original_header, &header) {
        (None, _) => {
            problems.push("no initialize answer gives the agent's name and version".to_string())
        }
        (Some(before), Some(after)) if before == after => {}
        _ => problems.push("the header's name or version changed while scrubbing".to_string()),
    }
    for ((n, e), method) in entries.iter().zip(&methods) {
        if e.msg["method"] != *method {
            problems.push(format!("line {n}: its method changed while scrubbing"));
        }
    }
    let email = Regex::new(r"(?i)[a-z0-9._%+-]+@([a-z0-9-]+(?:\.[a-z0-9-]+)*\.[a-z]{2,})\b")
        .expect("a pattern");
    let hosts = Hosts::new();
    let ipv4 = Regex::new(r"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b").expect("a pattern");
    let mut denied = found.denied();
    denied.extend(args.replace.iter().map(|(from, _)| {
        let re = RegexBuilder::new(&regex::escape(from))
            .case_insensitive(true)
            .build()
            .expect("an escaped pattern");
        (re, "a --replace FROM")
    }));
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
        let matches = |re: &Regex| texts.iter().any(|t| re.is_match(t));
        for (pattern, re) in &args.deny {
            if matches(re) {
                problems.push(format!("{name}: matches --deny {pattern:?}"));
            }
        }
        let still: BTreeSet<&str> = denied
            .iter()
            .filter(|(re, _)| matches(re))
            .map(|(_, what)| *what)
            .collect();
        for what in still {
            problems.push(format!("{name}: still holds {what}, in another spelling"));
        }
        if texts.iter().any(|t| has_email(&email, t)) {
            problems.push(format!("{name}: holds an email address"));
        }
        if texts.iter().any(|t| has_ip(&ipv4, t)) {
            problems.push(format!("{name}: holds an IP address"));
        }
        if texts.iter().any(|t| hosts.any_unknown(t)) {
            problems.push(format!(
                "{name}: names a host outside the allowed ones (--replace it)"
            ));
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
