//! Who wrote to the workspace folder, from the process FUSE names on each
//! request.
//!
//! The kernel tells the filesystem which process asked for every operation.
//! That process is mapped once per (pid, start time) to the writer it acts
//! for, most specific first:
//!
//! 1. a Tilo app, by the cgroup scope its launcher started it in
//!    (`app-tilo-<app id>-<pid>.scope`);
//! 2. a command in a terminal: the person;
//! 3. a desktop app, by the desktop id of its scope (`app-<launcher>-<desktop
//!    id>-<n>.scope`, the convention desktop environments and Flatpak share);
//! 4. otherwise the person at this device.
//!
//! A writer's writes are changes of the tree that name it as their invoker
//! ([`Writer::invoker`]), for the person at this device: a Tilo app or another
//! program. It grants nothing: the device vouches for the name. A program
//! writes with the person's own access, through it.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

/// How many resolved processes are remembered.
const REMEMBERED: usize = 4096;
/// How long a request waits for a file of a process to be read.
const PROC_WAIT: Duration = Duration::from_millis(100);

/// Who a process writes for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Writer {
    /// A Tilo app, by its id.
    App { id: String },
    /// Another program: a desktop app by its desktop id, named as its
    /// desktop entry names it.
    Program { id: String, name: String },
    /// The person at this device, in a terminal or otherwise.
    Person,
}

impl Writer {
    /// Who the file actions it writes are done by, for `person`: a Tilo app or another program
    /// acting for them, or the person themselves. `None` where the mount knows no person.
    #[must_use]
    pub fn invoker(&self, person: Option<&str>) -> Option<tilo_actions::ActionInvoker> {
        // A program writes as the person through it; a Tilo app by its catalogue name; the
        // person themselves by their key, which a change to a shared workspace must carry.
        match self {
            Self::App { id } => Some(tilo_actions::ActionInvoker::admitted(&through(person, id))),
            Self::Program { id, name } => Some(
                tilo_actions::ActionInvoker::admitted(&through(person, id))
                    .named(Some(bounded(name))),
            ),
            Self::Person => person.map(|person| {
                tilo_actions::ActionInvoker::admitted(&tilo_actions::Acting::person(person))
            }),
        }
    }
}

/// The person at this device acting through the program `id`; the program by itself while
/// the mount knows no person, which holds no grants.
fn through(person: Option<&str>, id: &str) -> tilo_actions::Acting {
    match person {
        Some(person) => tilo_actions::Acting::person(person).in_app(bounded(id)),
        None => tilo_actions::Acting::by_app(bounded(id)),
    }
}

/// Most bytes a name or session id of a writer carries.
const MAX_TEXT_BYTES: usize = 128;

/// `text` cut to [`MAX_TEXT_BYTES`], on a character boundary, with no control characters.
fn bounded(text: &str) -> String {
    let mut out = String::new();
    for character in text.chars().filter(|c| !c.is_control()) {
        if out.len().saturating_add(character.len_utf8()) > MAX_TEXT_BYTES {
            break;
        }
        out.push(character);
    }
    out
}

