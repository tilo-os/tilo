//! Where a person's presence is and the colour they wear, as plain values: the scope a presence
//! update is keyed by, the part of a view it says they look at, the selection they hold, the
//! twelve presence hues, how available they say they are and the status they show. The
//! collaboration model's wire types carry these, and the toolkit draws other people's cursors,
//! selections and faces from them without linking that model.

mod availability;
mod hue;
mod key;
mod status;

pub use availability::{Availability, IDLE_AFTER};

pub use hue::{
    PEER_PALETTE, PRESENCE_HUES, PRESENCE_LEN, PresenceHue, agent_client, agent_identity,
    identity_color, identity_rgb, member_color, user_color,
};
pub use key::{
    AppKind, DIGEST_KEY_FIELD, PathSegment, PointerSpace, PresenceKey, SelectionRange, ViewRect,
    ViewSubset,
};
pub use status::{
    INFERRED_STATUS_TTL, LiveStatus, MAX_ACTIVITY_BYTES, MAX_STATUS_CHARS, PresenceVisibility,
    StatusOrigin, StatusSharing,
};
