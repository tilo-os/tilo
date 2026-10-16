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
    /// The feedback requested by each surface whose commit this frame composed.
    callbacks: Vec<(SurfaceId, Callbacks)>,
    output: Option<Output>,
    submitted_at: Instant,
}

impl FrameFeedback {
    /// Take over the feedback of `older`, a frame this one replaced before it rendered.
    /// Callbacks of a surface this frame composed again are dropped, which discards them.
    fn absorb(&mut self, older: Self) {
        let Self {
            mut callbacks,
            output,
            submitted_at,
        } = older;
        callbacks.retain(|(id, _)| !self.callbacks.iter().any(|(newer, _)| newer == id));
        self.callbacks.append(&mut callbacks);
        self.output = self.output.take().or(output);
        self.submitted_at = self.submitted_at.min(submitted_at);
    }

    pub fn complete(mut self, proof: Option<PhysicalPresentation>) {
        let Some((proof, output)) = proof.zip(self.output.as_ref()) else {
            return;
        };
        for callback in self
            .callbacks
            .iter_mut()
            .flat_map(|(_, callbacks)| callbacks.0.drain(..))
        {
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
        let mut callbacks = Vec::new();
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
            if exact && let Some((_, pending)) = self.pending.remove(&id) {
                callbacks.push((id, pending));
            }
        }
        FrameFeedback {
            callbacks,
            output,
            submitted_at: Instant::now(),
        }
    }

    pub fn submitted(&mut self, serial: u64, feedback: FrameFeedback) {
        if feedback
            .callbacks
            .iter()
            .any(|(_, callbacks)| !callbacks.0.is_empty())
        {
            self.submitted.insert(serial, feedback);
        }
    }

    /// [`Self::submitted`] for a frame that replaced the frame `replaced` before it rendered
    /// (latest wins). A commit that frame composed is presented by this one, unless this one
    /// composed a newer commit of the same surface: that commit was never shown, and its
    /// feedback is discarded now.
    pub fn submitted_replacing(
        &mut self,
        serial: u64,
        mut feedback: FrameFeedback,
        replaced: Option<u64>,
    ) {
        if let Some(older) = replaced.and_then(|replaced| self.submitted.remove(&replaced)) {
            feedback.absorb(older);
        }
        self.submitted(serial, feedback);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn feedback(surfaces: &[SurfaceId]) -> FrameFeedback {
        FrameFeedback {
            callbacks: surfaces
                .iter()
                .map(|id| (*id, Callbacks::default()))
                .collect(),
            output: None,
            submitted_at: Instant::now(),
        }
    }

    /// A frame that replaced a waiting one presents the commits that frame composed, except
    /// where it composed a newer commit of the same surface: the older commit was never shown.
    #[test]
    fn a_replacing_frame_takes_over_the_feedback_of_surfaces_it_did_not_recompose() {
        let older = feedback(&[1, 2]);
        let mut newer = feedback(&[2, 3]);
        newer.absorb(older);
        let mut surfaces: Vec<SurfaceId> = newer.callbacks.iter().map(|(id, _)| *id).collect();
        surfaces.sort_unstable();
        assert_eq!(
            surfaces,
            vec![1, 2, 3],
            "surface 1 is still shown by the newer frame; surface 2 appears once, with the \
             newer commit's feedback"
        );
        assert_eq!(
            newer.callbacks.iter().filter(|(id, _)| *id == 2).count(),
            1,
            "the replaced commit of surface 2 is discarded, not presented"
        );
    }
}
