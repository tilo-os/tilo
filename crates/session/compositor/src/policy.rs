//! Window management policy, asked by the compositor and decided elsewhere.
//!
//! The compositor knows windows as Wayland toplevels; where they go, which
//! one has focus and how big they start is a policy's answer. The desktop's
//! strips, workspaces and tabs implement [`WindowPolicy`] on the desktop
//! model; [`StackPolicy`] is the plain floating stack the compositor runs
//! with on its own (tests, screenshots, a bare session).

use smithay::utils::{Logical, Point, Rectangle};

use crate::input::{GestureEvent, Switch};

/// What a policy learns about a window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowInfo {
    /// The window's identity, never reused while the compositor runs.
    pub id: u64,
    /// `xdg_toplevel.set_app_id`, when the client set one.
    pub app_id: Option<String>,
    /// `xdg_toplevel.set_title`, when the client set one.
    pub title: Option<String>,
    /// The window it belongs to (`xdg_toplevel.set_parent`, an X11
    /// window's `WM_TRANSIENT_FOR`): a dialog of that window.
    pub parent: Option<u64>,
    /// The smallest size the client accepts, when it set one.
    pub min_size: Option<(i32, i32)>,
    /// The largest size the client accepts, when it set one.
    pub max_size: Option<(i32, i32)>,
}

/// Where a window goes and the size it is asked to take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arrangement {
    /// The window ([`WindowInfo::id`]).
    pub id: u64,
    /// Its top-left corner in the logical layout.
    pub location: Point<i32, Logical>,
    /// The size it is configured to, or `None` to leave it to the client.
    pub size: Option<(i32, i32)>,
}

/// Which edges of a window a resize drags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a window has four edges, each dragged or not"
)]
pub struct ResizeEdges {
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
}

/// What a window asks of the window manager (Wayland and X11 alike).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowRequest {
    /// Move it with the pointer while the pressed button is held.
    Move,
    /// Resize it with the pointer by these edges.
    Resize(ResizeEdges),
    /// Maximize it (`true`) or restore it.
    Maximize(bool),
    /// Make it fullscreen (on the named output, else its own) or restore it.
    Fullscreen { on: bool, output: Option<String> },
    /// Hide it until it is brought back.
    Minimize,
}

/// Decides placement, initial size and focus for windows.
pub trait WindowPolicy {
    /// The size a new toplevel is configured to before its first buffer, or
    /// `None` to let the client choose.
    fn initial_size(
        &mut self,
        window: &WindowInfo,
        outputs: &[Rectangle<i32, Logical>],
    ) -> Option<(i32, i32)>;

    /// Where a toplevel of `size` goes in the logical layout when it first
    /// shows a buffer.
    fn place(
        &mut self,
        window: &WindowInfo,
        size: (i32, i32),
        outputs: &[Rectangle<i32, Logical>],
    ) -> Point<i32, Logical>;

    /// Lay every mapped window out again (`windows` bottom first, with their
    /// current rectangles) after one mapped or went away, or the outputs
    /// changed. `None` leaves every window where it is.
    fn arrange(
        &mut self,
        _windows: &[(WindowInfo, Rectangle<i32, Logical>)],
        _outputs: &[Rectangle<i32, Logical>],
    ) -> Option<Vec<Arrangement>> {
        None
    }

    /// The outputs, each with the monitor on it: its identity from the
    /// display's EDID (make, model and serial), else its connector, which
    /// a layout follows between ports. Told when they change, before the
    /// next arrangement.
    fn monitors(&mut self, _monitors: &[(Rectangle<i32, Logical>, String)]) {}

    /// The pointer moved to `(x, y)` in the logical layout, wherever it
    /// went: over a window, a hosted surface or nothing.
    fn pointer_moved(&mut self, _x: f64, _y: f64) {}

    /// A window asked its size of ([`Self::initial_size`]) went away before
    /// it showed a buffer, so it never maps: whatever place it was given
    /// goes.
    fn withdrawn(&mut self, _window: &WindowInfo) {}

