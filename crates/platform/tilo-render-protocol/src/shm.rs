//! Shared memory between an app and the processes it draws through: the ring an app writes its
//! frames into, and the images, fonts and large patch batches it hands over once.
//!
//! The app creates every buffer, so its pages are charged to the app: an app cannot push its
//! memory onto the service.
//!
//! On Linux a buffer is a sealed memfd, and the service maps it read-only only after checking its
//! seals:
//!
//! - every memfd is sealed against shrinking, so a mapping the service reads
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
//! Other Unix systems (macOS) cannot seal memory. There an immutable buffer is unlinked POSIX
//! shared memory ([`tilo_sandbox::shm`]), and [`Mapping::map_sealed`] copies it once into this
//! process's own read-only memory: the sender can still write the object, but never what the
//! receiver validated. The writable ring and `SharedMemory` need seals, so they stay Linux's.

#[cfg(target_os = "linux")]
use std::collections::VecDeque;
use std::io;
use std::os::fd::OwnedFd;
#[cfg(target_os = "linux")]
use std::os::fd::{AsFd, BorrowedFd};
use std::ptr::NonNull;

#[cfg(target_os = "linux")]
use rustix::fs::{MemfdFlags, SealFlags};
use rustix::mm::{MapFlags, ProtFlags};

/// Seals every memfd carries.
#[cfg(target_os = "linux")]
const SIZE_SEALS: SealFlags = SealFlags::SHRINK
    .union(SealFlags::GROW)
    .union(SealFlags::SEAL);

/// A read-only shared mapping of a sealed memfd.
pub struct Mapping {
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: the mapping is read-only memory shared with another process; this
// side never writes through it, and reading it from any thread is sound.
unsafe impl Send for Mapping {}
// SAFETY: as above; `&Mapping` only hands out shared byte slices.
unsafe impl Sync for Mapping {}

impl Mapping {
    /// Map `fd` read-only after checking it is a memfd sealed against
    /// resizing (and against writes when `immutable`), at least `min_len`
    /// bytes and at most `max_len`.
    #[cfg(target_os = "linux")]
    pub fn map_sealed(
        fd: &OwnedFd,
        immutable: bool,
        min_len: usize,
        max_len: usize,
    ) -> io::Result<Self> {
        let seals = rustix::fs::fcntl_get_seals(fd)?;
        let required = if immutable {
            SIZE_SEALS.union(SealFlags::WRITE)
        } else {
            SIZE_SEALS
        };
        if !seals.contains(required) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "a shared buffer must be a sealed memfd",
            ));
        }
        let size = rustix::fs::fstat(fd)?.st_size;
        let len = usize::try_from(size).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        if len < min_len || len > max_len || len == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a shared buffer's size is out of range",
            ));
        }
        // SAFETY: a fresh shared read-only mapping of a file we hold, at an
        // address the kernel chooses. Its size is sealed, so no page of it can
        // disappear while mapped. It is unmapped exactly once, in `Drop`.
        let ptr = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                len,
                ProtFlags::READ,
                MapFlags::SHARED,
                fd,
                0,
            )
        }?;
        let ptr =
            NonNull::new(ptr.cast::<u8>()).ok_or_else(|| io::Error::from(io::ErrorKind::Other))?;
        Ok(Self { ptr, len })
    }

    /// The bytes of the immutable buffer `fd`, at least `min_len` and at most `max_len` of them,
    /// copied once into this process's own read-only memory: nothing seals the buffer here, so a
    /// sender that kept it could still change it, but never the copy. A shared memory object's
    /// size is rounded up to whole pages, so it may exceed `max_len` by less than a page; what
    /// lies beyond `max_len` is not copied. A writable buffer (`immutable` false) needs seals and
    /// is refused.
    #[cfg(not(target_os = "linux"))]
    pub fn map_sealed(
        fd: &OwnedFd,
        immutable: bool,
        min_len: usize,
        max_len: usize,
    ) -> io::Result<Self> {
        /// The largest page size of a supported system: Apple silicon's 16 KiB.
        const MAX_PAGE: usize = 16 * 1024;
        let out_of_range = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "a shared buffer's size is out of range",
            )
        };
        if !immutable {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a writable shared buffer needs sealed memory, which only Linux has",
            ));
        }
        let size = rustix::fs::fstat(fd)?.st_size;
        let size =
            usize::try_from(size).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        let len = size.min(max_len);
        if size < min_len || len == 0 || size.saturating_sub(max_len) >= MAX_PAGE {
            return Err(out_of_range());
        }
        // SAFETY: a fresh shared read-only mapping of a file we hold, at an address the kernel
        // chooses, read once below and unmapped before returning. A shared memory object cannot
        // be resized once sized, so no page of it disappears while it is mapped.
        let shared = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                len,
                ProtFlags::READ,
                MapFlags::SHARED,
                fd,
                0,
            )
        }?;
        // SAFETY: fresh private anonymous memory of `len` bytes, this process's alone.
        let private = unsafe {
            rustix::mm::mmap_anonymous(
                std::ptr::null_mut(),
                len,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::PRIVATE,
            )
        };
        let private = match private {
            Ok(private) => private,
            Err(error) => {
                // SAFETY: unmaps exactly the shared mapping made above.
                let _ = unsafe { rustix::mm::munmap(shared, len) };
                return Err(error.into());
            }
        };
        // SAFETY: both regions are `len` mapped bytes and distinct. The sender may write the
        // shared one meanwhile, which only changes what this one copy holds.
        unsafe {
            std::ptr::copy_nonoverlapping(shared.cast::<u8>(), private.cast::<u8>(), len);
            let _ = rustix::mm::munmap(shared, len);
        }
        // SAFETY: the private region made above, read-only from now on for its life.
        unsafe { rustix::mm::mprotect(private, len, rustix::mm::MprotectFlags::READ) }?;
        let ptr = NonNull::new(private.cast::<u8>())
            .ok_or_else(|| io::Error::from(io::ErrorKind::Other))?;
        Ok(Self { ptr, len })
    }

    /// Map a whole regular file read-only: a font from the shared read-only
    /// view, which lives on the image's read-only `/usr` and so cannot shrink
    /// under the mapping.
    pub fn map_file(fd: &OwnedFd, max_len: usize) -> io::Result<Self> {
        let stat = rustix::fs::fstat(fd)?;
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        let len = usize::try_from(stat.st_size)
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        if len == 0 || len > max_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a file's size is out of range",
            ));
        }
        // SAFETY: a fresh private read-only mapping of a file we hold,
        // unmapped exactly once in `Drop`.
        let ptr = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                len,
                ProtFlags::READ,
                MapFlags::PRIVATE,
                fd,
                0,
            )
        }?;
        let ptr =
            NonNull::new(ptr.cast::<u8>()).ok_or_else(|| io::Error::from(io::ErrorKind::Other))?;
        Ok(Self { ptr, len })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// `len` bytes at `offset`, or `None` outside the mapping.
    pub fn get(&self, offset: usize, len: usize) -> Option<&[u8]> {
        let end = offset.checked_add(len)?;
        self.as_ref().get(offset..end)
    }
}

