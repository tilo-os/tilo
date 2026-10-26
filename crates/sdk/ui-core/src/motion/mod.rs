//! Tilo's standard motions: the one library every animation in the toolkit,
//! the shell and the apps is composed from.
//!
//! A call site says **what** moves, never how:
//!
//! | Motion | For |
//! |---|---|
//! | [`Appear`] / [`Appearing`] | popovers, menus, dropdowns, tooltips, sheets, dialogs coming from their origin and going back |
//! | [`Drill`] / [`DrillDepth`] | drill-in and back inside a panel; whole-column pushes |
//! | [`Disclose`] / [`Extent`] / [`Disclosure`] | expand and collapse; a panel's height following its content; a find bar or banner coming and going |
//! | [`PanelSlide`] | sidebars, drawers, the inspector |
//! | [`CrossFade`] | swapping content in place |
//! | [`ListChange`] | rows arriving, leaving and changing places |
//! | [`Snap`] | carousels and paged strips settling on a detent |
//! | [`ScrollTo`] | a scroll view going to what it was asked to show (find next, a revealed row) |
//! | [`Zoom`] | one thing becoming another (window and overview card) |
//! | [`WindowMotion`] | windows opening, closing and moving on the strip |
//! | [`Ambient`] | chrome that comes back on its own (desk widgets) |
//! | [`Transient`] | a readout or acknowledgement that goes on its own |
//! | [`Follow`] | a peer's pointer gliding between the samples it is heard at |
//! | [`Float`] | a reaction rising from the pointer it was sent from |
//! | [`Trace`] | a collaborator's just-typed run fading on its own |
//! | [`Breathe`] | a live state's soft ring (someone in a call) |
//! | [`Feedback`] | press and hover tints |
//! | [`ControlChange`] / [`Thumb`] / [`Mark`] | a switch's knob and tint, a checkbox's mark, a slider's thumb, a badge's count |
//! | [`Attention`] | a refusal's shake, a single pulse |
//! | [`Beckon`] | a control hint arriving, showing its gesture, and acknowledged |
//!
//! Every motion runs on [`MotionValue`]: a spring solved in closed form, so it
//! is exact at any frame rate, retargets from where it is drawn *and how fast
//! it is moving there* (interruptible, never a jump, never a queue), and is
//! exactly at rest once within tolerance (a settled motion asks for no
//! frames). [`Presence`] is the openness of anything that comes and goes.
//! Each named motion's [`MotionSpec`] decides its pace in and out, what a
//! shortcut gets (the result lands at once) and what reduced motion becomes
//! (spatial movement lands; changes of what is shown still fade).
//!
//! Rules every motion keeps (the design note is
//! `HIG.md` §9.7): input never waits for a motion; routine motion never
//! bounces; content is laid out once and moved by transform, clip and
//! opacity; nothing moves under a pointer that is aiming at it
//! ([`held_by_aim`]); a moving surface is hit-tested where it is drawn
//! ([`Pose::to_local`]).
//!
//! A motion that is genuinely new is added here, with its spec and pose,
//! rather than written at its call site: `tools/policy/check_motion_library.py`
//! rejects easing math, ad-hoc springs and lerp-over-time anywhere else.

mod appearing;
mod beckon;
mod clock;
mod disclosure;
mod kept;
mod named;
mod pose;
mod presence;
mod rubber_band;
mod scroll_to;
mod source;
mod spec;
mod spring;
mod value;

#[cfg(test)]
mod tests;

pub use appearing::Appearing;
pub use beckon::{Arrival, Beckon, Completion};
#[cfg(test)]
pub(crate) use clock::forget_frames_for_tests;
pub(crate) use clock::pin_frame;
pub use clock::{
    frame_drawn, frame_now, frames_until, frames_wanted, frames_wanted_by, testing,
    want_frames_until,
};
pub use disclosure::Disclosure;
pub use kept::{HandOn, keep, take_kept, take_kept_at, take_kept_where, window_has_shown};
pub(crate) use kept::{frame_ended, laying_out_window};
pub use named::{
    Ambient, Appear, Attention, Breathe, CROSS_FADE, ControlChange, CrossFade, Disclose, Drill,
    DrillDepth, Extent, Feedback, Float, Follow, ListChange, Mark, PanelSlide, Reveal, Snap,
    Speaking, Thumb, Trace, Transient, WindowMotion, Zoom, held_by_aim, stage, unit_opacity,
};
pub use pose::Pose;
pub use presence::Presence;
pub use rubber_band::RubberBand;
pub use scroll_to::{ScrollJump, ScrollTo};
pub use source::{input_source, note_input, note_source, recent_input, source_of};
pub use spec::{Direction, MotionSpec, Pace, Reduced, Shortcut};
pub use spring::SpringSpec;
pub use value::{Animatable, Channels, MotionValue, Timing};
