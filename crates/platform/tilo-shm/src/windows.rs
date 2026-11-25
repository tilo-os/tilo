//! Windows's buffers: pagefile-backed sections.
//!
//! The writer creates a section at its final size, fills it through a view of its own, closes that
//! view, and keeps only a handle that maps the section read-only: [`from_bytes`] duplicates its
//! handle with read access alone and closes the one that could write. Once the reader holds that
//! handle, nothing anywhere can map the section writable again, which is the seal a memfd gets on
//! Linux. A reader is still handed whatever handle the other side chose to send, so
//! [`Mapping::map_sealed`] copies the bytes out once, as on macOS: a sender that kept a writable
//! handle can change the section, never the copy.

use std::io;
use std::os::windows::io::{
    AsHandle, AsRawHandle as _, BorrowedHandle, FromRawHandle as _, OwnedHandle,
};

use windows_sys::Win32::Foundation::{
    DUPLICATE_CLOSE_SOURCE, DuplicateHandle, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_BASIC_INFORMATION,
    MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, PAGE_READWRITE, SEC_COMMIT, UnmapViewOfFile,
    VirtualQuery,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use super::{Backing, MAX_PAGE, Mapping};

/// A view of a section, unmapped when dropped.
struct View {
    address: MEMORY_MAPPED_VIEW_ADDRESS,
    /// The bytes the view spans: the section's size, rounded up to whole pages.
    len: usize,
}

impl View {
    /// The whole of `section`, mapped with `access`.
    fn of(section: BorrowedHandle<'_>, access: u32) -> io::Result<Self> {
        // SAFETY: a live section handle; offset 0 and length 0 map all of it.
        let address = unsafe { MapViewOfFile(section.as_raw_handle(), access, 0, 0, 0) };
        if address.Value.is_null() {
            return Err(io::Error::last_os_error());
        }
        let view = Self { address, len: 0 };
        let mut region = MEMORY_BASIC_INFORMATION {
            BaseAddress: std::ptr::null_mut(),
            AllocationBase: std::ptr::null_mut(),
            AllocationProtect: 0,
            PartitionId: 0,
            RegionSize: 0,
            State: 0,
            Protect: 0,
            Type: 0,
        };
        // SAFETY: the address of the view just mapped, and a buffer of the size passed.
        let written = unsafe {
            VirtualQuery(
                view.address.Value,
                &raw mut region,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if written == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            len: region.RegionSize,
            ..view
        })
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: the view spans `len` mapped bytes and lives as long as `self`.
        unsafe { std::slice::from_raw_parts(self.address.Value.cast::<u8>(), self.len) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: a writable view this process alone holds, spanning `len` mapped bytes.
        unsafe { std::slice::from_raw_parts_mut(self.address.Value.cast::<u8>(), self.len) }
    }
}

impl Drop for View {
    fn drop(&mut self) {
        // SAFETY: a view `MapViewOfFile` returned, unmapped once.
        unsafe { UnmapViewOfFile(self.address) };
    }
}

/// A section holding a copy of `bytes`, as a handle that only maps it read-only: no process can
/// write it once this returns. `name` is a diagnostic label; sections here are unnamed.
///
/// # Errors
/// The system's error creating, filling or narrowing the section.
pub fn from_bytes(name: &str, bytes: &[u8]) -> io::Result<OwnedHandle> {
    let _ = name;
    // A section is never empty: an empty buffer is one zero byte the reader never reads.
    let size = u64::try_from(bytes.len().max(1)).map_err(io::Error::other)?;
    // SAFETY: a pagefile-backed section (no file), no attributes, no name.
    let section = unsafe {
        CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            std::ptr::null(),
            PAGE_READWRITE | SEC_COMMIT,
            u32::try_from(size >> 32).unwrap_or(u32::MAX),
            u32::try_from(size & u64::from(u32::MAX)).unwrap_or(u32::MAX),
            std::ptr::null(),
        )
    };
    if section.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a section handle CreateFileMappingW just returned, owned from here.
    let section = unsafe { OwnedHandle::from_raw_handle(section) };
    {
        let mut view = View::of(section.as_handle(), FILE_MAP_WRITE)?;
        view.bytes_mut()
            .get_mut(..bytes.len())
            .ok_or_else(|| io::Error::other("a section shorter than requested"))?
            .copy_from_slice(bytes);
    }
    read_only(section)
}

/// `section` as a handle with read access alone; the handle that could write it is closed.
fn read_only(section: OwnedHandle) -> io::Result<OwnedHandle> {
    let mut narrowed: HANDLE = std::ptr::null_mut();
    // DUPLICATE_CLOSE_SOURCE closes the writable handle whether or not the duplicate is made.
    let source = section.as_raw_handle();
    std::mem::forget(section);
    // SAFETY: a section handle this process owns, given up to the call, and an output pointer.
    let duplicated = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            source,
            GetCurrentProcess(),
            &raw mut narrowed,
            FILE_MAP_READ,
            0,
            DUPLICATE_CLOSE_SOURCE,
        )
    };
    if duplicated == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a handle DuplicateHandle just made in this process, owned from here.
    Ok(unsafe { OwnedHandle::from_raw_handle(narrowed) })
}

impl Mapping {
    /// The bytes of `section`, at least `min_len` and at most `max_len` of them, copied once into
    /// this process's own memory. A section's size is whole pages, so it may exceed `max_len` by
    /// less than a page; what lies beyond `max_len` is not copied. A writable buffer (`immutable`
    /// false) is shared memory both sides keep writing, which nothing here hands over, and is
    /// refused.
    ///
    /// # Errors
    /// A writable buffer, a size out of range, or the system's error mapping the section.
    pub fn map_sealed(
        section: &impl AsHandle,
        immutable: bool,
        min_len: usize,
        max_len: usize,
    ) -> io::Result<Self> {
        if !immutable {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a writable shared buffer needs sealed memory, which only Linux has",
            ));
        }
        let view = View::of(section.as_handle(), FILE_MAP_READ)?;
        let size = view.len;
        let len = size.min(max_len);
        if size < min_len || len == 0 || size.saturating_sub(max_len) >= MAX_PAGE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a shared buffer's size is out of range",
            ));
        }
        let copied = view.bytes().get(..len).unwrap_or_default().to_vec();
        Ok(Self {
            backing: Backing::Copied(copied),
            len,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_section_round_trips_and_its_handle_cannot_write() {
        let section = from_bytes("t", b"hello").unwrap();
        let bytes = Mapping::map_sealed(&section, true, 5, 5).unwrap();
        assert_eq!(&bytes[..], b"hello");
        assert!(
            View::of(section.as_handle(), FILE_MAP_WRITE).is_err(),
            "the handle a reader gets maps the section read-only"
        );
        assert!(Mapping::map_sealed(&section, true, 0, 0).is_err());
        assert!(Mapping::map_sealed(&section, true, 1 << 30, 1 << 30).is_err());
        assert!(Mapping::map_sealed(&section, false, 5, 5).is_err());
    }
}
