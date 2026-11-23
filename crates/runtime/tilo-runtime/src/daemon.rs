//! Persistent native session shared by independently launched GUI apps and MCP.
use crate::device::services::Daemon;
use crate::{BLOB_SOCKET, COLLAB_SOCKET, IDENTITY_SOCKET, RuntimeServices, host};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tilo_lock::MutexRecover as _;
use tilo_mcp::McpServer;
use tilo_mcp::lines::{Answer, Frame, mcp_answer, read_frame, serve_lines, spawn_reader};
use tilo_mcp::session_api::{Asking, bridge_stdio};

use crate::daemon_args::invalid;
pub use crate::daemon_args::{Mode, default_profile, parse_args, validate_profile};

const PROBE_TIMEOUT: Duration = Duration::from_millis(300);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Status {
    version: u32,
    pid: u32,
    /// The workspace the daemon serves. The probe answers it as `workspace`, the
    /// name launchers and scripts outside this crate read; `workspace` is
    /// read as the same.
    #[serde(rename = "workspace", alias = "workspace")]
    workspace: String,
}

fn probe(profile: &Path) -> io::Result<Status> {
    let mut stream = tilo_files::native::socket::connect(&profile.join("native.sock"))?;
    stream.set_read_timeout(Some(PROBE_TIMEOUT))?;
    stream.set_write_timeout(Some(PROBE_TIMEOUT))?;
    stream.write_all(b"{\"native\":\"status\"}\n")?;
    let mut bytes = Vec::new();
    BufReader::new(stream)
        .take(16_385)
        .read_until(b'\n', &mut bytes)?;
    if bytes.len() > 16_384 || bytes.last() != Some(&b'\n') {
        return Err(invalid("invalid native runtime status frame"));
    }
    let status: Status = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if status.version != 1
        || status.pid == 0
        || status.workspace.is_empty()
        || status.workspace.len() > 4096
    {
        return Err(invalid("invalid native runtime status"));
    }
    Ok(status)
}

/// How long a starting daemon may take. Startup is not bounded by a fixed
/// deadline: a loaded machine can take far longer than usual to start a
/// session that is still working. It keeps waiting while the process is
/// alive and making progress, and gives up once progress stops for `stall`
/// or the whole start exceeds `cap`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StartupLimits {
    pub(crate) stall: Duration,
    pub(crate) cap: Duration,
    pub(crate) poll: Duration,
}

pub(crate) const STARTUP: StartupLimits = StartupLimits {
    stall: Duration::from_secs(15),
    cap: Duration::from_secs(120),
    poll: Duration::from_millis(25),
};

/// What a starting process looks like from outside at one instant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Observation {
    /// What the process is doing, for errors: "while <stage>".
    pub(crate) stage: String,
    /// Every sign of progress by name, in the same order at each instant:
    /// its current value, or `None` while it cannot be read. Any value that
    /// changes counts as progress.
    pub(crate) signals: Vec<(&'static str, Option<u64>)>,
}

/// A spawned process that has not reported ready yet.
pub(crate) trait StartingProcess {
    type Ready;
    /// The ready answer, once the process gives one.
    fn ready(&mut self) -> Option<Self::Ready>;
    /// How the process ended, if it already has.
    fn exited(&mut self) -> io::Result<Option<String>>;
    fn observe(&mut self) -> Observation;
    /// End a process that will not become ready.
    fn stop(&mut self);
    /// Where to look next, with the process's most recent output and, while
    /// it still runs, what it is blocked on.
    fn diagnostics(&self) -> String;
    /// The time the wait measures stalls and its cap against.
    fn now(&self) -> Instant {
        Instant::now()
    }
    /// Wait `poll` before looking at the process again.
    fn pause(&mut self, poll: Duration) {
        std::thread::sleep(poll);
    }
}

