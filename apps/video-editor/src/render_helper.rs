//! `tilo-video-render`: the confined process an agent's host renders a
//! timeline in.
//!
//! An agent's record lives in an app worker that has no GPU, no media stack
//! and no paths (`tilo-agent-participant`'s worker), so its host renders
//! the record's timeline in this helper instead. The host prepares a job
//! directory — the record's document ([`proto::SNAPSHOT`]) and every source
//! it places, named by content hash under [`proto::SOURCES`] — opens an
//! output file the helper could not open itself, and starts the helper with
//! one end of a socket as its stdin and the job directory as its argument.
//!
//! Before it reads a byte of the document or the footage the helper:
//!
//! 1. arranges to die with the host and closes every inherited descriptor
//!    but the socket and stdio;
//! 2. reads the host's request (a size, a rate or a time: nothing the
//!    room wrote);
//! 3. starts `GStreamer` without forking a registry scanner, loads every
//!    plugin a render can need, opens the GPU and draws one frame at the
//!    requested size, so no code or shader is loaded later;
//! 4. caps its descriptors and (on Linux) its address space, and confines
//!    itself through `tilo-sandbox`: read-only access to the plugins, the
//!    registry, the fonts and the job directory, connected socket pairs
//!    (`GStreamer`'s buses wake through them), and nothing else — no
//!    writes, no network, no `exec`, no further GPU or driver service.
//!
//! Then it says it is ready, receives the output file as a descriptor,
//! renders into it while reporting progress, and exits. A helper that
//! cannot confine itself renders nothing.

/// What the host and the helper say to each other: postcard frames behind
/// a little-endian length prefix (`wire-frame`), and the output file as one
/// `SCM_RIGHTS` descriptor after [`proto::Reply::Ready`].
pub mod proto {
    use serde::{Deserialize, Serialize};

    use crate::export::{AgentExportSettings, AgentFrameSettings, AgentRendered};

    /// Bumped whenever a message changes shape: the host and the helper are
    /// built together, so a mismatch means a stale binary.
    pub const PROTOCOL_VERSION: u32 = 1;

    /// Frames in either direction: small messages only; the document and
    /// the footage travel as files in the job directory.
    pub const FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(64 * 1024);

    /// The record's document in the job directory, as Automerge saves it.
    pub const SNAPSHOT: &str = "project.automerge";

    /// The directory in the job directory holding each placed source as a
    /// file named by its content hash.
    pub const SOURCES: &str = "content";

    /// The one byte the output descriptor travels with.
    pub const FD_MARKER: &[u8] = b"o";

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
        /// Confined and prepared; send the output descriptor.
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
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use gstreamer as gst;
    use tilo_sandbox::confine;

    use super::proto::{FD_MARKER, FRAMING, PROTOCOL_VERSION, Reply, Request, SNAPSHOT, SOURCES};
    use crate::export::{AgentExportSettings, Encoding, RenderState, Sink};
    use crate::model::Project;

    /// The plugins a render can reach: the decode graph `tilo-media`
    /// builds (`playbin`, demuxers, parsers, software decoders, converters),
    /// the stills it imports, and the encode graph. Loaded before
    /// confinement, since a confined helper maps no new code; one this host
    /// lacks is simply absent.
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
        "videoconvert",
        "videoscale",
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

    /// Descriptors the helper may hold: the socket, stdio, the output file,
    /// and what the decoders, the GPU and `GStreamer`'s pollers open.
    const MAX_OPEN_FILES: u64 = 256;

    /// Address space a helper may map on Linux.
    const MAX_ADDRESS_SPACE: u64 = 4 << 30;

    /// Entry point of `tilo-video-render`. Returns the exit code.
    #[must_use]
    pub fn run() -> u8 {
        // SAFETY: descriptor 0 is the socket the host handed over as stdin;
        // this process takes sole ownership of it and nothing else reads
        // stdin.
        let socket = UnixStream::from(unsafe { OwnedFd::from_raw_fd(0) });
        let mut channel = Channel(socket);
        let job = std::env::args_os().nth(1).map(PathBuf::from);
        let outcome = job
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

        fn output(&self) -> Result<std::fs::File, String> {
            let (marker, fd) = runtime_portal::fd_transport::recv_with_owned_fd(self.0.as_raw_fd())
                .map_err(|error| format!("the output file did not arrive: {error}"))?;
            match fd {
                Some(fd) if marker == FD_MARKER => Ok(std::fs::File::from(fd)),
                _ => Err("the host sent no output file".to_owned()),
            }
        }
    }

