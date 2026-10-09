//! Persistent native session shared by independently launched GUI apps and MCP.
use crate::{
    BLOB_SOCKET, COLLAB_SOCKET, IDENTITY_SOCKET, RuntimeServices,
    mcp::McpServer,
    mcp_process::{Answer, Frame, mcp_answer, read_frame, serve_lines, spawn_reader},
    unix,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const PROBE_TIMEOUT: Duration = Duration::from_millis(300);

#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    Daemon,
    Connect,
    RunApp {
        app_id: String,
        /// A room this profile joined; the profile's own room when `None`.
        room: Option<String>,
        executable: OsString,
        args: Vec<OsString>,
    },
    /// Join the room an invitation names, through the running daemon.
    Join {
        invitation: String,
    },
}
#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub profile: PathBuf,
    pub mode: Mode,
    /// `--dev`, for `--daemon` only: serve builds under development, which
    /// are not installed.
    pub dev: bool,
}
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

/// Validate the private profile before any startup side effects.
/// # Errors
/// Rejects public directories, symlinks, foreign ownership or creation errors.
pub fn validate_profile(profile: &Path) -> io::Result<()> {
    if !profile.is_absolute() {
        return Err(invalid("runtime profile must be absolute"));
    }
    unix::check_private_profile(profile)
}

/// The same persistent profile is used by daemon, app attachment and MCP proxy.
/// # Errors
/// Rejects a missing home directory or a relative profile override.
pub fn default_profile() -> io::Result<PathBuf> {
    let path = if let Some(value) = std::env::var_os("TILO_RUNTIME_PROFILE") {
        PathBuf::from(value)
    } else {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| invalid("HOME is missing; set TILO_RUNTIME_PROFILE"))?;
        #[cfg(target_os = "macos")]
        let suffix = "Library/Application Support/Tilo/runtime";
        #[cfg(not(target_os = "macos"))]
        let suffix = ".local/state/tilo/runtime";
        PathBuf::from(home).join(suffix)
    };
    if !path.is_absolute() {
        return Err(invalid("runtime profile must be absolute"));
    }
    Ok(path)
}

/// Parse native modes while preserving opaque application arguments.
/// # Errors
/// Rejects ambiguous modes, missing arguments and relative profiles.
pub fn parse_args(arguments: impl IntoIterator<Item = OsString>) -> io::Result<Args> {
    let mut arguments = arguments.into_iter();
    let mut profile = None;
    let mut mode = None;
    let mut dev = false;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--dev") if !dev => dev = true,
            Some("--profile") if profile.is_none() => {
                profile = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| invalid("--profile requires a path"))?,
                ));
            }
            Some("--daemon") if mode.is_none() => mode = Some(Mode::Daemon),
            Some("--connect") if mode.is_none() => mode = Some(Mode::Connect),
            Some("--join") if mode.is_none() => {
                let invitation = arguments
                    .next()
                    .and_then(|arg| arg.into_string().ok())
                    .filter(|invitation| !invitation.is_empty() && invitation.len() <= 384 * 1024)
                    .ok_or_else(|| invalid("--join requires an invitation"))?;
                mode = Some(Mode::Join { invitation });
            }
            Some("--run-app") if mode.is_none() => {
                let app_id = arguments
                    .next()
                    .and_then(|arg| arg.into_string().ok())
                    .ok_or_else(|| invalid("--run-app requires an application ID"))?;
                if app_id.trim().is_empty() || app_id.len() > 4096 {
                    return Err(invalid("invalid application ID"));
                }
                let mut next = arguments.next();
                let mut room = None;
                if next.as_deref() == Some(std::ffi::OsStr::new("--room")) {
                    room = Some(
                        arguments
                            .next()
                            .and_then(|arg| arg.into_string().ok())
                            .filter(|room| uuid::Uuid::parse_str(room).is_ok())
                            .ok_or_else(|| invalid("--room requires a room id"))?,
                    );
                    next = arguments.next();
                }
                if next.as_deref() != Some(std::ffi::OsStr::new("--")) {
                    return Err(invalid(
                        "--run-app APP_ID [--room ROOM] requires -- EXECUTABLE ARGS...",
                    ));
                }
                let executable = arguments
                    .next()
                    .ok_or_else(|| invalid("missing app executable"))?;
                mode = Some(Mode::RunApp {
                    app_id,
                    room,
                    executable,
                    args: arguments.collect(),
                });
                break;
            }
            _ => {
                return Err(invalid(format!(
                    "unexpected argument: {}",
                    argument.to_string_lossy()
                )));
            }
        }
    }
    let profile = match profile {
        Some(path) => path,
        None => default_profile()?,
    };
    if !profile.is_absolute() {
        return Err(invalid("runtime profile must be absolute"));
    }
    let mode = mode.ok_or_else(|| invalid("choose --daemon, --connect, --join INVITATION, --run-app APP_ID [--room ROOM] -- EXECUTABLE, or --profile PATH --apps MANIFEST"))?;
    if dev && mode != Mode::Daemon {
        return Err(invalid("--dev starts a daemon: use it with --daemon"));
    }
    Ok(Args { profile, mode, dev })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Status {
    version: u32,
    pid: u32,
    room: String,
}