/// When each progress signal last changed, for the error that names which
/// of them went quiet.
fn signal_report(observation: &Observation, changed: &[Option<Instant>], now: Instant) -> String {
    observation
        .signals
        .iter()
        .zip(changed)
        .map(|((name, value), changed)| match (value, changed) {
            (_, Some(at)) => format!(
                "{name} last changed {:.1} s ago",
                now.saturating_duration_since(*at).as_secs_f32()
            ),
            (None, None) => format!("{name} unreadable"),
            (Some(_), None) => format!("{name} unchanged since launch"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Wait until `process` is ready, failing as soon as it exits, stalls or
/// exceeds the absolute cap.
pub(crate) fn wait_until_ready<P: StartingProcess>(
    process: &mut P,
    limits: StartupLimits,
) -> io::Result<P::Ready> {
    let started = process.now();
    let mut last = process.observe();
    let mut changed = vec![None; last.signals.len()];
    let mut last_progress = started;
    loop {
        if let Some(ready) = process.ready() {
            return Ok(ready);
        }
        let observed = process.observe();
        let now = process.now();
        for (index, (signal, value)) in observed.signals.iter().enumerate() {
            if last.signals.get(index) != Some(&(*signal, *value)) {
                last_progress = now;
                if let Some(slot) = changed.get_mut(index) {
                    *slot = Some(now);
                }
            }
        }
        last = observed;
        if let Some(status) = process.exited()? {
            return Err(io::Error::other(format!(
                "native runtime exited while {} ({status}); {}",
                last.stage,
                process.diagnostics()
            )));
        }
        let failure = if now.saturating_duration_since(last_progress) >= limits.stall {
            format!(
                "native runtime stalled while {}: no progress for {} s ({})",
                last.stage,
                limits.stall.as_secs_f32(),
                signal_report(&last, &changed, now)
            )
        } else if now.saturating_duration_since(started) >= limits.cap {
            format!(
                "native runtime did not become ready within {} s (still {}; {})",
                limits.cap.as_secs_f32(),
                last.stage,
                signal_report(&last, &changed, now)
            )
        } else {
            process.pause(limits.poll);
            continue;
        };
        // What it is blocked on is only visible while it still runs.
        let diagnostics = process.diagnostics();
        process.stop();
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("{failure}; {diagnostics}"),
        ));
    }
}

/// The file a starting daemon names each startup step in, one line per step,
/// before taking it.
const STAGES_FILE: &str = "startup.stages";
/// What a daemon does before it names its first step: the executable loads
/// and prepares its profile environment.
const FIRST_STAGE: &str = "loading the runtime";
const OUTPUT_TAIL: u64 = 4096;
/// The longest step name a launcher repeats from the stages file.
const STAGE_LIMIT: usize = 200;
/// Processes, and threads per process, a launcher inspects in a daemon's tree.
#[cfg(target_os = "linux")]
const TREE_LIMIT: usize = 256;
/// Processes a launcher reads when the kernel lists no children per task.
#[cfg(target_os = "linux")]
const PROCESS_SCAN_LIMIT: usize = 8192;

struct StartupStages(std::fs::File);

impl StartupStages {
    fn open(profile: &Path) -> io::Result<Self> {
        let file = host::private_file(&profile.join(STAGES_FILE))?;
        // A daemon started by hand still reports only its own steps.
        file.set_len(0)?;
        Ok(Self(file))
    }
    /// Report is best effort: a launcher that cannot read it still sees the
    /// daemon's other signs of progress.
    fn enter(&mut self, stage: &str) {
        let _ = self.0.write_all(format!("{stage}\n").as_bytes());
    }
}

/// The most recent step named in a stages file's contents.
fn current_stage(stages: &[u8]) -> String {
    let stages = String::from_utf8_lossy(stages);
    stages
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map_or_else(
            || FIRST_STAGE.to_owned(),
            |line| line.chars().take(STAGE_LIMIT).collect(),
        )
}

/// One `/proc/<pid>/stat` or `/proc/<pid>/task/<tid>/stat`: the scheduler
/// state letter and the user plus system CPU time in clock ticks. A process
/// stat counts every thread it has ever run; a task stat only that thread.
#[cfg(target_os = "linux")]
fn stat(path: &Path) -> Option<(char, u64)> {
    let stat = std::fs::read_to_string(path).ok()?;
    // The command name may contain spaces; the fields after it are fixed:
    // state is the first, user and system time the 12th and 13th.
    let mut fields = stat.get(stat.rfind(')')?.checked_add(2)?..)?.split(' ');
    let state = fields.next()?.chars().next()?;
    let user = fields.nth(10)?.parse::<u64>().ok()?;
    let system = fields.next()?.parse::<u64>().ok()?;
    Some((state, user.checked_add(system)?))
}

/// Total CPU time the process has used, in clock ticks.
#[cfg(target_os = "linux")]
fn cpu_ticks(pid: u32) -> Option<u64> {
    stat(Path::new(&format!("/proc/{pid}/stat"))).map(|(_, ticks)| ticks)
}

/// Bytes the process has moved through read and write calls and to or from
/// storage, over all its threads.
#[cfg(target_os = "linux")]
fn io_bytes(pid: u32) -> Option<u64> {
    let io = std::fs::read_to_string(format!("/proc/{pid}/io")).ok()?;
    io.lines()
        .filter_map(|line| line.split_once(": "))
        .filter(|(key, _)| matches!(*key, "rchar" | "wchar" | "read_bytes" | "write_bytes"))
        .try_fold(0_u64, |total, (_, value)| {
            total.checked_add(value.trim().parse().ok()?)
        })
}

/// The daemon and every process it started, at one instant.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TreeSample {
    /// CPU time of the whole tree, in clock ticks.
    cpu_ticks: u64,
    /// Read, write and storage bytes of the tree, when any are readable.
    io_bytes: Option<u64>,
    /// Some thread is running, waiting for a CPU, or waiting on the disk:
    /// the tree is held up by the machine rather than stuck.
    held_by_machine: bool,
    /// Each thread as "name state kernel-wait", when asked for.
    threads: Vec<String>,
}

/// Thread ids of `pid`, bounded.
#[cfg(target_os = "linux")]
fn tasks(pid: u32) -> Vec<String> {
    std::fs::read_dir(format!("/proc/{pid}/task")).map_or_else(
        |_| Vec::new(),
        |entries| {
            entries
                .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                .take(TREE_LIMIT)
                .collect()
        },
    )
}

/// Sample `root` and its descendants, or `None` once `root` is gone.
/// `describe` also names each thread and what it waits on in the kernel.
#[cfg(target_os = "linux")]
fn sample_tree(root: u32, describe: bool) -> Option<TreeSample> {
    let mut sample = TreeSample::default();
    let mut pending = vec![root];
    let mut parents = None;
    for _ in 0..TREE_LIMIT {
        let Some(pid) = pending.pop() else { break };
        let Some(ticks) = cpu_ticks(pid) else {
            if pid == root {
                return None;
            }
            continue;
        };
        sample.cpu_ticks = sample.cpu_ticks.saturating_add(ticks);
        if let Some(bytes) = io_bytes(pid) {
            let total = sample.io_bytes.get_or_insert(0);
            *total = total.saturating_add(bytes);
        }
        for tid in tasks(pid) {
            let task = PathBuf::from(format!("/proc/{pid}/task/{tid}"));
            if let Some((state, _)) = stat(&task.join("stat")) {
                sample.held_by_machine |= matches!(state, 'R' | 'D');
            }
            if describe && let Some((state, _)) = stat(&task.join("stat")) {
                let read = |name: &str| {
                    std::fs::read_to_string(task.join(name))
                        .map(|text| text.trim().to_owned())
                        .unwrap_or_default()
                };
                let wchan = read("wchan");
                let wchan = if wchan.is_empty() || wchan == "0" {
                    String::new()
                } else {
                    format!(" {wchan}")
                };
                sample
                    .threads
                    .push(format!("{} {state}{wchan}", read("comm")));
            }
            match std::fs::read_to_string(task.join("children")) {
                Ok(children) => pending.extend(
                    children
                        .split_whitespace()
                        .filter_map(|child| child.parse::<u32>().ok()),
                ),
                // Kernels without per-task child lists: a process's children
                // are the processes naming it as their parent.
                Err(_) if tid == pid.to_string() => pending.extend(
                    parents
                        .get_or_insert_with(parent_pids)
                        .iter()
                        .filter(|(_, parent)| *parent == pid)
                        .map(|(child, _)| *child),
                ),
                Err(_) => {}
            }
        }
    }
    Some(sample)
}

/// Every visible process with its parent, bounded.
#[cfg(target_os = "linux")]
fn parent_pids() -> Vec<(u32, u32)> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .take(PROCESS_SCAN_LIMIT)
        .filter_map(|pid| {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let parent = stat
                .get(stat.rfind(')')?.checked_add(2)?..)?
                .split(' ')
                .nth(1)?
                .parse()
                .ok()?;
            Some((pid, parent))
        })
        .collect()
}
#[cfg(not(target_os = "linux"))]
fn sample_tree(_root: u32, _describe: bool) -> Option<TreeSample> {
    None
}