impl AsRef<[u8]> for Mapping {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: `ptr` maps `len` readable bytes for the life of `self`. The
        // writer may change them concurrently (the ring); readers copy what
        // they read once and never rely on two reads agreeing.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly the mapping made in `map_sealed`.
        let _ = unsafe { rustix::mm::munmap(self.ptr.as_ptr().cast(), self.len) };
    }
}

#[cfg(target_os = "linux")]
fn memfd(name: &str) -> io::Result<OwnedFd> {
    Ok(rustix::fs::memfd_create(
        name,
        MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
    )?)
}

/// An immutable buffer holding `bytes`: what an app registers an image's pixels or a font's
/// bytes in, and hands a large patch batch or a module over in. A memfd sealed against writes and
/// resizing on Linux, unlinked shared memory elsewhere; [`Mapping::map_sealed`] reads either.
pub fn sealed_blob(name: &str, bytes: &[u8]) -> io::Result<OwnedFd> {
    tilo_sandbox::shm::from_bytes(name, bytes)
}

#[cfg(target_os = "linux")]
/// The app's side of the ring: a writable mapping and the records the
/// service has not finished with yet.
pub struct RingWriter {
    fd: OwnedFd,
    ptr: NonNull<u8>,
    len: usize,
    /// Records the service has not answered yet, oldest first.
    outstanding: VecDeque<(u64, usize, usize)>,
    /// Where the next record goes.
    head: usize,
}

#[cfg(target_os = "linux")]
// SAFETY: the writable mapping belongs to this value alone; moving it to
// another thread moves the only writer.
unsafe impl Send for RingWriter {}

#[cfg(target_os = "linux")]
impl RingWriter {
    /// A ring of `len` bytes, sealed against resizing.
    pub fn create(len: usize) -> io::Result<Self> {
        let fd = memfd("tilo-render-ring")?;
        rustix::fs::ftruncate(&fd, len as u64)?;
        rustix::fs::fcntl_add_seals(&fd, SIZE_SEALS)?;
        // SAFETY: a fresh shared read-write mapping of our own memfd, unmapped
        // once in `Drop`; its size is sealed.
        let ptr = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                len,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                &fd,
                0,
            )
        }?;
        let ptr =
            NonNull::new(ptr.cast::<u8>()).ok_or_else(|| io::Error::from(io::ErrorKind::Other))?;
        Ok(Self {
            fd,
            ptr,
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
        // SAFETY: `start..end` lies inside the `len`-byte mapping (checked by
        // `place`), and only this writer writes to it.
        let ring = unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) };
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

#[cfg(target_os = "linux")]
impl Drop for RingWriter {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly the mapping made in `create`.
        let _ = unsafe { rustix::mm::munmap(self.ptr.as_ptr().cast(), self.len) };
    }
}

#[cfg(target_os = "linux")]
/// A writable shared memfd: a buffer the service without a GPU draws into
/// and the app attaches as `wl_shm`. Sealed against shrinking, so the
/// compositor's mapping of it never faults.
pub struct SharedMemory {
    fd: OwnedFd,
    ptr: NonNull<u8>,
    len: usize,
}

#[cfg(target_os = "linux")]
// SAFETY: the writable mapping belongs to this value alone.
unsafe impl Send for SharedMemory {}

#[cfg(target_os = "linux")]
impl SharedMemory {
    /// `len` zeroed bytes.
    pub fn create(name: &str, len: usize) -> io::Result<Self> {
        let fd = memfd(name)?;
        rustix::fs::ftruncate(&fd, len as u64)?;
        rustix::fs::fcntl_add_seals(&fd, SIZE_SEALS)?;
        // SAFETY: a fresh shared read-write mapping of our own memfd, unmapped
        // once in `Drop`; its size is sealed.
        let ptr = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                len,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                &fd,
                0,
            )
        }?;
        let ptr =
            NonNull::new(ptr.cast::<u8>()).ok_or_else(|| io::Error::from(io::ErrorKind::Other))?;
        Ok(Self { fd, ptr, len })
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `ptr` maps `len` writable bytes for the life of `self`, and
        // `&mut self` makes this the only reference through this process.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

#[cfg(target_os = "linux")]
impl Drop for SharedMemory {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly the mapping made in `create`.
        let _ = unsafe { rustix::mm::munmap(self.ptr.as_ptr().cast(), self.len) };
    }
}
