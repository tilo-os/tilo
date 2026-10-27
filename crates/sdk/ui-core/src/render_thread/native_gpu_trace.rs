//! Default-off native Vello samples, attributed to their originating recording.
//! No GPU waits: the render worker drives existing map callbacks with Poll.
use std::collections::VecDeque;

use serde_json::{Value, json};
use vello::{GpuDispatchTimestampStatus, GpuTimestampStatus, Renderer};

const OWNER_LIMIT: usize = 32;
const ROW_LIMIT: usize = 2048;
const IDLE_POLLS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub worker_id: u64,
    pub worker_frame_ix: u64,
    pub cpu_frame_id: Option<u64>,
}

struct Reading {
    kind: &'static str,
    latest: Option<u64>,
    status: String,
    supported: bool,
    pending: bool,
    sample: Option<(u64, Value)>,
}

fn readings(renderer: &mut Renderer) -> [Reading; 2] {
    let status = renderer.gpu_timestamp_status();
    let passes = Reading {
        kind: "passes",
        latest: renderer.gpu_timestamp_latest_submission_id(),
        supported: !matches!(
            status,
            GpuTimestampStatus::Disabled | GpuTimestampStatus::Unsupported
        ),
        pending: matches!(status, GpuTimestampStatus::Pending { .. }),
        status: format!("{status:?}"),
        sample: renderer.take_gpu_timestamp_sample().map(|sample| {
            (
                sample.submission_id,
                json!({
                    "pass_count": sample.pass_count, "span_ns": sample.span_ns,
                    "summed_pass_ns": sample.summed_pass_ns,
                }),
            )
        }),
    };
    let status = renderer.gpu_dispatch_timestamp_status();
    let dispatch = Reading {
        kind: "dispatches",
        latest: renderer.gpu_dispatch_timestamp_latest_submission_id(),
        supported: !matches!(status, GpuDispatchTimestampStatus::Disabled | GpuDispatchTimestampStatus::Unsupported),
        pending: matches!(status, GpuDispatchTimestampStatus::Pending { .. }),
        status: format!("{status:?}"),
        sample: renderer.take_gpu_dispatch_timestamp_sample().map(|sample| {
            let stages: Vec<_> = sample.stages.iter().take(usize::try_from(sample.stage_count).unwrap_or(0).min(sample.stages.len())).map(|stage| json!({
                "stage_id": stage.stage_id,
                "stage": vello::gpu_dispatch_timestamp_stage_name(stage.stage_id).unwrap_or("unknown"),
                "dispatch_count": stage.dispatch_count,
                "duration_ns": stage.duration_ns.to_string(),
            })).collect();
            (sample.submission_id, json!({"stage_count": sample.stage_count, "stages": stages}))
        }),
    };
    [passes, dispatch]
}

#[derive(Default)]
struct Ledger {
    last: Option<u64>,
    owners: VecDeque<(u64, Frame)>,
}

/// One recorder belongs to one render worker / renderer lifetime. IDs from a
/// resumed or replaced renderer are never compared with another recorder.
pub(crate) struct Observer {
    ledgers: [Ledger; 2],
    rows_left: usize,
    idle_polls_left: usize,
    pending: bool,
    worker_id: u64,
}

impl Observer {
    pub(crate) fn from_environment(worker_id: u64) -> Option<Self> {
        enabled(
            crate::host::environment().diagnostics.gpu_timestamps,
            worker_id,
        )
    }

    pub(crate) fn frame(
        &mut self,
        renderer: &mut Renderer,
        worker_frame_ix: u64,
        cpu_frame_id: Option<u64>,
    ) {
        self.observe(
            renderer,
            Some(Frame {
                worker_id: self.worker_id,
                worker_frame_ix,
                cpu_frame_id,
            }),
        );
    }

    pub(crate) fn poll_pending(&self) -> bool {
        self.pending && self.rows_left > 0 && self.idle_polls_left > 0
    }

    pub(crate) fn idle_poll(&mut self, renderer: &mut Renderer) {
        self.idle_polls_left = self.idle_polls_left.saturating_sub(1);
        self.observe(renderer, None);
    }

    /// Drain BEFORE the next Vello begin_recording can discard a ready sample.
    /// After rendering, `frame` registers changed recording IDs before draining.
    pub(crate) fn observe(&mut self, renderer: &mut Renderer, frame: Option<Frame>) {
        if self.rows_left == 0 {
            return;
        }
        let rows = self.read(readings(renderer), frame);
        for row in rows {
            crate::native_scenario::log::emit_timing_json("tilo-native-gpu-stages", row);
        }
    }

