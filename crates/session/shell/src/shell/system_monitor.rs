//! sysinfo-backed source of rolling CPU/RAM samples for the topbar sparkline.
//!
//! [`SystemMonitor`] owns a [`sysinfo::System`] and a bounded ring buffer of
//! [`SparkSample`]s; call [`SystemMonitor::refresh`] on a timer (typically
//! once per second) to push a sample, evicting the oldest once the buffer
//! is full. CPU is sysinfo's global average across cores; RAM is
//! `used_memory / total_memory * 100`. Both in `[0.0, 100.0]`.

use std::collections::VecDeque;

use sysinfo::System;

/// A single CPU+RAM data point. Values are percentages in `[0.0, 100.0]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SparkSample {
    pub cpu_pct: f32,
    pub ram_pct: f32,
}

/// Rolling monitor that produces [`SparkSample`]s from `sysinfo`, capped at
/// `cap` entries. `cap == 0` means `samples()` is always empty; pass a
/// sensible cap (e.g. 60 for a 1-minute sparkline at 1 Hz).
pub struct SystemMonitor {
    sys: System,
    samples: VecDeque<SparkSample>,
    cap: usize,
}

impl SystemMonitor {
    pub fn new(cap: usize) -> Self {
        Self {
            sys: System::new_all(),
            samples: VecDeque::with_capacity(cap.max(1)),
            cap,
        }
    }

    pub fn refresh(&mut self) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        let cpu = self.sys.global_cpu_usage().clamp(0.0, 100.0);
        let total = self.sys.total_memory();
        let ram = if total == 0 {
            0.0
        } else {
            // Display-only ratio for a sparkline: a few bytes of precision
            // loss out of a multi-GB total is invisible at percentage
            // granularity.
            #[allow(clippy::cast_precision_loss)]
            let pct = self.sys.used_memory() as f32 / total as f32 * 100.0;
            pct.clamp(0.0, 100.0)
        };
        if self.cap == 0 {
            return;
        }
        if self.samples.len() == self.cap {
            self.samples.pop_front();
        }
        self.samples.push_back(SparkSample {
            cpu_pct: cpu,
            ram_pct: ram,
        });
    }

    /// Return the rolling samples oldest-first.
    pub fn samples(&self) -> Vec<&SparkSample> {
        self.samples.iter().collect()
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }
}
