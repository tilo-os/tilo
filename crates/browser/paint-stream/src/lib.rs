//! A web page as a layered display list.
//!
//! The engine runs where the session lives (the host): it owns the JS heap,
//! cookies and storage, and none of that leaves it. What every client receives
//! is a [`Frame`] — what the page *looks like*, as paint-ordered items — and
//! afterwards only [`Patch`]es. Each client draws the frame with its own Vello
//! and text stack, exactly like every other Tilo surface, so a WASM client
//! renders a page the same way it renders Notes.
//!
//! Layers keep patches small. A scroll container, a fixed or a sticky box is a
//! [`Layer`] with its own origin and scroll offset, and its items are stored
//! relative to it, so scrolling the page or a sidebar, or a sticky header
//! sliding, changes one layer header instead of every item beneath it. The
//! client scrolls the root layer locally and tells the host afterwards.

mod model;
mod patch;
pub mod snapshot;
pub mod svg;
pub mod wire;

pub use model::{
    Anchor, BlobRef, Color, FontSpec, Frame, Item, ItemKey, Layer, LayerId, Point, ROOT, Rect,
    Shape,
};
pub use patch::{LayerChange, Patch, PatchError};

/// Wire encoding shared by host and client.
pub fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_stdvec(value)
}

/// Decode a value produced by [`encode`].
pub fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, postcard::Error> {
    postcard::from_bytes(bytes)
}