    fn read(&mut self, readings: [Reading; 2], frame: Option<Frame>) -> Vec<Value> {
        let mut rows = Vec::new();
        self.pending = readings.iter().any(|reading| reading.pending);
        for (ledger, reading) in self.ledgers.iter_mut().zip(readings) {
            if let Some(frame) = frame {
                self.idle_polls_left = IDLE_POLLS;
                let changed = reading.latest.is_some() && reading.latest != ledger.last;
                if changed && let Some(id) = reading.latest {
                    if ledger.owners.len() == OWNER_LIMIT
                        && let Some(evicted) = ledger.owners.pop_front()
                    {
                        rows.push(row("owner_evicted", &reading, Some(evicted), None));
                    }
                    ledger.owners.push_back((id, frame));
                    ledger.last = Some(id);
                }
                let mut attempt = row(
                    "attempt",
                    &reading,
                    reading.latest.filter(|_| changed).map(|id| (id, frame)),
                    None,
                );
                attempt["worker_id"] = json!(frame.worker_id);
                attempt["worker_frame_ix"] = json!(frame.worker_frame_ix);
                attempt["cpu_frame_id"] = json!(frame.cpu_frame_id);
                rows.push(attempt);
            }
            if frame.is_none() {
                ledger.last = reading.latest;
            }
            if let Some((id, sample)) = reading.sample.as_ref() {
                let owner = ledger
                    .owners
                    .iter()
                    .position(|(known, _)| known == id)
                    .and_then(|position| ledger.owners.remove(position));
                rows.push(row(
                    "completed",
                    &reading,
                    owner,
                    Some((*id, sample.clone())),
                ));
            }
        }
        rows.truncate(self.rows_left);
        self.rows_left = self.rows_left.saturating_sub(rows.len());
        rows
    }
}

fn enabled(requested: bool, worker_id: u64) -> Option<Observer> {
    requested.then(|| Observer {
        ledgers: std::array::from_fn(|_| Ledger::default()),
        rows_left: ROW_LIMIT,
        idle_polls_left: IDLE_POLLS,
        pending: false,
        worker_id,
    })
}