/// The daemon child of this launcher. Progress is anything it visibly does:
/// naming its next startup step, writing output, using CPU time, reading or
/// writing, or waiting for a CPU or the disk, anywhere in its process tree.
struct DaemonStart {
    log: PathBuf,
    stages: PathBuf,
    child: Child,
    profile: PathBuf,
    /// Samples in which the machine, not the daemon, held it up.
    held_samples: u64,
}

impl DaemonStart {
    fn new(profile: &Path, child: Child) -> Self {
        Self {
            log: profile.join("daemon.log"),
            stages: profile.join(STAGES_FILE),
            child,
            profile: profile.to_path_buf(),
            held_samples: 0,
        }
    }
}

impl StartingProcess for DaemonStart {
    type Ready = Status;
    fn ready(&mut self) -> Option<Status> {
        probe(&self.profile).ok()
    }
    fn exited(&mut self) -> io::Result<Option<String>> {
        Ok(self.child.try_wait()?.map(|status| status.to_string()))
    }
    fn observe(&mut self) -> Observation {
        let stages = std::fs::read(&self.stages).ok();
        let tree = sample_tree(self.child.id(), false);
        if tree.as_ref().is_some_and(|tree| tree.held_by_machine) {
            self.held_samples = self.held_samples.saturating_add(1);
        }
        let length = |bytes: usize| u64::try_from(bytes).unwrap_or(u64::MAX);
        Observation {
            stage: current_stage(stages.as_deref().unwrap_or_default()),
            signals: vec![
                (
                    "startup step",
                    stages.as_ref().map(|stages| length(stages.len())),
                ),
                (
                    "output",
                    std::fs::metadata(&self.log)
                        .map(|metadata| metadata.len())
                        .ok(),
                ),
                ("CPU time", tree.as_ref().map(|tree| tree.cpu_ticks)),
                ("I/O", tree.as_ref().and_then(|tree| tree.io_bytes)),
                (
                    "waiting for CPU or disk",
                    tree.as_ref().map(|_| self.held_samples),
                ),
            ],
        }
    }
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
    fn diagnostics(&self) -> String {
        let mut tail = Vec::new();
        if let Ok(mut file) = std::fs::File::open(&self.log) {
            use std::io::{Seek, SeekFrom};
            let length = file.metadata().map_or(0, |metadata| metadata.len());
            if file
                .seek(SeekFrom::Start(length.saturating_sub(OUTPUT_TAIL)))
                .is_ok()
            {
                let _ = file.take(OUTPUT_TAIL).read_to_end(&mut tail);
            }
        }
        let tail = String::from_utf8_lossy(&tail);
        let tail = tail.trim();
        let output = if tail.is_empty() {
            format!("it wrote nothing to {}", self.log.display())
        } else {
            format!("last output from {}:\n{tail}", self.log.display())
        };
        match sample_tree(self.child.id(), true) {
            Some(tree) if !tree.threads.is_empty() => {
                format!("threads: {}; {output}", tree.threads.join(", "))
            }
            _ => output,
        }
    }
}

