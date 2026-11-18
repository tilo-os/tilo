//! dma-bufs: the app allocates where the desktop says (its default feedback, passed on as the
//! keeper's own), and every buffer is made again on the desktop from the same descriptors.
//! While no desktop is attached the last feedback stands.
//!
//! A desktop without a GPU node offers version 3: its formats, no feedback. The app is offered
//! the same, so its graphics library finds no device and draws in software, as it would on that
//! desktop directly. Across a restart onto a desktop of the other kind, objects the app already
//! holds keep working: a version-3 global's formats are fixed once bound, so later binds get a
//! new global and the old one is only withdrawn; a global with feedback stays, and is told the
//! new desktop's formats on the device it last named (the protocol wants one main device, and a
//! version-3 desktop names none; every format in it is one that desktop imports).

use std::collections::{HashMap, HashSet};
use std::os::fd::OwnedFd;

use smithay::backend::allocator::{Format, Fourcc, Modifier};
use smithay::wayland::dmabuf::DmabufGlobal;
use smithay::reexports::wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1::TrancheFlags;
use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::wayland::compositor::with_states;
use smithay::wayland::dmabuf::{
    DmabufFeedback, DmabufFeedbackBuilder, SurfaceDmabufFeedbackState,
};
use wayland_client::{Proxy as _, WEnum};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::Event;
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1;

use crate::state::State;

/// The per-surface feedback in flight, keyed by the app surface's id.
pub(crate) type SurfaceFeedbacks = HashMap<ObjectId, SurfaceFeedback>;

/// One tranche as the desktop sent it.
#[derive(Default)]
struct Tranche {
    device: Option<libc::dev_t>,
    formats: Vec<u16>,
    scanout: bool,
}

/// What the desktop answered a surface's own feedback request, and the feedback built from it.
#[derive(Default)]
pub(crate) struct SurfaceFeedback {
    parts: FeedbackParts,
    /// The last feedback the app was given for the surface; kept while no desktop is attached,
    /// and replaced when a later desktop answers differently.
    pub(crate) built: Option<DmabufFeedback>,
}

/// The parts of the desktop's default feedback, as they arrive.
#[derive(Default)]
pub(crate) struct FeedbackParts {
    table: Option<(OwnedFd, u32)>,
    main_device: Option<libc::dev_t>,
    tranches: Vec<Tranche>,
    current: Tranche,
}

fn device(bytes: &[u8]) -> Option<libc::dev_t> {
    bytes.try_into().ok().map(libc::dev_t::from_ne_bytes)
}

/// The format table: 16 bytes per entry, a format and a modifier.
fn read_table(fd: &OwnedFd, size: u32) -> Option<Vec<Format>> {
    let size = usize::try_from(size).ok()?;
    let mut bytes = vec![0_u8; size];
    let mut read = 0;
    while read < size {
        let rest = bytes.get_mut(read..)?;
        let count = rustix::io::pread(fd, rest, u64::try_from(read).ok()?).ok()?;
        if count == 0 {
            return None;
        }
        read = read.saturating_add(count);
    }
    Some(
        bytes
            .chunks_exact(16)
            .filter_map(|entry| {
                let code = u32::from_ne_bytes(entry.get(..4)?.try_into().ok()?);
                let modifier = u64::from_ne_bytes(entry.get(8..16)?.try_into().ok()?);
                Some(Format {
                    code: Fourcc::try_from(code).ok()?,
                    modifier: Modifier::from(modifier),
                })
            })
            .collect(),
    )
}

/// The dma-buf global the app is offered.
pub(crate) struct Offered {
    global: DmabufGlobal,
    /// The format codes it takes buffers of: smithay ends an app whose buffer has another, so
    /// nothing the app is told names one outside them.
    codes: HashSet<Fourcc>,
    /// For a version-3 global, the formats it lists; for one with feedback, `None`.
    listed: Option<HashSet<Format>>,
    /// The main device a global with feedback last named.
    main_device: libc::dev_t,
}

/// A desktop's feedback, read: its main device and its tranches, best first.
struct Offer {
    main_device: libc::dev_t,
    tranches: Vec<(libc::dev_t, bool, Vec<Format>)>,
}

impl Offer {
    fn read(parts: &FeedbackParts) -> Option<Self> {
        let (Some((fd, size)), Some(main_device)) = (&parts.table, parts.main_device) else {
            return None;
        };
        let table = read_table(fd, *size)?;
        let tranches = parts
            .tranches
            .iter()
            .map(|tranche| {
                let formats = tranche
                    .formats
                    .iter()
                    .filter_map(|index| table.get(usize::from(*index)).copied())
                    .collect();
                (
                    tranche.device.unwrap_or(main_device),
                    tranche.scanout,
                    formats,
                )
            })
            .collect();
        Some(Self {
            main_device,
            tranches,
        })
    }

