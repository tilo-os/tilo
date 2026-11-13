//! `look`: what a window this session does not run shows, pictured for
//! whoever reads the desk — an agent, a test, a script.
//!
//! The desk publishes the two read actions of such a window
//! (`tilo_desktop_surface::window`); the picture itself is made here, where
//! the compositor is, by the same read-back window sharing captures with,
//! and kept in the workspace's content store under the address every caller
//! of the action protocol reads content by. The surface that names the
//! window never draws it: an invocation from the session's own loop passes
//! this in ([`tilo_session_core::door::Look`]), and one from anywhere else finds
//! none and is refused.

use tilo_compositor::Compositor;
use tilo_compositor::capture::Target;
use tilo_session_core::door::{Look, Shot};

use crate::content::Plane;

/// The session's pictures: `compositor`'s read-back of a window, kept as
/// content of the workspace the window stands in.
pub(crate) fn look<'a>(compositor: &'a mut Compositor, content: Option<&'a Plane>) -> Look<'a> {
    Look::over(move |window: u64, workspace: &str| {
        let identifier = compositor
            .capture_identifier(window)
            .ok_or("the window is not on the desk (any more)")?;
        let shot = compositor
            .read_back(&Target::window(identifier))
            .map_err(|error| error.to_string())?;
        let png = ui::png::encode_rgba8(shot.width, shot.height, &shot.rgba)
            .map_err(|error| error.to_string())?;
        let content = content.ok_or("this session keeps no content")?;
        let hash = content.keep(workspace, &png)?;
        let address =
            tilo_actions::content_resource(&hash).ok_or("the picture has no content address")?;
        Ok(Shot {
            content: address,
            width: shot.width,
            height: shot.height,
        })
    })
}
