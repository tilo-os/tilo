//! The System monitor desk widget's readings: how busy the processor is,
//! how full memory is, and how warm the machine runs.
//!
//! Read from the kernel (`/proc/stat`, `/proc/meminfo`, the thermal zones),
//! and only while the widget shows: a widget that is away reads nothing and
//! wakes nothing. Processor load is the share of time not idle between two
//! samples, so the first reading after the widget appears has memory and
//! temperature, and load follows one interval later. A reading the machine
//! does not offer is left out, never shown as a stand-in.

use std::time::{Duration, Instant};

/// How often a showing widget reads again: often enough to follow a build
/// or a stuck process, rarely enough to cost nothing.
pub const SAMPLE_EVERY: Duration = Duration::from_secs(2);

/// Cumulative processor time from `/proc/stat`'s first line, in ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuTimes {
    pub busy: u64,
    pub total: u64,
}

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
}

/// Where the readings come from: the live kernel, or a fixture.
pub trait Source {
    fn cpu(&self) -> Option<CpuTimes>;
    fn memory_percent(&self) -> Option<u8>;
    fn temperature_celsius(&self) -> Option<i16>;
}

/// The running machine.
#[derive(Clone, Copy, Debug, Default)]
pub struct Kernel;

impl Source for Kernel {
    fn cpu(&self) -> Option<CpuTimes> {
        parse_cpu(&std::fs::read_to_string("/proc/stat").ok()?)
    }

    fn memory_percent(&self) -> Option<u8> {
        parse_memory_percent(&std::fs::read_to_string("/proc/meminfo").ok()?)
    }

    fn temperature_celsius(&self) -> Option<i16> {
        let zones = std::fs::read_dir("/sys/class/thermal").ok()?;
        zones
            .filter_map(Result::ok)
            .filter(|zone| {
                zone.file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("thermal_zone"))
            })
            .filter_map(|zone| std::fs::read_to_string(zone.path().join("temp")).ok())
            .filter_map(|text| parse_millidegrees(&text))
            .max()
    }
}

/// The readings, and when they were last taken.
#[derive(Debug, Default)]
pub struct Monitor {
    previous: Option<CpuTimes>,
    reading: Reading,
    sampled_at: Option<Instant>,
}

impl Monitor {
    /// The latest reading.
    #[must_use]
    pub fn reading(&self) -> Reading {
        self.reading
    }

    /// Read again if a sample is due at `now`. Returns whether the reading
    /// changed.
    pub fn sample_if_due(&mut self, source: &dyn Source, now: Instant) -> bool {
        if self
            .sampled_at
            .is_some_and(|at| now.saturating_duration_since(at) < SAMPLE_EVERY)
        {
            return false;
        }
        self.sampled_at = Some(now);
        let cpu = source.cpu();
        let cpu_percent = match (self.previous, cpu) {
            (Some(before), Some(after)) => load_percent(before, after),
            _ => None,
        };
        self.previous = cpu;
        let reading = Reading {
            cpu_percent,
            memory_percent: source.memory_percent(),
            temperature_celsius: source.temperature_celsius(),
        };
        let changed = reading != self.reading;
        self.reading = reading;
        changed
    }

    /// The widget went away: forget the interval, so the next appearance
    /// reads at once and never reports load across the time it was hidden.
    pub fn pause(&mut self) {
        self.previous = None;
        self.sampled_at = None;
    }

    /// When the next sample is due, while the widget shows.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.sampled_at.and_then(|at| at.checked_add(SAMPLE_EVERY))
    }
}

/// The share of ticks spent busy between two samples.
fn load_percent(before: CpuTimes, after: CpuTimes) -> Option<u8> {
    let total = after.total.checked_sub(before.total)?;
    let busy = after.busy.checked_sub(before.busy)?;
    if total == 0 {
        return None;
    }
    let percent = busy.saturating_mul(100).checked_div(total)?.min(100);
    u8::try_from(percent).ok()
}

