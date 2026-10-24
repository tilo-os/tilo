//! Presence as apps and the session exchange it, without the toolkit that
//! draws it: the colour each person wears. The toolkit re-exports these
//! (`ui::tokens`, `ui::presence`), so apps name them where they always have,
//! and the session's services use them without linking a renderer.

mod hue;

pub use hue::{
    PEER_PALETTE, PRESENCE_HUES, PRESENCE_LEN, PresenceHue, assign_peer_colors, identity_rgb,
    user_color,
};
