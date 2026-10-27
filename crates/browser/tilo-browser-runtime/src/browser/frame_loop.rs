//! The page's one winit loop: the event handler that mounts a request,
//! dispatches each window's input to its mount and decides when the next
//! frame is due, and the watcher that drives a mount's first frame out and
//! says what graphics start-up is waiting on.
//!
//! Every entry point here runs on the page's thread, inside the loop or a
//! future the loop spawned; what it does to a mount goes through the
//! runtime's lease ([`super::with_mount`]), never through a second borrow.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::Function;
use wasm_bindgen::prelude::*;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::window::WindowId;

use tilo_desktop::feedback::PrivateFeedback;

use super::{
    Closure, DEFERRED_EVENTS, DeferredEvent, GPU_STEP, Gpu, GpuStep, LOOP, MOUNTS, Mount,
    PIPELINES_BUILT, REPLAYING, REQUESTS, SERVICING, STOP, admit_runtime, await_runtime_admission,
    begin, defer_event, emit, error, finish_shutdown, page_visible, pixels_waiting,
    poll_bump_counters, with_mount, with_mount_quietly,
};
use crate::first_frame;

mod timer {
    use js_sys::Function;
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_name = setTimeout)]
        pub(super) fn set_timeout(handler: &Function, ms: u32) -> JsValue;
        #[wasm_bindgen(js_name = requestAnimationFrame)]
        pub(super) fn request_animation_frame(handler: &JsValue) -> JsValue;
    }
}

/// Hand a timeout to the page's timer.
pub(super) fn set_timeout(handler: &Function, ms: u32) -> JsValue {
    timer::set_timeout(handler, ms)
}

