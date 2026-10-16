//! Explicit opt-in, numeric-only companion to the existing frame trace.
use super::{FrameSample, MAX_TRACE_ROWS, TRACE_FLUSH_INTERVAL_US, us};
use std::io::Write;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(super) const ENV: &str = "TILO_FRAME_CPU_TRACE";
const HEADER: &str = "seq,start_us,anchor_monotonic_begin_us,anchor_unix_us,anchor_monotonic_end_us,anchor_offset_us,paint_wall_us,layout_us,scene_us,submit_us,paint_thread_cpu_us,paint_process_cpu_us,layout_thread_cpu_us,layout_process_cpu_us,scene_thread_cpu_us,scene_process_cpu_us,submit_thread_cpu_us,submit_process_cpu_us,present_async";

#[derive(Clone, Copy)]
pub(crate) enum CpuPhase {
    Paint,
    Layout,
    Scene,
    Submit,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct CpuSnapshot {
    thread: Option<Duration>,
    process: Option<Duration>,
}

impl CpuSnapshot {
    fn capture() -> Self {
        Self {
            thread: super::thread_cpu_time(),
            process: process_cpu_time(),
        }
    }

    fn elapsed(self, end: Self) -> Self {
        Self {
            thread: self
                .thread
                .zip(end.thread)
                .and_then(|(a, b)| b.checked_sub(a)),
            process: self
                .process
                .zip(end.process)
                .and_then(|(a, b)| b.checked_sub(a)),
        }
    }
}

#[derive(Default)]
struct Detail {
    monotonic_begin: Option<Duration>,
    unix: Option<Duration>,
    monotonic_end: Option<Duration>,
    anchor_offset: Duration,
    paint_wall: Duration,
    paint: CpuSnapshot,
    layout: CpuSnapshot,
    scene: CpuSnapshot,
    submit: CpuSnapshot,
}

pub(super) struct CpuTrace {
    out: Option<std::io::BufWriter<std::fs::File>>,
    detail: Detail,
    rows: usize,
    next_flush_us: u64,
}

impl CpuTrace {
    pub(super) fn open(path: &std::path::Path) -> std::io::Result<Self> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut out = std::io::BufWriter::new(options.open(path)?);
        writeln!(out, "{HEADER}")?;
        out.flush()?;
        Ok(Self {
            out: Some(out),
            detail: Detail::default(),
            rows: 0,
            next_flush_us: TRACE_FLUSH_INTERVAL_US,
        })
    }

    pub(super) fn snapshot(&self) -> Option<CpuSnapshot> {
        self.out.as_ref().map(|_| CpuSnapshot::capture())
    }

    pub(super) fn begin_tick(&mut self, at: Instant) {
        if self.out.is_none() {
            return;
        }
        // Bracket the wall-clock anchor; guest and host monotonic epochs differ.
        let monotonic_begin = monotonic_time();
        let anchor_at = Instant::now();
        let unix = SystemTime::now().duration_since(UNIX_EPOCH).ok();
        let monotonic_end = monotonic_time();
        self.detail = Detail {
            monotonic_begin,
            unix,
            monotonic_end,
            anchor_offset: anchor_at.saturating_duration_since(at),
            ..Default::default()
        };
    }

    pub(super) fn mark(&mut self, phase: CpuPhase, start: Option<CpuSnapshot>) {
        let Some(start) = start else { return };
        let Some(end) = self.snapshot() else { return };
        let destination = match phase {
            CpuPhase::Paint => &mut self.detail.paint,
            CpuPhase::Layout => &mut self.detail.layout,
            CpuPhase::Scene => &mut self.detail.scene,
            CpuPhase::Submit => &mut self.detail.submit,
        };
        *destination = start.elapsed(end);
    }

    pub(super) fn mark_paint_wall(&mut self, wall: Duration) {
        if self.out.is_some() {
            self.detail.paint_wall = wall;
        }
    }

    pub(super) fn finish(&mut self) {
        if let Some(mut out) = self.out.take()
            && let Err(error) = out.flush()
        {
            eprintln!("frame-metrics: CPU trace flush failed ({error})");
        }
    }

