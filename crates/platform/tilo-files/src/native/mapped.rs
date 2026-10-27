//! A whole native file's bytes, read-only, for a reader that wants many
//! large files at hand but reads few of them (a font database whose faces
//! are shaped on demand).
//!
//! On Unix the file is mapped privately and read-only, so its pages load
//! only when read; elsewhere its bytes are read into memory. On the web the
//! standard library has no file system and opening fails.

use std::io;
use std::path::Path;

/// A read-only copy of a file's bytes as they were when it was opened; see
/// the [module docs](self).
pub struct MappedFile {
    bytes: Bytes,
}

impl std::fmt::Debug for MappedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MappedFile")
            .field("len", &self.as_ref().len())
            .finish()
    }
}

impl MappedFile {
    /// Map (or read) the whole file at `path`, following symbolic links.
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            bytes: Bytes::open(path)?,
        })
    }
}

impl AsRef<[u8]> for MappedFile {
    fn as_ref(&self) -> &[u8] {
        self.bytes.as_slice()
    }
}

impl std::ops::Deref for MappedFile {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.bytes.as_slice()
    }
}

#[cfg(unix)]
struct Bytes {
    ptr: *mut libc::c_void,
    len: usize,
}

// SAFETY: the mapping is read-only and owned by this value until it drops;
// nothing writes through `ptr`.
#[cfg(unix)]
unsafe impl Send for Bytes {}
// SAFETY: as above; shared reads of immutable memory.
#[cfg(unix)]
unsafe impl Sync for Bytes {}

#[cfg(unix)]
impl Bytes {
    fn open(path: &Path) -> io::Result<Self> {
        use std::os::fd::AsRawFd as _;
        let file = std::fs::File::open(path)?;
        let len = usize::try_from(file.metadata()?.len())
            .map_err(|_| io::Error::new(io::ErrorKind::FileTooLarge, "file too large to map"))?;
        if len == 0 {
            return Ok(Self {
                ptr: std::ptr::null_mut(),
                len: 0,
            });
        }
        // SAFETY: a fresh read-only private mapping of a whole open file;
        // the descriptor may close once mapped.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { ptr, len })
    }

    fn as_slice(&self) -> &[u8] {
        if self.ptr.is_null() {
            return &[];
        }
        // SAFETY: `ptr` maps `len` readable bytes for as long as `self`.
        unsafe { std::slice::from_raw_parts(self.ptr.cast::<u8>(), self.len) }
    }
}

#[cfg(unix)]
impl Drop for Bytes {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: unmapping exactly the mapping this value owns.
            unsafe {
                libc::munmap(self.ptr, self.len);
            }
        }
    }
}

#[cfg(not(unix))]
struct Bytes(Vec<u8>);

#[cfg(not(unix))]
impl Bytes {
    fn open(path: &Path) -> io::Result<Self> {
        std::fs::read(path).map(Self)
    }

    fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mapped_file_reads_back_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("face.ttf");
        std::fs::write(&path, b"font bytes").unwrap();
        assert_eq!(&*MappedFile::open(&path).unwrap(), b"font bytes");
        let empty = dir.path().join("empty");
        std::fs::write(&empty, b"").unwrap();
        assert!(MappedFile::open(&empty).unwrap().is_empty());
        assert!(MappedFile::open(&dir.path().join("missing")).is_err());
    }
}
