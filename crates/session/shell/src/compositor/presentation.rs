//! Standard presentation feedback bound to committed pixels and an exact composed frame.
use std::collections::HashMap;
use std::time::{Duration, Instant};

use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;
use smithay::wayland::presentation::{PresentationFeedbackCachedState, PresentationFeedbackCallback, Refresh};
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind;

use crate::shell::foreign::{ForeignFrameStore, SurfaceId};
use crate::shell::present::PhysicalPresentation;

#[derive(Clone, Copy, Debug)]
pub enum PixelCommit {
    Captured(u64),
    Shm(u64),
    Unavailable,
}

#[derive(Default)]
struct Callbacks(Vec<PresentationFeedbackCallback>);

impl Drop for Callbacks {
    fn drop(&mut self) {
        for callback in self.0.drain(..) {
            callback.discarded();
        }
    }
}

pub struct FrameFeedback {
    callbacks: Callbacks,
    output: Option<Output>,
    submitted_at: Instant,
}

impl FrameFeedback {
    pub fn complete(mut self, proof: Option<PhysicalPresentation>) {
        let Some((proof, output)) = proof.zip(self.output.as_ref()) else {
            return;
        };
        for callback in self.callbacks.0.drain(..) {
            if callback.clk_id() == u32::try_from(libc::CLOCK_MONOTONIC).unwrap_or(1) {
                callback.presented(
                    output,
                    proof.timestamp,
                    proof.refresh.map_or(Refresh::Unknown, Refresh::fixed),
                    proof.sequence,
                    Kind::Vsync | Kind::HwClock | Kind::HwCompletion,
                );
            } else {
                callback.discarded();
            }
        }
    }
}

#[derive(Default)]
pub struct PresentationFeedback {
    pixels: HashMap<SurfaceId, PixelCommit>,
    pending: HashMap<SurfaceId, (Instant, Callbacks)>,
    submitted: HashMap<u64, FrameFeedback>,
}

impl PresentationFeedback {
    pub fn committed(&mut self, surface: &WlSurface, id: SurfaceId, pixels: Option<PixelCommit>) {
        if let Some(pixels) = pixels {
            self.pending.remove(&id);
            self.pixels.insert(id, pixels);
        }
        let callbacks = with_states(surface, |states| {
            std::mem::take(
                &mut states
                    .cached_state
                    .get::<PresentationFeedbackCachedState>()
                    .current()
                    .callbacks,
            )
        });
        if !callbacks.is_empty() {
            self.pending
                .insert(id, (Instant::now(), Callbacks(callbacks)));
        }
    }

    pub fn destroyed(&mut self, id: SurfaceId) {
        self.pending.remove(&id);
        self.pixels.remove(&id);
    }

    /// Drain only callbacks whose committed pixels were actually composed.
    /// A worker may publish asynchronously; a newer/unfinished generation can
    /// never receive credit for a widget's older cached image.
    pub fn composed(
        &mut self,
        frames: &ForeignFrameStore,
        output: Option<Output>,
    ) -> FrameFeedback {
        let painted = frames.take_painted_frames();
        let mut callbacks = Callbacks::default();
        for (id, seq) in painted {
            let Some((captured, shm_generation)) = frames.presentation_pixels(id) else {
                continue;
            };
            if captured != seq {
                continue;
            }
            let exact = match self.pixels.get(&id) {
                Some(PixelCommit::Captured(expected)) => *expected == seq,
                Some(PixelCommit::Shm(expected)) => *expected == shm_generation,
                _ => false,
            };
            if exact && let Some((_, mut pending)) = self.pending.remove(&id) {
                callbacks.0.append(&mut pending.0);
            }
        }
        FrameFeedback {
            callbacks,
            output,
            submitted_at: Instant::now(),
        }
    }

    pub fn submitted(&mut self, serial: u64, feedback: FrameFeedback) {
        if !feedback.callbacks.0.is_empty() {
            self.submitted.insert(serial, feedback);
        }
    }

    pub fn completed(&mut self, serial: u64, proof: Option<PhysicalPresentation>) {
        if let Some(feedback) = self.submitted.remove(&serial) {
            feedback.complete(proof);
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending
            .values()
            .map(|(at, _)| *at)
            .chain(self.submitted.values().map(|frame| frame.submitted_at))
            .filter_map(|at| at.checked_add(Duration::from_secs(1)))
            .min()
    }

    pub fn expire(&mut self, now: Instant) {
        self.pending
            .retain(|_, (at, _)| now.saturating_duration_since(*at) < Duration::from_secs(1));
        self.submitted.retain(|_, feedback| {
            now.saturating_duration_since(feedback.submitted_at) < Duration::from_secs(1)
        });
    }
}
