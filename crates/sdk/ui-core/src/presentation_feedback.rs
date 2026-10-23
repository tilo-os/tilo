//! Opt-in native drawable feedback; independent of render-worker API completion.
#[cfg(all(target_os = "macos", feature = "presentation-feedback"))]
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc,
};

pub(crate) struct Observer {
    worker_id: u64,
    window: Option<std::sync::Arc<winit::window::Window>>,
    #[cfg(all(target_os = "macos", feature = "presentation-feedback"))]
    sender: mpsc::SyncSender<Sample>,
}

#[cfg(all(target_os = "macos", feature = "presentation-feedback"))]
struct Sample {
    worker_frame_ix: u64,
    cpu_frame_id: Option<u64>,
    presented_time_s: f64,
    drawable_id: u64,
}

impl Observer {
    pub(crate) fn worker_id(&self) -> Option<u64> {
        Some(self.worker_id)
    }

    /// Emitted only under the native timing gate. The caller brackets complete
    /// row construction and this emission, reporting that cost one frame later.
    pub(crate) fn log_native_handoff(&self, row: serde_json::Value) {
        let Some(row) = handoff_row(row, self.worker_id()) else {
            eprintln!("tilo-display-error handoff row is not an object");
            return;
        };
        crate::native_scenario::log::emit_timing_json("tilo-native-worker-handoff", row);
    }

    pub(crate) fn log_native_present(
        &self,
        worker_frame_ix: u64,
        cpu_frame_id: Option<u64>,
        bounds: crate::render_thread::timing::CallBounds,
    ) {
        crate::native_scenario::log::emit_timing_json(
            "tilo-native-present-call",
            serde_json::json!({
                "worker_id": self.worker_id(), "worker_frame_ix": worker_frame_ix,
                "cpu_frame_id": cpu_frame_id,
                "host_clock": crate::native_scenario::host_clock::CLOCK_NAME,
                "before_host_lo_s": bounds.before.map(|b| b.lo_s),
                "before_host_hi_s": bounds.before.map(|b| b.hi_s),
                "after_host_lo_s": bounds.after.map(|b| b.lo_s),
                "after_host_hi_s": bounds.after.map(|b| b.hi_s),
            }),
        );
    }

    pub(crate) fn log_native_worker(
        &self,
        worker_frame_ix: u64,
        cpu_frame_id: Option<u64>,
        bounds: (
            Option<crate::native_scenario::host_clock::HostClockBracket>,
            Option<crate::native_scenario::host_clock::HostClockBracket>,
            Option<crate::native_scenario::host_clock::HostClockBracket>,
        ),
        outcome: crate::render_thread::FrameOutcome,
    ) {
        let (receive, start, end) = bounds;
        crate::native_scenario::log::emit_timing_json(
            "tilo-native-worker",
            serde_json::json!({
                "worker_id": self.worker_id(), "worker_frame_ix": worker_frame_ix,
                "cpu_frame_id": cpu_frame_id,
                "host_clock": crate::native_scenario::host_clock::CLOCK_NAME,
                "receive_host_lo_s": receive.map(|b| b.lo_s),
                "receive_host_hi_s": receive.map(|b| b.hi_s),
                "render_start_host_lo_s": start.map(|b| b.lo_s),
                "render_start_host_hi_s": start.map(|b| b.hi_s),
                "render_end_host_lo_s": end.map(|b| b.lo_s),
                "render_end_host_hi_s": end.map(|b| b.hi_s),
                "outcome": format!("{outcome:?}"),
            }),
        );
    }

