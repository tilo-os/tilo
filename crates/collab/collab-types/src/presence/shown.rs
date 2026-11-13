//! One member of a workspace, as a surface reads them: the board's merged word,
//! flattened into what a roster lists, a pointer draws and a caret needs. A page
//! and a native desktop read the same record, so both show the same people the
//! same way.

use serde::{Deserialize, Serialize};

use super::{ShellChat, ShellReaction, ShellSelection};

/// The window a member works in: its app, and the short digest of the record
/// it shows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShownFocus {
    /// The app, as its stable id.
    pub app: String,
    /// The record, as its 16-hex short digest: what a reader matches against
    /// what it holds itself.
    pub record: String,
}

/// A member's latest wave at someone here.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShownWave {
    /// A new count is a new wave; the same one restated is the same wave.
    pub seq: u32,
    /// The member it is for, by their publisher.
    pub target: String,
}

/// Where a member's pointer is over a tile, and how long ago they said it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShownPoint {
    /// The tab, as a decimal string: its id can pass 2^53.
    pub tab: String,
    /// The point in that tile's logical pixels.
    pub at: [f64; 2],
    /// How long ago its publisher said it, in milliseconds.
    pub age_ms: f64,
}

/// Where a member's windows are on their screen: a mini view of it is drawn
/// from this and from nothing else.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShownLayout {
    /// Their screen, in logical pixels.
    pub size: [u16; 2],
    /// Its windows, in the order they paint.
    pub windows: Vec<ShownWindow>,
}

/// One window of a [`ShownLayout`]: the app and record digest it shows, where
/// it stands and whether the person works in it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShownWindow {
    /// The app, as its stable id.
    pub app: String,
    /// The record, as its 16-hex short digest.
    pub record: String,
    /// Its edges across the screen, in 1/10000 of [`ShownLayout::size`].
    pub rect: [u16; 4],
    /// Whether the person works in it.
    pub focused: bool,
}

/// One member of a workspace as a surface reads them: who they are, what they
/// say they are doing, where they work and what they say beside their
/// pointer.
///
/// Every part is optional because every member says only what its host says:
/// a native desktop has no screen layout to share, a page has no certificate
/// label until one arrives.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shown {
    /// Their device's publisher, hex.
    pub publisher: String,
    /// When this stay here began: it moves each time the device comes back
    /// after leaving.
    pub epoch: u64,
    /// Their presence says they are an agent.
    pub agent: bool,
    /// The workspace's host, hex, when it speaks for this member: an
    /// agent's or a key's run it executes, which says nothing itself. A
    /// surface shows them as the agent, said by the host.
    #[serde(default)]
    pub spoken_by: Option<String>,
    /// The name everyone here calls them.
    pub label: String,
    /// What they are doing: an agent's own line, or a person's status.
    pub activity: Option<String>,
    /// Their device read [`Self::activity`] off the steps they took, rather
    /// than anyone saying it.
    #[serde(default)]
    pub automatic: bool,
    /// The colour their face wears, `#rrggbb`.
    pub colour: String,
    /// The person behind the device (user root, hex), once certified.
    pub user: Option<String>,
    /// When the person last changed their profile, as their device says it (Unix milliseconds,
    /// `0` for unsaid).
    #[serde(default)]
    pub profile_ms: u64,
    /// What their device calls itself, from its certificate.
    pub device: String,
    /// The window they work in and the record it shows.
    pub focus: Option<ShownFocus>,
    /// Where within the workspace they are, as their shell says.
    pub place: Option<String>,
    /// Whose view they follow, by that member's publisher.
    pub following: Option<String>,
    /// Their latest wave at someone here.
    pub wave: Option<ShownWave>,
    /// Where their pointer is over a tile, and how long ago they said it.
    pub pointer: Option<ShownPoint>,
    /// The caret or selection they published, as live anchors (hex).
    pub selection: Option<ShellSelection>,
    /// What they are saying beside their pointer.
    pub chat: Option<ShellChat>,
    /// Their latest reaction.
    pub reaction: Option<ShellReaction>,
    /// Where their windows are on their screen.
    pub layout: Option<ShownLayout>,
    /// How available they said they were, in the wire's own word.
    pub availability: String,
    /// The voice channel they are in, as their presence carries it.
    pub call: Option<String>,
}