fn probe(profile: &Path) -> io::Result<Status> {
    let mut stream = UnixStream::connect(profile.join("native.sock"))?;
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
    if status.version != 1 || status.pid == 0 || status.room.is_empty() || status.room.len() > 4096
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
    /// # Errors
    /// Returns failures to query the process.
    fn exited(&mut self) -> io::Result<Option<String>>;
    fn observe(&mut self) -> Observation;
    /// End a process that will not become ready.
    fn stop(&mut self);
    /// Where to look next, with the process's most recent output and, while
    /// it still runs, what it is blocked on.
    fn diagnostics(&self) -> String;
}

/// When each progress signal last changed, for the error that names which
/// of them went quiet.
fn signal_report(observation: &Observation, changed: &[Option<Instant>]) -> String {
    observation
        .signals
        .iter()
        .zip(changed)
        .map(|((name, value), changed)| match (value, changed) {
            (_, Some(at)) => format!(
                "{name} last changed {:.1} s ago",
                at.elapsed().as_secs_f32()
            ),
            (None, None) => format!("{name} unreadable"),
            (Some(_), None) => format!("{name} unchanged since launch"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Wait until `process` is ready, failing as soon as it exits, stalls or
/// exceeds the absolute cap.
/// # Errors
/// Names the stage the process reached, when each progress signal last
/// changed, and the process's diagnostics.
pub(crate) fn wait_until_ready<P: StartingProcess>(
    process: &mut P,
    limits: StartupLimits,
) -> io::Result<P::Ready> {
    let started = Instant::now();
    let mut last = process.observe();
    let mut changed = vec![None; last.signals.len()];
    let mut last_progress = started;
    loop {
        if let Some(ready) = process.ready() {
            return Ok(ready);
        }
        let now = process.observe();
        for (index, (signal, value)) in now.signals.iter().enumerate() {
            if last.signals.get(index) != Some(&(*signal, *value)) {
                last_progress = Instant::now();
                if let Some(slot) = changed.get_mut(index) {
                    *slot = Some(last_progress);
                }
            }
        }
        last = now;
        if let Some(status) = process.exited()? {
            return Err(io::Error::other(format!(
                "native runtime exited while {} ({status}); {}",
                last.stage,
                process.diagnostics()
            )));
        }
        let failure = if last_progress.elapsed() >= limits.stall {
            format!(
                "native runtime stalled while {}: no progress for {} s ({})",
                last.stage,
                limits.stall.as_secs_f32(),
                signal_report(&last, &changed)
            )
        } else if started.elapsed() >= limits.cap {
            format!(
                "native runtime did not become ready within {} s (still {}; {})",
                limits.cap.as_secs_f32(),
                last.stage,
                signal_report(&last, &changed)
            )
        } else {
            std::thread::sleep(limits.poll);
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

/// The daemon's side of [`STAGES_FILE`].
struct StartupStages(std::fs::File);

impl StartupStages {
    fn open(profile: &Path) -> io::Result<Self> {
        let file = unix::private_file(&profile.join(STAGES_FILE))?;
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
        advisory_lock::FileLock::exclusive(unix::private_file(&profile.join("start.lock"))?)?;
    if let Ok(status) = probe(profile) {
        return Ok(status);
    }
    let log = unix::private_file(&profile.join("daemon.log"))?;
    // Each start's log and steps are only its own daemon's.
    log.set_len(0)?;
    unix::private_file(&profile.join(STAGES_FILE))?.set_len(0)?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--profile")
        .arg(profile)
        .arg("--daemon")
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

/// Start one production Session. The executable prepares its profile environment
/// before entering this function; reusable serving code never mutates it.
/// # Errors
/// Returns profile ownership, service startup, socket or worker-spawn errors.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the broker is resolved before the endpoint is touched so a missing broker fails first"
)]
pub fn serve<S: RuntimeServices>(profile: &Path, dev: bool) -> io::Result<()> {
    validate_profile(profile)?;
    let mut stages = StartupStages::open(profile)?;
    let mut stage = |stage: &str| stages.enter(stage);
    let session = if dev {
        S::start_in_dev_mode(profile, &mut stage)?
    } else {
        S::start_reporting(profile, &mut stage)?
    };
    let host = session.action_host()?;
    let mcp = crate::mcp_http::Config::from_env()?
        .map(|config| {
            session
                .mcp_room(config.room.as_deref())
                .map(|room| (config, room))
        })
        .transpose()?;
    let search = crate::os_search::start(host.clone(), session.room(), session.actor())?;
    let computer = crate::computer::Computer::from_env(profile)?.map(Arc::new);
    let path = profile.join("native.sock");
    // Session already owns runtime.lock. Only that owner may remove a stale
    // endpoint, and a second --daemon cannot reach this point.
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) => {
            use std::os::unix::fs::FileTypeExt;
            if !metadata.file_type().is_socket() {
                return Err(invalid("native runtime endpoint is not a socket"));
            }
            std::fs::remove_file(&path)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    stages.enter("binding the native endpoint");
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    if let Some((config, room)) = mcp {
        stages.enter("binding the MCP HTTP endpoint");
        // Computer, viewer and media are scoped to this profile's private
        // desktop. A shared-room endpoint has only the semantic action door.
        let private = room == session.room();
        let viewer = if private {
            crate::viewer::start_from_env(profile)?
        } else {
            None
        };
        let endpoint = crate::mcp_http::Endpoint::with_viewer(
            host,
            room.clone(),
            session.actor().to_owned(),
            config.token.clone(),
            private.then(|| computer.clone()).flatten(),
            viewer,
        );
        endpoint.serve_sessions(session.mcp_session_factory(&room)?)?;
        if private {
            endpoint.serve_media(session.media());
        }
        let bound = crate::mcp_http::spawn(&config, endpoint)?;
        tracing::info!(%bound, "MCP HTTP endpoint listening");
    }
    stages.enter("answering the status probe");
    drop(stages);
    let status = Status {
        version: 1,
        pid: std::process::id(),
        room: session.room().to_owned(),
    };
    let endpoint = Endpoint {
        session_factory: session.mcp_session_factory(session.room())?,
        status,
        computer,
        joiner: session.joiner(),
        search,
        media: session.media(),
    };
    endpoint.accept(&listener)
}

/// What every native connection is served with.
struct Endpoint<H> {
    session_factory: crate::mcp::SessionFactory<H>,
    status: Status,
    computer: Option<Arc<crate::computer::Computer>>,
    joiner: Option<crate::Joiner>,
    /// Opens the host search results another process was handed.
    search: Arc<tilo_os_search::OsSearch>,
    /// Where answers read the content actions produced.
    media: Option<Arc<dyn crate::MediaReader>>,
}

impl<H: tilo_actions::remote::ActionHost + Clone + Send + Sync + 'static> Endpoint<H> {
    /// Serve each connection to `listener` on its own thread, at most 64 at once.
    fn accept(self, listener: &UnixListener) -> io::Result<()> {
        let active = Arc::new(AtomicUsize::new(0));
        for stream in listener.incoming() {
            let stream = stream?;
            if active.load(Ordering::Relaxed) >= 64 {
                drop(stream);
                continue;
            }
            let factory = Arc::clone(&self.session_factory);
            let status = self.status.clone();
            let computer = self.computer.clone();
            let joiner = self.joiner.clone();
            let search = Arc::clone(&self.search);
            let media = self.media.clone();
            let active = Arc::clone(&active);
            active.fetch_add(1, Ordering::Relaxed);
            let slot = ConnectionSlot(active);
            std::thread::Builder::new()
                .name("tilo-native-mcp".into())
                .spawn(move || {
                    let _slot = slot;
                    let server = match factory() {
                        Ok(server) => Arc::new(server.with_computer(computer).with_media(media)),
                        Err(error) => {
                            tracing::debug!(%error, "native MCP session admission unavailable");
                            return;
                        }
                    };
                    if let Err(error) =
                        serve_connection(stream, &server, &status, joiner.as_ref(), &search)
                    {
                        tracing::debug!(%error, "native runtime client disconnected");
                    }
                    // The connection was one agent run: what it still holds
                    // on the compositor's seat, and the strip it worked in,
                    // are released with it.
                    server.leave_computer();
                })?;
        }
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
fn serve_connection<H: tilo_actions::remote::ActionHost + Send + Sync + 'static>(
    mut stream: UnixStream,
    server: &Arc<McpServer<H>>,
    status: &Status,
    joiner: Option<&crate::Joiner>,
    search: &tilo_os_search::OsSearch,
) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    // Every connection thread waits on the one app runtime rather than
    // building a runtime per connection.
    let runtime = tilo_rt::try_app().map_err(io::Error::other)?;
    let input = spawn_reader(stream.try_clone()?)?;
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
            Some("join") => {
                let answer = answer_join(joiner, &value);
                Box::pin(async move { Ok(Some(answer)) })
            }
            Some(tilo_host_search::OPEN) => {
                Box::pin(async move { Ok(Some(answer_open(search, &value).await)) })
            }
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
async fn answer_activate<H: tilo_actions::remote::ActionHost + Send + Sync + 'static>(
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

/// A native join request's answer: the joined room, or why not.
fn answer_join(joiner: Option<&crate::Joiner>, request: &Value) -> Value {
    let Some(joiner) = joiner else {
        return json!({"error": "this host joins rooms through its own surfaces"});
    };
    let Some(invitation) = request.get("invitation").and_then(Value::as_str) else {
        return json!({"error": "a join request needs an invitation"});
    };
    match joiner(invitation) {
        Ok(room) => json!({"room": room}),
        Err(error) => json!({"error": error.to_string()}),
    }
}

/// Join the room `invitation` names, through the running daemon, and print
/// the joined room's id.
/// # Errors
/// Returns startup or transport errors, or why the daemon did not join.
pub fn join(profile: &Path, invitation: &str) -> io::Result<()> {
    ensure_running(profile)?;
    let mut stream = UnixStream::connect(profile.join("native.sock"))?;
    let mut request = serde_json::to_vec(&json!({"native": "join", "invitation": invitation}))
        .map_err(io::Error::other)?;
    request.push(b'\n');
    stream.write_all(&request)?;
    let mut reply = Vec::new();
    BufReader::new(stream)
        .take(16_385)
        .read_until(b'\n', &mut reply)?;
    let reply: Value = serde_json::from_slice(&reply).map_err(io::Error::other)?;
    match (
        reply.get("room").and_then(Value::as_str),
        reply.get("error"),
    ) {
        (Some(room), None) => {
            println!("{room}");
            Ok(())
        }
        (_, error) => Err(io::Error::other(
            error
                .and_then(Value::as_str)
                .unwrap_or("invalid join reply")
                .to_owned(),
        )),
    }
}

/// Bridge this process's stdio to the persistent daemon. EOF closes only this
/// client's connection; neither applications nor the daemon are owned here.
/// # Errors
/// Returns bounded startup, transport, thread-spawn or stdio errors.
pub fn connect(profile: &Path) -> io::Result<()> {
    ensure_running(profile)?;
    let mut stream = UnixStream::connect(profile.join("native.sock"))?;
    let mut writer = stream.try_clone()?;
    std::thread::Builder::new()
        .name("tilo-mcp-proxy-stdin".into())
        .spawn(move || {
            let result = relay(&mut io::stdin().lock(), &mut writer);
            let _ = writer.shutdown(std::net::Shutdown::Write);
            result
        })?;
    relay(&mut stream, &mut io::stdout().lock())?;
    Ok(())
}

/// Copy `from` to `to`, flushing after every read. A client waits for each
/// reply before it sends the next request, so a reply must not sit in a
/// buffer until more arrives: `io::copy` into a buffered stdout fills its
/// buffer and writes only when it is full or the stream ends.
pub(crate) fn relay(from: &mut impl io::Read, to: &mut impl io::Write) -> io::Result<u64> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0_u64;
    loop {
        let read = match from.read(&mut buffer) {
            Ok(0) => return Ok(total),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        to.write_all(buffer.get(..read).unwrap_or_default())?;
        to.flush()?;
        total = total.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
    }
}

/// Attach connected service descriptors and replace this host with the app.
/// # Errors
/// Returns startup, handshake or exec errors without running disconnected.
pub fn run_app(
    profile: &Path,
    app_id: &str,
    room: Option<&str>,
    executable: &OsString,
    args: &[OsString],
) -> io::Result<()> {
    let status = ensure_running(profile)?;
    // Only the argument-free default presentation is interchangeable here.
    // App-specific arguments must run their own parser and are never discarded.
    let launch_guard = if args.is_empty() && room.is_none_or(|room| room == status.room) {
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
    // This process is introducing the local runtime room. A launcher that
    // already named a workspace keeps its participation, including fail-closed
    // viewer. A personal attach has no room yet, and the person who owns this
    // runtime may edit their own app.
    // A joined room's role is the one its governor admitted.
    if room.is_none()
        && std::env::var_os("TILO_WORKSPACE_ID").is_none()
        && std::env::var_os("TILO_PARTICIPATION").is_none()
    {
        command.env("TILO_PARTICIPATION", "author");
    }
    let connections = unix::Connections::prepare(
        &mut command,
        app_id,
        room.unwrap_or(&status.room),
        &unix::ServiceSockets {
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
    let mut stream = UnixStream::connect(profile.join("native.sock"))?;
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
    unix::private_directory(&directory)?;
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
        match advisory_lock::FileLock::try_exclusive(unix::private_file(&path)?) {
            Ok(guard) => {
                guard.record_owner()?;
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