fn ensure_running(profile: &Path) -> io::Result<Status> {
    validate_profile(profile)?;
    if let Ok(status) = probe(profile) {
        return Ok(status);
    }
    // A holder is another launcher starting the daemon, which gives up
    // within the startup cap, so waiting for it is bounded by that cap.
    let _start =
        advisory_lock::FileLock::exclusive(host::private_file(&profile.join("start.lock"))?)?;
    if let Ok(status) = probe(profile) {
        return Ok(status);
    }
    let log = host::private_file(&profile.join("daemon.log"))?;
    // Each start's log and steps are only its own daemon's.
    log.set_len(0)?;
    host::private_file(&profile.join(STAGES_FILE))?.set_len(0)?;
    let mut command = Command::new(std::env::current_exe()?);
    // The person's own daemon runs in their environment, as the user service runs it; one of a
    // profile named apart keeps its environment inside that profile.
    if std::env::var_os("TILO_RUNTIME_PROFILE").is_some() || default_profile()? != profile {
        command.arg("--profile").arg(profile);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    // SAFETY: setsid is async-signal-safe and runs in the new child only. The
    // daemon must outlive the launcher/MCP terminal without holding its pipes.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    wait_until_ready(&mut DaemonStart::new(profile, command.spawn()?), STARTUP)
}

/// Start this computer's daemon over `profile` and serve it: the native endpoint the apps and
/// `--connect` reach, and the device as [`crate::device::hold`] serves it. The executable
/// prepares its profile environment before entering this function; reusable serving code never
/// mutates it.
pub fn serve(profile: &Path, dev: bool) -> io::Result<()> {
    validate_profile(profile)?;
    let mut stages = StartupStages::open(profile)?;
    stages.enter("locking the device");
    let device = crate::device::unclaimed::ThisDevice::lock()?;
    // The native endpoint answers from boot, before anyone's certificate names this device and
    // while its person's services start: the device's own actions first, the person's once they
    // run. This process has the device's lock, so only it removes a stale endpoint.
    let path = profile.join("native.sock");
    stages.enter("binding the native endpoint");
    let listener = tilo_files::native::private::bind_socket(&path)?;
    let native = Arc::new(Native::new(device.answers()));
    {
        let native = Arc::clone(&native);
        std::thread::Builder::new()
            .name("tilo-native".into())
            .spawn(move || {
                if let Err(error) = native.accept(&listener) {
                    tracing::error!(%error, "the native endpoint stopped taking connections");
                }
            })?;
    }
    tracing::info!("answering this device's actions on its native endpoint");
    // Before a person's certificate names this computer's key, a computer of one account's
    // founds its person; Tilo OS's device, and one handed a ticket, are served with the key alone,
    // the ticket spent on the way, and the person's services start once a certificate names it.
    if !device.claimed() {
        if crate::device::services::founds_a_person() {
            stages.enter("founding this computer's person");
            crate::device::services::found_a_person()?;
        } else {
            stages.enter("serving this device before anyone's certificate names it");
            device.spend_handed_ticket();
            device.until_claimed();
        }
    }
    let locked = device.into_lock();
    let mut stage = |stage: &str| stages.enter(stage);
    let session = Arc::new(Daemon::start_locked(profile, &mut stage, dev, locked)?);
    // A computer that just logged in takes its workspace in while the device is held (below):
    // nothing is admitted to the workspace before then. The endpoint admits each connection as
    // it comes, and host search starts once the workspace is here, rather than the daemon ending
    // before it could take the workspace in.
    let search = Arc::new(OnceLock::new());
    search_once_held(&session, &search)?;
    stages.enter("answering as the person");
    drop(stages);
    let endpoint = Endpoint {
        session_factory: admitted_per_connection(&session),
        status: Status {
            version: 1,
            pid: std::process::id(),
            workspace: session.workspace().to_owned(),
        },
        search,
        media: session.media(),
        other_devices: session.other_devices(),
    };
    native.serve_the_person(endpoint);
    // The device is served until the daemon is told to stop, or another takes the device over:
    // the process ends then, its apps' connections with it.
    let held = crate::device::hold(&session);
    // Removed while the daemon still holds the device's lock: a daemon starting after it binds
    // its own endpoint there.
    let _ = std::fs::remove_file(&path);
    drop(session);
    held.map_err(|error| io::Error::other(format!("{error:#}")))?;
    Ok(())
}

/// A connection the native endpoint takes, with the slot it counts against.
type Take = Box<dyn Fn(UnixStream, ConnectionSlot) -> io::Result<()> + Send + Sync>;

/// The native endpoint over the daemon's whole life: the device's own actions from boot, and the
/// person's endpoint once their services run. A connection on a device no certificate names yet
/// is answered with the device's own actions at once; one on a set-up device while its person's
/// services start waits on its own thread until they run, so an app is never served the device
/// alone for its life. The listener itself never waits.
struct Native {
    device: Endpoint<crate::device::unclaimed::DeviceOnly>,
    answers: crate::device::unclaimed::DeviceOnly,
    person: OnceLock<Take>,
    /// Told when [`Self::person`] is set.
    started: (Mutex<()>, Condvar),
}

impl Native {
    fn new(device: crate::device::unclaimed::DeviceOnly) -> Self {
        let answers = device.clone();
        let factory: tilo_mcp::SessionFactory<_> =
            Arc::new(move || Ok(McpServer::new(device.clone(), String::new(), String::new())));
        Self {
            answers,
            started: (Mutex::new(()), Condvar::new()),
            device: Endpoint {
                session_factory: factory,
                // No workspace is served yet.
                status: Status {
                    version: 1,
                    pid: std::process::id(),
                    workspace: String::new(),
                },
                search: Arc::new(OnceLock::new()),
                media: None,
                other_devices: None,
            },
            person: OnceLock::new(),
        }
    }

    /// Serve every connection from now on with `endpoint`, the person's.
    fn serve_the_person<H: tilo_actions::remote::ActionProvider + Clone + Send + Sync + 'static>(
        &self,
        endpoint: Endpoint<H>,
    ) {
        let take: Take = Box::new(move |stream, slot| endpoint.take(stream, slot));
        assert!(
            self.person.set(take).is_ok(),
            "one daemon starts its person's services once"
        );
        let _told = self.started.0.lock_recover();
        self.started.1.notify_all();
    }

    /// Serve each connection to `listener` as whatever answers when it comes ([`accept`]).
    fn accept(self: &Arc<Self>, listener: &UnixListener) -> io::Result<()> {
        accept(listener, |stream, slot| match self.person.get() {
            Some(person) => person(stream, slot),
            None if !self.answers.claimed() => self.device.take(stream, slot),
            None => {
                let native = Arc::clone(self);
                std::thread::Builder::new()
                    .name("tilo-native-wait".into())
                    .spawn(move || {
                        let mut told = native.started.0.lock_recover();
                        let person = loop {
                            if let Some(person) = native.person.get() {
                                break person;
                            }
                            told = native
                                .started
                                .1
                                .wait(told)
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                        };
                        drop(told);
                        if let Err(error) = person(stream, slot) {
                            tracing::debug!(%error, "a native connection that waited while the person's services started was not served");
                        }
                    })
                    .map(drop)
            }
        })
    }
}

