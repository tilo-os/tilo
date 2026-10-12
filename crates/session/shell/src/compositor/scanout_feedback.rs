//! Telling a fullscreen client what the display can scan out.
//!
//! A fullscreen client buffer is page-flipped onto the primary plane instead
//! of composited when the plane takes it (`backend::drm::client_scanout`).
//! Whether it does depends on how the client allocated it: its format and
//! modifier, and — what only the driver knows — whether it was allocated for
//! scanout at all. Clients learn that from `zwp_linux_dmabuf_feedback_v1`:
//! a tranche flagged `scanout`, targeting the device that drives the display,
//! lists what the plane takes; Mesa then allocates with scanout usage (and on
//! a laptop with two GPUs, for the GPU that drives the panel).
//!
//! Every surface starts on the default feedback, the render node and the
//! formats the compositor can composite. While a surface is the frame's
//! scanout candidate ([`crate::shell::scanout_candidate`]) — fullscreen, on
//! top of everything on an output, pixel for pixel — and its buffer is opaque,
//! its feedback gains a scanout tranche for that output's plane
//! ([`ScanoutPlane`]); on the frame anything covers it, it turns translucent
//! or leaves fullscreen, it goes back to the default. The feedback only steers
//! allocation: each frame's scanout is still decided by the presenter, so a
//! wrong guess costs a reallocation, never a wrong pixel.
//!
//! The scanout tranche never lists a buffer the compositor could not
//! composite ([`scanout_tranche`]): when a popup opens over the client, its
//! buffer is drawn like any other.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use smithay::backend::allocator::{Format, Fourcc};
use smithay::reexports::wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1::TrancheFlags;
use smithay::reexports::wayland_server::Resource as _;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor;
use smithay::wayland::dmabuf::{DmabufFeedback, DmabufFeedbackBuilder, SurfaceDmabufFeedbackState};

use crate::compositor::backend::{Output, OutputId};
use crate::shell::scanout_candidate::ScanoutCandidate;

/// What one output's primary plane scans out of a fullscreen client: the
/// client buffer formats, with their modifiers, the presenter would flip
/// there, and the device that drives the output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanoutPlane {
    /// `dev_t` of the KMS device the output is on.
    pub device: libc::dev_t,
    /// Client buffer formats the plane scans out. An `ARGB` format here
    /// scans out only from a surface that declared itself opaque — the only
    /// surfaces offered the plane ([`ScanoutOpacity`]).
    pub formats: Vec<Format>,
}

/// A frame's scanout candidate and the plane of the output it covers.
#[derive(Clone, Debug)]
pub struct ScanoutOffer {
    /// The candidate's surface.
    pub surface: u64,
    pub plane: Arc<ScanoutPlane>,
}

/// The offer for the frame whose scanout candidate is `candidate`: the plane
/// of the output it covers exactly, when that output has one (`plane`, the
/// backend's [`crate::compositor::backend::Backend::scanout_plane`]).
pub fn offer(
    candidate: Option<&ScanoutCandidate>,
    outputs: &[Output],
    plane: impl Fn(OutputId) -> Option<Arc<ScanoutPlane>>,
) -> Option<ScanoutOffer> {
    let candidate = candidate?;
    let output = outputs
        .iter()
        .find(|output| candidate.covers(output.position, output.size))?;
    Some(ScanoutOffer {
        surface: candidate.surface,
        plane: plane(output.id)?,
    })
}

/// The formats a scanout tranche for `plane` lists: those the plane scans
/// out that are also `importable`, the formats every surface's feedback
/// offers, so a buffer allocated from the tranche can still be composited.
pub fn scanout_tranche(importable: &[Format], plane: &ScanoutPlane) -> Vec<Format> {
    importable
        .iter()
        .filter(|format| plane.formats.contains(format))
        .copied()
        .collect()
}

