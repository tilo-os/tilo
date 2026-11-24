//! A countdown read live: the time left until a moment, as a hero readout or as a draining ring,
//! and the time since one when it has come — a stopwatch.
//!
//! A view cannot read the clock itself, so a countdown a person watches is the toolkit's: the
//! widget reads the device's clock once a second, redraws its readout and its ring, and comes to
//! rest at zero — a timer that has gone off wakes nothing.

use std::cell::Cell;

use crate::component::{Component, Described, Moment};
use crate::time::{Duration, Instant};
use crate::widget::{
    AppModel, Column, Decorator, ModelPath, ProgressRing, ProgressRingSize, Text, Widget,
};

/// The seconds between readings.
const SECOND: i64 = 1;

/// A countdown, live: the time left until `at` — or, once it has come, the time since it — and,
/// with `total`, a ring of how much of it is left.
#[derive(Clone, Component, Debug, Default, PartialEq)]
#[component(name = "Countdown", role = "Timer", label = "label")]
pub struct CountdownProps {
    /// The moment it counts to: the time left until it, or the time since it once it has come.
    #[component(required, positional)]
    pub at: Moment,
    /// How long the whole is, in seconds: a ring drains as it goes. Without it the readout stands
    /// alone, as a stopwatch's does.
    pub total: Option<f64>,
    /// What the ring measures, under its readout.
    pub label: Option<String>,
    /// How large the readout is: `regular` unless said.
    pub size: Option<ProgressRingSize>,
}

/// What a countdown reads at `now`: the seconds left (or since), how full the ring is, and whether
/// it still moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CountdownReading {
    /// The whole seconds left until `at`, or since it when it has come.
    pub secs: u64,
    /// How much of the whole is left, from 0 to 1; `None` for a countdown with no whole.
    pub fraction: Option<f64>,
    /// Whether the next second reads differently: a countdown that reached zero stays there.
    pub moving: bool,
}

/// What a countdown to `at` (seconds since 1970), of `total` seconds where it has a whole, reads
/// at `now`.
#[must_use]
pub fn reading(at: i64, total: Option<f64>, now: i64) -> CountdownReading {
    let left = at.saturating_sub(now);
    let Some(total) = total.filter(|total| *total > 0.0) else {
        return CountdownReading {
            secs: u64::try_from(left.saturating_neg()).unwrap_or(u64::MAX),
            fraction: None,
            moving: true,
        };
    };
    let secs = u64::try_from(left.max(0)).unwrap_or(u64::MAX);
    let fraction = (left.max(0) as f64 / total).clamp(0.0, 1.0);
    CountdownReading {
        secs,
        fraction: Some(fraction),
        moving: left > 0,
    }
}

/// [`CountdownProps`]' widget: a readout, inside a ring where it has a whole.
pub struct Countdown {
    column: Column,
    at: i64,
    total: Option<f64>,
    /// When the next reading differs, in seconds since 1970; `None` once it has come to rest.
    changes_at: Cell<Option<i64>>,
}

impl Countdown {
    fn new(at: i64, total: Option<f64>, label: Option<String>, key: &str) -> Self {
        let mut children: Vec<Box<dyn Widget>> = Vec::new();
        if total.is_some_and(|total| total > 0.0) {
            children.push(Box::new(
                ProgressRing::new(1.0)
                    .value(format_timecode(0))
                    .caption(label.unwrap_or_default()),
            ));
        } else {
            children.push(Box::new(
                Text::new(&format_timecode(0))
                    .with_role(crate::tokens::TextRole::Display)
                    .with_id(key),
            ));
        }
        let mut column = Column::new().with_no_padding().with_intrinsic_height();
        column.set_children(children);
        let mut countdown = Self {
            column,
            at,
            total,
            changes_at: Cell::new(None),
        };
        countdown.read(crate::time::unix_now_i64());
        countdown
    }

    /// Read it at `now`: what it says, how full its ring is, and when that next changes.
    fn read(&mut self, now: i64) -> bool {
        let reading = reading(self.at, self.total, now);
        self.changes_at
            .set(reading.moving.then(|| now.saturating_add(SECOND)));
        let text = format_timecode(reading.secs);
        let mut changed = false;
        let mut children = self.column.take_children();
        for child in &mut children {
            let Some(any) = child.as_any_mut() else {
                continue;
            };
            if let Some(ring) = any.downcast_mut::<ProgressRing>() {
                if let Some(fraction) = reading.fraction {
                    ring.set(fraction, text.clone());
                    changed = true;
                }
            } else if let Some(label) = any.downcast_mut::<Text>() {
                let before = label.resolved_label();
                label.show_content(text.clone());
                changed |= label.resolved_label() != before;
            }
        }
        self.column.set_children(children);
        changed
    }

    fn follow_clock(&mut self) -> bool {
        let now = crate::time::unix_now_i64();
        match self.changes_at.get() {
            Some(due) if due > now => false,
            None => false,
            Some(_) => self.read(now),
        }
    }
}

/// A length of time as a clock reads it (`4:32`, `1:04:32`).
fn format_timecode(secs: u64) -> String {
    crate::format::timecode(secs)
}

impl Decorator for Countdown {
    type Inner = Column;

    fn inner(&self) -> &Column {
        &self.column
    }

    fn inner_mut(&mut self) -> &mut Column {
        &mut self.column
    }

    fn name(&self) -> &'static str {
        "Countdown"
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let followed = self.follow_clock();
        let inner = Widget::update(&mut self.column, state, path);
        followed || inner
    }

    fn serve_deadline(&mut self, _now: Instant) -> bool {
        self.follow_clock()
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let due = self.changes_at.get()?;
        let wait_ms = due
            .saturating_mul(1000)
            .saturating_sub(crate::time::unix_now_ms_i64())
            .max(0);
        crate::time::now().checked_add(Duration::from_millis(u64::try_from(wait_ms).ok()?))
    }
}

impl Described for Countdown {
    type Props = CountdownProps;

    fn from_props(key: &str, props: CountdownProps) -> Self {
        Self::new(props.at.0, props.total, props.label, key)
    }

    /// Another moment, another whole or another look builds it again: its ring is drawn from what
    /// it is, never patched from another countdown's.
    fn set_props(&mut self, props: CountdownProps) -> bool {
        props.at.0 == self.at && props.total == self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timer_counts_down_to_zero_and_comes_to_rest() {
        let at = 1_000;
        assert_eq!(
            reading(at, Some(60.0), 940),
            CountdownReading {
                secs: 60,
                fraction: Some(1.0),
                moving: true
            }
        );
        assert_eq!(
            reading(at, Some(60.0), 970),
            CountdownReading {
                secs: 30,
                fraction: Some(0.5),
                moving: true
            }
        );
        assert_eq!(
            reading(at, Some(60.0), 1_005),
            CountdownReading {
                secs: 0,
                fraction: Some(0.0),
                moving: false
            }
        );
    }

    #[test]
    fn a_stopwatch_counts_the_time_since() {
        assert_eq!(
            reading(1_000, None, 1_075),
            CountdownReading {
                secs: 75,
                fraction: None,
                moving: true
            }
        );
        assert_eq!(format_timecode(75), "1:15");
    }
}