/// Hand each connection to `listener` to `take`, at most [`MAX_CONNECTIONS`] at once. One more
/// is told there is no room and closed ([`refuse_busy`]).
fn accept(
    listener: &UnixListener,
    take: impl Fn(UnixStream, ConnectionSlot) -> io::Result<()>,
) -> io::Result<()> {
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = stream?;
        if active.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            refuse_busy(stream);
            continue;
        }
        active.fetch_add(1, Ordering::Relaxed);
        take(stream, ConnectionSlot(Arc::clone(&active)))?;
    }
    Ok(())
}

/// What every native connection is served with.
struct Endpoint<H> {
    session_factory: tilo_mcp::SessionFactory<H>,
    status: Status,
    /// Opens the host search results another process was handed, once host search started.
    search: Arc<OnceLock<Arc<tilo_os_search::OsSearch>>>,
    /// Where answers read the content actions produced.
    media: Option<Arc<dyn tilo_mcp::MediaReader>>,
    /// The person's other devices, as this runtime asks them.
    other_devices: Option<crate::other_devices::OtherDevices>,
}

/// The sessions of the native endpoint, each admitted to `session`'s workspace when its
/// connection comes: a connection before the workspace reached this computer is told why it is
/// not served, and one after it is served.
fn admitted_per_connection(
    session: &Arc<Daemon>,
) -> tilo_mcp::SessionFactory<<Daemon as RuntimeServices>::ActionProvider> {
    let session = Arc::downgrade(session);
    let made: OnceLock<tilo_mcp::SessionFactory<_>> = OnceLock::new();
    Arc::new(move || {
        if let Some(factory) = made.get() {
            return factory();
        }
        let daemon = session
            .upgrade()
            .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
        let factory = daemon.mcp_session_factory(daemon.workspace())?;
        made.get_or_init(|| factory)()
    })
}

