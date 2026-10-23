//! The host's side of `tilo-video-render`: lay out a job directory, start
//! the helper on it with the output file it writes into, and follow it to
//! the end.

use std::io::Write as _;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::proto::{FRAMING, PROTOCOL_VERSION, Reply, Request, SNAPSHOT, SOURCES};

/// The variables a helper inherits as they are: where fonts and caches are,
/// where `GStreamer`'s plugin scanner is, the locale and logging. Never the
/// host's own (model keys, state paths). Plugin search paths pass through
/// [`plugin_dirs`] instead, and the registry is the helper's own
/// ([`registry`]).
const PASSED_ENV: &[&str] = &[
    "HOME",
    "LANG",
    "LC_ALL",
    "XDG_CACHE_HOME",
    "XDG_DATA_DIRS",
    "GST_PLUGIN_SCANNER",
    "GST_DEBUG",
    "RUST_LOG",
    "RUST_BACKTRACE",
];

/// The variables naming where `GStreamer` looks for plugins besides its own
/// directory (an encoder installed outside the system's packages). The
/// helper is given them from the host's environment, validated, and may
/// read the directories they name once confined.
pub const PLUGIN_PATH_VARS: &[&str] = &[
    "GST_PLUGIN_PATH",
    "GST_PLUGIN_PATH_1_0",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_SYSTEM_PATH_1_0",
];

/// What every helper runs with: `ORC` interprets rather than generating
/// code (a confined helper maps no new executable memory), and decode and
/// encode stay in software, which is deterministic and reaches no media
/// service.
const FIXED_ENV: &[(&str, &str)] = &[
    ("ORC_CODE", "backup"),
    ("TILO_MEDIA_HWDEC", "off"),
    ("TILO_MEDIA_HWENC", "off"),
];

/// The name of the helper's executable.
pub const HELPER: &str = "tilo-video-render";

/// The variable naming the helper explicitly.
pub const HELPER_ENV: &str = "TILO_VIDEO_RENDER";

/// Where the helper is: `$TILO_VIDEO_RENDER`, else beside the running
/// executable (or one directory up, where a test binary's build puts it).
#[must_use]
pub fn locate() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(HELPER_ENV) {
        let path = PathBuf::from(path);
        return (path.is_absolute() && path.is_file()).then_some(path);
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [Some(dir), dir.parent()]
        .into_iter()
        .flatten()
        .map(|dir| dir.join(HELPER))
        .find(|candidate| candidate.is_file())
}

