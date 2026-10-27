//! Memory shared with a child process this one starts.
//!
//! The parent creates the memory and maps it; the child inherits its
//! descriptor at spawn ([`SharedMemory::hand_to`]) and maps the same pages.
//! Bulk data then crosses the process boundary as one copy into the memory
//! and one out, with no pipe or socket in between.
//!
//! On Linux the memory is a memfd sealed against resizing, so the child can
//! never shrink it under the parent's mapping. On macOS it is a POSIX shared
//! memory object unlinked at once, which macOS sizes once and never again.
//! The web has no child processes: [`SharedMemory::create`] fails there.

use std::io;
use std::process::Command;

/// Memory this process maps and a child it starts inherits.
pub struct SharedMemory(imp::Memory);

impl SharedMemory {
    /// New zeroed memory of `len` bytes. `name` labels it for diagnostics.
    ///
    /// # Errors
    ///
    /// Creation, sizing, sealing or mapping failures; `Unsupported` where
    /// there are no child processes.
    pub fn create(name: &str, len: usize) -> io::Result<Self> {
        imp::Memory::create(name, len).map(Self)
    }

    /// The first byte of this process's mapping, valid for [`Self::len`]
    /// bytes until the memory drops. The child writes the same bytes: read
    /// and write them only as the two processes agree.
    #[must_use]
    pub fn as_ptr(&self) -> *mut u8 {
        self.0.as_ptr()
    }

    /// The memory's size in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the memory has no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Lets the child `command` starts inherit the memory: its descriptor
    /// stays open across exec and the child is passed `{flag}{descriptor}`
    /// as an argument. Does nothing once [`Self::handed_over`].
    pub fn hand_to(&self, command: &mut Command, flag: &str) {
        self.0.hand_to(command, flag);
    }

    /// The child holds its own descriptor now: this process closes its
    /// one and keeps the mapping.
    pub fn handed_over(&mut self) {
        self.0.handed_over();
    }
}

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::process::Command;
    use std::ptr::NonNull;

    pub struct Memory {
        base: NonNull<u8>,
        len: usize,
        descriptor: Option<OwnedFd>,
    }

    // SAFETY: the mapping is owned by this value and lives until it drops;
    // what is read or written through it is the caller's agreement with the
    // child, as `as_ptr` documents.
    unsafe impl Send for Memory {}
    // SAFETY: as above; `&Memory` hands out only the pointer.
    unsafe impl Sync for Memory {}

    impl Memory {
        pub fn create(name: &str, len: usize) -> io::Result<Self> {
            if len == 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty memory"));
            }
            let descriptor = create_descriptor(name)?;
            let size = libc::off_t::try_from(len).map_err(io::Error::other)?;
            // SAFETY: an owned descriptor and a size that fits `off_t`.
            if unsafe { libc::ftruncate(descriptor.as_raw_fd(), size) } != 0 {
                return Err(io::Error::last_os_error());
            }
            seal(&descriptor)?;
            // SAFETY: a fresh shared mapping of the whole object; nothing in
            // this process aliases it.
            let base = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    descriptor.as_raw_fd(),
                    0,
                )
            };
            if base == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
            let base = NonNull::new(base.cast()).ok_or_else(|| io::Error::other("null mapping"))?;
            Ok(Self {
                base,
                len,
                descriptor: Some(descriptor),
            })
        }

        pub const fn as_ptr(&self) -> *mut u8 {
            self.base.as_ptr()
        }

        pub const fn len(&self) -> usize {
            self.len
        }

        pub fn hand_to(&self, command: &mut Command, flag: &str) {
            use std::os::unix::process::CommandExt;
            let Some(fd) = self.descriptor.as_ref().map(AsRawFd::as_raw_fd) else {
                return;
            };
            command.arg(format!("{flag}{fd}"));
            // SAFETY: one async-signal-safe fcntl on a descriptor this value
            // keeps open until the child has been spawned.
            unsafe {
                command.pre_exec(move || {
                    if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }

        pub fn handed_over(&mut self) {
            self.descriptor = None;
        }
    }

    impl Drop for Memory {
        fn drop(&mut self) {
            // SAFETY: the mapping `create` made, unmapped once.
            unsafe { libc::munmap(self.base.as_ptr().cast(), self.len) };
        }
    }

    #[cfg(target_os = "linux")]
    fn create_descriptor(name: &str) -> io::Result<OwnedFd> {
        let name = std::ffi::CString::new(name).map_err(io::Error::other)?;
        // SAFETY: a NUL-terminated name and fixed flags.
        let fd = unsafe {
            libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING)
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: memfd_create returned a new descriptor nobody else owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// The child could otherwise shrink the memory under this process's
    /// mapping, which would then fault reading it.
    #[cfg(target_os = "linux")]
    fn seal(descriptor: &OwnedFd) -> io::Result<()> {
        // SAFETY: fixed seals on an owned memfd.
        let sealed = unsafe {
            libc::fcntl(
                descriptor.as_raw_fd(),
                libc::F_ADD_SEALS,
                libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL,
            )
        };
        if sealed != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn create_descriptor(_name: &str) -> io::Result<OwnedFd> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..8 {
            // Within macOS's 31 bytes for a shared memory name, which is
            // gone again before the descriptor is handed on.
            let name = format!(
                "/tilo-m{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let name = std::ffi::CString::new(name).map_err(io::Error::other)?;
            // SAFETY: a NUL-terminated name, fixed flags and mode.
            let fd = unsafe {
                libc::shm_open(
                    name.as_ptr(),
                    libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
                    libc::c_uint::from(0o600u16),
                )
            };
            if fd < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EEXIST) {
                    continue;
                }
                return Err(error);
            }
            // SAFETY: shm_open returned a new descriptor nobody else owns.
            let descriptor = unsafe { OwnedFd::from_raw_fd(fd) };
            // SAFETY: the name this call created.
            unsafe { libc::shm_unlink(name.as_ptr()) };
            // SAFETY: an owned descriptor.
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
                return Err(io::Error::last_os_error());
            }
            return Ok(descriptor);
        }
        Err(io::Error::from(io::ErrorKind::AlreadyExists))
    }

    /// macOS sizes a shared memory object once: it cannot shrink later.
    #[cfg(not(target_os = "linux"))]
    #[allow(
        clippy::unnecessary_wraps,
        clippy::missing_const_for_fn,
        reason = "the signature is shared with the Linux seal, which can fail"
    )]
    fn seal(_descriptor: &OwnedFd) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(not(unix))]
