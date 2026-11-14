//! Who wrote to the workspace folder, from the process FUSE names on each
//! request.
//!
//! The kernel tells the filesystem which process asked for every operation.
//! That process is mapped once per (pid, start time) to the writer it acts
//! for, most specific first:
//!
//! 1. an agent session: Claude Code's own process (its Edit and Write tools
//!    write from it) by the file it keeps per process,
//!    `<claude dir>/sessions/<pid>.json`; else a session the process's
//!    environment names (`PI_SESSION_ID`, `CLAUDE_CODE_SESSION_ID`,
//!    `CODEX_THREAD_ID`), so a command an agent runs is that agent's. A
//!    subagent runs in its parent's process, so it is its parent's session;
//! 2. a Tilo app, by the cgroup scope its launcher started it in
//!    (`app-tilo-<app id>-<pid>.scope`);
//! 3. a command in a terminal: the person;
//! 4. a desktop app, by the desktop id of its scope (`app-<launcher>-<desktop
//!    id>-<n>.scope`, the convention desktop environments and Flatpak share);
//! 5. otherwise the person at this device.
//!
//! The person and the device are the version's signer already; a writer is a
//! label the device asserts beside it ([`FileAttributionV1`]), never a key.
//! Whoever reads the history sees that this device vouches for it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use fs_protocol::FileAttributionV1;

/// How many ancestors are looked through for an agent's process.
const MAX_ANCESTORS: usize = 64;
/// How many resolved processes are remembered.
const REMEMBERED: usize = 4096;

/// Which agent harness a session is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Harness {
    ClaudeCode,
    Codex,
    Pi,
}

impl Harness {
    /// The principal's prefix: `agent:<prefix>:<session id>`.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::Pi => "pi",
        }
    }

    /// What a person calls it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Pi => "pi",
        }
    }

    /// The harness a principal's prefix names.
    #[must_use]
    pub fn from_prefix(prefix: &str) -> Option<Self> {
        [Self::ClaudeCode, Self::Codex, Self::Pi]
            .into_iter()
            .find(|harness| harness.prefix() == prefix)
    }
}

/// Who a process writes for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Writer {
    /// An agent session: one run in History.
    Agent {
        harness: Harness,
        session: String,
        /// The session's title, when it has one. Never the person's prompts.
        title: Option<String>,
    },
    /// A Tilo app, by its id.
    App { id: String },
    /// Another program: a desktop app by its desktop id, named as its
    /// desktop entry names it.
    Program { id: String, name: String },
    /// The person at this device, in a terminal or otherwise.
    Person,
}

impl Writer {
    /// The label this device records beside its version; `None` for the
    /// person, whom the version's signature names already.
    #[must_use]
    pub fn attribution(&self) -> Option<FileAttributionV1> {
        let (actor, label) = match self {
            Self::Agent {
                harness,
                session,
                title,
            } => (
                format!("agent:{}:{session}", harness.prefix()),
                title.clone().unwrap_or_else(|| harness.name().to_owned()),
            ),
            Self::App { id } => (format!("app:{id}"), id.clone()),
            Self::Program { id, name } => (format!("program:{id}"), name.clone()),
            Self::Person => return None,
        };
        let attribution = FileAttributionV1 {
            color: colour_of(&actor),
            actor: bounded(&actor),
            label: bounded(&label),
        };
        attribution.validate().is_ok().then_some(attribution)
    }
}

/// `text` cut to what an attribution holds, on a character boundary, with no
/// control characters.
fn bounded(text: &str) -> String {
    let mut out = String::new();
    for character in text.chars().filter(|c| !c.is_control()) {
        if out.len().saturating_add(character.len_utf8())
            > fs_protocol::files::MAX_ATTRIBUTION_TEXT_BYTES
        {
            break;
        }
        out.push(character);
    }
    out
}

/// A steady colour per principal, so one session reads as one colour.
fn colour_of(actor: &str) -> [u8; 3] {
    let hash = blake3::hash(actor.as_bytes());
    let bytes = hash.as_bytes();
    // Kept off the extremes, so it reads on light and dark.
    let channel = |byte: u8| 64_u8.saturating_add(byte / 2);
    [channel(bytes[0]), channel(bytes[1]), channel(bytes[2])]
}

