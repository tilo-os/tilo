//! One-frame delayed epilogue diagnostics. No queues or default-path clock work.
use super::{HostClockBracket, capture};

#[derive(Clone, Copy)]
pub(crate) enum Stage {
    RenderEnd,
    AfterNativeWorkerLog,
    AfterOutcomeLog,
    AfterSceneRecycle,
    AfterPoolSend,
    AfterOutcomeSend,
    AfterCompletionNotify,
}
const NAMES: [&str; 7] = [
    "previous_render_end",
    "previous_after_native_worker_log",
    "previous_after_outcome_log",
    "previous_after_scene_recycle",
    "previous_after_pool_send",
    "previous_after_outcome_send",
    "previous_after_completion_notify",
];
struct Frame {
    ix: u64,
    cpu: Option<u64>,
    points: [Option<HostClockBracket>; 7],
    log_before: Option<HostClockBracket>,
    log_after: Option<HostClockBracket>,
}
#[derive(Default)]
pub(crate) struct Recorder {
    frame: Option<Frame>,
}

pub(crate) fn received(
    recorder: &mut Option<Recorder>,
    ix: u64,
    cpu: Option<u64>,
    before: Option<HostClockBracket>,
    after: Option<HostClockBracket>,
    emit: impl FnOnce(serde_json::Value),
) {
    received_with(recorder, ix, cpu, before, after, capture, emit);
}
fn received_with(
    recorder: &mut Option<Recorder>,
    ix: u64,
    cpu: Option<u64>,
    before: Option<HostClockBracket>,
    after: Option<HostClockBracket>,
    mut clock: impl FnMut() -> Option<HostClockBracket>,
    emit: impl FnOnce(serde_json::Value),
) {
    if let Some(recorder) = recorder {
        let log_before = clock();
        let row = recorder.row(ix, cpu, before, after);
        emit(row);
        let log_after = clock();
        recorder.frame = Some(Frame {
            ix,
            cpu,
            points: [None; 7],
            log_before,
            log_after,
        });
    }
}
impl Recorder {
    fn row(
        &self,
        ix: u64,
        cpu: Option<u64>,
        before: Option<HostClockBracket>,
        after: Option<HostClockBracket>,
    ) -> serde_json::Value {
        let previous = self.frame.as_ref();
        let mut row = serde_json::json!({
            "worker_frame_ix": ix, "cpu_frame_id": cpu,
            "previous_worker_frame_ix": previous.map(|frame| frame.ix),
            "previous_cpu_frame_id": previous.and_then(|frame| frame.cpu),
            "host_clock": crate::native_scenario::host_clock::CLOCK_NAME,
        });
        for (index, name) in NAMES.into_iter().enumerate() {
            put_bounds(
                &mut row,
                name,
                previous.and_then(|frame| frame.points.get(index).copied().flatten()),
            );
        }
        put_bounds(&mut row, "before_receive", before);
        put_bounds(&mut row, "after_receive", after);
        put_bounds(
            &mut row,
            "previous_handoff_log_before",
            previous.and_then(|frame| frame.log_before),
        );
        put_bounds(
            &mut row,
            "previous_handoff_log_after",
            previous.and_then(|frame| frame.log_after),
        );
        row
    }
}
fn put_bounds(row: &mut serde_json::Value, name: &str, bounds: Option<HostClockBracket>) {
    row[format!("{name}_host_lo_s")] = serde_json::json!(bounds.map(|bounds| bounds.lo_s));
    row[format!("{name}_host_hi_s")] = serde_json::json!(bounds.map(|bounds| bounds.hi_s));
}

pub(crate) fn set(recorder: &mut Option<Recorder>, stage: Stage, bounds: Option<HostClockBracket>) {
    if let Some(point) = recorder
        .as_mut()
        .and_then(|recorder| recorder.frame.as_mut())
        .and_then(|frame| frame.points.get_mut(stage as usize))
    {
        *point = bounds;
    }
}
pub(crate) fn record(recorder: &mut Option<Recorder>, stage: Stage) {
    resource_with(recorder, stage, capture);
}
fn resource_with(
    recorder: &mut Option<Recorder>,
    stage: Stage,
    clock: impl FnOnce() -> Option<HostClockBracket>,
) {
    if recorder.is_some() {
        set(recorder, stage, clock());
    }
}