/// Whether a buffer of `fourcc` is opaque for scanout: an 8-bit `X` format,
/// or an 8-bit alpha format whose commit declared the whole surface opaque
/// (`compositor::opaque_region`), which scans out as its `X` twin.
pub fn scanout_opaque(fourcc: Fourcc, declared_opaque: bool) -> bool {
    match fourcc {
        Fourcc::Xrgb8888 | Fourcc::Xbgr8888 => true,
        Fourcc::Argb8888 | Fourcc::Abgr8888 => declared_opaque,
        _ => false,
    }
}

/// Surface data: the surface's latest buffer is opaque for scanout
/// ([`scanout_opaque`]). Recorded by every buffer commit; a surface whose
/// buffer is translucent, not a dma-buf, or gone is not offered a plane.
#[derive(Debug, Default)]
pub struct ScanoutOpacity(AtomicBool);

impl ScanoutOpacity {
    /// Record the opacity of the buffer a commit to the surface `states`
    /// describe attaches: `commit` when it is a dma-buf, else not opaque.
    pub fn record(
        states: &compositor::SurfaceData,
        commit: Option<&crate::compositor::foreign::DmabufCommit>,
    ) {
        use smithay::backend::allocator::Buffer as _;
        let opaque =
            commit.is_some_and(|commit| scanout_opaque(commit.buffer.format().code, commit.opaque));
        states.data_map.insert_if_missing_threadsafe(Self::default);
        if let Some(opacity) = states.data_map.get::<Self>() {
            opacity.0.store(opaque, Ordering::Relaxed);
        }
    }

    /// The opacity last recorded for `surface`; `false` before any.
    pub fn of(surface: &WlSurface) -> bool {
        surface.is_alive()
            && compositor::with_states(surface, |states| {
                states
                    .data_map
                    .get::<Self>()
                    .is_some_and(|opacity| opacity.0.load(Ordering::Relaxed))
            })
    }
}

/// The dma-buf feedback surfaces get: the default, and the scanout feedback
/// of the one surface offered a plane.
pub struct ScanoutFeedback {
    /// The render node every tranche's buffers are allocated for by default.
    main_device: libc::dev_t,
    /// The formats the default feedback offers: what the compositor can
    /// composite.
    formats: Vec<Format>,
    default: DmabufFeedback,
    /// The feedback last built for a plane — `None` when the plane scans out
    /// nothing importable — reused while the offer stays the same.
    built: Option<(Arc<ScanoutPlane>, Option<DmabufFeedback>)>,
    /// The surface that has a scanout feedback, and that feedback.
    offered: Option<(WlSurface, DmabufFeedback)>,
}

impl ScanoutFeedback {
    /// Feedback over `default`, the global's default feedback, which names
    /// `main_device` and `formats`.
    pub fn new(main_device: libc::dev_t, formats: Vec<Format>, default: DmabufFeedback) -> Self {
        Self {
            main_device,
            formats,
            default,
            built: None,
            offered: None,
        }
    }

    /// What `surface` gets when it asks for feedback for the first time:
    /// the scanout feedback when it is the surface offered one, else `None`
    /// for the default.
    pub fn for_new_surface(&self, surface: &WlSurface) -> Option<DmabufFeedback> {
        self.offered
            .as_ref()
            .filter(|(offered, _)| offered == surface)
            .map(|(_, feedback)| feedback.clone())
    }

    /// Give `offer`'s surface the scanout feedback for its plane, and every
    /// other surface the default: the surface that had a scanout feedback
    /// gets the default back. Resends only what changed.
    pub fn offer(&mut self, offer: Option<(&WlSurface, &Arc<ScanoutPlane>)>) {
        let next = offer.and_then(|(surface, plane)| {
            let feedback = self.feedback_for(plane)?;
            Some((surface.clone(), feedback))
        });
        let unchanged = match (&self.offered, &next) {
            (Some((was, was_feedback)), Some((now, feedback))) => {
                was == now && was_feedback == feedback
            }
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return;
        }
        if let Some((previous, _)) = self.offered.take()
            && next.as_ref().is_none_or(|(now, _)| *now != previous)
        {
            send(&previous, &self.default);
        }
        if let Some((surface, feedback)) = &next {
            send(surface, feedback);
        }
        self.offered = next;
    }