/// The directories of the plugin search path `value` a helper may be given
/// and read: each entry that is an absolute path to an existing directory
/// other than the root, canonical. The rest (a relative entry, a file, a
/// missing path) is dropped, so a host's environment widens a confined
/// helper's reads by whole plugin directories at most.
#[must_use]
pub fn plugin_dirs(value: &std::ffi::OsStr) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in std::env::split_paths(value) {
        if !entry.is_absolute() {
            continue;
        }
        let Ok(dir) = entry.canonicalize() else {
            continue;
        };
        if dir.is_dir() && dir.parent().is_some() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// The registry a helper keeps of `GStreamer`'s plugins: its own file in
/// the user's cache, not `GStreamer`'s shared one, which every other program
/// rewrites for its own plugin path (and so drops a plugin only this host's
/// `GST_PLUGIN_PATH` names). `None` without a cache directory.
fn registry() -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .map(|home| home.join(".cache"))
        })?;
    let dir = cache.join("tilo");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!(
        "video-render-registry.{}.bin",
        std::env::consts::ARCH
    )))
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
    /// The hash is not one, the sources together outgrow the budget, or the
    /// copy failed.
    pub fn put_source(&mut self, hash: &str, reader: &mut dyn std::io::Read) -> Result<(), String> {
        if hash.is_empty() || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("{hash:?} is not a content hash"));
        }
        let mut file =
            std::fs::File::create(self.path.join(SOURCES).join(hash.to_ascii_lowercase()))
                .map_err(|error| format!("a source could not be handed over: {error}"))?;
        let mut limited = std::io::Read::take(reader, self.budget.saturating_add(1));
        let copied = std::io::copy(&mut limited, &mut file)
            .map_err(|error| format!("a source could not be handed over: {error}"))?;
        if copied > self.budget {
            return Err(format!(
                "the timeline's sources are larger than a render may read ({})",
                ui::format::size(self.limit)
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

/// Why a render stopped short of finishing.
pub const STOPPED: &str = "the render was cancelled";

/// Run the helper at `helper` on `job` for `request`, until `deadline`.
/// `progress` hears each report and returns `false` to stop the render
/// (the helper is killed then).
///
/// # Errors
///
/// The helper did not start, could not confine itself, failed, was
/// stopped ([`STOPPED`]), or outlived the deadline (it is killed then).
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
    // The helper is handed the open file; no name of it remains in the job
    // directory it may read.
    let _ = std::fs::remove_file(&output_path);
    let handed = output
        .try_clone()
        .map_err(|error| format!("the render's output could not be handed over: {error}"))?;
    let mut helper = Helper::start(helper, job.path(), handed)?;
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
            Ok(None) => {
                let status = helper
                    .child
                    .wait()
                    .map_or_else(|error| error.to_string(), |status| status.to_string());
                return Err(format!(
                    "the render helper stopped without answering ({status})"
                ));
            }
            Err(wire_frame::FrameError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err("the render did not finish in time and was stopped".into());
            }
            Err(error) => return Err(format!("the render helper stopped: {error}")),
        };
        match reply {
            Reply::Ready { version } if version == PROTOCOL_VERSION => {}
            Reply::Ready { .. } => {
                return Err("the render helper speaks another protocol (a stale build)".into());
            }
            Reply::Progress { done, total } => {
                if !progress(done, total) {
                    return Err(STOPPED.to_owned());
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
    fn start(helper: &Path, job: &Path, output: std::fs::File) -> Result<Self, String> {
        let (ours, theirs) = UnixStream::pair().map_err(|error| error.to_string())?;
        let mut command = Command::new(helper);
        command
            .arg(job)
            .env_clear()
            .stdin(Stdio::from(OwnedFd::from(theirs)))
            .stdout(Stdio::from(output))
            .stderr(Stdio::inherit());
        for key in PASSED_ENV {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        for key in PLUGIN_PATH_VARS {
            let dirs = std::env::var_os(key)
                .map(|value| plugin_dirs(&value))
                .unwrap_or_default();
            // A path with nothing left in it is not passed: an empty system
            // path would hide `GStreamer`'s own plugins.
            if let Ok(value) = std::env::join_paths(dirs)
                && !value.is_empty()
            {
                command.env(key, value);
            }
        }
        if let Some(registry) = registry() {
            command.env("GST_REGISTRY_1_0", registry);
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

#[cfg(test)]
mod tests {
    use super::{JobDir, plugin_dirs};

    #[test]
    fn a_plugin_path_keeps_only_absolute_existing_directories_other_than_the_root() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let plugins = dir.path().join("gstreamer-1.0");
        std::fs::create_dir(&plugins).expect("the plugin directory");
        let file = dir.path().join("libgstsomething.dylib");
        std::fs::write(&file, b"").expect("a file");
        let path = std::env::join_paths([
            plugins.clone(),
            "relative/gstreamer-1.0".into(),
            file,
            dir.path().join("missing"),
            "/".into(),
            plugins.join("."),
        ])
        .expect("a joinable path");
        let canonical = plugins.canonicalize().expect("the plugin directory exists");
        assert_eq!(plugin_dirs(&path), vec![canonical]);
    }

    #[test]
    fn an_empty_plugin_path_names_no_directory() {
        assert!(plugin_dirs(std::ffi::OsStr::new("")).is_empty());
    }

    #[test]
    fn a_job_directory_refuses_sources_past_its_budget_and_is_removed_when_dropped() {
        let parent = tempfile::tempdir().expect("a temporary directory");
        let mut job = JobDir::create(parent.path(), 10).expect("a job directory");
        let path = job.path().to_path_buf();
        job.put_source(&"ab".repeat(32), &mut &b"12345"[..])
            .expect("five bytes fit");
        assert!(
            job.put_source(&"cd".repeat(32), &mut &b"123456"[..])
                .is_err(),
            "six more do not"
        );
        assert!(job.put_source("../escape", &mut &b""[..]).is_err());
        drop(job);
        assert!(!path.exists());
    }
}