    pub(super) fn end_tick(&mut self, sample: FrameSample) {
        if !sample.painted || self.out.is_none() {
            return;
        }
        if let Err(error) = self.write_row(sample) {
            eprintln!("frame-metrics: CPU trace write failed ({error}); disabling CPU trace");
            self.out = None;
        }
    }

    fn write_row(&mut self, sample: FrameSample) -> std::io::Result<()> {
        let Some(out) = self.out.as_mut() else {
            return Ok(());
        };
        let d = &self.detail;
        let fields = [
            Some(u128::from(sample.seq)),
            Some(u128::from(sample.start_us)),
            d.monotonic_begin.map(|v| v.as_micros()),
            d.unix.map(|v| v.as_micros()),
            d.monotonic_end.map(|v| v.as_micros()),
            Some(d.anchor_offset.as_micros()),
            Some(d.paint_wall.as_micros()),
            Some(u128::from(sample.layout_us)),
            Some(u128::from(sample.scene_us)),
            Some(u128::from(sample.submit_us)),
        ]
        .into_iter()
        .chain(
            [d.paint, d.layout, d.scene, d.submit]
                .into_iter()
                .flat_map(|cpu| {
                    [
                        cpu.thread.map(|v| u128::from(us(v))),
                        cpu.process.map(|v| u128::from(us(v))),
                    ]
                }),
        )
        .chain(std::iter::once(Some(u128::from(u8::from(
            sample.present_async,
        )))));
        for (index, value) in fields.enumerate() {
            if index > 0 {
                write!(out, ",")?;
            }
            if let Some(value) = value {
                write!(out, "{value}")?;
            }
        }
        writeln!(out)?;
        self.rows = self.rows.saturating_add(1);
        if self.rows == 1 || sample.start_us >= self.next_flush_us || self.rows >= MAX_TRACE_ROWS {
            out.flush()?;
            self.next_flush_us = sample.start_us.saturating_add(TRACE_FLUSH_INTERVAL_US);
        }
        if self.rows >= MAX_TRACE_ROWS {
            eprintln!("frame-metrics: CPU trace row limit reached; disabling CPU trace");
            self.out = None;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn posix_time(clock: libc::clockid_t) -> Option<Duration> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: time is writable storage; unsupported clocks return an error.
    if unsafe { libc::clock_gettime(clock, &raw mut time) } != 0 {
        return None;
    }
    Some(Duration::new(
        u64::try_from(time.tv_sec).ok()?,
        u32::try_from(time.tv_nsec).ok()?,
    ))
}

pub(crate) fn process_cpu_time() -> Option<Duration> {
    #[cfg(unix)]
    {
        posix_time(libc::CLOCK_PROCESS_CPUTIME_ID)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

fn monotonic_time() -> Option<Duration> {
    #[cfg(unix)]
    {
        posix_time(libc::CLOCK_MONOTONIC)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use crate::compositor::metrics::FrameRecorder;

    fn busy_cpu(duration: Duration) {
        let start = super::super::thread_cpu_time().expect("test requires thread CPU clock");
        while super::super::thread_cpu_time()
            .expect("thread CPU clock remains available")
            .saturating_sub(start)
            < duration
        {
            std::hint::black_box(17_u64.wrapping_mul(97));
        }
    }

    #[test]
    fn cpu_trace_distinguishes_main_thread_work_sleep_and_worker_cpu() {
        let dir = tempfile::tempdir().expect("private trace fixture");
        let path = dir.path().join("cpu.csv");
        let mut recorder = FrameRecorder::disabled();
        recorder.enabled = true;
        recorder.env_enabled = true;
        recorder.cpu_trace = Some(CpuTrace::open(&path).expect("open real CPU trace"));
        let before = SystemTime::now().duration_since(UNIX_EPOCH).expect("UTC");
        let mono_lower = monotonic_time().expect("monotonic clock").as_micros();
        let paint_start = Instant::now();
        let mono_upper = monotonic_time().expect("monotonic clock").as_micros();
        recorder.begin_tick(paint_start);
        let paint_cpu = recorder.cpu_snapshot();
        let phase = Instant::now();
        busy_cpu(Duration::from_millis(8));
        recorder.mark_layout(phase.elapsed());
        recorder.mark_cpu(CpuPhase::Layout, paint_cpu);
        let phase = Instant::now();
        let cpu = recorder.cpu_snapshot();
        std::thread::sleep(Duration::from_millis(30));
        recorder.mark_scene(phase.elapsed());
        recorder.mark_cpu(CpuPhase::Scene, cpu);
        let phase = Instant::now();
        let cpu = recorder.cpu_snapshot();
        let worker = std::thread::spawn(|| busy_cpu(Duration::from_millis(10)));
        worker.join().expect("owned CPU worker");
        let done = Instant::now();
        recorder.mark_submit(done.saturating_duration_since(phase), false, done, None);
        recorder.mark_cpu(CpuPhase::Submit, cpu);
        recorder.mark_cpu(CpuPhase::Paint, paint_cpu);
        recorder.mark_paint_wall(done.saturating_duration_since(paint_start));
        recorder.end_tick();
        let after = SystemTime::now().duration_since(UNIX_EPOCH).expect("UTC");
        // Read before Drop: the actual first-row flush must expose the sample.
        let text = std::fs::read_to_string(&path).expect("read flushed companion");
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some(HEADER));
        let row = lines.next().expect("actual painted sample");
        assert!(lines.next().is_none());
        let values = row
            .split(',')
            .map(|v| v.parse::<u128>().expect("numeric clocks, not missing"))
            .collect::<Vec<_>>();
        assert_eq!(values.len(), HEADER.split(',').count());
        assert!(values[2] <= values[4], "monotonic clock brackets UTC read");
        assert!(
            values[2].saturating_sub(values[5]) <= mono_upper.saturating_add(2),
            "anchor bracket includes tick start"
        );
        assert!(
            values[4].saturating_sub(values[5]).saturating_add(2) >= mono_lower,
            "offset is sampled inside the bracket"
        );
        assert!((before.as_micros()..=after.as_micros()).contains(&values[3]));
        assert!(values[12] >= 8_000, "layout records real main-thread work");
        assert!(
            values[8] >= 30_000,
            "scene wall includes the deliberate wait"
        );
        assert!(
            values[14] < values[8] / 2,
            "sleep is not charged as thread CPU"
        );
        assert!(
            values[17] >= values[16] + 5_000,
            "submit includes worker CPU absent from calling thread"
        );
        assert!(
            values[11] > values[10],
            "total paint separates process and calling-thread CPU"
        );
    }

    #[test]
    fn cpu_trace_is_bounded_and_records_missing_clocks_as_missing() {
        let dir = tempfile::tempdir().expect("private trace fixture");
        let path = dir.path().join("bounded.csv");
        let mut trace = CpuTrace::open(&path).expect("open companion");
        // Drive the production boundary without generating 200k rows.
        trace.rows = MAX_TRACE_ROWS - 1;
        trace.end_tick(FrameSample {
            seq: 4,
            painted: true,
            ..Default::default()
        });
        assert!(
            trace.snapshot().is_none(),
            "reaching the bound stops new CPU reads"
        );
        trace.end_tick(FrameSample {
            seq: 5,
            painted: true,
            ..Default::default()
        });
        let text = std::fs::read_to_string(path).expect("read bounded trace");
        assert_eq!(text.lines().count(), 2, "no row beyond the limit");
        let fields = text
            .lines()
            .nth(1)
            .expect("last permitted row")
            .split(',')
            .collect::<Vec<_>>();
        assert_eq!(fields.len(), HEADER.split(',').count());
        assert!(
            fields[10..18].iter().all(|v| v.is_empty()),
            "unobserved CPU is never fabricated zero"
        );
        assert!(FrameRecorder::disabled().cpu_snapshot().is_none());
    }
}
