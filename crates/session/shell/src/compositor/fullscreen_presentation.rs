//! Where a client in full screen is drawn, frame by frame.
//!
//! A client's full screen is a per-output overlay drawn above everything,
//! and on a display that allows it, scanned out directly. While the shell
//! takes the input over it (the search palette, a sheet), it is instead laid
//! out full screen in the desktop on its own display, beneath the shell's
//! surfaces: the palette shows above it with its scrim, no frame of it goes
//! straight to the display, and the client stays in full screen at the same
//! size. When the shell lets go, the overlay comes back.

use crate::compositor::backend::{Output, OutputId};
use crate::compositor::frame::FrameBuilder;
use crate::compositor::output_globals::OutputGlobals;
use crate::compositor::state::State;

/// Lay the full-screen client out in the desktop beneath the shell while
/// the shell takes the input, else clear that layout so the client is the
/// overlay again ([`client_fullscreen_beneath_shell`]).
pub fn lay_out_beneath_shell(state: &State, frame: &mut FrameBuilder) {
    let beneath = client_fullscreen_beneath_shell(state, frame);
    frame.set_fullscreen(beneath);
}

/// The group of the client in full screen when the shell takes the input
/// over it: it is then drawn in the desktop's full-screen layout beneath the
/// shell's surfaces rather than as the overlay above them. A presenter view
/// the compositor placed is not a full screen of the person's.
pub fn client_fullscreen_beneath_shell(state: &State, frame: &FrameBuilder) -> Option<u64> {
    state
        .fullscreen_target
        .as_ref()
        .filter(|target| target.pinned_output.is_none())
        .and(state.fullscreen_group)
        .filter(|_| crate::compositor::client_input::chrome_captures_input(frame))
}

/// Place the fullscreen client, if any, and return the output it covers.
/// Called once a frame after [`lay_out_beneath_shell`].
pub fn reconcile_overlay(
    state: &mut State,
    frame: &mut FrameBuilder,
    globals: &OutputGlobals,
    outputs: &[Output],
) -> Option<OutputId> {
    let Some(target) = state.fullscreen_target.as_mut() else {
        frame.set_fullscreen_surface(None);
        return None;
    };
    let requested_id = target.pinned_output.or_else(|| {
        target
            .requested_output
            .as_ref()
            .and_then(|output| globals.id_for_resource(output))
    });
    let current_rect = frame.foreign_surface_rect(target.surface_id);
    let centre = current_rect.map(|rect| rect.center());
    let output = select_fullscreen_output(requested_id, centre, outputs);
    let Some(output) = output else {
        frame.set_fullscreen_surface(None);
        return None;
    };
    // Logical, like every other configure: a 2x output's fullscreen client
    // is asked for half its pixels and renders them at scale 2.
    let size = size_to_signed(output.logical_size());
    // A compositor-placed presenter view is sized through its pinned
    // placement by the ordinary configure pass, never told it is fullscreen.
    if target.pinned_output.is_none() && target.configured_size != Some(size) {
        target.surface.with_pending_state(|pending| {
            pending.states.set(
                smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen,
            );
            pending.size = Some(size.into());
        });
        target.surface.send_configure();
        target.configured_size = Some(size);
    }
    if target.pinned_output.is_none() {
        crate::shell::foreign::store().note_fullscreen_extent(target.surface_id, size);
    }
    let rect = output.logical_rect();
    if frame.chrome().fullscreen_group().is_some()
        && frame.chrome().user_fullscreen_group().is_none()
    {
        // Laid out in the desktop beneath the shell's surfaces this frame
        // ([`client_fullscreen_beneath_shell`]); nothing is drawn above
        // them, and no frame of it goes straight to the display.
        frame.set_fullscreen_surface(None);
    } else {
        frame.set_fullscreen_surface(Some((target.surface_id, "Fullscreen".to_string(), rect)));
    }
    Some(output.id)
}

pub(crate) fn select_fullscreen_output(
    requested: Option<OutputId>,
    current_center: Option<vello::kurbo::Point>,
    outputs: &[Output],
) -> Option<&Output> {
    requested
        .and_then(|id| outputs.iter().find(|output| output.id == id))
        .or_else(|| {
            current_center.and_then(|point| {
                outputs
                    .iter()
                    .find(|output| output.logical_rect().contains(point))
            })
        })
        .or_else(|| {
            outputs
                .iter()
                .find(|output| output.size.0 > 0 && output.size.1 > 0)
        })
}

fn size_to_signed(size: (u32, u32)) -> (i32, i32) {
    (
        i32::try_from(size.0).unwrap_or(i32::MAX),
        i32::try_from(size.1).unwrap_or(i32::MAX),
    )
}