    /// The scanout feedback for `plane`: a tranche flagged `scanout` on the
    /// plane's device with [`scanout_tranche`], before the default tranche.
    /// `None` when the plane scans out nothing the compositor can composite.
    fn feedback_for(&mut self, plane: &Arc<ScanoutPlane>) -> Option<DmabufFeedback> {
        if let Some((built_for, feedback)) = &self.built
            && (Arc::ptr_eq(built_for, plane) || built_for == plane)
        {
            return feedback.clone();
        }
        let tranche = scanout_tranche(&self.formats, plane);
        let feedback = if tranche.is_empty() {
            None
        } else {
            DmabufFeedbackBuilder::new(self.main_device, self.formats.iter().copied())
                .add_preference_tranche(plane.device, Some(TrancheFlags::Scanout), tranche)
                .build()
                .map_err(|error| {
                    // The format table lives in a sealed memfd; without one
                    // the surface keeps the default feedback.
                    tracing::warn!(
                        target: "shell::compositor::dmabuf",
                        %error,
                        "could not build scanout dmabuf feedback"
                    );
                })
                .ok()
        };
        self.built = Some((Arc::clone(plane), feedback.clone()));
        feedback
    }
}

/// Send `feedback` to every feedback object `surface` asked for; none when
/// it asked for none (it gets [`ScanoutFeedback::for_new_surface`] if it
/// asks later) or is gone.
fn send(surface: &WlSurface, feedback: &DmabufFeedback) {
    if !surface.is_alive() {
        return;
    }
    compositor::with_states(surface, |states| {
        if let Some(state) = SurfaceDmabufFeedbackState::from_states(states) {
            state.set_feedback(feedback);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::allocator::Modifier;

    fn format(code: Fourcc, modifier: Modifier) -> Format {
        Format { code, modifier }
    }

    #[test]
    fn the_tranche_lists_only_what_the_plane_scans_out_and_the_compositor_composites() {
        let importable = [
            format(Fourcc::Argb8888, Modifier::Linear),
            format(Fourcc::Xrgb8888, Modifier::Linear),
            format(Fourcc::Abgr8888, Modifier::Linear),
            format(Fourcc::Xbgr2101010, Modifier::Linear),
        ];
        let plane = ScanoutPlane {
            device: 7,
            formats: vec![
                format(Fourcc::Xrgb8888, Modifier::Linear),
                format(Fourcc::Argb8888, Modifier::Linear),
                // Tiled: the plane takes it, the importer does not.
                format(Fourcc::Xrgb8888, Modifier::from(0x0100_0000_0000_0001_u64)),
            ],
        };
        assert_eq!(
            scanout_tranche(&importable, &plane),
            vec![
                format(Fourcc::Argb8888, Modifier::Linear),
                format(Fourcc::Xrgb8888, Modifier::Linear),
            ]
        );
        let tiled_only = ScanoutPlane {
            device: 7,
            formats: vec![format(
                Fourcc::Xrgb8888,
                Modifier::from(0x0100_0000_0000_0001_u64),
            )],
        };
        assert!(scanout_tranche(&importable, &tiled_only).is_empty());
    }

    #[test]
    fn only_eight_bit_buffers_opaque_by_format_or_declaration_are_opaque_for_scanout() {
        assert!(scanout_opaque(Fourcc::Xrgb8888, false));
        assert!(scanout_opaque(Fourcc::Xbgr8888, false));
        assert!(scanout_opaque(Fourcc::Argb8888, true));
        assert!(scanout_opaque(Fourcc::Abgr8888, true));
        assert!(!scanout_opaque(Fourcc::Argb8888, false));
        assert!(!scanout_opaque(Fourcc::Abgr8888, false));
        // HDR swapchains composite through the HDR path.
        assert!(!scanout_opaque(Fourcc::Xrgb2101010, true));
        assert!(!scanout_opaque(Fourcc::Xbgr16161616f, true));
    }
}
