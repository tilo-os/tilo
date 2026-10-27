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
    record_with(recorder, stage, capture);
}
fn record_with(
    recorder: &mut Option<Recorder>,
    stage: Stage,
    clock: impl FnOnce() -> Option<HostClockBracket>,
) {
    if recorder.is_some() {
        set(recorder, stage, clock());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn point(n: u32) -> Option<HostClockBracket> {
        Some(HostClockBracket {
            lo_s: f64::from(n),
            hi_s: f64::from(n) + 0.1,
        })
    }
    #[test]
    fn handoff_rows_associate_the_right_frame_and_never_fabricate_clocks() {
        {
            let mut recorder = None;
            received_with(
                &mut recorder,
                0,
                None,
                None,
                None,
                || panic!("disabled clock"),
                |_| panic!("disabled JSON emission"),
            );
            record_with(&mut recorder, Stage::AfterPoolSend, || {
                panic!("disabled stage clock")
            });
        }

        {
            let mut recorder = Some(Recorder::default());
            let ticks = std::cell::Cell::new(2);
            let mut first = None;
            received_with(
                &mut recorder,
                0,
                None,
                point(0),
                point(1),
                || {
                    ticks.set(ticks.get() + 1);
                    point(ticks.get())
                },
                |row| {
                    first = Some(row);
                    ticks.set(ticks.get() + 10);
                },
            );
            let first = first.unwrap();
            assert_eq!(first["worker_frame_ix"], 0);
            assert!(first["cpu_frame_id"].is_null());
            assert!(first["previous_worker_frame_ix"].is_null());
            for name in NAMES
                .into_iter()
                .chain(["previous_handoff_log_before", "previous_handoff_log_after"])
            {
                for side in ["lo", "hi"] {
                    let key = format!("{name}_host_{side}_s");
                    assert!(first.get(&key).is_some(), "missing explicit null {key}");
                    assert!(first[&key].is_null());
                }
            }
            for (index, stage) in [
                Stage::RenderEnd,
                Stage::AfterNativeWorkerLog,
                Stage::AfterOutcomeLog,
                Stage::AfterSceneRecycle,
                Stage::AfterPoolSend,
                Stage::AfterOutcomeSend,
                Stage::AfterCompletionNotify,
            ]
            .into_iter()
            .enumerate()
            {
                record_with(&mut recorder, stage, || point(30 + index as u32));
            }
            let mut second = None;
            received_with(
                &mut recorder,
                1,
                Some(353),
                point(40),
                point(41),
                || point(42),
                |row| second = Some(row),
            );
            let second = second.unwrap();
            assert_eq!(second["worker_frame_ix"], 1);
            assert_eq!(second["cpu_frame_id"], 353);
            assert_eq!(second["previous_worker_frame_ix"], 0);
            assert!(
                second["previous_cpu_frame_id"].is_null(),
                "auxiliary is not CPU frame zero"
            );
            assert_eq!(second["previous_handoff_log_before_host_lo_s"], 3.0);
            assert_eq!(second["previous_handoff_log_after_host_lo_s"], 14.0);
            assert_eq!(second["before_receive_host_lo_s"], 40.0);
            assert_eq!(second["after_receive_host_lo_s"], 41.0);
            for (index, name) in NAMES.into_iter().enumerate() {
                assert_eq!(second[format!("{name}_host_lo_s")], (30 + index) as f64);
            }
            // The current final received frame already has a row; no next input is
            // needed to emit it. Only its outgoing epilogue awaits a future receive.
            assert_eq!(recorder.unwrap().frame.unwrap().ix, 1);
        }

        {
            let mut recorder = Some(Recorder::default());
            received_with(&mut recorder, 8, Some(90), None, None, || None, |_| {});
            set(&mut recorder, Stage::AfterOutcomeSend, point(30));
            received_with(
                &mut recorder,
                9,
                None,
                None,
                None,
                || None,
                |row| {
                    assert_eq!(row["previous_cpu_frame_id"], 90);
                    assert!(row["previous_after_pool_send_host_lo_s"].is_null());
                    assert_eq!(row["previous_after_outcome_send_host_lo_s"], 30.0);
                    assert!(row["after_receive_host_lo_s"].is_null());
                },
            );
        }
    }
}