    /// Every format it names.
    fn formats(&self) -> HashSet<Format> {
        self.tranches
            .iter()
            .flat_map(|(_, _, formats)| formats.iter().copied())
            .collect()
    }

    /// As feedback for a global that takes the format `codes` (any, when `None`).
    fn feedback(&self, codes: Option<&HashSet<Fourcc>>) -> Option<DmabufFeedback> {
        let taken = |format: &Format| codes.is_none_or(|codes| codes.contains(&format.code));
        let mut tranches = self.tranches.iter();
        let (_, _, first) = tranches.next()?;
        let mut builder =
            DmabufFeedbackBuilder::new(self.main_device, first.iter().copied().filter(taken));
        for (device, scanout, formats) in tranches {
            builder = builder.add_preference_tranche(
                *device,
                scanout.then_some(TrancheFlags::Scanout),
                formats.iter().copied().filter(taken),
            );
        }
        builder.build().ok()
    }
}

impl State {
    pub(crate) fn desktop_feedback(&mut self, event: Event) {
        let Some(up) = self.up.as_mut() else {
            return;
        };
        let parts = &mut up.feedback;
        match event {
            Event::FormatTable { fd, size } => parts.table = Some((fd, size)),
            Event::MainDevice { device: bytes } => parts.main_device = device(&bytes),
            Event::TrancheTargetDevice { device: bytes } => parts.current.device = device(&bytes),
            Event::TrancheFormats { indices } => parts.current.formats.extend(
                indices
                    .chunks_exact(2)
                    .filter_map(|pair| pair.try_into().ok().map(u16::from_ne_bytes)),
            ),
            Event::TrancheFlags {
                flags: WEnum::Value(flags),
            } => parts.current.scanout = !flags.is_empty(),
            Event::TrancheDone => {
                let tranche = std::mem::take(&mut parts.current);
                parts.tranches.push(tranche);
            }
            Event::Done => {
                let parts = std::mem::take(parts);
                let Some(offer) = Offer::read(&parts) else {
                    tracing::warn!("the desktop's dma-buf feedback could not be read");
                    return;
                };
                self.offer_feedback(&offer);
            }
            _ => {}
        }
    }

    /// The desktop answered a surface's own feedback request: give the app what it says for
    /// that surface, and keep it while no desktop is attached.
    pub(crate) fn desktop_surface_feedback(&mut self, id: &ObjectId, event: Event) {
        if self.up.as_ref().is_none_or(|up| up.dmabuf.is_none()) {
            return;
        }
        let Some(entry) = self.surface_feedback.get_mut(id) else {
            return;
        };
        match event {
            Event::FormatTable { fd, size } => entry.parts.table = Some((fd, size)),
            Event::MainDevice { device: bytes } => entry.parts.main_device = device(&bytes),
            Event::TrancheTargetDevice { device: bytes } => {
                entry.parts.current.device = device(&bytes);
            }
            Event::TrancheFormats { indices } => entry.parts.current.formats.extend(
                indices
                    .chunks_exact(2)
                    .filter_map(|pair| pair.try_into().ok().map(u16::from_ne_bytes)),
            ),
            Event::TrancheFlags {
                flags: WEnum::Value(flags),
            } => entry.parts.current.scanout = !flags.is_empty(),
            Event::TrancheDone => {
                let tranche = std::mem::take(&mut entry.parts.current);
                entry.parts.tranches.push(tranche);
            }
            Event::Done => {
                let parts = std::mem::take(&mut entry.parts);
                let codes = self.dmabuf_offer.as_ref().map(|offered| &offered.codes);
                let Some(feedback) = Offer::read(&parts).and_then(|offer| offer.feedback(codes))
                else {
                    tracing::warn!("the desktop's per-surface dma-buf feedback could not be read");
                    return;
                };
                let built = entry.built.replace(feedback.clone());
                if built.as_ref() == Some(&feedback) {
                    return;
                }
                if let Some(kept) = self.surfaces.get(id) {
                    with_states(&kept.surface, |states| {
                        if let Some(state) = SurfaceDmabufFeedbackState::from_states(states) {
                            state.set_feedback(&feedback);
                        }
                    });
                }
            }
            _ => {}
        }
    }