/// Start host search over `session`'s workspace into `search` once the workspace can be admitted
/// to: at once on a computer that holds it, and when it arrives on one that just logged in.
fn search_once_held(
    session: &Arc<Daemon>,
    search: &Arc<OnceLock<Arc<tilo_os_search::OsSearch>>>,
) -> io::Result<()> {
    let entered = session.entered();
    let (session, search) = (Arc::downgrade(session), Arc::clone(search));
    let runtime = tilo_rt::try_app()
        .map_err(io::Error::other)?
        .handle()
        .clone();
    std::thread::Builder::new()
        .name("tilo-os-search".into())
        .spawn(move || {
            let mut said = false;
            loop {
                // Armed before the look, so a workspace held between the two is not missed.
                let mut held = std::pin::pin!(entered.notified());
                held.as_mut().enable();
                let Some(daemon) = session.upgrade() else {
                    return;
                };
                match daemon.action_provider() {
                    Ok(host) => {
                        match crate::os_search::start(host, daemon.workspace(), daemon.actor()) {
                            Ok(started) => {
                                let _ = search.set(started);
                            }
                            Err(error) => {
                                tracing::warn!(%error, "host search does not search this computer's workspace");
                            }
                        }
                        return;
                    }
                    Err(error) if !said => {
                        said = true;
                        tracing::warn!(%error, "host search starts once this computer holds its workspace");
                    }
                    Err(_) => {}
                }
                drop(daemon);
                runtime.block_on(held);
            }
        })?;
    Ok(())
}

/// The longest first line a native connection is read for before it is
/// served: a forward to another device, or the first request of any other.
const FIRST_LINE: u64 = 64 * 1024;

/// Connections the native endpoint serves at once.
const MAX_CONNECTIONS: usize = 64;
/// What a connection past [`MAX_CONNECTIONS`] is told before it is closed.
const NO_ROOM: &str = "this runtime is serving 64 connections, as many as it serves at once, so \
     this one is closed and nothing it asked ran; connect again once one of them has closed";
const _: () = assert!(MAX_CONNECTIONS == 64, "NO_ROOM names the bound");

/// Tell a client the endpoint has no room for why it is not served, then
/// close: a JSON-RPC error that names no request, since none of its requests
/// is read. The line is written without waiting on the client, so one that
/// reads nothing cannot keep the endpoint from the next connection.
fn refuse_busy(mut stream: UnixStream) {
    let refusal = json!({"jsonrpc":"2.0","id":null,"error":{"code":-32000,"message":NO_ROOM}});
    let told = stream
        .set_write_timeout(Some(Duration::from_secs(1)))
        .and_then(|()| writeln!(stream, "{refusal}"));
    if let Err(error) = told {
        tracing::debug!(%error, "a native client past the bound was not told why it is closed");
    }
}

impl<H: tilo_actions::remote::ActionProvider + Clone + Send + Sync + 'static> Endpoint<H> {
    /// Serve `stream` on its own thread, counted in `slot` while it lasts.
    fn take(&self, stream: UnixStream, slot: ConnectionSlot) -> io::Result<()> {
        let factory = Arc::clone(&self.session_factory);
        let status = self.status.clone();
        let search = Arc::clone(&self.search);
        let media = self.media.clone();
        let other_devices = self.other_devices.clone();
        std::thread::Builder::new()
            .name("tilo-native-mcp".into())
            .spawn(move || {
                let _slot = slot;
                // The first line says whether the client asks to be
                // carried to another device's MCP server; anything else
                // is served here, that line first.
                let mut reader = match stream.try_clone() {
                    Ok(clone) => BufReader::new(clone),
                    Err(error) => {
                        tracing::debug!(%error, "native runtime client socket");
                        return;
                    }
                };
                let mut first = Vec::new();
                if let Err(error) = (&mut reader).take(FIRST_LINE).read_until(b'\n', &mut first) {
                    tracing::debug!(%error, "native runtime client disconnected");
                    return;
                }
                if let Some(forward) = first
                    .strip_suffix(b"\n")
                    .and_then(tilo_mcp::devices::forward_request)
                {
                    match &other_devices {
                        // A client asks for nothing more before it is told
                        // whether the connection opened.
                        Some(other_devices) if reader.buffer().is_empty() => {
                            other_devices.serve_forward(stream, &forward);
                        }
                        Some(_) => tilo_mcp::devices::refuse_forward(
                            stream,
                            "a forward is the only line before its answer".into(),
                        ),
                        None => tilo_mcp::devices::refuse_forward(
                            stream,
                            "this runtime reaches none of your other devices".into(),
                        ),
                    }
                    return;
                }
                let input = io::Cursor::new(first).chain(reader);
                let server = match factory() {
                    Ok(server) => Arc::new(server.with_media(media).with_devices(
                        other_devices.map(|other_devices| {
                            Arc::new(other_devices) as Arc<dyn tilo_mcp::devices::DeviceDirectory>
                        }),
                    )),
                    Err(error) => {
                        tracing::debug!(%error, "native MCP session admission unavailable");
                        return;
                    }
                };
                if let Err(error) = serve_connection(stream, input, &server, &status, &search) {
                    tracing::debug!(%error, "native runtime client disconnected");
                }
            })?;
        Ok(())
    }
}
struct ConnectionSlot(Arc<AtomicUsize>);
impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Answer one native connection: the daemon's own requests in order, and
/// MCP requests as [`serve_lines`] does, overlapping one another.
fn serve_connection<H: tilo_actions::remote::ActionProvider + Send + Sync + 'static>(
    mut stream: UnixStream,
    input: impl Read + Send + 'static,
    server: &Arc<McpServer<H>>,
    status: &Status,
    search: &OnceLock<Arc<tilo_os_search::OsSearch>>,
) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    // Every connection thread waits on the one app runtime rather than
    // building a runtime per connection.
    let runtime = tilo_rt::try_app().map_err(io::Error::other)?;
    let input = spawn_reader(input)?;
    let status = serde_json::to_value(status).map_err(io::Error::other)?;
    let answer = |value: Value| {
        let native = value.get("native").and_then(Value::as_str);
        Answer::InOrder(match native {
            Some("status") if value == json!({"native":"status"}) => {
                let status = status.clone();
                Box::pin(async move { Ok(Some(status)) })
            }
            Some("activate") => {
                Box::pin(async move { Ok(Some(answer_activate(server, &value).await)) })
            }
            Some(tilo_host_search::OPEN) => Box::pin(async move {
                Ok(Some(match search.get() {
                    Some(search) => answer_open(search, &value).await,
                    None => json!({"error": SEARCH_NOT_STARTED}),
                }))
            }),
            _ => return mcp_answer(server, value),
        })
    };
    let ended = serve_lines(runtime, input, &mut stream, answer, None);
    // The reader thread holds a clone of the socket: shut it, so the reader
    // sees its end and the client its connection close.
    let _ = stream.shutdown(std::net::Shutdown::Both);
    ended
}