    /// Whether a window takes keyboard focus when it first shows.
    fn focus_on_map(&mut self, _window: &WindowInfo) -> bool {
        true
    }

    /// Whether pressing a pointer button on a window focuses and raises it.
    fn focus_on_click(&mut self, _window: &WindowInfo) -> bool {
        true
    }

    /// A touchpad gesture of three or more fingers, which belongs to the
    /// desktop (workspaces, strips, the overview; [`crate::gestures`] turns
    /// the stream into those actions). Asked at the gesture's begin: return
    /// whether the desktop takes it. A taken gesture's updates and end come
    /// here too, their answer ignored; one not taken goes to the client
    /// under the pointer, as gestures of one or two fingers always do.
    fn gesture(&mut self, _gesture: &GestureEvent) -> bool {
        false
    }

    /// A switch changed state: the lid closed or opened, a convertible
    /// folded into or out of tablet shape.
    fn switch(&mut self, _switch: Switch, _on: bool) {}

    /// A window asked to be moved, resized, maximized, made fullscreen or
    /// minimized. Return `true` to have the compositor do what a floating
    /// window manager does (a pointer grab; covering the output and
    /// restoring), `false` when the policy answers it itself (through
    /// [`WindowPolicy::arrange`], which is asked next). By default every
    /// request but minimizing, which needs somewhere to bring the window
    /// back from, is granted.
    fn request(&mut self, _window: &WindowInfo, request: &WindowRequest) -> bool {
        !matches!(request, WindowRequest::Minimize)
    }

    /// The window the policy brought forward itself (a person switching
    /// windows from the desktop's own controls), asked after every
    /// arrangement: the compositor gives it the keyboard. `None` leaves
    /// focus where it is.
    fn focused(&mut self) -> Option<u64> {
        None
    }

    /// `window` asked for the keyboard (an xdg-activation request).
    /// `fresh` is whether its token is recent enough to come from what the
    /// person just did. Whether it gets the keyboard; a window refused is
    /// one waiting for the person, which a policy shows where they look.
    fn activate(&mut self, _window: &WindowInfo, fresh: bool) -> bool {
        fresh
    }

    /// The windows a person closed through the policy's own controls since
    /// it was last asked: the compositor asks each client to close them.
    fn take_close_requests(&mut self) -> Vec<u64> {
        Vec::new()
    }
}

/// Floating windows centred on the first output, each focused as it maps
/// and on click.
#[derive(Clone, Copy, Debug, Default)]
pub struct StackPolicy;

impl WindowPolicy for StackPolicy {
    fn initial_size(
        &mut self,
        _window: &WindowInfo,
        _outputs: &[Rectangle<i32, Logical>],
    ) -> Option<(i32, i32)> {
        None
    }

    fn place(
        &mut self,
        _window: &WindowInfo,
        size: (i32, i32),
        outputs: &[Rectangle<i32, Logical>],
    ) -> Point<i32, Logical> {
        let Some(output) = outputs.first() else {
            return Point::from((0, 0));
        };
        let x = output
            .loc
            .x
            .saturating_add(output.size.w.saturating_sub(size.0).max(0) / 2);
        let y = output
            .loc
            .y
            .saturating_add(output.size.h.saturating_sub(size.1).max(0) / 2);
        Point::from((x, y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::utils::Size;

    #[test]
    fn stack_policy_centres_on_the_first_output_and_clamps_oversized_windows() {
        let outputs = [
            Rectangle::new(Point::from((100, 0)), Size::from((800, 600))),
            Rectangle::new(Point::from((900, 0)), Size::from((800, 600))),
        ];
        let window = WindowInfo::default();
        assert_eq!(
            StackPolicy.place(&window, (200, 100), &outputs),
            Point::from((400, 250))
        );
        assert_eq!(
            StackPolicy.place(&window, (2000, 2000), &outputs),
            Point::from((100, 0))
        );
        assert_eq!(
            StackPolicy.place(&window, (10, 10), &[]),
            Point::from((0, 0))
        );
    }
}
