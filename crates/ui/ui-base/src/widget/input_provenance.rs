//! Where a control invocation came from: the person's input kind and origin, carried as thread-scoped
//! provenance from the producer through the queue that drains it to the execution that records it.
use super::controls::Origin;
#[cfg(feature = "model")]
use crate::widget::Widget;
use std::cell::Cell;

/// How a person's input produced a control invocation: the provenance that decides whether it joins the
/// undo step before it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Input {
    /// Programmatic, assistive or one-off: a step of it.
    #[default]
    Discrete,
    /// Continues the person's gesture (typing, a drag): it joins the step before it when the resource
    /// says it continues it.
    Continuing,
    /// Starts a gesture (the first value of a drag): it never joins the
    /// step before it, and the rest of the gesture joins it.
    Starting,
}

thread_local! {
    static INPUT: Cell<Input> = const { Cell::new(Input::Discrete) };
    static DRAINED_INPUT: Cell<Input> = const { Cell::new(Input::Discrete) };
    static DRAINED_ORIGIN: Cell<Origin> = const { Cell::new(Origin::Person) };
    #[cfg(feature = "model")]
    static NATIVE_TEXT_EXECUTION: Cell<Input> = const { Cell::new(Input::Discrete) };
}

struct InputScope(Input);
impl Drop for InputScope {
    fn drop(&mut self) {
        INPUT.with(|flag| flag.set(self.0));
    }
}

/// Private producer provenance, copied into the exact owned queue entry.
/// It confers no execution authority and is never decoded from a request.
pub fn with_input<T>(input: Input, run: impl FnOnce() -> T) -> T {
    let _scope = InputScope(INPUT.with(|flag| flag.replace(input)));
    run()
}

/// [`with_input`] for typing: `physical` text continues the person's typing; anything else is discrete.
pub fn with_physical_text<T>(physical: bool, run: impl FnOnce() -> T) -> T {
    with_input(
        if physical {
            Input::Continuing
        } else {
            Input::Discrete
        },
        run,
    )
}

pub(crate) fn input() -> Input {
    INPUT.with(Cell::get)
}

#[cfg(any(test, feature = "test-support"))]
pub fn physical_text() -> bool {
    input() != Input::Discrete
}

pub(crate) fn note_drained_input(input: Input) {
    DRAINED_INPUT.with(|flag| flag.set(input));
}

#[cfg(feature = "model")]
struct DrainTextScope(Input);
#[cfg(feature = "model")]
impl Drop for DrainTextScope {
    fn drop(&mut self) {
        DRAINED_INPUT.with(|flag| flag.set(self.0));
    }
}

/// The queue that hands over the invocation being drained says who asked for it ([`Origin::App`]: the
/// app on its); unsaid, it is a person's.
pub(crate) fn note_drained_origin(origin: Origin) {
    DRAINED_ORIGIN.with(|flag| flag.set(origin));
}

#[cfg(feature = "model")]
struct DrainOriginScope(Origin);
#[cfg(feature = "model")]
impl Drop for DrainOriginScope {
    fn drop(&mut self) {
        DRAINED_ORIGIN.with(|flag| flag.set(self.0));
    }
}

#[cfg(feature = "model")]
/// The next invocation `widget`'s queues hand over, the effect invocation it continues, and who asked
/// for it.
pub fn take_attributed_with_origin(
    widget: &mut dyn Widget,
) -> Option<(
    tilo_actions::ControlInvocation,
    Option<tilo_action_types::InvocationId>,
    Origin,
)> {
    let (taken, _, origin) = capture_physical_drain(|| {
        crate::widget::controls::take_attributed_control_invocation(widget)
    });
    taken.map(|(intent, on_behalf_of)| (intent, on_behalf_of, origin))
}

#[cfg(feature = "model")]
pub fn capture_physical_drain<T>(run: impl FnOnce() -> T) -> (T, Input, Origin) {
    let _scope = DrainTextScope(DRAINED_INPUT.with(|flag| flag.replace(Input::Discrete)));
    let _origin = DrainOriginScope(DRAINED_ORIGIN.with(|flag| flag.replace(Origin::Person)));
    let result = run();
    (
        result,
        DRAINED_INPUT.with(Cell::get),
        DRAINED_ORIGIN.with(Cell::get),
    )
}

#[cfg(feature = "model")]
struct NativeTextScope(Input);
#[cfg(feature = "model")]
impl Drop for NativeTextScope {
    fn drop(&mut self) {
        NATIVE_TEXT_EXECUTION.with(|flag| flag.set(self.0));
    }
}

#[cfg(feature = "model")]
pub fn with_native_text<T>(input: Input, run: impl FnOnce() -> T) -> T {
    let _scope = NativeTextScope(NATIVE_TEXT_EXECUTION.with(|flag| flag.replace(input)));
    run()
}

#[cfg(feature = "model")]
/// The provenance of the native text execution running on this thread: [`Input::Discrete`] outside
/// [`with_native_text`].
pub fn native_text_input() -> Input {
    NATIVE_TEXT_EXECUTION.with(Cell::get)
}

/// The input kind the last drain on this thread recorded, outside any drain.
#[cfg(any(test, feature = "test-support"))]
pub fn drained_input() -> Input {
    DRAINED_INPUT.with(Cell::get)
}