/// Maps the processes FUSE names to writers, remembering each.
#[derive(Debug)]
pub struct WriterResolver {
    proc_root: PathBuf,
    /// Where desktop entries are looked up, most specific first.
    applications: Vec<PathBuf>,
    remembered: Mutex<HashMap<(u32, u64), Writer>>,
}

impl Default for WriterResolver {
    fn default() -> Self {
        Self::new(PathBuf::from("/proc"), default_application_dirs())
    }
}

impl WriterResolver {
    /// A resolver reading processes under `proc_root` and desktop entries in
    /// `applications`.
    #[must_use]
    pub fn new(proc_root: PathBuf, applications: Vec<PathBuf>) -> Self {
        Self {
            proc_root,
            applications,
            remembered: Mutex::new(HashMap::new()),
        }
    }

    /// Who the thread or process `pid` writes for (FUSE names the calling
    /// thread). A process that is gone is the person.
    pub fn resolve(&self, pid: u32) -> Writer {
        let pid = self.process_of(pid);
        let Some(stat) = self.stat(pid) else {
            return Writer::Person;
        };
        let key = (pid, stat.start);
        if let Some(known) = self
            .remembered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
        {
            return known.clone();
        }
        let writer = self.find(pid, &stat);
        let mut remembered = self
            .remembered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if remembered.len() >= REMEMBERED {
            remembered.clear();
        }
        remembered.insert(key, writer.clone());
        writer
    }

    fn find(&self, pid: u32, stat: &Stat) -> Writer {
        if let Some(agent) = self.agent(pid, stat) {
            return agent;
        }
        let cgroup = std::fs::read_to_string(self.proc_root.join(pid.to_string()).join("cgroup"))
            .unwrap_or_default();
        let scope = scope_of(&cgroup);
        if let Some(Scope::Tilo(id)) = &scope {
            return Writer::App { id: id.clone() };
        }
        if stat.tty != 0 {
            return Writer::Person;
        }
        if let Some(Scope::Desktop(id)) = scope {
            let name = self.desktop_name(&id).unwrap_or_else(|| short_name(&id));
            return Writer::Program { id, name };
        }
        Writer::Person
    }

    /// The agent session `pid` acts in, if any.
    fn agent(&self, pid: u32, stat: &Stat) -> Option<Writer> {
        let own = self.environ(pid);
        // The nearest Claude Code process, the writer itself included.
        let mut at = pid;
        let mut at_stat = stat.clone();
        let mut claude = None;
        for _ in 0..MAX_ANCESTORS {
            if let Some(session) = self.claude_session(at, &at_stat) {
                claude = Some((at, session));
                break;
            }
            if at_stat.parent <= 1 {
                break;
            }
            at = at_stat.parent;
            let Some(next) = self.stat(at) else { break };
            at_stat = next;
        }
        let set = |name: &str| own.get(name).filter(|value| !value.is_empty()).cloned();
        let pi = set("PI_SESSION_ID");
        let codex = set("CODEX_THREAD_ID");
        if let Some((claude_pid, (session, title))) = claude {
            // An agent started from the Claude Code session's commands sets an
            // id of its own that the Claude Code process does not have.
            let theirs = self.environ(claude_pid);
            let started_inside = |name: &str, value: &Option<String>| {
                value
                    .as_ref()
                    .is_some_and(|value| theirs.get(name) != Some(value))
            };
            if started_inside("PI_SESSION_ID", &pi) {
                return pi.map(|session| agent(Harness::Pi, session, None));
            }
            if started_inside("CODEX_THREAD_ID", &codex) {
                return codex.map(|session| agent(Harness::Codex, session, None));
            }
            return Some(agent(Harness::ClaudeCode, session, title));
        }
        pi.map(|session| agent(Harness::Pi, session, None))
            .or_else(|| set("CLAUDE_CODE_SESSION_ID").map(|s| agent(Harness::ClaudeCode, s, None)))
            .or_else(|| codex.map(|session| agent(Harness::Codex, session, None)))
    }

