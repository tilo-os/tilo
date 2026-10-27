//! The helper's side: confine, render, answer.

use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use gstreamer as gst;
use tilo_sandbox::confine;
use video_editor::Project;
use video_editor::agent_render::{self, AgentExportSettings};

use crate::proto::{FRAMING, PROTOCOL_VERSION, Reply, Request, SNAPSHOT, SOURCES};

/// The plugins a render can reach: the decode graph `tilo-media` builds
/// (`playbin`, demuxers, parsers, software decoders, converters), the
/// stills it imports, and the encode graph. Loaded before confinement,
/// since a confined helper maps no new code; one this host lacks is
/// simply absent.
const PLUGINS: &[&str] = &[
    "coreelements",
    "app",
    "playback",
    "typefindfunctions",
    "isomp4",
    "matroska",
    "libav",
    "videoparsersbad",
    "audioparsers",
    "videoconvertscale",
    "videorate",
    "audioconvert",
    "audioresample",
    "audiorate",
    "volume",
    "deinterlace",
    "videofilter",
    "imagefreeze",
    "png",
    "jpeg",
    "wavparse",
    "mpg123",
    "vpx",
    "opus",
    "dav1d",
    "openh264",
    "faad",
    "fdkaac",
    "voaacenc",
    "pbtypes",
];

/// Descriptors the helper may hold: the socket, stdio, and what the
/// decoders, the GPU and `GStreamer`'s pollers open.
const MAX_OPEN_FILES: u64 = 256;

/// Address space a helper may map on Linux.
const MAX_ADDRESS_SPACE: u64 = 4 << 30;

/// How often progress is reported.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// Entry point of `tilo-video-render`. Returns the exit code.
#[must_use]
pub fn run() -> u8 {
    // Before any thread starts, so the address space capped below does not
    // depend on how many cores the host has.
    confine::share_allocator_arenas();
    // First, before any descriptor of its own is made: everything
    // inherited above stdio is closed.
    if let Err(error) = confine::detach() {
        tracing::error!(%error, "render helper");
        return 2;
    }
    let channel = std::io::stdin()
        .as_fd()
        .try_clone_to_owned()
        .map(UnixStream::from)
        .map_err(|error| format!("the host's socket is not stdin: {error}"));
    let mut channel = match channel {
        Ok(socket) => Channel(socket),
        Err(error) => {
            tracing::error!(%error, "render helper");
            return 2;
        }
    };
    let outcome = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| "the render helper is started with its job directory".to_owned())
        .and_then(|job| serve(&mut channel, &job));
    match outcome {
        Ok(reply) => u8::from(channel.send(&reply).is_err()),
        Err(error) => {
            let _ = channel.send(&Reply::Failed(error));
            1
        }
    }
}

struct Channel(UnixStream);

impl Channel {
    fn send(&mut self, reply: &Reply) -> Result<(), String> {
        FRAMING
            .write_value(&mut self.0, reply)
            .map_err(|error| format!("the host went away: {error}"))
    }

    fn request(&mut self) -> Result<Request, String> {
        FRAMING
            .read_value(&mut self.0)
            .map_err(|error| format!("the host's request did not decode: {error}"))?
            .ok_or_else(|| "the host sent no request".to_owned())
    }
}

fn serve(channel: &mut Channel, job: &Path) -> Result<Reply, String> {
    // The file the host opened for the render: written through, never
    // opened here.
    let output = std::io::stdout()
        .as_fd()
        .try_clone_to_owned()
        .map(std::fs::File::from)
        .map_err(|error| format!("the output file is not stdout: {error}"))?;
    let request = channel.request()?;
    let job = job
        .canonicalize()
        .map_err(|error| format!("the job directory is unreadable: {error}"))?;
    let plugins = prepare(&request)?;
    confine::limit_resources(MAX_OPEN_FILES).map_err(|error| error.to_string())?;
    confine::limit_address_space(MAX_ADDRESS_SPACE).map_err(|error| error.to_string())?;
    confine::confine(&policy(&job, &plugins))
        .map_err(|error| format!("the render helper could not confine itself: {error}"))?;
    channel.send(&Reply::Ready {
        version: PROTOCOL_VERSION,
    })?;
    video_editor::media::set_content_root(Some(job.join(SOURCES)));
    let project = load(&job.join(SNAPSHOT))?;
    match request {
        Request::Video { settings } => video(channel, &project, settings, &output),
        Request::Frame { frame } => {
            let png = agent_render::render_still(&project, &frame)?;
            let mut out = &output;
            out.write_all(&png)
                .and_then(|()| out.flush())
                .map_err(|error| format!("the still could not be written: {error}"))?;
            Ok(Reply::Frame)
        }
    }
}