/// A native activation request's answer: the newest live instance of the
/// named application brought forward, or that none runs.
async fn answer_activate<H: tilo_actions::remote::ActionProvider + Send + Sync + 'static>(
    server: &McpServer<H>,
    request: &Value,
) -> Value {
    match request.get("app_id").and_then(Value::as_str) {
        Some(app) if !app.is_empty() && app.len() <= 4096 => {
            match server.activate_existing(app).await {
                Ok(Some(peer)) => json!({"activated": true, "peer_id": peer}),
                Ok(None) => json!({"activated": false}),
                Err(reason) => json!({"error": reason}),
            }
        }
        _ => json!({"error": "activation needs an application identity"}),
    }
}

/// What a host search result chosen before host search started is answered.
const SEARCH_NOT_STARTED: &str =
    "host search starts once this computer holds its workspace; nothing it found is open yet";

/// A native open request's answer: a host search result another process of
/// this runtime was handed (a Spotlight click reaches the bundle's app),
/// opened after the same re-check as any other.
pub(crate) async fn answer_open(search: &tilo_os_search::OsSearch, request: &Value) -> Value {
    let Some(entry) = request.get("entry").and_then(Value::as_str) else {
        return json!({"error": "an open request needs an entry"});
    };
    match search.open(entry).await {
        Ok(()) => json!({"opened": true}),
        Err(error) => json!({"error": format!("{error:?}")}),
    }
}

/// Bridge this process's stdio to the MCP host it belongs to: inside a
/// confined environment, the server the environment names
/// ([`tilo_paths::login::AGENT_SOCKET_ENV`]); inside a Tilo login, the
/// desktop's MCP server, which is never replaced by a standalone runtime;
/// elsewhere, or when `standalone`, the persistent daemon of `profile`,
/// started when none runs. EOF closes only this client's connection; neither
/// applications nor the daemon are owned here.
///
/// With `forward`, the host this process belongs to serves it as the person's agent working on
/// the device it names, sending that device each of its requests, once it said it would
/// ([`tilo_mcp::devices::ask_forward`]). `workspace` is
/// the workspace the client asks this desktop's MCP server for, written into
/// its `initialize`.
pub fn connect(
    profile: &Path,
    standalone: bool,
    forward: Option<&tilo_mcp::devices::Forward>,
    workspace: Option<&str>,
) -> io::Result<()> {
    let route = tilo_mcp::session_api::route(
        standalone,
        tilo_mcp::session_api::environment_server(std::env::var_os(
            tilo_paths::login::AGENT_SOCKET_ENV,
        )),
        tilo_paths::login::is_tilo_login(),
        tilo_mcp::session_api::agent_socket_path(
            tilo_paths::login::login_runtime_dir().map(Into::into),
        ),
        Path::exists,
    )?;
    let mut stream = match route {
        tilo_mcp::session_api::Route::Session(server) => {
            tilo_files::native::socket::connect(&server)
                .map_err(|error| tilo_mcp::session_api::unreachable_server(&server, &error))?
        }
        tilo_mcp::session_api::Route::Standalone if workspace.is_some() => {
            return Err(invalid(
                "--workspace asks a Tilo desktop's MCP server for a workspace, and this process \
                 is not in a Tilo login",
            ));
        }
        tilo_mcp::session_api::Route::Standalone => {
            ensure_running(profile)?;
            tilo_files::native::socket::connect(&profile.join("native.sock"))?
        }
    };
    if let Some(forward) = forward {
        tilo_mcp::devices::ask_forward(&mut stream, forward)?;
    }
    bridge_stdio(
        stream,
        Asking {
            workspace: workspace.map(str::to_owned),
            key: tilo_mcp::session_api::agent_key()?,
            // A client started by an agent is that agent's session, shown by its name there when
            // the agent's harness gives it one.
            session_name: tilo_mcp::session_api::session_name(),
        },
    )
}

