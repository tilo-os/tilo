//! sysinfo-backed source of rolling CPU/RAM samples for the topbar sparkline.
//!
//! The [`SystemMonitor`] owns a [`sysinfo::System`] and a bounded ring buffer of
//! [`SparkSample`]s. Call [`SystemMonitor::refresh`] on a timer (typically once
//! per second from the shell tick) to push a new sample; older samples are
//! evicted once the buffer reaches its capacity.
//!
//! CPU usage uses sysinfo's global average across all logical cores and RAM
//! usage is `used_memory / total_memory * 100`. Both are expressed as
//! percentages in `[0.0, 100.0]`.

use std::collections::VecDeque;

use sysinfo::System;

/// A single CPU+RAM data point. Values are percentages in `[0.0, 100.0]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SparkSample {
    pub cpu_pct: f32,
    pub ram_pct: f32,
}

/// Rolling monitor that produces [`SparkSample`]s from `sysinfo`.
///
/// The buffer is capped at `cap` entries. `cap == 0` means no samples will be
/// retained (each refresh immediately evicts its own push, so `samples()` is
/// always empty); callers should pass a sensible non-zero cap (e.g. 60 for a
/// 1-minute sparkline at 1 Hz).
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

    /// Refresh CPU + memory stats and push a new sample, evicting the oldest
    /// entry if the buffer is at capacity.
    pub fn refresh(&mut self) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        let cpu = self.sys.global_cpu_usage().clamp(0.0, 100.0);
        let total = self.sys.total_memory();
        let ram = if total == 0 {
            0.0
        } else {
            (self.sys.used_memory() as f32 / total as f32 * 100.0).clamp(0.0, 100.0)
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

    /// Capacity the monitor was constructed with.
    pub fn capacity(&self) -> usize {
        self.cap
    }
}
