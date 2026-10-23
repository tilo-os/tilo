//! The initramfs of a micro-VM step: an uncompressed `newc` cpio archive, which the kernel
//! unpacks on top of its built-in one (that one has `/dev/console`).

use std::io::{self, Write};

/// A `newc` archive being written.
pub struct Cpio<W: Write> {
    out: W,
    written: u64,
    ino: u32,
}

impl<W: Write> Cpio<W> {
    pub fn new(out: W) -> Self {
        Self {
            out,
            written: 0,
            ino: 0,
        }
    }

    /// A directory, mode 0755.
    pub fn dir(&mut self, name: &str) -> io::Result<()> {
        self.entry(name, 0o040_755, &[])
    }

    /// A regular file with permissions `perm`.
    pub fn file(&mut self, name: &str, perm: u32, data: &[u8]) -> io::Result<()> {
        self.entry(name, 0o100_000 | (perm & 0o7777), data)
    }

    /// The trailer, and the writer back.
    pub fn finish(mut self) -> io::Result<W> {
        self.entry("TRAILER!!!", 0, &[])?;
        self.out.flush()?;
        Ok(self.out)
    }

    fn entry(&mut self, name: &str, mode: u32, data: &[u8]) -> io::Result<()> {
        let too_big = || {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "an initramfs entry is too large",
            )
        };
        let size = u32::try_from(data.len()).map_err(|_| too_big())?;
        let name_size = u32::try_from(name.len().saturating_add(1)).map_err(|_| too_big())?;
        self.ino = self.ino.saturating_add(1);
        let nlink = if mode & 0o040_000 != 0 { 2 } else { 1 };
        // magic, ino, mode, uid, gid, nlink, mtime, filesize, devmajor, devminor, rdevmajor,
        // rdevminor, namesize, check
        let header = format!(
            "070701{:08x}{mode:08x}{:08x}{:08x}{nlink:08x}{:08x}{size:08x}{:08x}{:08x}{:08x}{:08x}{name_size:08x}{:08x}",
            self.ino, 0, 0, 0, 0, 0, 0, 0, 0
        );
        self.put(header.as_bytes())?;
        self.put(name.as_bytes())?;
        self.put(&[0])?;
        self.pad()?;
        self.put(data)?;
        self.pad()
    }

    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write_all(bytes)?;
        self.written = self.written.saturating_add(bytes.len() as u64);
        Ok(())
    }

    /// Up to the next multiple of four bytes, as `newc` aligns headers and data.
    fn pad(&mut self) -> io::Result<()> {
        let zeros = [0_u8; 3];
        let n = usize::try_from(self.written.wrapping_neg() % 4).unwrap_or(0);
        self.put(zeros.get(..n).unwrap_or(&[]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_aligned_and_end_with_the_trailer() {
        let mut c = Cpio::new(Vec::new());
        c.dir("bin").unwrap();
        c.file("bin/x", 0o755, b"hello").unwrap();
        let out = c.finish().unwrap();
        assert_eq!(out.len() % 4, 0);
        assert!(out.starts_with(b"070701"));
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("bin/x\0"));
        assert!(text.contains("hello"));
        assert!(text.contains("TRAILER!!!\0"));
        let at = text.find("bin/x").unwrap() - 110;
        assert_eq!(text.get(at + 14..at + 22), Some("000081ed"));
        assert_eq!(text.get(at + 54..at + 62), Some("00000005"));
    }
}
