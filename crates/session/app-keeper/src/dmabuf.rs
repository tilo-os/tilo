//! dma-bufs: the app allocates where the desktop says (its default feedback, passed on as the
//! keeper's own), and every buffer is made again on the desktop from the same descriptors.
//! While no desktop is attached the last feedback stands.

use std::os::fd::OwnedFd;

use smithay::backend::allocator::{Format, Fourcc, Modifier};
use smithay::reexports::wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1::TrancheFlags;
use smithay::wayland::dmabuf::DmabufFeedbackBuilder;
use wayland_client::WEnum;
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::Event;

use crate::state::State;

/// One tranche as the desktop sent it.
#[derive(Default)]
struct Tranche {
    device: Option<libc::dev_t>,
    formats: Vec<u16>,
    scanout: bool,
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
                self.offer_feedback(&parts);
            }
            _ => {}
        }
    }

    /// Offer the app what the desktop offers: the dma-buf global the first time, its default
    /// feedback replaced afterwards.
    fn offer_feedback(&mut self, parts: &FeedbackParts) {
        let (Some((fd, size)), Some(main_device)) = (&parts.table, parts.main_device) else {
            return;
        };
        let Some(table) = read_table(fd, *size) else {
            tracing::warn!("the desktop's dma-buf format table could not be read");
            return;
        };
        let formats_of = |tranche: &Tranche| -> Vec<Format> {
            tranche
                .formats
                .iter()
                .filter_map(|index| table.get(usize::from(*index)).copied())
                .collect()
        };
        let mut tranches = parts.tranches.iter();
        let Some(first) = tranches.next() else {
            return;
        };
        let mut builder = DmabufFeedbackBuilder::new(main_device, formats_of(first));
        for tranche in tranches {
            builder = builder.add_preference_tranche(
                tranche.device.unwrap_or(main_device),
                tranche.scanout.then_some(TrancheFlags::Scanout),
                formats_of(tranche),
            );
        }
        let feedback = match builder.build() {
            Ok(feedback) => feedback,
            Err(error) => {
                tracing::warn!(%error, "the desktop's dma-buf feedback could not be offered");
                return;
            }
        };
        let dh = self.dh.clone();
        let global = match self.dmabuf_global.take() {
            Some(global) => {
                self.dmabuf_state_mut()
                    .set_default_feedback(&global, &feedback);
                global
            }
            None => self
                .dmabuf_state_mut()
                .create_global_with_default_feedback::<Self>(&dh, &feedback),
        };
        self.dmabuf_global = Some(global);
    }
}
