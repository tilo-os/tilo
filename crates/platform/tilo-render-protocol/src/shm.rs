//! Shared memory between an app and the processes it draws through: the ring an app writes its
//! frames into, and the images, fonts and large patch batches it hands over once.
//!
//! The app creates every buffer, so its pages are charged to the app: an app cannot push its
//! memory onto the service.
//!
//! The buffers themselves are [`tilo_shm`]'s: a sealed memfd on Linux, unlinked POSIX shared
//! memory on macOS. On Linux the service maps a buffer read-only only after checking its seals:
//!
//! - every buffer is sealed against shrinking, so a mapping the service reads
//!   can never turn into `SIGBUS` under it, and against growing and further
//!   sealing, so its size is the size the service checked;
//! - an image or font is also sealed against writes: it is immutable once
//!   registered, so what was validated is what is drawn.
//!
//! The ring stays writable for the app. The service never trusts what it
//! read from the ring twice: a frame is decoded into the service's own
//! memory in one pass ([`crate::scene::decode`]), and only that copy is
//! validated and drawn.
//!
//! Other Unix systems (macOS) cannot seal memory: there an immutable buffer is copied once into
//! the reader's own memory. The writable ring and `SharedMemory` need seals, so they stay
//! Linux's.

#[cfg(any(target_os = "linux", target_os = "android"))]
use std::collections::VecDeque;
use std::io;
use std::os::fd::OwnedFd;
#[cfg(any(target_os = "linux", target_os = "android"))]
use std::os::fd::{AsFd, BorrowedFd};

#[cfg(any(target_os = "linux", target_os = "android"))]
use memmap2::MmapMut;

pub use tilo_shm::Mapping;

/// An immutable buffer holding `bytes`: what an app registers an image's pixels or a font's
/// bytes in, and hands a large patch batch or a module over in. A memfd sealed against writes and
/// resizing on Linux, unlinked shared memory elsewhere; [`Mapping::map_sealed`] reads either.
pub fn sealed_blob(name: &str, bytes: &[u8]) -> io::Result<OwnedFd> {
    tilo_shm::from_bytes(name, bytes)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
/// The app's side of the ring: a writable mapping and the records the
/// service has not finished with yet.
pub struct RingWriter {
    fd: OwnedFd,
    map: MmapMut,
    len: usize,
    /// Records the service has not answered yet, oldest first.
    outstanding: VecDeque<(u64, usize, usize)>,
    /// Where the next record goes.
    head: usize,
}

#[cfg(any(target_os = "linux", target_os = "android"))]
impl RingWriter {
    /// A ring of `len` bytes, sealed against resizing.
    pub fn create(len: usize) -> io::Result<Self> {
        let file = tilo_shm::create("tilo-render-ring", len as u64)?;
        tilo_shm::seal_sizes(&file)?;
        let map = tilo_shm::map_writable(&file)?;
        Ok(Self {
            fd: file.into(),
            map,
            len,
            outstanding: VecDeque::new(),
            head: 0,
        })
    }

    /// The memfd, to send with the hello.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether the service still holds any record.
    pub fn busy(&self) -> bool {
        !self.outstanding.is_empty()
    }

    /// Copy `record` into the ring as frame `sequence` and return its
    /// offset, or `None` when the free space cannot take it now.
    pub fn write(&mut self, sequence: u64, record: &[u8]) -> Option<usize> {
        let len = record.len();
        if len == 0 || len > self.len {
            return None;
        }
        let start = self.place(len)?;
        let end = start.checked_add(len)?;
        // `start..end` lies inside the `len`-byte mapping (checked by `place`), and only this
        // writer writes to it.
        let ring: &mut [u8] = &mut self.map;
        ring.get_mut(start..end)?.copy_from_slice(record);
        self.outstanding.push_back((sequence, start, end));
        self.head = end;
        Some(start)
    }

    /// Where `len` bytes fit without overwriting an outstanding record:
    /// after the head, or from the start of the ring.
    fn place(&self, len: usize) -> Option<usize> {
        let Some(&(_, oldest, _)) = self.outstanding.front() else {
            return Some(if self.head.checked_add(len)? <= self.len {
                self.head
            } else {
                0
            });
        };
        let head = self.head;
        if head >= oldest {
            // Free: head..end and 0..oldest.
            if head.checked_add(len)? <= self.len {
                return Some(head);
            }
            return (len <= oldest).then_some(0);
        }
        // Free: head..oldest.
        (head.checked_add(len)? <= oldest).then_some(head)
    }

    /// The service answered every frame up to `sequence`: their space is
    /// free.
    pub fn release(&mut self, sequence: u64) {
        while self
            .outstanding
            .front()
            .is_some_and(|(seq, _, _)| *seq <= sequence)
        {
            self.outstanding.pop_front();
        }
    }

    /// Forget every outstanding record (the service went away).
    pub fn reset(&mut self) {
        self.outstanding.clear();
        self.head = 0;
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
/// A writable shared memfd: a buffer the service without a GPU draws into
/// and the app attaches as `wl_shm`. Sealed against shrinking, so the
/// compositor's mapping of it never faults.
pub struct SharedMemory {
    fd: OwnedFd,
    map: MmapMut,
}

#[cfg(any(target_os = "linux", target_os = "android"))]
impl SharedMemory {
    /// `len` zeroed bytes.
    pub fn create(name: &str, len: usize) -> io::Result<Self> {
        let file = tilo_shm::create(name, len as u64)?;
        tilo_shm::seal_sizes(&file)?;
        let map = tilo_shm::map_writable(&file)?;
        Ok(Self {
            fd: file.into(),
            map,
        })
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.map
    }
}
