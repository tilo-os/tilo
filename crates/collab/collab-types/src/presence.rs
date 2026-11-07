//! Presence as apps and the session exchange it, without the toolkit that
//! draws it: the colour each person wears, the scope each presence update is
//! keyed by, what it says, the call it carries, and who is in a room. The
//! toolkit re-exports these (`ui::tokens`, `ui::presence`, `ui::permissions`, `ui::view`,
//! `ui::collaboration`), so apps name them where they always have, and the
//! session's services use them without linking a renderer.

mod call;
mod hue;
mod key;
mod payload;
mod said;
mod shell;

pub use call::CallPresence;
pub use hue::{
    PEER_PALETTE, PRESENCE_HUES, PRESENCE_LEN, PresenceHue, assign_peer_colors, identity_rgb,
    user_color,
};
pub use key::{
    AppKind, DIGEST_KEY_FIELD, PathSegment, PointerSpace, PresenceKey, SelectionRange, UserId,
    ViewRect, ViewSubset,
};
pub use payload::{Availability, Collaborator, IDLE_AFTER, PresencePayload};
pub use said::{DESKTOP_APP, Presence};

/// What a participant says in one presence frame: the semantics every
/// publisher and reader of a workspace's presence shares. One enum, so a
/// frame is read the same wherever it is taken from.
pub use crate::peer::PeerPresencePayload as Payload;
pub use shell::{
    MAX_ACTIVITY_BYTES, ShellChat, ShellPresence, ShellReaction, ShellSelection, ShellWindow,
};
