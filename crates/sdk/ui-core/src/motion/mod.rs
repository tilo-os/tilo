//! Tilo's standard motions: the one library every animation in the toolkit, the shell and the apps is
//! composed from. A call site says **what** moves, never how: | Motion | For | |---|---| | [`Appear`] /
//! [`Appearing`] / [`Appearance`] | popovers, menus, dropdowns, tooltips, sheets, dialogs coming from
//! their origin and going back | | [`Drill`] / [`DrillDepth`] | drill-in and back inside a panel;
//! whole-column pushes | | [`Disclose`] / [`Extent`] / [`Disclosure`] | expand and collapse; a panel's
//! height following its content; a find bar or banner coming and going | | [`PanelSlide`] | sidebars,
//! drawers, the inspector | | [`CrossFade`] | swapping content in place | | [`ListChange`] | rows
//! arriving, leaving and changing places | | [`Snap`] | carousels and paged strips settling on a detent
//! | | [`ScrollTo`] | a scroll view going to what it was asked to show (find next, a revealed row) | |
//! [`Zoom`] | one thing becoming another (window and overview card) | | [`WindowMotion`] | windows
//! opening, closing and moving on the strip | | [`Ambient`] | chrome that comes back on its own (desk
//! widgets) | | [`Transient`] | a readout or acknowledgement that goes on its own | | [`Follow`] | a
//! peer's pointer gliding between the samples it is heard at | | [`Float`] | a reaction rising from the
//! pointer it was sent from | | [`Trace`] | a collaborator's just-typed run fading on its own | |
//! [`Breathe`] | a live state's soft ring (someone in a call) | | [`Feedback`] | press and hover tints
//! | | [`ControlChange`] / [`Thumb`] / [`Mark`] | a switch's knob and tint, a checkbox's mark, a
//! slider's thumb, a badge's count | | [`Attention`] | a refusal's shake, a single pulse | | [`Beckon`]
//! | a control hint arriving, showing its gesture, and acknowledged | Every motion runs on
//! [`MotionValue`]: a spring solved in closed form, so it is exact at any frame rate, retargets from
//! where it is drawn *and how fast it is moving there* (interruptible, never a jump, never a queue),
//! and is exactly at rest once within tolerance (a settled motion asks for no frames).

mod appearing;
mod beckon;
mod clock;
mod curve;
mod disclosure;
mod feel;
mod kept;
mod named;
mod placed;
mod pose;
mod presence;
mod rubber_band;
mod scroll_to;
mod source;
mod spec;
mod spring;
mod transition;
mod value;

#[cfg(test)]
mod tests;

pub use appearing::{Appearance, Appearing};
pub use beckon::{Arrival, Beckon, Completion};
#[cfg(test)]
pub(crate) use clock::forget_frames_for_tests;
#[cfg(feature = "native")]
pub(crate) use clock::laying_out_frame;
#[cfg(feature = "native")]
pub(crate) use clock::own;
pub(crate) use clock::owning;
pub(crate) use clock::pin_frame;
#[cfg(test)]
pub(crate) use clock::pin_frame_at;
pub use clock::{
    MotionSurface, frame_drawn, frame_laying_out, frame_now, frames_until, frames_wanted,
    frames_wanted_by, on_surface, testing, want_frames_until,
};
pub use curve::blend_across;
pub use disclosure::Disclosure;
pub use feel::Feel;
pub use kept::{
    Found, HandOn, LayingOutWindow, keep, laying_out_window, take_kept, take_kept_at,
    take_kept_where, window_has_shown,
};
pub(crate) use kept::{closing_window, frame_ended};
pub use named::{
    Ambient, Appear, Attention, Breathe, CROSS_FADE, ControlChange, CrossFade, Disclose, Drill,
    DrillDepth, Extent, Feedback, Float, Follow, ListChange, Mark, PanelSlide, Reveal, Snap,
    Speaking, Thumb, Trace, Transient, WindowMotion, Zoom, held_by_aim, stage, unit_opacity,
};
pub use placed::Placed;
pub use pose::Pose;
pub use presence::Presence;
pub use rubber_band::RubberBand;
pub use scroll_to::{ScrollJump, ScrollTo};
pub use source::{input_source, note_input, note_source, recent_input, source_of};
pub use spec::{Direction, MotionSpec, Pace, Reduced, Shortcut};
pub use spring::SpringSpec;
pub(crate) use transition::progress as transition_progress;
pub use value::{Animatable, Channels, MotionValue, Timing};
