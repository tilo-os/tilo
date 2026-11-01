//! What a shared window's stream is told about its window.
//!
//! A window shared with the workspace's other devices is captured into a
//! share that lives above this crate, with its encoder and its viewers. The
//! compositor is where a client's commit arrives, so a share registers itself
//! here while its window is shared
//! ([`crate::Compositor::share_window`]) and takes itself back when it ends
//! ([`crate::Compositor::unshare_window`]).
//!
//! The commit that carries a `wp_color_management_surface_v1` image
//! description then tells the stream what the window's content is in, and a
//! commit after the client unset it tells the stream the configured
//! treatment applies again. Nothing polls: the colour travels on the commit
//! that changed it, and a window nobody shares costs one map lookup per
//! commit.

use std::sync::Arc;

use neighbour_wire::colorimetry::Colorimetry;
use neighbour_wire::hdr_metadata::Hdr10Metadata;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

use crate::state::State;

/// A shared window's stream, as the compositor tells it what the window's
/// content is in.
pub trait ShareStream: Send + Sync {
    /// The window's client attached an image description: the frames that
    /// follow are in `colorimetry`, with the mastering volume `hdr` when it
    /// declared one.
    fn set_content_color(&self, colorimetry: Colorimetry, hdr: Option<Hdr10Metadata>);

    /// The window's client attached none: the colour the share was set up
    /// with applies again.
    fn clear_content_color(&self);
}

impl State {
    /// Share `window` through `stream`, from now on: it is told what the
    /// window's content is in now, and on every commit that changes it.
    ///
    /// The client usually said what its content is in before anyone shared
    /// the window, and the stream may announce itself before that window
    /// commits again — so what it already says is handed over here, not
    /// waited for.
    pub(crate) fn share_window(&mut self, window: u64, stream: Arc<dyn ShareStream>) {
        match self.window_color(window) {
            Some((colorimetry, hdr)) => stream.set_content_color(colorimetry, hdr),
            None => stream.clear_content_color(),
        }
        self.share_streams.insert(window, stream);
    }

    /// The share of `window` ended: its stream hears nothing more from here.
    pub(crate) fn unshare_window(&mut self, window: u64) {
        self.share_streams.remove(&window);
    }

    /// Tell the stream sharing `surface`'s window what the surface's client
    /// says its content is in; nothing at all when that window is not shared
    /// or `surface` is not a window's own surface (a popup's, a subsurface's).
    pub(crate) fn tell_share_color(&self, surface: &WlSurface) {
        let Some(stream) =
            crate::state::window_id_of(surface).and_then(|window| self.share_streams.get(&window))
        else {
            return;
        };
        match crate::color_management::colorimetry_of(surface) {
            Some((colorimetry, hdr)) => stream.set_content_color(colorimetry, hdr),
            None => stream.clear_content_color(),
        }
    }
}

impl crate::Compositor {
    /// Share `window` with the workspace's other devices through `stream`:
    /// from now on the stream is told what the window's content is in, and
    /// tags its frames with it. Sharing the same window again replaces the
    /// stream of the earlier share.
    pub fn share_window(&mut self, window: u64, stream: Arc<dyn ShareStream>) {
        self.data.state.share_window(window, stream);
    }

    /// The share of `window` ended: its stream hears nothing more from here.
    pub fn unshare_window(&mut self, window: u64) {
        self.data.state.unshare_window(window);
    }
}
