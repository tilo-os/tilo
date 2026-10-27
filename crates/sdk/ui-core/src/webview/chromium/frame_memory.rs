//! A page's frames in memory the app and its engine host both map.
//!
//! Piping each frame cost two kernel copies (into the pipe, out of it) and,
//! at the pipe's 64 KiB, hundreds of writes and wakeups per frame. Instead the
//! app creates this memory (a sealed memfd on Linux, an unlinked POSIX shared
//! memory object on macOS) and hands its descriptor to the host once, at spawn
//! (`--frame-memory-fd=N`). The host copies a paint's BGRA pixels into a free
//! slot, marks it ready and sends a notice down the pipe
//! ([`protocol::SharedFrame`]); the reader copies the pixels out, converting
//! them on the way, and marks the slot free again.
//!
//! The layout is the host's `frame_memory.h`: a header page holding one
//! 32-bit state per slot at [`SLOT_STATES`], then [`SLOTS`] slots of
//! [`SLOT_BYTES`]. A host started without the memory, or one older than it,
//! pipes its frames as before.

use std::io;
use std::sync::atomic::{AtomicU32, Ordering};

use tilo_process::shared_memory::SharedMemory;

use super::Routes;
use super::protocol::{self, SharedFrame};

const HEADER_BYTES: usize = 4096;
const SLOT_STATES: usize = 64;
/// Every frame the protocol accepts fits one slot.
const SLOT_BYTES: usize = protocol::MAX_PACKET as usize;
const SLOTS: u32 = 2;
const MEMORY_BYTES: usize = HEADER_BYTES + SLOTS as usize * SLOT_BYTES;
const FREE: u32 = 0;
const READY: u32 = 1;

/// What the reader makes of a slot's pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pixels {
    /// Converted for drawing here.
    Rgba,
    /// As the host wrote them, for a client process to convert.
    Bgra,
    /// Nobody shows the tab any more: only the slot is freed.
    Discard,
}

/// The app's side of one host's frame memory.
pub(super) struct FrameMemory(SharedMemory);

impl FrameMemory {
    /// New frame memory with every slot free.
    pub(super) fn create() -> io::Result<Self> {
        SharedMemory::create("tilo-chromium-frames", MEMORY_BYTES).map(Self)
    }

    /// Lets the host `command` starts inherit the memory.
    pub(super) fn hand_to(&self, command: &mut std::process::Command) {
        self.0.hand_to(command, "--frame-memory-fd=");
    }

    /// The host holds its own descriptor now; the mapping stays.
    pub(super) fn spawned(&mut self) {
        self.0.handed_over();
    }

    fn state(&self, slot: u32) -> &AtomicU32 {
        debug_assert!(slot < SLOTS);
        // SAFETY: inside the header page, 4-byte aligned (the mapping is
        // page aligned), and only ever accessed atomically by either process.
        unsafe { AtomicU32::from_ptr(self.0.as_ptr().add(SLOT_STATES + 4 * slot as usize).cast()) }
    }

    fn slot(&self, slot: u32) -> *mut u8 {
        debug_assert!(slot < SLOTS);
        // SAFETY: slot < SLOTS keeps the offset inside the mapping.
        unsafe {
            self.0
                .as_ptr()
                .add(HEADER_BYTES + slot as usize * SLOT_BYTES)
        }
    }

    /// Takes the frame `notice` announces out of its slot and frees the slot
    /// for the host's next frame.
    pub(super) fn take(&self, notice: &SharedFrame, pixels: Pixels) -> io::Result<Option<Vec<u8>>> {
        let size = notice.size();
        if notice.slot >= SLOTS || size > SLOT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid Chromium frame slot",
            ));
        }
        let state = self.state(notice.slot);
        if state.load(Ordering::Acquire) != READY {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Chromium frame slot is not ready",
            ));
        }
        // SAFETY: the slot holds `size` bytes inside the mapping. The host
        // leaves a ready slot alone until it is freed below; one that broke
        // that could only change the pixel values copied.
        let source = unsafe { std::slice::from_raw_parts(self.slot(notice.slot), size) };
        let taken = match pixels {
            Pixels::Rgba => Some(protocol::rgba_from_bgra(source)),
            Pixels::Bgra => Some(source.to_vec()),
            Pixels::Discard => None,
        };
        state.store(FREE, Ordering::Release);
        Ok(taken)
    }

    /// Writes a frame into `slot` and marks it ready, as the host does.
    #[cfg(test)]
    pub(super) fn publish(&self, slot: u32, bgra: &[u8]) {
        assert!(slot < SLOTS && bgra.len() <= SLOT_BYTES);
        assert_eq!(self.state(slot).load(Ordering::Acquire), FREE);
        // SAFETY: in bounds, and the slot is free: nobody reads it.
        unsafe { std::ptr::copy_nonoverlapping(bgra.as_ptr(), self.slot(slot), bgra.len()) };
        self.state(slot).store(READY, Ordering::Release);
    }

    /// Whether the reader has freed `slot`.
    #[cfg(test)]
    pub(super) fn is_free(&self, slot: u32) -> bool {
        self.state(slot).load(Ordering::Acquire) == FREE
    }
}

/// Takes a frame out of the frame memory for the tab it belongs to, freeing
/// its slot whether or not the tab is still shown: a client tab's frame goes
/// on as the host wrote it, a frame drawn here is converted as it is copied.
pub(super) fn deliver(
    routes: &Routes,
    memory: &FrameMemory,
    notice: &SharedFrame,
) -> io::Result<()> {
    let shared = super::route(routes, notice.tab);
    let pixels = match &shared {
        None => Pixels::Discard,
        Some(shared) if shared.remote.is_some() => Pixels::Bgra,
        Some(_) => Pixels::Rgba,
    };
    let (Some(shared), Some(pixels)) = (shared, memory.take(notice, pixels)?) else {
        return Ok(());
    };
    shared.frame_asked.store(false, Ordering::Relaxed);
    if let Some(outbox) = &shared.remote {
        outbox.frame(
            notice.tab,
            protocol::WireFrame {
                width: notice.width,
                height: notice.height,
                bgra: pixels,
            },
        );
    } else {
        let frame = protocol::rgba_frame(notice.tab, notice.width, notice.height, pixels);
        super::present_frame(&shared, notice.tab, frame);
    }
    Ok(())
}
