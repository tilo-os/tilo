//! Opt-in receipt-to-scene identity; not evidence of input causality.
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::{AtomicU64, Ordering};
use std::{cell::RefCell, sync::OnceLock};

#[derive(Default)]
struct SceneFrames {
    ids: Vec<u64>,
    overflow: bool,
}
impl SceneFrames {
    fn record(&mut self, id: u64) {
        if self.ids.contains(&id) {
            return;
        }
        if self.ids.len() == 64 {
            self.overflow = true;
        } else {
            self.ids.push(id);
        }
    }
    #[cfg(feature = "native")]
    fn take(&mut self) -> (Vec<u64>, bool) {
        (
            std::mem::take(&mut self.ids),
            std::mem::take(&mut self.overflow),
        )
    }
}
thread_local! { static DRAWN: RefCell<SceneFrames> = RefCell::default(); }
fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("TILO_WEBVIEW_FRAME_TRACE").is_ok_and(|v| v == "1"))
}
/// Unique within this Rust process, including across native helpers and tabs.
/// Receipt is after transport/decode; it is not the CEF paint timestamp.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn received(tab: u32, rgba: &[u8], width: u32, height: u32) -> Option<u64> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    static SCROLL_MARKER: OnceLock<bool> = OnceLock::new();
    if !enabled() {
        return None;
    }
    let id = NEXT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .ok()?;
    let (_, host) = crate::native_scenario::host_clock::sample();
    let scroll_marker_delta = if *SCROLL_MARKER
        .get_or_init(|| std::env::var("TILO_WEBVIEW_SCROLL_MARKER").is_ok_and(|v| v == "1"))
    {
        super::scroll_marker::decode(rgba, width, height)
    } else {
        None
    };
    crate::native_scenario::log::emit_json(
        "tilo-webview-received",
        serde_json::json!({
            "frame_id": id, "tab_id": tab, "process_id": std::process::id(),
            "scroll_marker_delta": scroll_marker_delta,
            "host_clock": crate::native_scenario::host_clock::CLOCK_NAME,
            "received_host_lo_s": host.map(|h| h.lo_s), "received_host_hi_s": host.map(|h| h.hi_s),
        }),
    );
    Some(id)
}
/// The browser build has no native scenario log to receive frames into.
#[cfg(target_arch = "wasm32")]
pub(crate) fn received(_: u32, _: &[u8], _: u32, _: u32) -> Option<u64> {
    None
}
/// Discard observations from any offscreen draw before this window scene.
#[cfg(feature = "native")]
pub(crate) fn begin_scene() {
    if enabled() {
        DRAWN.with(|scene| {
            scene.borrow_mut().take();
        });
    }
}
pub(crate) fn drawn(id: Option<u64>) {
    if let Some(id) = id {
        DRAWN.with(|scene| scene.borrow_mut().record(id));
    }
}
/// Called after recording a complete scene, even when it has no fresh input.
/// Missing CPU IDs or overflow must be rejected by the measurement analyzer.
#[cfg(feature = "native")]
pub(crate) fn finish_scene(cpu_frame_id: Option<u64>) {
    if !enabled() {
        return;
    }
    let (ids, overflow) = DRAWN.with(|scene| scene.borrow_mut().take());
    crate::native_scenario::log::emit_json(
        "tilo-webview-scene",
        serde_json::json!({
            "process_id": std::process::id(), "cpu_frame_id": cpu_frame_id,
            "webview_frame_ids": ids, "overflow": overflow,
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frame_traces_are_per_scene_and_overflow_explicitly() {
        // phase: scenes_keep_repeated_cached_frames_but_do_not_leak_previous_scene
        {
            let mut scene = SceneFrames::default();
            scene.record(7);
            scene.record(7);
            scene.record(9);
            assert_eq!(scene.take(), (vec![7, 9], false));
            assert_eq!(scene.take(), (vec![], false));
            scene.record(7);
            assert_eq!(scene.take(), (vec![7], false));
        }
        // phase: overflow_is_explicit_and_resets_for_the_next_scene
        {
            let mut scene = SceneFrames::default();
            for id in 1..=65 {
                scene.record(id);
            }
            let (ids, overflow) = scene.take();
            assert_eq!(ids.len(), 64);
            assert!(overflow);
            assert_eq!(scene.take(), (vec![], false));
        }
    }
}

#[cfg(test)]
pub(crate) fn take_for_test() -> (Vec<u64>, bool) {
    DRAWN.with(|scene| scene.borrow_mut().take())
}
