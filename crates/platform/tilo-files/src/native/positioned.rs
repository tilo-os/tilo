//! Reads and writes at an offset in a file, on every host: `pread`/`pwrite` on Unix, and on
//! Windows the same calls given the offset with each request.
//!
//! On Windows a positioned read or write also moves the handle's cursor; nothing here reads or
//! writes through the cursor, so a file used through these calls alone behaves as on Unix.

use std::fs::File;
use std::io;

/// Positioned reads and writes on an open file.
pub trait FileAt {
    /// Read into `buf` from `offset`, answering how much was read: fewer bytes than asked only at
    /// the end of the file.
    ///
    /// # Errors
    /// The system's error reading the file.
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;

    /// Write `buf` from `offset`, answering how much was written.
    ///
    /// # Errors
    /// The system's error writing the file.
    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<usize>;

    /// Fill `buf` from `offset`.
    ///
    /// # Errors
    /// The file ends first ([`io::ErrorKind::UnexpectedEof`]), or the system's error reading it.
    fn read_exact_at(&self, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
        while !buf.is_empty() {
            match self.read_at(buf, offset) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the file ended before the bytes asked for",
                    ));
                }
                Ok(read) => {
                    buf = buf.get_mut(read..).unwrap_or_default();
                    offset = offset.saturating_add(read as u64);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Write all of `buf` from `offset`.
    ///
    /// # Errors
    /// The system's error writing the file.
    fn write_all_at(&self, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
        while !buf.is_empty() {
            match self.write_at(buf, offset) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
                Ok(written) => {
                    buf = buf.get(written..).unwrap_or_default();
                    offset = offset.saturating_add(written as u64);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

impl FileAt for File {
    #[cfg(unix)]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(self, buf, offset)
    }

    #[cfg(unix)]
    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<usize> {
        std::os::unix::fs::FileExt::write_at(self, buf, offset)
    }

    #[cfg(windows)]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        std::os::windows::fs::FileExt::seek_read(self, buf, offset)
    }

    #[cfg(windows)]
    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<usize> {
        std::os::windows::fs::FileExt::seek_write(self, buf, offset)
    }

    #[cfg(not(any(unix, windows)))]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let _ = (buf, offset);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    #[cfg(not(any(unix, windows)))]
    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<usize> {
        let _ = (buf, offset);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}
