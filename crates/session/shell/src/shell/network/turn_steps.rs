//! How long each step of a network turn held the compositor's loop, for the
//! loop's own timing (`TILO_LOOP_TIMING` and compositor profiles), which
//! records them beside its other pump phases.

use std::time::{Duration, Instant};

/// The steps of one [`super::ShellNetwork::timed_frame_tick`], in order;
/// nothing is timed unless asked for.
#[derive(Default)]
pub struct TurnSteps {
    timed: bool,
    steps: Vec<(&'static str, Duration)>,
}

impl TurnSteps {
    pub(super) fn timed() -> Self {
        Self {
            timed: true,
            steps: Vec::new(),
        }
    }

    /// Run `step`, timed as `name` when this turn is timed.
    pub(super) fn time<T>(&mut self, name: &'static str, step: impl FnOnce() -> T) -> T {
        self.time_named(|| name, step)
    }

    /// [`Self::time`] whose name is only worked out when it is timed.
    pub(super) fn time_named<T>(
        &mut self,
        name: impl FnOnce() -> &'static str,
        step: impl FnOnce() -> T,
    ) -> T {
        if !self.timed {
            return step();
        }
        let started = Instant::now();
        let value = step();
        self.steps.push((name(), started.elapsed()));
        value
    }
}

impl IntoIterator for TurnSteps {
    type Item = (&'static str, Duration);
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.steps.into_iter()
    }
}

/// The step one handled event of `kind` is timed as.
pub(super) fn event_step(kind: &'static str) -> &'static str {
    static NAMES: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<&'static str, &'static str>>,
    > = std::sync::LazyLock::new(std::sync::Mutex::default);
    let mut names = NAMES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // One name per event and message kind, a fixed set.
    names
        .entry(kind)
        .or_insert_with(|| Box::leak(format!("network/event/{kind}").into_boxed_str()))
}