    pub(crate) fn log_native_config(&self, surface: &vello::util::RenderSurface<'_>) {
        #[cfg(all(target_os = "macos", feature = "presentation-feedback"))]
        let readback = {
            // SAFETY: borrow the configured HAL surface only to read layer
            // properties under its existing lock; retain no raw handles.
            unsafe { surface.surface.as_hal::<vello::wgpu::hal::api::Metal>() }.map(|metal| {
                let layer = metal.render_layer().lock();
                (
                    layer.maximumDrawableCount(),
                    layer.displaySyncEnabled(),
                    layer.presentsWithTransaction(),
                )
            })
        };
        #[cfg(not(all(target_os = "macos", feature = "presentation-feedback")))]
        let readback: Option<(usize, bool, bool)> = None;
        crate::native_scenario::log::emit_timing_json(
            "tilo-native-render-config",
            serde_json::json!({
                "worker_id": self.worker_id(),
                "requested_present_mode": format!("{:?}", surface.config.present_mode),
                "requested_maximum_frame_latency": surface.config.desired_maximum_frame_latency,
                "width": surface.config.width, "height": surface.config.height,
                "effective_config_available": readback.is_some(),
                "maximum_drawable_count": readback.map(|r| r.0),
                "display_sync_enabled": readback.map(|r| r.1),
                "presents_with_transaction": readback.map(|r| r.2),
            }),
        );
    }

    pub(crate) fn from_env(_window: Option<std::sync::Arc<winit::window::Window>>) -> Option<Self> {
        if !std::env::var("TILO_PROFILE_PRESENTATION").is_ok_and(|value| value == "1") {
            return None;
        }
        #[cfg(all(target_os = "macos", feature = "presentation-feedback"))]
        {
            static NEXT_WORKER: AtomicU64 = AtomicU64::new(1);
            let worker_id = NEXT_WORKER.fetch_add(1, Ordering::Relaxed);
            let (sender, receiver) = mpsc::sync_channel::<Sample>(4096);
            let result = std::thread::Builder::new().name("ui-presentation-log".into()).spawn(move || {
                while let Ok(sample) = receiver.recv() {
                    // Serialize off the Metal callback thread. A missing record,
                    // including queue overflow or I/O failure, fails completeness.
                    let row = serde_json::json!({
                        "worker_id": worker_id, "worker_frame_ix": sample.worker_frame_ix,
                        "cpu_frame_id": sample.cpu_frame_id, "presented_time_s": sample.presented_time_s,
                        "drawable_id": sample.drawable_id, "backend": "metal",
                    });
                    let _ = crate::native_scenario::log::write_json("tilo-display-frame", row);
                }
            });
            if let Err(error) = result {
                eprintln!("tilo-display-error {error}");
                return None;
            }
            Some(Self {
                worker_id,
                sender,
                window: _window,
            })
        }
        #[cfg(not(all(target_os = "macos", feature = "presentation-feedback")))]
        {
            if tilo_presentation::supported() {
                use std::sync::atomic::{AtomicU64, Ordering};
                static NEXT_WORKER: AtomicU64 = AtomicU64::new(1);
                return Some(Self {
                    worker_id: NEXT_WORKER.fetch_add(1, Ordering::Relaxed),
                    window: _window,
                });
            }
            eprintln!(
                "tilo-display-error presentation feedback requires a supported native backend"
            );
            None
        }
    }

