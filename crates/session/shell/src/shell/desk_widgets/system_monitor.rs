//! The System monitor desk widget's readings: how busy the processor is,
//! how full memory is, and how warm the machine runs.
//!
//! From the session's system information service
//! ([`tilo_system_stats::hub::Hub`]), the same readings apps are served,
//! and only while the widget shows: a showing widget holds a subscription,
//! whose readings arrive every [`SAMPLE_EVERY`] and wake the desk; a widget
//! that is away holds none, so nothing is read and nothing wakes for it.
//! Processor load needs two readings, so the first one after the widget
//! appears has memory and temperature, and load follows one interval later.
//! A reading the machine does not offer is left out, never shown as a
//! stand-in.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tilo_system_stats::Grants;
use tilo_system_stats::hub::{Hub, Spec, Subscription};
use tilo_system_stats::protocol::SystemStats;

/// How often a showing widget is sent a reading: often enough to follow a
/// build or a stuck process, rarely enough to cost nothing.
pub const SAMPLE_EVERY: Duration = Duration::from_secs(2);

/// What the widget shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    pub cpu_percent: Option<u8>,
    pub memory_percent: Option<u8>,
    pub temperature_celsius: Option<i16>,
}

impl Reading {
    /// Whether there is anything to show.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cpu_percent.is_none()
            && self.memory_percent.is_none()
            && self.temperature_celsius.is_none()
    }

    /// The widget's three numbers from one of the service's readings.
    #[must_use]
    pub fn from_stats(stats: &SystemStats) -> Self {
        let memory = &stats.memory;
        let memory_percent = memory
            .used()
            .saturating_mul(100)
            .checked_div(memory.total)
            .and_then(|share| u8::try_from(share.min(100)).ok());
        let temperature_celsius = stats
            .thermal
            .iter()
            .map(|zone| zone.celsius.round())
            .filter(|celsius| (-40.0..=150.0).contains(celsius))
            .reduce(f32::max)
            .map(whole_degrees);
        Self {
            cpu_percent: stats.cpu.as_ref().map(|cpu| whole_percent(cpu.total)),
            memory_percent,
            temperature_celsius,
        }
    }
}

/// `value` percent, rounded to a whole one in 0..=100.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "rounded and clamped to 0..=100 first, so the conversion is exact"
)]
fn whole_percent(value: f32) -> u8 {
    value.round().clamp(0.0, 100.0) as u8
}

/// Whole degrees of a sensor already held to -40..=150.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the caller holds it to -40..=150, inside i16"
)]
fn whole_degrees(celsius: f32) -> i16 {
    celsius as i16
}

/// Where the readings come from: the session's service, or a fixture.
pub trait Source {
    /// The widget appeared: start sending readings, calling `wake` as each
    /// arrives. Starting again while started changes nothing.
    fn start(&mut self, wake: Option<crate::session_runtime::Wake>);
    /// The widget went away: stop.
    fn stop(&mut self);
    /// The newest reading since the last call, if one arrived.
    fn latest(&mut self) -> Option<SystemStats>;
}

/// The session's hub.
#[derive(Default)]
pub struct Service {
    subscription: Option<(Subscription, Arc<Mutex<Option<SystemStats>>>)>,
}

impl Source for Service {
    fn start(&mut self, wake: Option<crate::session_runtime::Wake>) {
        if self.subscription.is_some() {
            return;
        }
        let slot = Arc::new(Mutex::new(None));
        let filled = Arc::clone(&slot);
        let interval = u32::try_from(SAMPLE_EVERY.as_millis()).unwrap_or(u32::MAX);
        let subscription = Hub::shared().subscribe(
            Spec::every(interval, false),
            Grants {
                system_info: true,
                ..Grants::default()
            },
            Box::new(move |snapshot| {
                *filled.lock().unwrap_or_else(PoisonError::into_inner) = snapshot.system;
                if let Some(wake) = &wake {
                    wake();
                }
            }),
        );
        self.subscription = Some((subscription, slot));
    }