mod imp {
    use std::io;
    use std::process::Command;

    pub struct Memory(std::convert::Infallible);

    impl Memory {
        pub fn create(_name: &str, _len: usize) -> io::Result<Self> {
            Err(io::ErrorKind::Unsupported.into())
        }

        pub const fn as_ptr(&self) -> *mut u8 {
            match self.0 {}
        }

        pub const fn len(&self) -> usize {
            match self.0 {}
        }

        pub const fn hand_to(&self, _command: &mut Command, _flag: &str) {
            match self.0 {}
        }

        pub const fn handed_over(&self) {
            match self.0 {}
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Runs `script` with the memory handed to it as its last argument,
    /// `--memory=N`, and gives back its exit status.
    fn run_child(memory: &mut SharedMemory, script: &str) -> io::Result<std::process::ExitStatus> {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            &format!("for argument; do fd=${{argument#--memory=}}; done\n{script}"),
            "child",
        ]);
        memory.hand_to(&mut command, "--memory=");
        let mut child = command.spawn()?;
        memory.handed_over();
        child.wait()
    }

    /// The child holds the descriptor; the parent's own is gone once it is
    /// handed over, and is not handed on again.
    #[test]
    fn a_child_inherits_the_memory_once() {
        let mut memory = SharedMemory::create("tilo-process-test", 4096).unwrap();
        assert_eq!(memory.len(), 4096);
        assert!(
            run_child(&mut memory, "[ -e /dev/fd/$fd ]")
                .unwrap()
                .success()
        );
        let mut again = Command::new("/bin/true");
        memory.hand_to(&mut again, "--memory=");
        assert_eq!(again.get_args().count(), 0);
    }

    /// What the child writes into the memory, the parent reads.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_child_writes_into_the_memory_it_inherits() {
        let mut memory = SharedMemory::create("tilo-process-test", 4096).unwrap();
        let status = run_child(
            &mut memory,
            "printf '\\001\\002\\003\\004' | dd of=/dev/fd/$fd bs=1 seek=8 conv=notrunc status=none",
        )
        .unwrap();
        assert!(status.success(), "{status}");
        // SAFETY: in bounds, and the child has exited.
        let written = unsafe { std::slice::from_raw_parts(memory.as_ptr(), 12) };
        assert_eq!(written, [0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4]);
    }

    #[test]
    fn empty_memory_is_refused() {
        assert!(SharedMemory::create("tilo-process-test", 0).is_err());
    }
}
