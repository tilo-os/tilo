//! Exact committed-buffer feedback through the standard presentation protocol.
use std::sync::{Arc, Mutex};

use sctk::reexports::client::globals::{BindError, GlobalList};
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{delegate_dispatch, Connection, Dispatch, QueueHandle};
use sctk::reexports::protocols::wp::presentation_time::client::{
    wp_presentation, wp_presentation_feedback,
};

use crate::platform::wayland::{WaylandPresentation, WaylandPresentationObserver};
use crate::platform_impl::wayland::state::WinitState;

pub struct PresentationState {
    manager: wp_presentation::WpPresentation,
    clock: Arc<Mutex<Option<u32>>>,
}

struct Feedback {
    clock: Arc<Mutex<Option<u32>>>,
    observer: Mutex<Option<WaylandPresentationObserver>>,
}

impl Feedback {
    fn finish(&self, result: WaylandPresentation) {
        let callback = self.observer.lock().unwrap().take();
        if let Some(callback) = callback {
            callback(result);
        }
    }
}

impl Feedback {
    fn event(&self, event: wp_presentation_feedback::Event) {
        match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                seq_hi,
                seq_lo,
                flags,
            } => {
                let clock_id = *self.clock.lock().unwrap();
                let result = WaylandPresentation::from_protocol(
                    clock_id,
                    (tv_sec_hi, tv_sec_lo, tv_nsec),
                    refresh,
                    (seq_hi, seq_lo),
                    flags.into_result().ok().map(|flags| flags.bits()),
                );
                self.finish(result);
            }
            wp_presentation_feedback::Event::Discarded => {
                self.finish(WaylandPresentation::Discarded)
            }
            _ => {}
        }
    }
}

impl Drop for Feedback {
    fn drop(&mut self) {
        // Connection loss is missing presentation, never successful delivery.
        self.finish(WaylandPresentation::Discarded);
    }
}

impl PresentationState {
    pub fn bind(globals: &GlobalList, queue: &QueueHandle<WinitState>) -> Result<Self, BindError> {
        let clock = Arc::new(Mutex::new(None));
        let manager = globals.bind(queue, 1..=1, clock.clone())?;
        Ok(Self { manager, clock })
    }

    pub fn observe(
        &self,
        surface: &WlSurface,
        queue: &QueueHandle<WinitState>,
        observer: WaylandPresentationObserver,
    ) {
        self.manager.feedback(
            surface,
            queue,
            Feedback {
                clock: self.clock.clone(),
                observer: Mutex::new(Some(observer)),
            },
        );
    }
}

impl Dispatch<wp_presentation::WpPresentation, Arc<Mutex<Option<u32>>>, WinitState>
    for PresentationState
{
    fn event(
        _: &mut WinitState,
        _: &wp_presentation::WpPresentation,
        event: wp_presentation::Event,
        clock: &Arc<Mutex<Option<u32>>>,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            *clock.lock().unwrap() = Some(clk_id);
        }
    }
}

impl Dispatch<wp_presentation_feedback::WpPresentationFeedback, Feedback, WinitState>
    for PresentationState
{
    fn event(
        _: &mut WinitState,
        _: &wp_presentation_feedback::WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        data: &Feedback,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        data.event(event);
    }
}

delegate_dispatch!(WinitState: [wp_presentation::WpPresentation: Arc<Mutex<Option<u32>>>] => PresentationState);
delegate_dispatch!(WinitState: [wp_presentation_feedback::WpPresentationFeedback: Feedback] => PresentationState);