    fn serve(channel: &mut Channel, job: &Path) -> Result<Reply, String> {
        confine::detach().map_err(|error| error.to_string())?;
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
        let output = channel.output()?;
        crate::media::set_content_root(job.join(SOURCES));
        let project = load(&job.join(SNAPSHOT))?;
        match request {
            Request::Video { settings } => video(channel, &project, settings, &output),
            Request::Frame { frame } => {
                let png = crate::export::render_frame_png(&project, &frame)?;
                let mut out = &output;
                std::io::Write::write_all(&mut out, &png)
                    .map_err(|error| format!("the still could not be written: {error}"))?;
                Ok(Reply::Frame)
            }
        }
    }

    /// Render the video while a thread reports progress every half second.
    fn video(
        channel: &Channel,
        project: &Project,
        settings: AgentExportSettings,
        output: &std::fs::File,
    ) -> Result<Reply, String> {
        let state = Arc::new(RenderState::default());
        let reporter = {
            let state = Arc::clone(&state);
            let mut socket = channel
                .0
                .try_clone()
                .map_err(|error| format!("the socket could not be shared: {error}"))?;
            std::thread::Builder::new()
                .name("render-progress".into())
                .spawn(move || {
                    while !state.cancel.load(Ordering::Relaxed) {
                        let progress = Reply::Progress {
                            done: state.done.load(Ordering::Relaxed),
                            total: state.total.load(Ordering::Relaxed),
                        };
                        if FRAMING.write_value(&mut socket, &progress).is_err() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(500));
                    }
                })
                .map_err(|error| format!("the progress thread did not start: {error}"))?
        };
        let rendered = crate::export::run_to_sink(
            project,
            &Encoding::Agent(settings),
            &Sink::File(output),
            &state,
        );
        // The reporter stops before the answer goes out, so the answer is
        // the socket's last message.
        state.cancel.store(true, Ordering::Relaxed);
        let _ = reporter.join();
        rendered.map(Reply::Video)
    }

    fn load(snapshot: &Path) -> Result<Project, String> {
        let bytes = std::fs::read(snapshot)
            .map_err(|error| format!("the timeline could not be read: {error}"))?;
        let doc = automerge::AutoCommit::load(&bytes)
            .map_err(|error| format!("the timeline does not load: {error}"))?;
        Ok(crate::doc::VideoDoc::load_project(&doc))
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
        project.width = width;
        project.height = height;
        let track = crate::model::Track {
            id: uuid::Uuid::new_v4(),
            kind: crate::model::TrackKind::Video,
            ..Default::default()
        };
        let title = crate::model::MediaItem {
            id: uuid::Uuid::new_v4(),
            kind: crate::model::MediaKind::Text,
            duration: 1.0,
            hydration: crate::media::Hydration::Local,
            text: Some(crate::model::TextSource::new("Warm-up")),
            ..Default::default()
        };
        project.clips.push(crate::model::Clip {
            id: uuid::Uuid::new_v4(),
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

    /// Read-only: the plugins and the libraries beside them, the registry,
    /// the fonts, the job directory; and the socket pairs `GStreamer`'s
    /// buses wake through. The GPU device, opened before confinement, keeps
    /// working with no further grant: on macOS the Metal device and the
    /// pipelines the warm-up compiled need no driver service afterwards.
    fn policy(job: &Path, plugins: &[PathBuf]) -> confine::Policy {
        let mut read_only: Vec<PathBuf> = vec![job.to_path_buf()];
        for dir in plugins {
            read_only.push(dir.clone());
            if let Some(lib) = dir.parent() {
                read_only.push(lib.to_path_buf());
            }
        }
        read_only.extend(registry_file());
        read_only.extend(font_dirs());
        read_only.retain(|path| path.exists());
        read_only.dedup();
        confine::Policy {
            read_only,
            socket_pairs: true,
        }
    }

    /// The registry cache `GStreamer` read at start-up: the file it was
    /// told, else its default cache directory.
    fn registry_file() -> Option<PathBuf> {
        if let Some(path) =
            std::env::var_os("GST_REGISTRY_1_0").or_else(|| std::env::var_os("GST_REGISTRY"))
        {
            return Some(PathBuf::from(path));
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