    /// A version-3 desktop's dma-buf global listed a format.
    pub(crate) fn desktop_listed_dmabuf(&mut self, event: &zwp_linux_dmabuf_v1::Event) {
        let Some(up) = self.up.as_mut() else {
            return;
        };
        let format = match event {
            zwp_linux_dmabuf_v1::Event::Modifier {
                format,
                modifier_hi,
                modifier_lo,
            } => (
                *format,
                (u64::from(*modifier_hi) << 32) | u64::from(*modifier_lo),
            ),
            zwp_linux_dmabuf_v1::Event::Format { format } => {
                (*format, u64::from(Modifier::Invalid))
            }
            _ => return,
        };
        if let Ok(code) = Fourcc::try_from(format.0) {
            up.dmabuf_listed.push(Format {
                code,
                modifier: Modifier::from(format.1),
            });
        }
    }

    /// Offer the app what a desktop with feedback offers: a global with that feedback the
    /// first time (or in place of a version-3 one), its default feedback replaced afterwards.
    fn offer_feedback(&mut self, offer: &Offer) {
        if let Some(up) = self.up.as_mut() {
            up.dmabuf_formats = Some(offer.formats());
        }
        if let Some(offered) = self
            .dmabuf_offer
            .as_mut()
            .filter(|offered| offered.listed.is_none())
        {
            let Some(feedback) = offer.feedback(Some(&offered.codes)) else {
                tracing::warn!("the desktop's dma-buf feedback could not be offered");
                return;
            };
            offered.main_device = offer.main_device;
            self.dmabuf.set_default_feedback(&offered.global, &feedback);
            return;
        }
        let Some(feedback) = offer.feedback(None) else {
            tracing::warn!("the desktop's dma-buf feedback could not be offered");
            return;
        };
        let codes = offer
            .formats()
            .into_iter()
            .map(|format| format.code)
            .collect();
        let dh = self.dh.clone();
        self.withdraw_dmabuf();
        let global = self
            .dmabuf
            .create_global_with_default_feedback::<Self>(&dh, &feedback);
        self.dmabuf_offer = Some(Offered {
            global,
            codes,
            listed: None,
            main_device: offer.main_device,
        });
    }

    /// Offer the app what a version-3 desktop listed, once its global was bound.
    pub(crate) fn offer_listed_dmabuf(&mut self) {
        let Some(up) = self.up.as_mut() else {
            return;
        };
        if up
            .dmabuf
            .as_ref()
            .is_none_or(|dmabuf| dmabuf.version() >= 4)
        {
            return;
        }
        let listed: HashSet<Format> = std::mem::take(&mut up.dmabuf_listed).into_iter().collect();
        up.dmabuf_formats = Some(listed.clone());
        match self.dmabuf_offer.as_ref() {
            Some(offered) if offered.listed.is_none() => {
                let offer = Offer {
                    main_device: offered.main_device,
                    tranches: vec![(offered.main_device, false, listed.into_iter().collect())],
                };
                let Some(feedback) = offer.feedback(Some(&offered.codes)) else {
                    tracing::warn!("the desktop's dma-buf formats could not be offered");
                    return;
                };
                self.dmabuf.set_default_feedback(&offered.global, &feedback);
                // Surfaces told what the last desktop said for them hear this one: it asks
                // nothing per surface.
                for (id, entry) in &mut self.surface_feedback {
                    entry.built = Some(feedback.clone());
                    if let Some(kept) = self.surfaces.get(id) {
                        with_states(&kept.surface, |states| {
                            if let Some(state) = SurfaceDmabufFeedbackState::from_states(states) {
                                state.set_feedback(&feedback);
                            }
                        });
                    }
                }
            }
            Some(offered) if offered.listed.as_ref() == Some(&listed) => {}
            _ => {
                let codes = listed.iter().map(|format| format.code).collect();
                let dh = self.dh.clone();
                self.withdraw_dmabuf();
                let global = self
                    .dmabuf
                    .create_global::<Self>(&dh, listed.iter().copied());
                self.dmabuf_offer = Some(Offered {
                    global,
                    codes,
                    listed: Some(listed),
                    main_device: 0,
                });
            }
        }
    }

    /// No new bind gets the global offered so far; the objects the app holds of it keep
    /// working, so it is never destroyed.
    fn withdraw_dmabuf(&mut self) {
        if let Some(old) = self.dmabuf_offer.take() {
            let dh = self.dh.clone();
            self.dmabuf.disable_global::<Self>(&dh, &old.global);
        }
    }
}