/// Attach connected service descriptors and replace this host with the app.
pub fn run_app(
    profile: &Path,
    app_id: &str,
    workspace: Option<&str>,
    executable: &OsString,
    args: &[OsString],
) -> io::Result<()> {
    let status = ensure_running(profile)?;
    // Only the argument-free default presentation is interchangeable here.
    // App-specific arguments must run their own parser and are never discarded.
    let launch_guard =
        if args.is_empty() && workspace.is_none_or(|workspace| workspace == status.workspace) {
            match default_launch_guard(profile, app_id, Duration::from_secs(3))? {
                Some(guard) => Some(guard),
                None => return Ok(()),
            }
        } else {
            None
        };
    let owner = std::process::id().to_string();
    // Only the bootstrap exec for this exact process can request resumption.
    // A same-app ordinary child must run its own command handler first.
    let resume = std::env::var_os("TILO_RUNTIME_CONNECTION_OWNER_PID").as_deref()
        == Some(std::ffi::OsStr::new(&owner))
        && std::env::var_os("TILO_RUNTIME_RESUME_LIVE").as_deref()
            == Some(std::ffi::OsStr::new(app_id));
    let mut command = Command::new(executable);
    command.args(args);
    let connections = host::Connections::prepare(
        &mut command,
        app_id,
        workspace.unwrap_or(&status.workspace),
        &host::ServiceSockets {
            collab: &profile.join(COLLAB_SOCKET),
            identity: &profile.join(IDENTITY_SOCKET),
            blobs: &profile.join(BLOB_SOCKET),
        },
    )?;
    command
        .env("TILO_RUNTIME_CONNECTION_OWNER_PID", owner)
        .env("TILO_RUNTIME_PROFILE", profile);
    if resume {
        command.env("TILO_RUNTIME_RESUME_LIVE", app_id);
    }
    if let Some(guard) = &launch_guard {
        use std::os::fd::AsRawFd;
        let fd = guard.file().as_raw_fd();
        command.env("TILO_RUNTIME_LAUNCH_GUARD_FD", fd.to_string());
        // SAFETY: the guard stays owned until exec and only fcntl runs here.
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    } else {
        command.env_remove("TILO_RUNTIME_LAUNCH_GUARD_FD");
    }
    let error = command.exec();
    drop(connections);
    Err(error)
}

fn activate_existing(profile: &Path, app_id: &str) -> io::Result<bool> {
    let mut stream = tilo_files::native::socket::connect(&profile.join("native.sock"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let request = serde_json::to_vec(&json!({"native": "activate", "app_id": app_id}))
        .map_err(io::Error::other)?;
    stream.write_all(&request)?;
    stream.write_all(b"\n")?;
    let Frame::Line(bytes) = read_frame(&mut BufReader::new(stream))? else {
        return Err(invalid(
            "native activation endpoint returned no bounded reply",
        ));
    };
    let reply: Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    match reply.get("activated").and_then(Value::as_bool) {
        Some(activated) => Ok(activated),
        None => Err(io::Error::other(
            reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("invalid native activation reply")
                .to_owned(),
        )),
    }
}

/// Kernel-held admission survives exec and is released on owner exit. File
/// contents and PID observations are diagnostics, never proof of a live owner.
fn default_launch_guard(
    profile: &Path,
    app_id: &str,
    grace: Duration,
) -> io::Result<Option<advisory_lock::FileLock>> {
    let directory = profile.join("launches");
    tilo_files::native::private::directory(&directory)?;
    let path = directory.join(format!("{}.lock", blake3::hash(app_id.as_bytes()).to_hex()));
    let deadline = Instant::now().checked_add(grace).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "native launch startup grace is too large",
        )
    })?;
    loop {
        if activate_existing(profile, app_id)? {
            return Ok(None);
        }
        match advisory_lock::FileLock::try_exclusive(host::private_file(&path)?) {
            Ok(guard) => {
                guard.resource_owner()?;
                return Ok(Some(guard));
            }
            Err(advisory_lock::TryLockError::Error(error)) => return Err(error),
            Err(advisory_lock::TryLockError::WouldBlock) => {}
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "existing {app_id} launch has not published its window within the startup grace; its owner was retained"
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
#[path = "daemon_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "daemon_limits_tests.rs"]
mod limits_tests;
