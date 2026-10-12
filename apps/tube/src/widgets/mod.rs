//! App-local widgets: the host that mounts Tube's one persistent video player
//! into each rebuilt watch page.

pub mod player_host;

pub use player_host::{PlayerHost, SharedPlayer};