/// Maps the processes FUSE names to writers, remembering each.
#[derive(Debug)]
pub struct WriterResolver {
    proc_root: PathBuf,
    /// Where desktop entries are looked up, most specific first.
    applications: Vec<PathBuf>,
    remembered: Mutex<HashMap<(u32, u64), Writer>>,
    procs: ProcReader,
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
            procs: ProcReader::default(),
        }
    }

    /// Who the thread or process `pid` writes for (FUSE names the calling
    /// thread). A process that is gone is the person.
    pub fn resolve(&self, pid: u32) -> Writer {
        let unread = Cell::new(false);
        let pid = self.process_of(pid, &unread);
        let Some(stat) = self.stat(pid, &unread) else {
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
        let writer = self.find(pid, &stat, &unread);
        if unread.get() {
            // A process that could not be read now is asked again on its next request rather
            // than remembered as what it is not.
            return writer;
        }
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

    fn find(&self, pid: u32, stat: &Stat, unread: &Cell<bool>) -> Writer {
        let cgroup = self.read_proc(pid, "cgroup", unread).unwrap_or_default();
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

    /// The process thread `tid` is of: its thread group's leader.
    fn process_of(&self, tid: u32, unread: &Cell<bool>) -> u32 {
        self.read_proc(tid, "status", unread)
            .and_then(|status| {
                status
                    .lines()
                    .find_map(|line| line.strip_prefix("Tgid:"))
                    .and_then(|tgid| tgid.trim().parse().ok())
            })
            .unwrap_or(tid)
    }

    fn stat(&self, pid: u32, unread: &Cell<bool>) -> Option<Stat> {
        parse_stat(&self.read_proc(pid, "stat", unread)?)
    }

    /// The file `name` of process `pid`, as text: `None` when there is none, or, with
    /// `unread` set, when it could not be read in time ([`ProcReader`]).
    fn read_proc(&self, pid: u32, name: &str, unread: &Cell<bool>) -> Option<String> {
        match self
            .procs
            .read(self.proc_root.join(pid.to_string()).join(name))
        {
            Proc::Read(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
            Proc::Missing => None,
            Proc::Late => {
                unread.set(true);
                None
            }
        }
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

#[derive(Clone, Debug)]
struct Stat {
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
        tty: fields.get(4)?.parse().ok()?,
        start: fields.get(19)?.parse().ok()?,
    })
}

/// Reads the files of processes on a thread of its own, for at most [`PROC_WAIT`].
///
/// Reading what a process is (its `stat`, its cgroup) waits for its memory map. A process
/// faulting in a page of a file mapped from the mount holds that map while the mount serves the
/// fault, and another of its threads may be queued to change it meanwhile: a read of its files
/// then waits for the mount, and a mount that waited for that read would never serve the
/// fault. Git's `index-pack` is such a process. So the mount's request never waits on the read
/// for long, and while one read is still waiting, no other is started.
#[derive(Debug, Default)]
struct ProcReader {
    requests: Mutex<Option<mpsc::SyncSender<ProcRequest>>>,
    waiting: Arc<AtomicBool>,
}

type ProcRequest = (PathBuf, mpsc::SyncSender<Option<Vec<u8>>>);

/// What reading a file of a process found.
enum Proc {
    Read(Vec<u8>),
    /// There is no such file: the process is gone.
    Missing,
    /// It was not read within [`PROC_WAIT`], or an earlier read still waits.
    Late,
}

impl ProcReader {
    fn read(&self, path: PathBuf) -> Proc {
        if self.waiting.swap(true, Ordering::AcqRel) {
            return Proc::Late;
        }
        let (answer, answered) = mpsc::sync_channel(1);
        let sent = self
            .sender()
            .is_some_and(|requests| requests.send((path, answer)).is_ok());
        if !sent {
            self.waiting.store(false, Ordering::Release);
            return Proc::Late;
        }
        match answered.recv_timeout(PROC_WAIT) {
            Ok(Some(bytes)) => Proc::Read(bytes),
            Ok(None) => Proc::Missing,
            Err(_) => Proc::Late,
        }
    }

    fn sender(&self) -> Option<mpsc::SyncSender<ProcRequest>> {
        let mut requests = self
            .requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if requests.is_none() {
            // One read waits at a time, so the queue never holds more than one.
            let (sender, received) = mpsc::sync_channel::<ProcRequest>(1);
            let waiting = Arc::clone(&self.waiting);
            std::thread::Builder::new()
                .name("writer-procs".into())
                .spawn(move || {
                    for (path, answer) in received {
                        let bytes = std::fs::read(&path).ok();
                        waiting.store(false, Ordering::Release);
                        let _ = answer.send(bytes);
                    }
                })
                .ok()?;
            *requests = Some(sender);
        }
        requests.clone()
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
