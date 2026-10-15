//! The host's side of `tilo-video-render`: lay out a job directory, start
//! the helper on it, hand it the output file, and follow it to the end.

use std::io::Write as _;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::proto::{FD_MARKER, FRAMING, PROTOCOL_VERSION, Reply, Request, SNAPSHOT, SOURCES};

/// The variables a helper inherits: where `GStreamer` finds its plugins and
/// registry, where fonts are, the locale and logging. Never the host's own
/// (model API keys, state paths).
const PASSED_ENV: &[&str] = &[
    "HOME",
    "LANG",
    "LC_ALL",
    "XDG_CACHE_HOME",
    "XDG_DATA_DIRS",
    "GST_PLUGIN_PATH",
    "GST_PLUGIN_PATH_1_0",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_SYSTEM_PATH_1_0",
    "GST_PLUGIN_SCANNER",
    "GST_REGISTRY",
    "GST_REGISTRY_1_0",
    "GST_DEBUG",
    "RUST_LOG",
    "RUST_BACKTRACE",
];

/// What every helper runs with: `GStreamer` reads its registry and forks no
/// scanner (a confined helper can start no process), `ORC` interprets
/// rather than generating code (a confined helper maps no new executable
/// memory), and decode and encode stay in software, which is deterministic
/// and reaches no media service.
const FIXED_ENV: &[(&str, &str)] = &[
    ("GST_REGISTRY_FORK", "no"),
    ("GST_REGISTRY_UPDATE", "no"),
    ("ORC_CODE", "backup"),
    ("TILO_MEDIA_HWDEC", "off"),
    ("TILO_MEDIA_HWENC", "off"),
];

/// The name of the helper's executable.
pub const HELPER: &str = "tilo-video-render";

/// Where the helper is: `$TILO_VIDEO_RENDER`, else beside the running
/// executable (or one directory up, where a test binary's build puts it).
#[must_use]
pub fn locate() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("TILO_VIDEO_RENDER") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [Some(dir), dir.parent()]
        .into_iter()
        .flatten()
        .map(|dir| dir.join(HELPER))
        .find(|candidate| candidate.is_file())
}

/// A private directory a render's inputs are laid out in, removed when
/// dropped.
pub struct JobDir {
    path: PathBuf,
    /// Bytes of sources still to be had, of `limit`.
    budget: u64,
    limit: u64,
}

impl JobDir {
    /// Make a new job directory under `parent`, readable only by this user,
    /// holding at most `budget` bytes of sources.
    ///
    /// # Errors
    ///
    /// The directory could not be made.
    pub fn create(parent: &Path, budget: u64) -> Result<Self, String> {
        use std::os::unix::fs::DirBuilderExt as _;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = parent.join(format!("tilo-render-{}-{n}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .and_then(|()| {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(path.join(SOURCES))
            })
            .map_err(|error| format!("the render's job directory could not be made: {error}"))?;
        let path = path
            .canonicalize()
            .map_err(|error| format!("the render's job directory vanished: {error}"))?;
        Ok(Self {
            path,
            budget,
            limit: budget,
        })
    }

    /// The directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Keep the record's document for the helper.
    ///
    /// # Errors
    ///
    /// It could not be written.
    pub fn put_snapshot(&self, bytes: &[u8]) -> Result<(), String> {
        std::fs::write(self.path.join(SNAPSHOT), bytes)
            .map_err(|error| format!("the timeline could not be handed over: {error}"))
    }

