//! The confined process an agent's host renders a timeline in. The record lives
//! in an app worker with no GPU, no media stack and no paths, so its host
//! renders the timeline here instead; what is drawn and encoded is the app's.

/// What the host and the helper say: postcard frames behind a little-endian
/// length prefix over the helper's stdin socket.
pub mod proto {
    use serde::{Deserialize, Serialize};
    use video_editor::agent_render::{AgentExportSettings, AgentFrameSettings, AgentRendered};

    /// Bumped whenever a message changes shape; host and helper are built together,
    /// so a mismatch means a stale binary.
    pub const PROTOCOL_VERSION: u32 = 1;

    /// Small messages only: the document and the footage travel as files in the job
    /// directory.
    pub const FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(64 * 1024);

    pub const SNAPSHOT: &str = "project.automerge";

    pub const SOURCES: &str = "content";

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    pub enum Request {
        /// Render the timeline to MP4.
        Video {
            settings: AgentExportSettings,
        },
        Frame {
            frame: AgentFrameSettings,
        },
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    pub enum Reply {
        Ready { version: u32 },
        Progress { done: u64, total: u64 },
        Video(AgentRendered),
        Frame,
        Failed(String),
    }
}

#[cfg(unix)]
pub mod client;

#[cfg(all(feature = "helper", unix))]
mod helper;
#[cfg(all(feature = "helper", unix))]
pub use helper::run;
