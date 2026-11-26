//! Presence as apps and the session exchange it, without the toolkit that
//! draws it: the colour each person wears, the scope each presence update is
//! keyed by, what it says, the call it carries, and who is in a workspace. The
//! toolkit re-exports these (`ui::tokens`, `ui::presence`, `ui::permissions`, `ui::view`,
//! `ui::collaboration`), so apps name them where they always have, and the
//! session's services use them without linking a renderer.

mod bridged;
mod call;
mod payload;
mod said;
mod shell;
mod shown;

pub use crate::member::{Member, Person, joined_at_from_packed};
pub use bridged::{Bridged, MAX_ACCOUNT_CHARS, MAX_BRIDGED_NAME_CHARS};
pub use call::CallPresence;
pub use payload::PresencePayload;
pub use said::{DESKTOP_APP, Presence, stand_in};
pub use tilo_action_policy::{PersonId, identity_color};
pub use tilo_presence::{
    AppKind, Availability, DIGEST_KEY_FIELD, IDLE_AFTER, INFERRED_STATUS_TTL, LiveStatus,
    MAX_STATUS_CHARS, PEER_PALETTE, PRESENCE_HUES, PRESENCE_LEN, PathSegment, PointerSpace,
    PresenceHue, PresenceKey, PresenceVisibility, SelectionRange, StatusOrigin, StatusSharing,
    ViewRect, ViewSubset, agent_client, agent_identity, identity_rgb, member_color, user_color,
};

/// What a member says in one presence frame: the semantics every
/// publisher and reader of a workspace's presence shares. One enum, so a
/// frame is read the same wherever it is taken from.
pub use crate::peer::PeerPresencePayload as Payload;
pub use shell::{
    MAX_ACTIVITY_BYTES, ShellChat, ShellPresence, ShellReaction, ShellSelection, ShellWindow,
};
pub use shown::{Shown, ShownFocus, ShownLayout, ShownPoint, ShownWave, ShownWindow};
