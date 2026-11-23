//! Presence as apps and the session exchange it, without the toolkit that
//! draws it: the colour each person wears, the scope each presence update is
//! keyed by, what it says, the call it carries, and who is in a workspace. The
//! toolkit re-exports these (`ui::tokens`, `ui::presence`, `ui::permissions`, `ui::view`,
//! `ui::collaboration`), so apps name them where they always have, and the
//! session's services use them without linking a renderer.

mod bridged;
mod call;
mod hue;
mod key;
mod payload;
mod said;
mod shell;
mod shown;
mod status;

pub use crate::member::{Member, Person, joined_at_from_packed};
pub use bridged::{Bridged, MAX_ACCOUNT_CHARS, MAX_BRIDGED_NAME_CHARS};
pub use call::CallPresence;
pub use hue::{
    PEER_PALETTE, PRESENCE_HUES, PRESENCE_LEN, PresenceHue, agent_client, agent_identity,
    identity_color, identity_rgb, member_color, user_color,
};
pub use key::{
    AppKind, DIGEST_KEY_FIELD, PathSegment, PointerSpace, PresenceKey, SelectionRange, ViewRect,
    ViewSubset,
};
pub use payload::{Availability, IDLE_AFTER, PresencePayload};
pub use said::{DESKTOP_APP, Presence, stand_in};
pub use tilo_action_policy::PersonId;

/// What a member says in one presence frame: the semantics every
/// publisher and reader of a workspace's presence shares. One enum, so a
/// frame is read the same wherever it is taken from.
pub use crate::peer::PeerPresencePayload as Payload;
pub use shell::{
    MAX_ACTIVITY_BYTES, ShellChat, ShellPresence, ShellReaction, ShellSelection, ShellWindow,
};
pub use shown::{Shown, ShownFocus, ShownLayout, ShownPoint, ShownWave, ShownWindow};
pub use status::{
    INFERRED_STATUS_TTL, LiveStatus, MAX_STATUS_CHARS, PresenceVisibility, StatusOrigin,
    StatusSharing,
};