    /// The session Claude Code's process `pid` records it holds, with its
    /// title: `sessions/<pid>.json` in its directory (`$CLAUDE_CONFIG_DIR`,
    /// else `~/.claude`). A file a process of the same pid left before this
    /// one started is not this process's.
    fn claude_session(&self, pid: u32, stat: &Stat) -> Option<(String, Option<String>)> {
        let environ = self.environ(pid);
        let dir = match environ
            .get("CLAUDE_CONFIG_DIR")
            .filter(|dir| !dir.is_empty())
        {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from(environ.get("HOME")?).join(".claude"),
        };
        let file = dir.join("sessions").join(format!("{pid}.json"));
        let bytes = std::fs::read(&file).ok()?;
        let record: ClaudeSessionFile = serde_json::from_slice(&bytes).ok()?;
        if record.pid.is_some_and(|recorded| recorded != pid) {
            return None;
        }
        if let (Some(started), Some(boot)) = (record.started_at, self.boot_ms()) {
            let began = boot.saturating_add(
                stat.start
                    .saturating_mul(1000)
                    .checked_div(ticks_per_second())
                    .unwrap_or(0),
            );
            // Written by an earlier process of this pid.
            if started.saturating_add(5_000) < began {
                return None;
            }
        }
        let session = record.session_id.filter(|id| !id.trim().is_empty())?;
        let title = record.name.filter(|name| !name.trim().is_empty());
        Some((session, title))
    }

    /// The process thread `tid` is of: its thread group's leader.
    fn process_of(&self, tid: u32) -> u32 {
        std::fs::read_to_string(self.proc_root.join(tid.to_string()).join("status"))
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find_map(|line| line.strip_prefix("Tgid:"))
                    .and_then(|tgid| tgid.trim().parse().ok())
            })
            .unwrap_or(tid)
    }

    fn stat(&self, pid: u32) -> Option<Stat> {
        let text =
            std::fs::read_to_string(self.proc_root.join(pid.to_string()).join("stat")).ok()?;
        parse_stat(&text)
    }

    fn environ(&self, pid: u32) -> HashMap<String, String> {
        std::fs::read(self.proc_root.join(pid.to_string()).join("environ"))
            .map(|bytes| parse_environ(&bytes))
            .unwrap_or_default()
    }

    /// When the computer started, in Unix milliseconds.
    fn boot_ms(&self) -> Option<u64> {
        let text = std::fs::read_to_string(self.proc_root.join("stat")).ok()?;
        text.lines()
            .find_map(|line| line.strip_prefix("btime "))
            .and_then(|seconds| seconds.trim().parse::<u64>().ok())
            .map(|seconds| seconds.saturating_mul(1000))
    }

    /// The name a desktop entry gives `id`.
    fn desktop_name(&self, id: &str) -> Option<String> {
        self.applications.iter().find_map(|dir| {
            let text = std::fs::read_to_string(dir.join(format!("{id}.desktop"))).ok()?;
            let mut in_entry = false;
            for line in text.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_entry = line == "[Desktop Entry]";
                } else if in_entry && let Some(name) = line.strip_prefix("Name=") {
                    return Some(name.trim().to_owned()).filter(|name| !name.is_empty());
                }
            }
            None
        })
    }
}

fn agent(harness: Harness, session: String, title: Option<String>) -> Writer {
    Writer::Agent {
        harness,
        session,
        title,
    }
}

/// What Claude Code records per process.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeSessionFile {
    session_id: Option<String>,
    name: Option<String>,
    pid: Option<u32>,
    /// Unix milliseconds.
    started_at: Option<u64>,
}

#[derive(Clone, Debug)]
struct Stat {
    parent: u32,
    tty: i64,
    /// Clock ticks after boot.
    start: u64,
}