/// `/proc/stat`'s aggregate `cpu` line: idle and iowait are not busy.
#[must_use]
pub fn parse_cpu(stat: &str) -> Option<CpuTimes> {
    let line = stat.lines().find(|line| line.starts_with("cpu "))?;
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|field| field.parse().ok())
        .collect();
    let idle = fields
        .get(3)?
        .saturating_add(fields.get(4).copied().unwrap_or(0));
    // Guest time is already counted in user time.
    let total = fields
        .iter()
        .take(8)
        .fold(0_u64, |sum, field| sum.saturating_add(*field));
    Some(CpuTimes {
        busy: total.saturating_sub(idle),
        total,
    })
}

/// Memory in use as a share of all memory: what is not available.
#[must_use]
pub fn parse_memory_percent(meminfo: &str) -> Option<u8> {
    let field = |name: &str| {
        meminfo
            .lines()
            .find(|line| line.starts_with(name))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|kb| kb.parse::<u64>().ok())
    };
    let total = field("MemTotal:")?;
    let available = field("MemAvailable:")?;
    let used = total.saturating_sub(available);
    let percent = used.saturating_mul(100).checked_div(total)?.min(100);
    u8::try_from(percent).ok()
}

/// A thermal zone's `temp`, in millidegrees Celsius.
fn parse_millidegrees(text: &str) -> Option<i16> {
    let milli: i64 = text.trim().parse().ok()?;
    i16::try_from(milli / 1000)
        .ok()
        .filter(|c| (-40..=150).contains(c))
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "test timelines add seconds to one monotonic instant"
)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Fake {
        cpu: Cell<CpuTimes>,
    }

    impl Source for Fake {
        fn cpu(&self) -> Option<CpuTimes> {
            Some(self.cpu.get())
        }
        fn memory_percent(&self) -> Option<u8> {
            Some(48)
        }
        fn temperature_celsius(&self) -> Option<i16> {
            None
        }
    }

    #[test]
    fn the_kernels_text_reads_as_load_and_memory() {
        let stat = "cpu  100 0 100 700 100 0 0 0 0 0\ncpu0 1 2 3 4 5 6 7 8 9 10\n";
        assert_eq!(
            parse_cpu(stat),
            Some(CpuTimes {
                busy: 200,
                total: 1000
            })
        );
        let meminfo =
            "MemTotal:       16000000 kB\nMemFree:  1000 kB\nMemAvailable:    4000000 kB\n";
        assert_eq!(parse_memory_percent(meminfo), Some(75));
        assert_eq!(parse_millidegrees("47250\n"), Some(47));
        assert_eq!(parse_millidegrees("-273000"), None, "a broken sensor");
    }

    #[test]
    fn load_needs_two_samples_and_sampling_waits_for_its_interval() {
        let t0 = Instant::now();
        let source = Fake {
            cpu: Cell::new(CpuTimes {
                busy: 100,
                total: 1000,
            }),
        };
        let mut monitor = Monitor::default();
        assert!(monitor.sample_if_due(&source, t0));
        assert_eq!(
            monitor.reading().cpu_percent,
            None,
            "one sample: no load yet"
        );
        assert_eq!(monitor.reading().memory_percent, Some(48));
        assert_eq!(monitor.reading().temperature_celsius, None, "left out");
        assert_eq!(monitor.deadline(), Some(t0 + SAMPLE_EVERY));
        source.cpu.set(CpuTimes {
            busy: 150,
            total: 1200,
        });
        assert!(!monitor.sample_if_due(&source, t0 + Duration::from_secs(1)));
        assert!(monitor.sample_if_due(&source, t0 + SAMPLE_EVERY));
        assert_eq!(monitor.reading().cpu_percent, Some(25));
        monitor.pause();
        assert_eq!(monitor.deadline(), None, "nothing wakes while away");
    }
}