/// Wait `ms` on the page's timer.
pub(crate) async fn sleep_ms(ms: u32) {
    let timer = js_sys::Promise::new(&mut |resolve, _| {
        let _ = set_timeout(&resolve, ms);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(timer).await;
}

/// The page's winit handler: each user event mounts a request or wakes one
/// mount, each window event is that window's input to its mount, and an idle
/// loop works out when the next frame is due.
pub(super) struct Runner;

impl Runner {
    fn replay_deferred(&mut self, event_loop: &ActiveEventLoop) {
        let pending = REPLAYING
            .with(|active| {
                SERVICING.with(|servicing| {
                    DEFERRED_EVENTS.with(|events| {
                        crate::mount_service_custody::replay_snapshot(
                            active,
                            servicing,
                            events,
                            |event| match event {
                                DeferredEvent::User(id) => self.user_event(event_loop, id),
                                DeferredEvent::Window(id, event) => {
                                    self.window_event(event_loop, id, event);
                                }
                            },
                        )
                    })
                })
            })
            .unwrap_or(false);
        if pending && let Some(proxy) = LOOP.with_borrow(Clone::clone) {
            // One later turn drains the retained tail; id 0 retains its existing
            // terminal meaning only while STOP is set.
            let _ = proxy.send_event(0);
        }
    }
}

impl ApplicationHandler<u64> for Runner {
    fn resumed(&mut self, _: &ActiveEventLoop) {}

    fn user_event(&mut self, event_loop: &ActiveEventLoop, id: u64) {
        let Some(DeferredEvent::User(id)) = defer_event(DeferredEvent::User(id)) else {
            return;
        };
        if id == 0 && STOP.with_borrow(Option::is_some) {
            event_loop.exit();
            return;
        }
        if id == 0 {
            self.replay_deferred(event_loop);
            return;
        }
        if REQUESTS.with_borrow(|requests| requests.contains_key(&id)) {
            let result = SERVICING.with(|servicing| {
                let _construction = crate::mount_service_custody::Construction::enter(servicing, id)
                    .map_err(|_| error("the runtime is busy (mount construction)"))?;
                let Some(request) = REQUESTS.with_borrow_mut(|requests| requests.remove(&id)) else { return Ok(()); };
                let status = request.status.clone();
                if let Err(e) = begin(event_loop, id, request) {
                    emit(&status, &serde_json::json!({"phase":"failed","error":e.as_string().unwrap_or_else(||format!("{e:?}"))}));
                }
                Ok::<(), JsValue>(())
            });
            if let Err(error) = result {
                web_sys::console::warn_1(&error);
                DEFERRED_EVENTS.with_borrow_mut(|events| events.push(DeferredEvent::User(id)));
            }
            self.replay_deferred(event_loop);
            return;
        }
        // Owner model work shows in the frame it asks for, which judges it.
        let result = with_mount_quietly(id, |m| {
            m.service_owner_models();
            if let Some(failure) = m.gpu.as_ref().and_then(Gpu::failure) {
                m.error = Some(failure);
                m.publish(id);
            } else {
                m.window.request_redraw();
            }
            Ok(())
        });
        if let Err(error) = result {
            web_sys::console::warn_1(&error);
        }
        self.replay_deferred(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(DeferredEvent::Window(window_id, event)) =
            defer_event(DeferredEvent::Window(window_id, event))
        else {
            return;
        };
        let id = MOUNTS.with_borrow(|mounts| {
            mounts
                .iter()
                .find(|(_, m)| m.window.id() == window_id)
                .map(|(&id, _)| id)
        });
        let Some(id) = id else {
            return;
        };
        // A redraw is the frame itself, and the effect of a wheel or of the
        // pointer moving is that frame's to judge (`Mount::draw`) when the
        // frame lays it out; every other event counts.
        let pointer_motion = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::CursorEntered { .. }
                | WindowEvent::CursorLeft { .. }
        );
        let quiet = pointer_motion
            || matches!(
                event,
                WindowEvent::RedrawRequested | WindowEvent::MouseWheel { .. }
            );
        let handle = |m: &mut Mount| {
            match event {
                WindowEvent::RedrawRequested => {
                    m.redraws = m.redraws.wrapping_add(1);
                    if let Err(e) = m.draw() {
                        m.error = Some(e);
                        m.publish(id);
                    }
                }
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                    if m.queue_resize() && !m.viewport.suspended() {
                        m.window.request_redraw();
                    }
                }
                WindowEvent::Occluded(hidden) => {
                    m.hidden = hidden;
                    if !hidden {
                        m.pending = true;
                        m.window.request_redraw();
                    }
                }
                WindowEvent::Destroyed => {}
                mut event => {
                    // The old layout stays on screen (stretched by CSS) until
                    // redraw commits the requested viewport; map pointer and
                    // touch coordinates back onto it.
                    match &mut event {
                        WindowEvent::CursorMoved { position, .. } => {
                            (position.x, position.y) =
                                m.viewport.map_pointer(position.x, position.y);
                        }
                        WindowEvent::Touch(touch) => {
                            (touch.location.x, touch.location.y) =
                                m.viewport.map_pointer(touch.location.x, touch.location.y);
                        }
                        _ => {}
                    }
                    let start = m.metric_start();
                    let passes = m.frame.relayout_frames();
                    crate::clipboard::during_input(&m.status, || m.frame.send_host_event(&event));
                    m.metric_end("input", start);
                    m.publish_committed(&event);
                    if !matches!(event, WindowEvent::Touch(_)) {
                        m.show_cursor();
                    }
                    if m.frame.relayout_frames() != passes {
                        m.note_measured();
                        // Laid out here, outside the frame, which then
                        // cannot tell what the pointer changed.
                        if pointer_motion {
                            m.entries = m.entries.wrapping_add(1);
                        }
                    }
                    m.pending = true;
                    m.window.request_redraw();
                }
            }
            Ok(())
        };
        let result = if quiet {
            with_mount_quietly(id, handle)
        } else {
            with_mount(id, handle)
        };
        if let Err(error) = result {
            web_sys::console::warn_1(&error);
        }
        self.replay_deferred(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if admit_runtime().is_err() {
            return;
        }
        let now = ui::time::now();
        let mut model_wakes = Vec::new();
        let mut deadline: Option<ui::time::Instant> = None;
        let mut wake =
            |at: ui::time::Instant| deadline = Some(deadline.map_or(at, |old| old.min(at)));
        if let Some(at) = poll_bump_counters().and_then(|again| now.checked_add(again)) {
            wake(at);
        }
        let ids = MOUNTS.with_borrow(|mounts| mounts.keys().copied().collect::<Vec<_>>());
        for id in ids {
            let _ = with_mount_quietly(id, |m| {
                let feedback_due = m.desktop.as_ref().and_then(|desktop| {
                    desktop
                        .shared
                        .feedback
                        .borrow()
                        .as_ref()
                        .and_then(PrivateFeedback::next_wake_deadline)
                });
                if let Some(next) = feedback_due {
                    let due = *m.feedback_retry.get_or_insert(next);
                    if due <= now {
                        model_wakes.push(id);
                    } else {
                        wake(due);
                    }
                } else {
                    m.feedback_retry = None;
                }
                if m.frame.has_pending_owner_models() {
                    let due = m.background_retry.unwrap_or(now);
                    if due <= now {
                        model_wakes.push(id);
                    } else {
                        wake(due);
                    }
                }
                // A throttled semantics tree is delivered at its deadline even
                // when no further frame is presented.
                let semantics_due = if m.hidden || m.error.is_some() {
                    None
                } else {
                    m.semantics.borrow().due()
                };
                if let Some(due) = semantics_due {
                    let drawable = !m.viewport.suspended() && m.gpu.is_some();
                    if due <= now && m.frame.replayed_draws() && drawable {
                        // The last frame replayed windows instead of drawing
                        // them; the next one draws every window and delivers
                        // the tree, which then needs no second draw.
                        m.pending = true;
                        m.window.request_redraw();
                    } else if due <= now {
                        m.publish(id);
                    } else {
                        wake(due);
                    }
                }
                if m.hidden || m.error.is_some() || m.gpu.is_none() || m.viewport.suspended() {
                    return Ok(());
                }
                m.pending |= m.gpu.as_ref().is_some_and(Gpu::shows_lost_frame);
                let output = m.frame.frame_schedule();
                let desktop_woken = m
                    .desktop
                    .as_ref()
                    .is_some_and(|d| d.shared.woken.load(std::sync::atomic::Ordering::Acquire));
                if m.pending
                    || desktop_woken
                    || m.viewport.needs_present()
                    || output.queued_work
                    || output.needs_animation_frame
                    || pixels_waiting()
                {
                    m.window.request_redraw();
                }
                if let Some(at) = output.next_wake_deadline {
                    if at <= now {
                        m.window.request_redraw();
                    } else {
                        wake(at);
                    }
                }
                Ok(())
            });
        }
        if let Some(proxy) = LOOP.with_borrow(Clone::clone) {
            for id in model_wakes {
                let _ = proxy.send_event(id);
            }
        }
        self.replay_deferred(event_loop);
        event_loop.set_control_flow(deadline.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }

    fn exiting(&mut self, _: &ActiveEventLoop) {
        finish_shutdown();
    }
}

/// Whether the page gets animation frames while a mount waits on its first
/// one: asked for once a tick by [`watch_first_frame`], counted as they come.
/// A redraw needs one, so a first frame that waits without them waits on the
/// browser, not the renderer.
#[derive(Default)]
struct FrameProbe {
    asked: bool,
    came: u32,
}

impl FrameProbe {
    fn ask(probe: &Rc<RefCell<Self>>) {
        if std::mem::replace(&mut probe.borrow_mut().asked, true) {
            return;
        }
        let answered = Rc::clone(probe);
        let _ = timer::request_animation_frame(&Closure::once_into_js(move || {
            let mut answered = answered.borrow_mut();
            answered.asked = false;
            answered.came = answered.came.saturating_add(1);
        }));
    }
}

/// Watch mount `id` until its first frame is out ([`first_frame::Watch`]):
/// say each step graphics start-up moves to (`gpuStep`), say when it stays
/// visible on one step without a frame or a pipeline compiled for
/// [`first_frame::STALL_MS`] (`gpuStalled`), so the page offers to try
/// again while it keeps waiting, and draw the first frame when the redraws
/// it asks for do not arrive ([`first_frame::Driver`]), through the same
/// draw a redraw runs.
pub(super) async fn watch_first_frame(id: u64) {
    let mut watch = first_frame::Watch::new(first_frame::DRIVE_MS);
    let mut driver = first_frame::Driver::default();
    let probe = Rc::new(RefCell::new(FrameProbe::default()));
    loop {
        FrameProbe::ask(&probe);
        sleep_ms(first_frame::DRIVE_MS).await;
        if !await_runtime_admission().await {
            return;
        }
        let watching = with_mount_quietly(id, |m| {
            if m.error.is_some() {
                return Ok(false);
            }
            if m.gpu.is_some() {
                let visible = page_visible() && !m.hidden && !m.viewport.suspended();
                let wants_frame = m.pending || m.viewport.needs_present();
                match driver.tick(m.redraws, m.frames, visible, wants_frame) {
                    first_frame::Drive::Done => {
                        // The pipelines the first frame did not need compile
                        // one at a time (vello's deferred pipelines), each
                        // started by the poll after the one before it is in,
                        // and only while the page is shown.
                        if !visible {
                            return Ok(true);
                        }
                        let pending = m.gpu.as_ref().map_or(0, |gpu| {
                            gpu.shared.try_borrow_mut().map_or(1, |mut shared| {
                                shared.renderer.deferred_pipelines().pending
                            })
                        });
                        return Ok(pending > 0);
                    }
                    first_frame::Drive::Wait => {}
                    first_frame::Drive::Draw => {
                        if let Err(e) = m.draw() {
                            m.error = Some(e);
                            m.publish(id);
                            return Ok(false);
                        }
                    }
                }
            }
            if m.frames > 0 {
                return Ok(true);
            }
            let step = if m.gpu.is_some() {
                GpuStep::FirstFrame
            } else {
                GPU_STEP.get()
            };
            let visible = !m.hidden && !m.viewport.suspended();
            let tick = watch.tick(step, PIPELINES_BUILT.get(), visible);
            if tick.step_changed {
                probe.borrow_mut().came = 0;
            }
            if tick.step_changed || tick.stalled.is_some() {
                let stalled = tick.stalled.unwrap_or(false);
                if stalled {
                    // Waiting on the first frame, say what held it back.
                    let why = if step == GpuStep::FirstFrame {
                        let deferred = DEFERRED_EVENTS
                            .with(|events| events.try_borrow().map_or(0, |e| e.len()));
                        format!(
                            " ({} redraws arrived, the last draw held by {}; {} animation frames came meanwhile, {deferred} events wait deferred)",
                            m.redraws,
                            m.held,
                            probe.borrow().came
                        )
                    } else {
                        String::new()
                    };
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "desktop: no first frame after {} s on one step, {}{why}; still waiting",
                        first_frame::STALL_MS / 1000,
                        step.waiting_on()
                    )));
                }
                emit(
                    &m.status,
                    &serde_json::json!({"phase":"gpu-pending","id":id,"gpuStep":step.name(),"gpuStalled":stalled}),
                );
            }
            Ok(true)
        });
        if !matches!(watching, Ok(true)) {
            return;
        }
    }
}
