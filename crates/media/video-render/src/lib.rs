//! `tilo-video-render`: the confined process an agent's host renders a
//! timeline in.
//!
//! An agent's record lives in an app worker with no GPU, no media stack and
//! no paths, so its host renders the record's timeline here instead
//! (`video_editor::agent_render`, which also holds what is drawn and
//! encoded: this crate is the process, its confinement and its channel to
//! the host, platform code the portable editor does not carry). The host's
//! side is [`client`]; the helper's is [`run`], under the `helper` feature
//! (the media stack), which the `tilo-video-render` binary needs. The host lays out a private job directory —
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
pub mod proto {
    use serde::{Deserialize, Serialize};
    use video_editor::agent_render::{AgentExportSettings, AgentFrameSettings, AgentRendered};

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

#[cfg(all(feature = "helper", unix))]
mod helper;
#[cfg(all(feature = "helper", unix))]
pub use helper::run;