    /// Copy source `hash` from `reader` for the helper, within the budget.
    ///
    /// # Errors
    ///
    /// The sources together outgrow the budget, or the copy failed.
    pub fn put_source(&mut self, hash: &str, reader: &mut dyn std::io::Read) -> Result<(), String> {
        if hash.is_empty() || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("{hash:?} is not a content hash"));
        }
        let mut file = std::fs::File::create(self.path.join(SOURCES).join(hash))
            .map_err(|error| format!("a source could not be handed over: {error}"))?;
        let mut limited = std::io::Read::take(reader, self.budget.saturating_add(1));
        let copied = std::io::copy(&mut limited, &mut file)
            .map_err(|error| format!("a source could not be handed over: {error}"))?;
        if copied > self.budget {
            return Err(format!(
                "the timeline's sources are larger than a render may read ({} MiB)",
                self.limit >> 20
            ));
        }
        self.budget = self.budget.saturating_sub(copied);
        file.flush()
            .map_err(|error| format!("a source could not be handed over: {error}"))
    }
}

impl Drop for JobDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// What a finished helper produced: its last answer and the output file,
/// rewound to its start.
pub struct Finished {
    pub reply: Reply,
    pub output: std::fs::File,
}

/// Run the helper at `helper` on `job` for `request`, until `deadline`.
/// `progress` hears each report and returns `false` to stop the render.
///
/// # Errors
///
/// The helper did not start, could not confine itself, failed, was
/// stopped, or outlived the deadline (it is killed then).
pub fn render(
    helper: &Path,
    job: &JobDir,
    request: &Request,
    deadline: Instant,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<Finished, String> {
    let output_path = job.path().join(".output");
    let output = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&output_path)
        .map_err(|error| format!("the render's output could not be made: {error}"))?;
    // The helper is handed the open file; no name of it remains for anyone.
    let _ = std::fs::remove_file(&output_path);
    let mut helper = Helper::start(helper, job.path())?;
    FRAMING
        .write_value(&mut helper.socket, request)
        .map_err(|error| format!("the render helper stopped: {error}"))?;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("the render did not finish in time and was stopped".into());
        }
        helper
            .socket
            .set_read_timeout(Some(remaining.max(Duration::from_millis(10))))
            .map_err(|error| error.to_string())?;
        let reply: Reply = match FRAMING.read_value(&mut helper.socket) {
            Ok(Some(reply)) => reply,
            Ok(None) => return Err("the render helper stopped without answering".into()),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Err("the render did not finish in time and was stopped".into());
            }
            Err(error) => return Err(format!("the render helper stopped: {error}")),
        };
        match reply {
            Reply::Ready { version } if version == PROTOCOL_VERSION => {
                runtime_portal::fd_transport::send_with_fd(
                    helper.socket.as_raw_fd(),
                    FD_MARKER,
                    output.as_raw_fd(),
                )
                .map_err(|error| format!("the output could not be handed over: {error}"))?;
            }
            Reply::Ready { .. } => {
                return Err("the render helper speaks another protocol (a stale build)".into());
            }
            Reply::Progress { done, total } => {
                if !progress(done, total) {
                    return Err(crate::export::CANCELLED.to_owned());
                }
            }
            Reply::Failed(error) => return Err(error),
            finished @ (Reply::Video(_) | Reply::Frame) => {
                let _ = helper.child.wait();
                let mut output = output;
                std::io::Seek::seek(&mut output, std::io::SeekFrom::Start(0))
                    .map_err(|error| format!("the render could not be read: {error}"))?;
                return Ok(Finished {
                    reply: finished,
                    output,
                });
            }
        }
    }
}

/// A running helper. Dropping it ends the process.
struct Helper {
    child: Child,
    socket: UnixStream,
}

impl Helper {
    fn start(helper: &Path, job: &Path) -> Result<Self, String> {
        let (ours, theirs) = UnixStream::pair().map_err(|error| error.to_string())?;
        let mut command = Command::new(helper);
        command
            .arg(job)
            .env_clear()
            .stdin(Stdio::from(OwnedFd::from(theirs)))
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        for key in PASSED_ENV {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command.envs(FIXED_ENV.iter().copied());
        let child = command
            .spawn()
            .map_err(|error| format!("the render helper did not start: {error}"))?;
        Ok(Self {
            child,
            socket: ours,
        })
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
