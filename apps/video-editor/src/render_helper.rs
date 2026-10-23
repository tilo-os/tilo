//! `tilo-video-render`: the confined process an agent's host renders a
//! timeline in.
//!
//! An agent's record lives in an app worker with no GPU, no media stack and
//! no paths, so its host renders the record's timeline here instead
//! ([`crate::agent_render`]). The host lays out a private job directory —
//! the record's document ([`proto::SNAPSHOT`]) and every source it places,
//! named by content hash under [`proto::SOURCES`] — opens the output file
//! itself, and starts the helper with one end of a socket as its stdin, the
//! output file as its stdout and the job directory as its argument.
//!
//! Before it reads a byte of the document or the footage the helper:
//!
//! 1. arranges to die with the host and closes every inherited descriptor
//!    but stdio (the socket and the output file among them);
//! 2. reads the host's request (a size, a rate or a time: nothing the room
//!    wrote);
//! 3. starts `GStreamer` on its own registry, loads every plugin a render
//!    can need, opens the GPU and draws one frame at the requested size, so
//!    no code or shader is loaded later;
//! 4. caps its descriptors and (on Linux) its address space, and confines
//!    itself through `tilo-sandbox`: read-only access to the plugins (the
//!    validated `GST_PLUGIN_PATH` directories among them), the registry,
//!    the fonts and the job directory, connected socket pairs (`GStreamer`'s
//!    buses wake through them), and nothing else — no writes, no network,
//!    no `exec`.
//!
//! Then it says it is ready, renders into the file it was handed while
//! reporting progress, and exits. A helper that cannot confine itself
//! renders nothing.

/// What the host and the helper say to each other: postcard frames behind a
/// little-endian length prefix over the helper's stdin socket.
#[cfg(unix)]
pub mod proto {
    use serde::{Deserialize, Serialize};

    use crate::agent_render::{AgentExportSettings, AgentFrameSettings, AgentRendered};

    /// Bumped whenever a message changes shape: the host and the helper are
    /// built together, so a mismatch means a stale binary.
    pub const PROTOCOL_VERSION: u32 = 1;

    /// Frames in either direction: small messages only; the document and the
    /// footage travel as files in the job directory.
    pub const FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(64 * 1024);

    /// The record's document in the job directory, as Automerge saves it.
    pub const SNAPSHOT: &str = "project.automerge";

    /// The directory in the job directory holding each placed source as a
    /// file named by its content hash.
    pub const SOURCES: &str = "content";

    /// The host's one request.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    pub enum Request {
        /// Render the timeline to MP4.
        Video { settings: AgentExportSettings },
        /// Draw one still as PNG.
        Frame { frame: AgentFrameSettings },
    }

    /// What the helper answers.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    pub enum Reply {
        /// Confined and prepared; rendering begins.
        Ready { version: u32 },
        /// Frames written so far, of how many.
        Progress { done: u64, total: u64 },
        /// The video is complete in the output file.
        Video(AgentRendered),
        /// The still is complete in the output file.
        Frame,
        /// Why nothing was rendered.
        Failed(String),
    }
}

#[cfg(unix)]
pub mod client;

#[cfg(all(feature = "gst", feature = "decode", unix))]
pub use helper::run;

#[cfg(all(feature = "gst", feature = "decode", unix))]
mod helper {
    use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
    use std::os::fd::{AsFd as _, AsRawFd as _};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use gstreamer as gst;
    use tilo_sandbox::confine;
    use ui::tilo_resource::Resource as _;

    use super::proto::{FRAMING, PROTOCOL_VERSION, Reply, Request, SNAPSHOT, SOURCES};
    use crate::agent_render::{AgentExportSettings, AgentFrameSettings, AgentRendered};
    use crate::model::Project;

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

    /// How long a still waits for every source's real picture.
    const STILL_PATIENCE: Duration = Duration::from_secs(30);

