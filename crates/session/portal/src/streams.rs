//! What the session captures for screen sharing: the outputs and windows
//! it can share, and a stream of one of them feeding a `PipeWire` node.
//!
//! The compositor draws captures for trusted clients only (an output by
//! name, a window by its `ext_foreign_toplevel_list_v1` identifier). The
//! session implements [`CaptureStreams`] over it; the portal decides who
//! may see what and never touches the compositor itself.

use serde::{Deserialize, Serialize};

use crate::Pending;

/// `ScreenCast` source types, as `AvailableSourceTypes` counts them.
pub mod source_type {
    pub const MONITOR: u32 = 1;
    pub const WINDOW: u32 = 2;
    pub const VIRTUAL: u32 = 4;
    pub const ALL: u32 = MONITOR | WINDOW | VIRTUAL;
}

/// How a stream shows the pointer, as `AvailableCursorModes` counts them.
pub mod cursor_mode {
    pub const HIDDEN: u32 = 1;
    pub const EMBEDDED: u32 = 2;
    pub const METADATA: u32 = 4;
    pub const ALL: u32 = HIDDEN | EMBEDDED | METADATA;
}

/// What a source is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    /// A display, by its output name.
    Output,
    /// A window, by its foreign-toplevel identifier.
    Window,
}

impl SourceKind {
    /// The `ScreenCast` source type bit it is.
    #[must_use]
    pub const fn source_type(self) -> u32 {
        match self {
            Self::Output => source_type::MONITOR,
            Self::Window => source_type::WINDOW,
        }
    }
}

/// One output or window the session can share.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureSource {
    pub kind: SourceKind,
    /// The output's name or the window's foreign-toplevel identifier.
    pub id: String,
    /// The window's app, when it is a window that named one.
    pub app_id: Option<String>,
    /// The output's description or the window's title: data from the
    /// compositor or the window, never the session's own words.
    pub label: String,
}

impl CaptureSource {
    /// The id it is offered under in a question: `output:<name>` or
    /// `window:<identifier>`.
    #[must_use]
    pub fn choice(&self) -> String {
        let prefix = match self.kind {
            SourceKind::Output => "output",
            SourceKind::Window => "window",
        };
        format!("{prefix}:{}", self.id)
    }
}

/// A stream to start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamRequest {
    pub source: CaptureSource,
    /// One `cursor_mode` bit.
    pub cursor_mode: u32,
    /// The app it is for, which names the stream's node.
    pub app_id: String,
}

/// A stream feeding a `PipeWire` node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stream {
    pub node_id: u32,
    /// Its frames' size, in pixels.
    pub size: (u32, u32),
    /// Where an output sits in the compositor's layout.
    pub position: Option<(i32, i32)>,
}

/// The session's capture of its outputs and windows.
pub trait CaptureStreams: Send + Sync {
    /// The outputs and windows it can share now.
    fn sources(&self) -> Vec<CaptureSource>;

    /// The `cursor_mode` bits its streams can show.
    fn cursor_modes(&self) -> u32;

    /// Start streaming `request`'s source into a new node.
    fn start(&self, request: StreamRequest) -> Pending<Result<Stream, String>>;

    /// Stop the stream of node `node_id`.
    fn stop(&self, node_id: u32);
}

/// A session that shares nothing.
pub struct NoStreams;

impl CaptureStreams for NoStreams {
    fn sources(&self) -> Vec<CaptureSource> {
        Vec::new()
    }

    fn cursor_modes(&self) -> u32 {
        0
    }

    fn start(&self, _request: StreamRequest) -> Pending<Result<Stream, String>> {
        Box::pin(async { Err("this session shares no screen".to_owned()) })
    }

    fn stop(&self, _node_id: u32) {}
}

/// The source types `sources` offers.
#[must_use]
pub fn source_types(sources: &[CaptureSource]) -> u32 {
    sources
        .iter()
        .fold(0, |types, source| types | source.kind.source_type())
}
