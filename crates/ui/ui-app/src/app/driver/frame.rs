//! What a display's frame clock reads of a driven app between frames, and what a capture counts:
//! cheap wake demand (deliberately excluding accessibility and inspection trees) and the pace a
//! capture presents frames at.

use vello::Scene;

use tilo_ui_base::widget::Widget;

/// What a live frame runs for a widget a test drives on its own: its update, then its sync.
pub fn frame_widget(widget: &mut dyn Widget, doc: &tilo_ui_base::widget::AppModel) {
    widget.update(doc, &automerge::ROOT);
    widget.sync(doc, &automerge::ROOT);
}

/// Cheap wake demand; deliberately excludes accessibility and inspection trees.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameSchedule {
    /// A frame drawn now would show something the last one did not: queued work, a changed paint, an
    /// animation, a due deadline.
    pub needs_redraw: bool,
    /// The part of `needs_redraw` that is work queued off the tree: a component wake, a pending
    /// invocation, a presence change, a layout a row declaration asked for.
    pub queued_work: bool,
    pub needs_animation_frame: bool,
    pub next_wake_deadline: Option<crate::time::Instant>,
}

/// Why a widget asks a live host for a frame (the driven app's frame requesters).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameDemand {
    /// [`Widget::needs_animation_frame`] — a frame every vsync.
    Animation,
    /// [`Widget::next_wake_deadline`] is due.
    Deadline,
}

/// The pace a capture presents frames at, and so the length of one frame of a
/// capture's bound ([`capture_frames`]): a live window's 60 Hz, so a bound
/// reads as the motion a person would have seen by then.
pub const CAPTURE_FRAME_INTERVAL: crate::time::Duration = crate::time::Duration::from_millis(16);

/// The frames a capture bound of `bound` allows: the bound is counted in
/// frames at [`CAPTURE_FRAME_INTERVAL`], never in the time the machine takes
/// to draw them, so a loaded machine gets the frames an idle one does.
#[must_use]
pub fn capture_frames(bound: crate::time::Duration) -> u32 {
    let interval = CAPTURE_FRAME_INTERVAL.as_micros().max(1);
    u32::try_from(bound.as_micros() / interval).unwrap_or(u32::MAX)
}

/// Borrowed paint input without constructing an accessibility snapshot.
pub struct FrameRenderInput<'a> {
    pub scene: &'a Scene,
    pub background: vello::peniko::Color,
}

pub struct FrameOutput<'a> {
    pub scene: &'a Scene,
    pub background: vello::peniko::Color,
    pub logical_size: (u32, u32),
    pub device_size: (u32, u32),
    pub semantics: crate::inspection::InspectionSnapshot,
    pub needs_redraw: bool,
    /// Independent continuous-update demand, even while pixels are clean.
    pub needs_animation_frame: bool,
    /// Earliest future update across the active presentation roots.
    pub next_wake_deadline: Option<crate::time::Instant>,
    pub ime: Option<tilo_ui_base::ime::ImeWindowState>,
}
