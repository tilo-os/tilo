//! Tilo's standard motions: the one library every animation in the toolkit,
//! the shell and the apps is composed from.
//!
//! A call site says **what** moves, never how:
//!
//! | Motion | For |
//! |---|---|
//! | [`Appear`] / [`Appearing`] | popovers, menus, dropdowns, tooltips, sheets, dialogs coming from their origin and going back |
//! | [`Drill`] / [`DrillDepth`] | drill-in and back inside a panel; whole-column pushes |
//! | [`Disclose`] / [`Extent`] | expand and collapse; a panel's height following its content |
//! | [`PanelSlide`] | sidebars, drawers, the inspector |
//! | [`CrossFade`] | swapping content in place |
//! | [`ListChange`] | rows arriving, leaving and changing places |
//! | [`Snap`] | carousels and paged strips settling on a detent |
//! | [`Zoom`] | one thing becoming another (window and overview card) |
//! | [`WindowMotion`] | windows opening, closing and moving on the strip |
//! | [`Ambient`] | chrome that comes back on its own (desk widgets) |
//! | [`Transient`] | a readout or acknowledgement that goes on its own |
//! | [`Follow`] | a peer's pointer gliding between the samples it is heard at |
//! | [`Feedback`] | press and hover tints |
//! | [`Attention`] | a refusal's shake, a single pulse |
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
mod clock;
mod named;
mod pose;
mod presence;
mod source;
mod spec;
mod spring;
mod value;

#[cfg(test)]
mod tests;

pub use appearing::Appearing;
pub use clock::{frames_until, frames_wanted, testing, want_frames_until};
pub use named::{
    Ambient, Appear, Attention, CROSS_FADE, CrossFade, Disclose, Drill, DrillDepth, Extent,
    Feedback, Follow, ListChange, PanelSlide, Reveal, Snap, Transient, WindowMotion, Zoom,
    held_by_aim, stage, unit_opacity,
};
pub use pose::Pose;
pub use presence::Presence;
pub use source::{input_source, note_input, note_source, recent_input, source_of};
pub use spec::{Direction, MotionSpec, Pace, Reduced, Shortcut};
pub use spring::SpringSpec;
pub use value::{Animatable, Channels, MotionValue, Timing};
