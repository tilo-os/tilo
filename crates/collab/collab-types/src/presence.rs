//! Presence as apps and the session exchange it, without the toolkit that
//! draws it: the colour each person wears, the scope each presence update is
//! keyed by, what it says, and who is in a room. The toolkit re-exports these
//! (`ui::tokens`, `ui::presence`, `ui::permissions`, `ui::view`,
//! `ui::collaboration`), so apps name them where they always have, and the
//! session's services use them without linking a renderer.

mod hue;
mod key;
mod payload;

pub use hue::{
    PEER_PALETTE, PRESENCE_HUES, PRESENCE_LEN, PresenceHue, assign_peer_colors, identity_rgb,
    user_color,
};
pub use key::{
    AppKind, DIGEST_KEY_FIELD, PathSegment, PointerSpace, PresenceKey, SelectionRange, UserId,
    ViewRect, ViewSubset,
};
pub use payload::{Availability, Collaborator, IDLE_AFTER, PresencePayload};
