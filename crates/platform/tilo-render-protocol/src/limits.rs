//! What one app may ask of its room's render service.
//!
//! Every number bounds work the service would otherwise do on an app's
//! behalf, for every app in the room: an app over a limit gets that frame
//! dropped and a diagnostic ([`crate::control::ServiceMessage::FrameDropped`]),
//! never a stalled service. The defaults hold every toolkit frame measured
//! (a dense Notes page encodes ~400 KB, ~6 000 paths) with an order of
//! magnitude to spare; the service may lower them for a room under pressure.

use serde::{Deserialize, Serialize};

/// Per-app bounds the service enforces on records and resources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub frame_bytes: u32,
    /// Bytes of the ring an app may map into the service.
    pub ring_bytes: u32,
    /// Paths in one frame, all layers and backdrops together.
    pub paths: u32,
    /// Path segments in one frame, after glyphs are expanded estimates.
    pub segments: u32,
    pub draw_objects: u32,
    /// Glyphs in one frame.
    pub glyphs: u32,
    pub clip_depth: u32,
    /// Layers one surface keeps.
    pub layers: u32,
    /// Surfaces one connection keeps.
    pub surfaces: u32,
    pub surface_edge: u32,
    /// Pixels of all of a connection's surfaces together; their render
    /// targets and buffers are the service's GPU memory spent on the app.
    pub surface_pixels: u64,
    /// Bytes of all images one connection keeps registered. They live in
    /// memfds the app created, so its cgroup pays for them; this bounds what
    /// the service maps and uploads.
    pub image_bytes: u64,
    pub images: u32,
    /// Fonts one connection keeps registered.
    pub fonts: u32,
    /// Bytes of one font an app uploads itself (fonts from the shared view
    /// are mapped from their files and not counted).
    pub font_bytes: u64,
    /// GPU time one frame may take, in microseconds. A frame over it is
    /// presented, then the app's next frames wait out the overrun, so one app
    /// cannot take the room's GPU from the others.
    pub gpu_time_us: u32,
    /// Backdrop regions one frame blurs.
    pub backdrops: u32,
    /// Damage rectangles and scroll copies one frame lists.
    pub rects: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            frame_bytes: 16 << 20,
            ring_bytes: 64 << 20,
            paths: 200_000,
            segments: 4_000_000,
            draw_objects: 200_000,
            glyphs: 200_000,
            clip_depth: 64,
            layers: 256,
            surfaces: 32,
            surface_edge: 16_384,
            surface_pixels: 64 << 20,
            image_bytes: 1 << 30,
            images: 16_384,
            fonts: 1_024,
            font_bytes: 64 << 20,
            gpu_time_us: 50_000,
            backdrops: 4,
            rects: 4_096,
        }
    }
}
