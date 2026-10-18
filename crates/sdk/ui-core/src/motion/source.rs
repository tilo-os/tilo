//! Which input a motion follows. A keyboard shortcut lands its result at
//! once; a pointer or a finger gets the motion (HIG §9.7).

use crate::tokens::MotionSource;
use winit::event::WindowEvent;

/// The input `event` comes from, for a host that opens and closes surfaces
/// on later frames and needs to know what asked: `None` for events that are
/// not input.
#[must_use]
pub fn source_of(event: &WindowEvent) -> Option<MotionSource> {
    match event {
        WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => Some(MotionSource::Keyboard),
        WindowEvent::MouseInput { .. }
        | WindowEvent::MouseWheel { .. }
        | WindowEvent::CursorMoved { .. } => Some(MotionSource::Pointer),
        WindowEvent::Touch(_)
        | WindowEvent::PinchGesture { .. }
        | WindowEvent::PanGesture { .. }
        | WindowEvent::RotationGesture { .. } => Some(MotionSource::Gesture),
        _ => None,
    }
}

thread_local! {
    /// The input the toolkit handled last on this thread. Before any, what
    /// opens was not asked for by a hand (a surface restored at start, a
    /// script): it follows the action rule and lands at once.
    static LAST_INPUT: std::cell::Cell<MotionSource> =
        const { std::cell::Cell::new(MotionSource::Action) };
    /// When it was handled.
    static LAST_INPUT_AT: std::cell::Cell<Option<crate::time::Instant>> =
        const { std::cell::Cell::new(None) };
}

/// How soon after an input a change counts as that input's result: the
/// frame it was handled in and the one after, with room for a model round
/// trip on the UI thread. A change later than this the person did not
/// visibly cause (a sync, a timer), and it shows without motion.
const CAUSED_WITHIN: crate::time::Duration = crate::time::Duration::from_millis(300);

/// Note that `event` is being handled: what it opens or closes follows it
/// ([`input_source`]). The window's event entry points call this before
/// dispatching, so every surface the toolkit shows knows what asked for it
/// without each owner threading the source through.
///
/// The pointer merely moving asks for nothing, so it does not count: a menu
/// a key opened lands at once even if the mouse is nudged before the frame.
pub fn note_input(event: &WindowEvent) {
    if matches!(event, WindowEvent::CursorMoved { .. }) {
        return;
    }
    if let Some(source) = source_of(event) {
        note_source(source);
    }
}

/// Note that `source` is acting (an action door or agent performing a
/// control is [`MotionSource::Action`]).
pub fn note_source(source: MotionSource) {
    LAST_INPUT.with(|last| last.set(source));
    LAST_INPUT_AT.with(|at| at.set(Some(crate::time::now())));
}

/// The input a change happening at `now` follows, if the person just made
/// one (a few hundred milliseconds ago): what a list uses to animate a row the person
/// added or removed, and to leave one a sync added alone.
#[must_use]
pub fn recent_input(now: crate::time::Instant) -> Option<MotionSource> {
    let at = LAST_INPUT_AT.with(std::cell::Cell::get)?;
    (now.saturating_duration_since(at) <= CAUSED_WITHIN).then(input_source)
}

/// The input the toolkit handled last on this thread: what a surface that
/// opens or closes now follows. A key opened it, so it lands at once; a
/// click opened it, so it moves.
#[must_use]
pub fn input_source() -> MotionSource {
    LAST_INPUT.with(std::cell::Cell::get)
}
