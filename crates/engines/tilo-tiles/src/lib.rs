//! The pictures a window shows from the network, kept on this device.
//!
//! A window draws a map of the world from a tile server's imagery: hundreds of small pictures,
//! fetched as the person pans and zooms, and wanted again the moment they come back. The window
//! asks its host for a picture's bytes by its address (`ui::set_remote_fetcher`), and the host
//! answers here: the bytes kept from an earlier look, else the network's, kept for the next one.
//! What the person saved for offline use lives apart, so clearing it frees exactly that.
//!
//! Nothing here knows what a tile is beyond its address: the same cache serves every picture a
//! window shows by URL, which is what a map's basemap and an app's images both are. An area the
//! person saved is kept whole and listed as an area ([`Area`]), so it can be shown, listed and
//! removed rather than merely cached.

mod area;
pub mod tiles;

pub use area::{Area, Kept};
pub use tiles::Tiles;