    /// Entry point of `tilo-video-render`. Returns the exit code.
    #[must_use]
    pub fn run() -> u8 {
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
        crate::media::set_content_root(Some(job.join(SOURCES)));
        let project = load(&job.join(SNAPSHOT))?;
        match request {
            Request::Video { settings } => video(channel, &project, settings, &output),
            Request::Frame { frame } => {
                let png = still(&project, &frame)?;
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
        if !media::encode::registry_has(crate::agent_render::AGENT_VIDEO_ENCODER) {
            return Err(crate::agent_render::AGENT_ENCODER_MISSING.to_owned());
        }
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
        let with_audio = crate::audio::has_audio(project);
        let sink = format!("fdsink fd={}", output.as_raw_fd());
        let encode = crate::export::Encode {
            width: settings.width,
            height: settings.height,
            fps: f64::from(settings.fps),
            desc: crate::agent_render::pipeline_desc(&settings, &sink, with_audio),
            output: crate::export::Output::File(output),
        };
        let frames = crate::export::encode_to(project, &encode, Arc::default(), report)?;
        zero_times(output)?;
        Ok(Reply::Video(AgentRendered {
            frames,
            duration: frames as f64 / f64::from(settings.fps),
        }))
    }

    /// Rewrite `output` with its MP4 clock times zeroed.
    fn zero_times(output: &std::fs::File) -> Result<(), String> {
        let failed = |error: std::io::Error| format!("the render could not be finished: {error}");
        let mut file = output;
        let mut bytes = Vec::new();
        file.seek(SeekFrom::Start(0)).map_err(failed)?;
        file.read_to_end(&mut bytes).map_err(failed)?;
        crate::agent_render::zero_mp4_times(&mut bytes);
        file.seek(SeekFrom::Start(0)).map_err(failed)?;
        file.write_all(&bytes).map_err(failed)?;
        file.flush().map_err(failed)
    }

    /// Draw the timeline at `frame.at` as PNG, waiting for every source's
    /// real picture rather than a stand-in.
    fn still(project: &Project, frame: &AgentFrameSettings) -> Result<Vec<u8>, String> {
        let missing = crate::export::unreadable_sources(project);
        if let Some((name, _)) = missing.first() {
            return Err(format!("{name} cannot be read"));
        }
        let mut compositor = crate::gpu::HeadlessCompositor::new()
            .map_err(|error| format!("the render helper has no GPU: {error}"))?;
        let mut sources = crate::gpu::MediaFrameSource::new(project, crate::gpu::Fetch::Exact)
            .with_timeout(Duration::from_secs(2))
            .strict();
        let asked = Instant::now();
        let rgba = loop {
            let rgba = compositor.frame(project, frame.at, frame.width, frame.height, &mut sources);
            if sources.missing_pictures().is_empty() {
                break rgba;
            }
            if asked.elapsed() >= STILL_PATIENCE {
                return Err("the footage under that time gave no picture".to_owned());
            }
        };
        ::thumbnail::encode_png(&::thumbnail::RgbaThumb {
            rgba,
            width: frame.width,
            height: frame.height,
        })
        .ok_or_else(|| "the still could not be encoded as PNG".to_owned())
    }

    fn load(snapshot: &Path) -> Result<Project, String> {
        let bytes = std::fs::read(snapshot)
            .map_err(|error| format!("the timeline could not be read: {error}"))?;
        let doc = automerge::AutoCommit::load(&bytes)
            .map_err(|error| format!("the timeline does not load: {error}"))?;
        Project::read(&doc).map_err(|error| format!("the timeline does not read: {error}"))
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
        warm_up(width, height)?;
        Ok(dirs)
    }

    /// Draw a title at the render's size, so the GPU device, every pipeline
    /// the compositor and the title raster build, and the fonts, are made
    /// before confinement.
    fn warm_up(width: u32, height: u32) -> Result<(), String> {
        let mut compositor = crate::gpu::HeadlessCompositor::new()
            .map_err(|error| format!("the render helper has no GPU: {error}"))?;
        let mut project = Project::new("warm-up");
        project.set_frame_size(width, height);
        let track = crate::model::Track {
            id: uuid::Uuid::from_u128(1),
            kind: crate::model::TrackKind::Video,
            ..Default::default()
        };
        let title = crate::model::MediaItem {
            id: uuid::Uuid::from_u128(2),
            kind: crate::model::MediaKind::Text,
            duration: 1.0,
            text: Some(crate::model::TextSource::new("Warm-up")),
            ..Default::default()
        };
        project.clips.push(crate::model::Clip {
            id: uuid::Uuid::from_u128(3),
            track: track.id,
            media: title.id,
            out_point: 1.0,
            ..Default::default()
        });
        project.tracks.push(track);
        project.media.push(title);
        let _ = compositor.frame(&project, 0.5, width, height, &mut crate::gpu::NoSources);
        Ok(())
    }

    /// Read-only: the plugins and the libraries beside them, the plugin
    /// directories the host passed (already validated by
    /// [`client::plugin_dirs`](super::client::plugin_dirs)), the registry,
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
        for key in super::client::PLUGIN_PATH_VARS {
            if let Some(value) = std::env::var_os(key) {
                read_only.extend(super::client::plugin_dirs(&value));
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
}