fn row(
    event: &str,
    reading: &Reading,
    owner: Option<(u64, Frame)>,
    sample: Option<(u64, Value)>,
) -> Value {
    let matched = sample
        .as_ref()
        .is_some_and(|(id, _)| owner.is_some_and(|(known, _)| known == *id));
    json!({
        "event": event, "kind": reading.kind, "status": reading.status,
        "supported": reading.supported, "pending": reading.pending,
        "worker_id": owner.map(|(_, frame)| frame.worker_id),
        "worker_frame_ix": owner.map(|(_, frame)| frame.worker_frame_ix),
        "cpu_frame_id": owner.and_then(|(_, frame)| frame.cpu_frame_id),
        "expected_current_submission_id": owner.map(|(id, _)| id),
        "latest_submission_id": reading.latest,
        "sample_submission_id": sample.as_ref().map(|(id, _)| *id),
        "matched_current": matched, "missing_current": !matched,
        "stale_sample": sample.is_some() && !matched,
        "delayed": matched && sample.as_ref().map(|(id, _)| *id) != reading.latest,
        "sample": sample.map(|(_, value)| value),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(ix: u64) -> Frame {
        Frame {
            worker_id: 7,
            worker_frame_ix: ix,
            cpu_frame_id: Some(ix + 100),
        }
    }

    fn reading(latest: u64, sample: Option<u64>) -> [Reading; 2] {
        ["passes", "dispatches"].map(|kind| Reading {
            kind,
            latest: Some(latest),
            status: "Pending".into(),
            supported: true,
            pending: sample.is_none(),
            sample: sample.map(|id| (id, json!({"duration_ns": 123}))),
        })
    }

    #[test]
    fn delayed_sample_keeps_original_frame_and_cannot_settle_newer_frame() {
        let mut observer = enabled(true, 7).unwrap();
        let first = observer.read(reading(1, None), Some(frame(10)));
        assert!(first.iter().all(|row| row["missing_current"] == true));
        observer.read(reading(2, None), Some(frame(11)));
        let rows = observer.read(reading(2, Some(1)), None);
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert_eq!(row["worker_frame_ix"], 10);
            assert_eq!(row["cpu_frame_id"], 110);
            assert_eq!(row["worker_id"], 7);
            assert_eq!(row["matched_current"], true);
            assert_eq!(row["delayed"], true);
            assert_eq!(row["sample_submission_id"], 1);
        }
        let rows = observer.read(reading(2, Some(1)), None);
        assert!(
            rows.iter()
                .all(|row| row["stale_sample"] == true && row["cpu_frame_id"].is_null())
        );
    }

    #[test]
    fn immediate_missing_unknown_and_evicted_samples_are_explicit() {
        let mut observer = enabled(true, 7).unwrap();
        let rows = observer.read(reading(1, Some(1)), Some(frame(1)));
        assert_eq!(
            rows.iter()
                .filter(|row| row["matched_current"] == true)
                .count(),
            2
        );
        for id in 2..36 {
            observer.read(reading(id, None), Some(frame(id)));
        }
        let rows = observer.read(reading(35, Some(2)), None);
        assert!(rows.iter().all(|row| row["stale_sample"] == true));
        assert!(
            observer
                .ledgers
                .iter()
                .all(|ledger| ledger.owners.len() == OWNER_LIMIT)
        );
    }

    #[test]
    fn disabled_unsupported_and_row_budget_never_poll_indefinitely() {
        assert!(enabled(false, 7).is_none());
        let mut observer = enabled(true, 7).unwrap();
        let mut readings = reading(1, None);
        for reading in &mut readings {
            reading.supported = false;
            reading.pending = false;
            reading.latest = None;
            reading.status = "Unsupported".into();
        }
        let rows = observer.read(readings, Some(frame(0)));
        assert!(
            rows.iter()
                .all(|row| row["supported"] == false && row["missing_current"] == true)
        );
        assert!(!observer.poll_pending());
        for id in 1..2000 {
            observer.read(reading(id, None), Some(frame(id)));
        }
        assert_eq!(observer.rows_left, 0);
        assert!(!observer.poll_pending());
        assert!(observer.read(reading(2000, Some(1)), None).is_empty());
    }
    #[test]
    fn real_vello_recording_reports_only_its_owned_native_frame() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("VELLO_GPU_TIMESTAMPS", "1");
        env.set("VELLO_GPU_DISPATCH_TIMESTAMPS", "1");
        let instance = vello::wgpu::Instance::new(
            vello::wgpu::InstanceDescriptor::new_without_display_handle(),
        );
        let adapter = pollster::block_on(
            instance.request_adapter(&vello::wgpu::RequestAdapterOptions::default()),
        )
        .expect("native timestamp GPU adapter");
        let features = vello::wgpu::Features::TIMESTAMP_QUERY
            | vello::wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES;
        assert!(
            adapter.features().contains(features),
            "timestamp fixture must observe the supported GPU path"
        );
        let (device, queue) =
            pollster::block_on(adapter.request_device(&vello::wgpu::DeviceDescriptor {
                required_features: features,
                ..Default::default()
            }))
            .expect("timestamp fixture GPU device");
        let mut renderer = Renderer::new(
            &device,
            vello::RendererOptions {
                use_cpu: false,
                antialiasing_support: vello::AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .expect("native Vello renderer");
        let texture = device.create_texture(&vello::wgpu::TextureDescriptor {
            label: Some("native GPU attribution oracle"),
            size: vello::wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: vello::wgpu::TextureDimension::D2,
            format: vello::wgpu::TextureFormat::Rgba8Unorm,
            usage: vello::wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        let mut scene = vello::Scene::new();
        scene.fill(
            vello::peniko::Fill::NonZero,
            vello::kurbo::Affine::IDENTITY,
            vello::peniko::Color::WHITE,
            None,
            &vello::kurbo::Rect::new(2.0, 2.0, 60.0, 60.0),
        );
        let mut observer = enabled(true, 7).unwrap();
        observer.read(readings(&mut renderer), None);
        renderer
            .render_to_texture(
                &device,
                &queue,
                &scene,
                &texture.create_view(&Default::default()),
                &vello::RenderParams {
                    base_color: vello::peniko::Color::BLACK,
                    width: 64,
                    height: 64,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .expect("real native Vello submission");
        let mut rows = observer.read(readings(&mut renderer), Some(frame(40)));
        device
            .poll(vello::wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(tilo_test_env::hang_bound()),
            })
            .expect("test-only completion of actual GPU submission");
        rows.extend(observer.read(readings(&mut renderer), None));
        for kind in ["passes", "dispatches"] {
            let matches: Vec<_> = rows
                .iter()
                .filter(|row| row["kind"] == kind && row["matched_current"] == true)
                .collect();
            assert_eq!(matches.len(), 1, "one owned completion per actual tracker");
            assert_eq!(matches[0]["worker_frame_ix"], 40);
            assert_eq!(matches[0]["cpu_frame_id"], 140);
            assert_eq!(matches[0]["worker_id"], 7);
            assert_eq!(matches[0]["supported"], true);
            assert_eq!(matches[0]["stale_sample"], false);
        }
        assert!(
            observer.read(readings(&mut renderer), None).is_empty(),
            "consumed samples never attach twice"
        );
    }
}