    fn stop(&mut self) {
        self.subscription = None;
    }

    fn latest(&mut self) -> Option<SystemStats> {
        let (_, slot) = self.subscription.as_ref()?;
        slot.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

/// The readings the widget shows.
#[derive(Debug, Default)]
pub struct Monitor {
    reading: Reading,
    showing: bool,
}

impl Monitor {
    /// The latest reading.
    #[must_use]
    pub fn reading(&self) -> Reading {
        self.reading
    }

    /// The widget shows: make sure readings come, and take the newest one.
    /// Returns whether the reading changed.
    pub fn show(
        &mut self,
        source: &mut dyn Source,
        wake: Option<crate::session_runtime::Wake>,
    ) -> bool {
        if !self.showing {
            source.start(wake);
            self.showing = true;
        }
        let Some(stats) = source.latest() else {
            return false;
        };
        let reading = Reading::from_stats(&stats);
        let changed = reading != self.reading;
        self.reading = reading;
        changed
    }

    /// The widget went away: stop the readings, so the next appearance
    /// starts a new interval and never reports load across the time it was
    /// hidden.
    pub fn pause(&mut self, source: &mut dyn Source) {
        if self.showing {
            source.stop();
            self.showing = false;
        }
    }

    /// Whether readings are coming.
    #[must_use]
    pub fn is_showing(&self) -> bool {
        self.showing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_system_stats::protocol::{CpuStats, MemoryStats, ThermalZone};

    #[derive(Default)]
    struct Fake {
        started: usize,
        stopped: usize,
        queued: Option<SystemStats>,
    }

    impl Source for Fake {
        fn start(&mut self, _wake: Option<crate::session_runtime::Wake>) {
            self.started += 1;
        }
        fn stop(&mut self) {
            self.stopped += 1;
        }
        fn latest(&mut self) -> Option<SystemStats> {
            self.queued.take()
        }
    }

    fn stats(cpu: Option<f32>) -> SystemStats {
        SystemStats {
            cpu: cpu.map(|total| CpuStats {
                total,
                per_core: vec![total],
            }),
            memory: MemoryStats {
                total: 16_000,
                available: 4_000,
                ..MemoryStats::default()
            },
            thermal: vec![
                ThermalZone {
                    name: "cpu".into(),
                    celsius: 47.25,
                },
                ThermalZone {
                    name: "broken".into(),
                    celsius: -273.0,
                },
            ],
            ..SystemStats::default()
        }
    }

    #[test]
    fn the_services_reading_is_load_memory_and_the_warmest_sensor() {
        assert_eq!(
            Reading::from_stats(&stats(Some(24.6))),
            Reading {
                cpu_percent: Some(25),
                memory_percent: Some(75),
                temperature_celsius: Some(47),
            }
        );
        let first = Reading::from_stats(&stats(None));
        assert_eq!(first.cpu_percent, None, "one reading: no load yet");
        assert_eq!(first.memory_percent, Some(75));
        assert!(
            Reading::from_stats(&SystemStats::default()).is_empty(),
            "left out"
        );
    }

    #[test]
    fn readings_come_only_while_the_widget_shows() {
        let mut source = Fake::default();
        let mut monitor = Monitor::default();
        assert!(!monitor.show(&mut source, None), "nothing arrived yet");
        assert_eq!(source.started, 1, "showing subscribes");
        source.queued = Some(stats(None));
        assert!(monitor.show(&mut source, None));
        assert_eq!(monitor.reading().cpu_percent, None);
        source.queued = Some(stats(Some(40.0)));
        assert!(monitor.show(&mut source, None));
        assert_eq!(monitor.reading().cpu_percent, Some(40));
        assert_eq!(source.started, 1, "one subscription while it shows");
        monitor.pause(&mut source);
        monitor.pause(&mut source);
        assert_eq!(source.stopped, 1, "away: unsubscribed, once");
        assert!(!monitor.is_showing());
    }
}