/// `/proc/<pid>/stat`: the fields after the command's closing parenthesis
/// (a command may hold spaces and parentheses itself).
fn parse_stat(text: &str) -> Option<Stat> {
    let rest = text.get(text.rfind(')')?.checked_add(2)?..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After the command: state(3) ppid(4) pgrp(5) session(6) tty_nr(7) … starttime(22).
    Some(Stat {
        parent: fields.get(1)?.parse().ok()?,
        tty: fields.get(4)?.parse().ok()?,
        start: fields.get(19)?.parse().ok()?,
    })
}

fn parse_environ(bytes: &[u8]) -> HashMap<String, String> {
    bytes
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            let entry = std::str::from_utf8(entry).ok()?;
            let (name, value) = entry.split_once('=')?;
            Some((name.to_owned(), value.to_owned()))
        })
        .collect()
}

fn ticks_per_second() -> u64 {
    #[cfg(target_os = "linux")]
    {
        rustix::param::clock_ticks_per_second()
    }
    #[cfg(not(target_os = "linux"))]
    {
        100
    }
}

/// The app scope a process's cgroup names.
#[derive(Debug, PartialEq, Eq)]
enum Scope {
    /// A Tilo app launch: `app-tilo-<escaped id>-<pid>.scope`.
    Tilo(String),
    /// A desktop app: its desktop id.
    Desktop(String),
}

/// Launchers that put their own name before the desktop id
/// (<https://systemd.io/DESKTOP_ENVIRONMENTS>).
const LAUNCHERS: &[&str] = &[
    "gnome",
    "flatpak",
    "kde",
    "dbus",
    "xdg\\x2dautostart",
    "niri",
    "sway",
    "Hyprland",
    "cosmic",
];

fn scope_of(cgroup: &str) -> Option<Scope> {
    // The unified hierarchy's line: `0::/user.slice/…/app.slice/<unit>`.
    let path = cgroup.lines().find_map(|line| line.strip_prefix("0::"))?;
    path.rsplit('/').find_map(unit_scope)
}

fn unit_scope(unit: &str) -> Option<Scope> {
    let name = unit
        .strip_suffix(".scope")
        .or_else(|| unit.strip_suffix(".service"))?;
    let name = name.strip_prefix("app-")?;
    if let Some(tilo) = name.strip_prefix("tilo-") {
        let (escaped, _pid) = tilo.rsplit_once('-')?;
        return Some(Scope::Tilo(unescape(escaped)));
    }
    // `@<instance>` of a templated service, then the trailing random part.
    let name = name.split_once('@').map_or(name, |(name, _)| name);
    let mut fields: Vec<&str> = name.split('-').collect();
    if fields.len() > 1
        && fields
            .first()
            .is_some_and(|first| LAUNCHERS.contains(first))
    {
        fields.remove(0);
    }
    if fields.len() > 1
        && fields
            .last()
            .is_some_and(|last| last.chars().all(|c| c.is_ascii_hexdigit()))
    {
        fields.pop();
    }
    let id = unescape(&fields.join("-"));
    (!id.is_empty()).then_some(Scope::Desktop(id))
}

/// A unit name's field with its `\xNN` escapes decoded.
fn unescape(escaped: &str) -> String {
    let bytes = escaped.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        let decoded = bytes
            .get(at..at.saturating_add(4))
            .filter(|run| run.starts_with(b"\\x"))
            .and_then(|run| std::str::from_utf8(run.get(2..)?).ok())
            .and_then(|digits| u8::from_str_radix(digits, 16).ok());
        if let Some(decoded) = decoded {
            out.push(decoded);
            at = at.saturating_add(4);
        } else {
            out.push(byte);
            at = at.saturating_add(1);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `org.gnome.TextEditor` → `TextEditor`, when no desktop entry names it.
fn short_name(id: &str) -> String {
    id.rsplit('.').next().unwrap_or(id).to_owned()
}

/// Where desktop entries live: the user's, then the system's, Flatpak's
/// exports included.
fn default_application_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".local/share")));
    if let Some(data_home) = &data_home {
        dirs.push(data_home.join("applications"));
        dirs.push(data_home.join("flatpak/exports/share/applications"));
    }
    let data_dirs =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
    for dir in data_dirs.split(':').filter(|dir| !dir.is_empty()) {
        dirs.push(Path::new(dir).join("applications"));
    }
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    dirs
}

#[cfg(test)]
#[path = "writer_tests.rs"]
mod tests;
