//! Opt-in, payload-free diagnostics for the serialized action/signing handoff.
use std::{
    io::Write,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

fn write_event(
    writer: &mut impl Write,
    elapsed_ms: u128,
    sequence: u64,
    phase: &str,
    event: &str,
    phase_ms: u128,
    count: Option<usize>,
) -> std::io::Result<()> {
    write!(
        writer,
        "qos-action-timing elapsed_ms={elapsed_ms} sequence={sequence} phase={phase} event={event} phase_ms={phase_ms}"
    )?;
    if let Some(count) = count {
        write!(writer, " count={count}")?;
    }
    writeln!(writer)
}

/// Disabled unless QOS_ACTION_TIMING=1. Labels are static call-site literals;
/// only process-local timing sequence numbers, never request identifiers,
/// document data or error strings, enter these logs.
pub(crate) struct Timing {
    start: Option<Instant>,
    sequence: u64,
    phase: &'static str,
    slow_only: bool,
}

impl Timing {
    pub(crate) fn new(phase: &'static str) -> Self {
        Self::start(phase, false)
    }

    /// Idle pumps are frequent: emit only completed phases taking >=100ms.
    pub(crate) fn slow(phase: &'static str) -> Self {
        Self::start(phase, true)
    }

    fn start(phase: &'static str, slow_only: bool) -> Self {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        let enabled = *ENABLED
            .get_or_init(|| std::env::var_os("QOS_ACTION_TIMING").is_some_and(|v| v == "1"));
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let timing = Self {
            start: enabled.then(Instant::now),
            sequence: if enabled {
                NEXT.fetch_add(1, Ordering::Relaxed)
            } else {
                0
            },
            phase,
            slow_only,
        };
        if !slow_only {
            timing.mark("start");
        }
        timing
    }

    pub(crate) fn child(&self, phase: &'static str) -> Self {
        let timing = Self {
            start: self.start.map(|_| Instant::now()),
            sequence: self.sequence,
            phase,
            slow_only: false,
        };
        timing.mark("start");
        timing
    }

    pub(crate) fn mark(&self, event: &'static str) {
        self.mark_value(event, None);
    }

    pub(crate) fn mark_count(&self, event: &'static str, count: usize) {
        self.mark_value(event, Some(count));
    }

    fn mark_value(&self, event: &'static str, count: Option<usize>) {
        if let Some(start) = self.start {
            static ORIGIN: OnceLock<Instant> = OnceLock::new();
            let origin = ORIGIN.get_or_init(Instant::now);
            // Ignore diagnostic output failures; they must not change admission.
            let _ = write_event(
                &mut std::io::stderr().lock(),
                origin.elapsed().as_millis(),
                self.sequence,
                self.phase,
                event,
                start.elapsed().as_millis(),
                count,
            );
        }
    }
}

impl Drop for Timing {
    fn drop(&mut self) {
        if !self.slow_only
            || self
                .start
                .is_some_and(|start| start.elapsed() >= Duration::from_millis(100))
        {
            self.mark("finish");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counted_event_keeps_the_payload_free_static_timing_shape() {
        let mut output = Vec::new();
        write_event(
            &mut output,
            12,
            34,
            "aggregate_persist",
            "aggregate_bytes",
            56,
            Some(78),
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "qos-action-timing elapsed_ms=12 sequence=34 phase=aggregate_persist event=aggregate_bytes phase_ms=56 count=78\n"
        );
        let mut output = Vec::new();
        write_event(
            &mut output,
            12,
            34,
            "aggregate_persist",
            "encode_end",
            56,
            None,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "qos-action-timing elapsed_ms=12 sequence=34 phase=aggregate_persist event=encode_end phase_ms=56\n"
        );
    }
}