    pub(crate) fn present_wayland(
        &self,
        worker_frame_ix: u64,
        cpu_frame_id: Option<u64>,
        present: impl FnOnce(),
    ) {
        if !tilo_presentation::supported() {
            present();
            return;
        }
        use tilo_presentation::Feedback;
        let worker_id = self.worker_id;
        let process_id = std::process::id();
        let observer_session = crate::native_scenario::observer_session_id();
        let Some(window) = self.window.as_ref() else {
            eprintln!("tilo-display-error native window missing for Wayland presentation feedback");
            present();
            return;
        };
        let accepted = tilo_presentation::present(
            window,
            Box::new(move |result| {
                let row = match result {
                    Feedback::Presented {
                        clock_id,
                        timestamp,
                        refresh,
                        sequence,
                        flags,
                    } => serde_json::json!({
                        "worker_id": worker_id, "worker_frame_ix": worker_frame_ix, "process_id": process_id,
                        "observer_session": observer_session,
                        "cpu_frame_id": cpu_frame_id, "backend": "wayland",
                        "clock_id": clock_id, "presented_time_ns": timestamp.as_nanos(),
                        "refresh_ns": refresh.as_nanos(), "sequence": sequence, "flags": flags,
                        "discarded": false,
                    }),
                    Feedback::Discarded => serde_json::json!({
                        "worker_id": worker_id, "worker_frame_ix": worker_frame_ix, "process_id": process_id,
                        "observer_session": observer_session,
                        "cpu_frame_id": cpu_frame_id, "backend": "wayland", "discarded": true,
                    }),
                };
                crate::native_scenario::log::emit_json("tilo-display-frame", row);
            }),
            present,
        );
        if !accepted {
            eprintln!("tilo-display-error compositor lacks standard Wayland presentation feedback");
        }
    }
    #[cfg_attr(
        not(all(target_os = "macos", feature = "presentation-feedback")),
        allow(
            clippy::unused_self,
            reason = "the enabled backend reads the observer sender and identity"
        )
    )]
    pub(crate) fn arm(
        &self,
        surface: &vello::wgpu::Surface<'_>,
        worker_frame_ix: u64,
        cpu_frame_id: Option<u64>,
    ) {
        #[cfg(all(target_os = "macos", feature = "presentation-feedback"))]
        {
            // SAFETY: we only register a diagnostic observer. No raw resource is
            // destroyed or retained; this guard ends before SurfaceTexture::present.
            let Some(metal) = (unsafe { surface.as_hal::<vello::wgpu::hal::api::Metal>() }) else {
                eprintln!("tilo-display-error active surface is not Metal");
                return;
            };
            let sender = self.sender.clone();
            let result =
                metal.observe_next_present(Arc::new(move |presented_time_s, drawable_id| {
                    // Bounded and nonblocking; do not serialize, log, or call window
                    // APIs on Metal's callback thread. Lost samples cannot qualify.
                    let _ = sender.try_send(Sample {
                        worker_frame_ix,
                        cpu_frame_id,
                        presented_time_s,
                        drawable_id,
                    });
                }));
            if let Err(error) = result {
                eprintln!("tilo-display-error {error}");
                return;
            }
            crate::native_scenario::log::emit_json(
                "tilo-display-request",
                serde_json::json!({
                    "worker_id": self.worker_id, "worker_frame_ix": worker_frame_ix, "cpu_frame_id": cpu_frame_id,
                }),
            );
        }
        #[cfg(not(all(target_os = "macos", feature = "presentation-feedback")))]
        let _ = (surface, worker_frame_ix, cpu_frame_id);
    }
}

fn handoff_row(mut row: serde_json::Value, worker_id: Option<u64>) -> Option<serde_json::Value> {
    row.as_object_mut()?
        .insert("worker_id".into(), serde_json::json!(worker_id));
    Some(row)
}

#[cfg(test)]
mod tests {
    use super::handoff_row;

    #[test]
    fn handoff_rows_keep_identity_and_refuse_malformed_emission() {
        {
            let original =
                serde_json::json!({"cpu_frame_id": 42, "before_receive_host_lo_s": null});
            for worker in [None, Some(9)] {
                let row = handoff_row(original.clone(), worker).unwrap();
                assert_eq!(row["cpu_frame_id"], 42);
                assert!(row["before_receive_host_lo_s"].is_null());
                assert_eq!(row["worker_id"], serde_json::json!(worker));
                assert_eq!(row.as_object().unwrap().len(), 3);
            }
        }

        {
            for row in [
                serde_json::Value::Null,
                serde_json::json!([]),
                serde_json::json!(3),
            ] {
                assert!(handoff_row(row, Some(1)).is_none());
            }
        }
    }
}