/// Render the video into `output`, reporting progress, then zero the
/// container's clock times so the bytes depend on the timeline alone.
fn video(
    channel: &Channel,
    project: &Project,
    settings: AgentExportSettings,
    output: &std::fs::File,
) -> Result<Reply, String> {
    let socket = channel
        .0
        .try_clone()
        .map_err(|error| format!("the socket could not be shared: {error}"))?;
    let reporter = Mutex::new((socket, Instant::now()));
    let report = move |done: u64, total: u64| {
        let Ok(mut held) = reporter.lock() else {
            return;
        };
        let (socket, last) = &mut *held;
        if done < total && last.elapsed() < PROGRESS_EVERY {
            return;
        }
        *last = Instant::now();
        let _ = FRAMING.write_value(socket, &Reply::Progress { done, total });
    };
    let sink = format!("fdsink fd={}", output.as_raw_fd());
    let rendered = agent_render::render_video(project, &settings, &sink, &Handed(output), report)?;
    zero_times(output)?;
    Ok(Reply::Video(rendered))
}

/// The file the host handed the helper, as the editor's encode sees it.
struct Handed<'a>(&'a std::fs::File);

impl video_editor::export::HandedOutput for Handed<'_> {
    fn written(&self) -> Option<u64> {
        self.0.metadata().ok().map(|metadata| metadata.len())
    }

    fn discard(&self) {
        let _ = self.0.set_len(0);
    }
}

/// Rewrite `output` with its MP4 clock times zeroed.
fn zero_times(output: &std::fs::File) -> Result<(), String> {
    let failed = |error: std::io::Error| format!("the render could not be finished: {error}");
    let mut file = output;
    let mut bytes = Vec::new();
    file.seek(SeekFrom::Start(0)).map_err(failed)?;
    file.read_to_end(&mut bytes).map_err(failed)?;
    agent_render::zero_mp4_times(&mut bytes);
    file.seek(SeekFrom::Start(0)).map_err(failed)?;
    file.write_all(&bytes).map_err(failed)?;
    file.flush().map_err(failed)
}

fn load(snapshot: &Path) -> Result<Project, String> {
    let bytes = std::fs::read(snapshot)
        .map_err(|error| format!("the timeline could not be read: {error}"))?;
    agent_render::read_snapshot(&bytes)
}

/// Everything a render loads that a confined helper could no longer:
/// `GStreamer` and its plugins, the GPU device, its pipelines and the
/// fonts a title is set in. Returns the plugin files' directories.
fn prepare(request: &Request) -> Result<Vec<PathBuf>, String> {
    gst::init().map_err(|error| format!("GStreamer did not start: {error}"))?;
    let registry = gst::Registry::get();
    let mut dirs: Vec<PathBuf> = Vec::new();
    for name in PLUGINS {
        let Some(plugin) = registry.find_plugin(name) else {
            continue;
        };
        let Ok(plugin) = plugin.load() else {
            tracing::warn!(plugin = name, "render helper: a plugin did not load");
            continue;
        };
        if let Some(dir) = plugin.filename().as_deref().and_then(Path::parent) {
            let dir = dir.to_path_buf();
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    let (width, height) = match request {
        Request::Video { settings } => (settings.width, settings.height),
        Request::Frame { frame } => (frame.width, frame.height),
    };
    agent_render::warm_up(width, height)?;
    Ok(dirs)
}

/// Read-only: the plugins and the libraries beside them, the plugin
/// directories the host passed (already validated by
/// [`client::plugin_dirs`](crate::client::plugin_dirs)), the registry,
/// the fonts and the job directory; and the socket pairs `GStreamer`'s
/// buses wake through. The GPU device opened before confinement keeps
/// working with no further grant.
fn policy(job: &Path, plugins: &[PathBuf]) -> confine::Policy {
    let mut read_only: Vec<PathBuf> = vec![job.to_path_buf()];
    for dir in plugins {
        read_only.push(dir.clone());
        if let Some(lib) = dir.parent() {
            read_only.push(lib.to_path_buf());
        }
    }
    for key in crate::client::PLUGIN_PATH_VARS {
        if let Some(value) = std::env::var_os(key) {
            read_only.extend(crate::client::plugin_dirs(&value));
        }
    }
    read_only.extend(registry_file());
    read_only.extend(font_dirs());
    read_only.retain(|path| path.exists());
    read_only.sort();
    read_only.dedup();
    confine::Policy {
        read_only,
        socket_pairs: true,
    }
}

/// The registry `GStreamer` read at start-up: the file the host named
/// (only ever a file), else its default cache directory.
fn registry_file() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("GST_REGISTRY_1_0") {
        let path = PathBuf::from(path);
        return (path.is_absolute() && path.is_file()).then_some(path);
    }
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(cache.join("gstreamer-1.0"))
}

fn font_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = [
        "/System/Library/Fonts",
        "/Library/Fonts",
        "/usr/share/fonts",
        "/usr/local/share/fonts",
        "/etc/fonts",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join("Library/Fonts"));
        dirs.push(home.join(".local/share/fonts"));
    }
    dirs
}
