//! Bounded reads for host-owned path contracts and readers: never allocate an unbounded input.
//! Reads at most `max_bytes`, returning [`std::io::ErrorKind::NotFound`] for a missing file and
//! [`std::io::ErrorKind::FileTooLarge`] when the input exceeds the bound, never returning the bytes
//! of an input over the bound.

pub fn read(path: &std::path::Path, max_bytes: impl TryInto<u64>) -> std::io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    read_from(file, max_bytes)
}

fn invalid_bound() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "file byte bound is invalid",
    )
}

/// Reads at most `max_bytes` from `reader`, returning [`std::io::ErrorKind::FileTooLarge`] when it
/// yields more. Never reads more than one byte past `max_bytes`, so a growing or unbounded source
/// is refused without loading it.
pub fn read_from(
    reader: impl std::io::Read,
    max_bytes: impl TryInto<u64>,
) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let max_bytes: u64 = max_bytes.try_into().map_err(|_| invalid_bound())?;
    let bound = max_bytes.saturating_add(1);
    let mut bytes = Vec::new();
    reader.take(bound).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            "file exceeds its byte bound",
        ));
    }
    Ok(bytes)
}

/// [`read`], reporting a file over the bound as the caller's `too_large` error and other failures
/// (including a missing file) through its own.
pub fn read_or<E: From<std::io::Error>>(
    path: &std::path::Path,
    max_bytes: impl TryInto<u64>,
    too_large: E,
) -> Result<Vec<u8>, E> {
    read(path, max_bytes).map_err(|error| {
        if error.kind() == std::io::ErrorKind::FileTooLarge {
            too_large
        } else {
            error.into()
        }
    })
}

#[cfg(test)]
mod tests {
    use std::io;

    struct CountingReader<R> {
        inner: R,
        bytes_read: usize,
    }

    impl<R> CountingReader<R> {
        fn new(inner: R) -> Self {
            Self {
                inner,
                bytes_read: 0,
            }
        }
    }

    impl<R: io::Read> io::Read for CountingReader<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let count = self.inner.read(buf)?;
            self.bytes_read = self.bytes_read.saturating_add(count);
            Ok(count)
        }
    }

    #[test]
    fn a_bounded_read_reports_missing_and_large_files_and_follows_an_atomic_replacement() {
        let unique = tilo_clock::try_unix_now_ns().unwrap();
        let root =
            std::env::temp_dir().join(format!("tilo-bounded-{}-{unique}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("current.json");
        let read = || super::read(&path, 4);
        assert_eq!(
            read().unwrap_err().kind(),
            io::ErrorKind::NotFound,
            "a missing file is reported as NotFound"
        );
        std::fs::write(&path, b"old").unwrap();
        assert_eq!(read().unwrap(), b"old");
        let next = root.join("next");
        std::fs::write(&next, b"new!").unwrap();
        std::fs::rename(&next, &path).unwrap();
        assert_eq!(read().unwrap(), b"new!");
        std::fs::write(&path, b"12345").unwrap();
        let error = read().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::FileTooLarge);
        assert_eq!(error.to_string(), "file exceeds its byte bound");
        std::fs::write(&path, b"secret").unwrap();
        let error = read().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::FileTooLarge);
        assert!(!error.to_string().contains("secret"));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(read().unwrap_err().kind(), io::ErrorKind::NotFound);
        std::fs::create_dir(&path).unwrap();
        assert!(read().is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn read_from_reads_at_most_one_byte_past_the_limit_even_when_a_source_grows() {
        let mut exact = CountingReader::new(&b"abcd"[..]);
        assert_eq!(super::read_from(&mut exact, 4).unwrap(), b"abcd");
        assert_eq!(exact.bytes_read, 4);

        let mut one_over = CountingReader::new(&b"abcde"[..]);
        assert_eq!(
            super::read_from(&mut one_over, 4).unwrap_err().kind(),
            io::ErrorKind::FileTooLarge
        );
        assert_eq!(one_over.bytes_read, 5);

        let mut infinite = CountingReader::new(io::repeat(b'x'));
        assert_eq!(
            super::read_from(&mut infinite, 16).unwrap_err().kind(),
            io::ErrorKind::FileTooLarge
        );
        assert_eq!(infinite.bytes_read, 17);

        let mut empty = CountingReader::new(&b""[..]);
        assert_eq!(super::read_from(&mut empty, 0).unwrap(), b"");
        assert_eq!(empty.bytes_read, 0);

        let mut non_empty = CountingReader::new(&b"a"[..]);
        assert_eq!(
            super::read_from(&mut non_empty, 0).unwrap_err().kind(),
            io::ErrorKind::FileTooLarge
        );
        assert_eq!(non_empty.bytes_read, 1);

        let mut max_bound = CountingReader::new(&b"abc"[..]);
        assert_eq!(super::read_from(&mut max_bound, u64::MAX).unwrap(), b"abc");
        assert_eq!(max_bound.bytes_read, 3);

        // A file that grows after its metadata reported a smaller size is still refused without
        // reading more than one byte past the limit.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("growing.bin");
        std::fs::write(&path, b"ok").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        assert_eq!(file.metadata().unwrap().len(), 2);
        {
            use std::io::Write as _;
            let mut writer = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writer.write_all(&[b'z'; 64]).unwrap();
        }
        let mut counting = CountingReader::new(file);
        assert_eq!(
            super::read_from(&mut counting, 8).unwrap_err().kind(),
            io::ErrorKind::FileTooLarge
        );
        assert_eq!(counting.bytes_read, 9);

        #[cfg(unix)]
        {
            let zero = std::path::Path::new("/dev/zero");
            assert_eq!(std::fs::metadata(zero).unwrap().len(), 0);
            assert_eq!(
                super::read(zero, 16).unwrap_err().kind(),
                io::ErrorKind::FileTooLarge
            );
        }
    }
}
