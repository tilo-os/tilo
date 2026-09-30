//! Bulk file I/O for async code, on dedicated threads.
//!
//! `tokio::fs` sends every call through the runtime's blocking pool, one
//! thread hop per read or write, sharing that pool with every other blocking
//! call in the process. Storage data paths instead submit whole operations
//! here: std I/O on a small fixed pool of `tilo-io-N` threads.
//!
//! The operations are deliberately few and owned — whole-file read and
//! write, positioned read and write on a shared [`File`], `fsync` — because
//! that is the shape an `io_uring` submission queue takes. Replacing the pool
//! with a ring changes this module, not its callers. [`run`] is the escape
//! hatch for a compound operation (open, stat, rename) that has no dedicated
//! entry point yet.

use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use tokio::sync::oneshot;

use crate::pool::JobPool;

/// Threads in the I/O pool. Enough to keep a few operations in flight per
/// device without a thread per request.
const IO_THREADS: usize = 4;

fn pool() -> Result<&'static JobPool, std::io::Error> {
    static POOL: OnceLock<Result<JobPool, String>> = OnceLock::new();
    // A browser page has neither threads nor a local filesystem.
    if cfg!(target_family = "wasm") {
        return Err(std::io::ErrorKind::Unsupported.into());
    }
    POOL.get_or_init(|| JobPool::start("tilo-io", IO_THREADS).map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|message| std::io::Error::other(message.clone()))
}

/// Run `op` on an I/O thread and await its result.
///
/// # Errors
///
/// `op`'s error, or an error when the I/O pool is unavailable.
pub async fn run<R, F>(op: F) -> std::io::Result<R>
where
    F: FnOnce() -> std::io::Result<R> + Send + 'static,
    R: Send + 'static,
{
    let (tx, rx) = oneshot::channel();
    let submitted = pool()?.submit(move || {
        if !tx.is_closed() {
            let _ = tx.send(op());
        }
    });
    if !submitted {
        return Err(std::io::Error::other("the I/O pool has stopped"));
    }
    rx.await
        .map_err(|_| std::io::Error::other("an I/O operation panicked"))?
}

/// Read a whole file.
///
/// # Errors
///
/// As [`std::fs::read`].
pub async fn read(path: impl Into<PathBuf>) -> std::io::Result<Vec<u8>> {
    let path = path.into();
    run(move || std::fs::read(path)).await
}

/// Create or truncate `path` and write `contents` to it.
///
/// # Errors
///
/// As [`std::fs::write`].
pub async fn write(path: impl Into<PathBuf>, contents: Vec<u8>) -> std::io::Result<()> {
    let path = path.into();
    run(move || std::fs::write(path, contents)).await
}

/// Read up to `len` bytes at `offset`. Returns fewer only at end of file.
///
/// # Errors
///
/// The underlying positioned-read error.
pub async fn read_at(file: Arc<File>, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
    run(move || {
        let mut buffer = vec![0_u8; len];
        let mut filled = 0_usize;
        while let Some(rest) = buffer.get_mut(filled..).filter(|rest| !rest.is_empty()) {
            let at = offset.saturating_add(u64::try_from(filled).unwrap_or(u64::MAX));
            match positioned::read(&file, rest, at) {
                Ok(0) => break,
                Ok(read) => filled = filled.saturating_add(read),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        buffer.truncate(filled);
        Ok(buffer)
    })
    .await
}

/// Write all of `data` at `offset`.
///
/// # Errors
///
/// The underlying positioned-write error.
pub async fn write_all_at(file: Arc<File>, offset: u64, data: Vec<u8>) -> std::io::Result<()> {
    run(move || {
        let mut written = 0_usize;
        while let Some(rest) = data.get(written..).filter(|rest| !rest.is_empty()) {
            let at = offset.saturating_add(u64::try_from(written).unwrap_or(u64::MAX));
            match positioned::write(&file, rest, at) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(count) => written = written.saturating_add(count),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    })
    .await
}

/// Flush `file`'s data and metadata to the device.
///
/// # Errors
///
/// As [`File::sync_all`].
pub async fn sync_all(file: Arc<File>) -> std::io::Result<()> {
    run(move || file.sync_all()).await
}

#[cfg(unix)]
mod positioned {
    use std::fs::File;
    use std::os::unix::fs::FileExt;

    pub(super) fn read(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
        file.read_at(buffer, offset)
    }

    pub(super) fn write(file: &File, data: &[u8], offset: u64) -> std::io::Result<usize> {
        file.write_at(data, offset)
    }
}

#[cfg(windows)]
mod positioned {
    use std::fs::File;
    use std::os::windows::fs::FileExt;

    pub(super) fn read(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
        file.seek_read(buffer, offset)
    }

    pub(super) fn write(file: &File, data: &[u8], offset: u64) -> std::io::Result<usize> {
        file.seek_write(data, offset)
    }
}

#[cfg(not(any(unix, windows)))]
mod positioned {
    use std::fs::File;
    use std::io::{Error, ErrorKind};

    pub(super) fn read(_file: &File, _buffer: &mut [u8], _offset: u64) -> std::io::Result<usize> {
        Err(Error::from(ErrorKind::Unsupported))
    }

    pub(super) fn write(_file: &File, _data: &[u8], _offset: u64) -> std::io::Result<usize> {
        Err(Error::from(ErrorKind::Unsupported))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::current_dir()
            .unwrap()
            .join("target")
            .join("tilo-rt-io-tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!("{name}-{}", std::process::id()))
    }

    #[test]
    fn file_io_round_trips_off_the_runtime_and_returns_errors() {
        // phase: whole_file_and_positioned_io_round_trip_off_the_runtime
        {
            let path = scratch("round-trip");
            crate::block_on(async {
                write(path.clone(), b"hello world".to_vec()).await.unwrap();
                assert_eq!(read(path.clone()).await.unwrap(), b"hello world");
                let file = Arc::new(
                    std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&path)
                        .unwrap(),
                );
                write_all_at(Arc::clone(&file), 6, b"tilo!".to_vec())
                    .await
                    .unwrap();
                sync_all(Arc::clone(&file)).await.unwrap();
                assert_eq!(read_at(Arc::clone(&file), 6, 64).await.unwrap(), b"tilo!");
                let thread = run(|| Ok(std::thread::current().name().map(str::to_owned)))
                    .await
                    .unwrap();
                assert!(thread.unwrap().starts_with("tilo-io-"));
            })
            .unwrap();
            std::fs::remove_file(path).unwrap();
        }
        // phase: errors_come_back_to_the_caller
        {
            let missing = scratch("missing").join("absent");
            let error = crate::block_on(read(missing)).unwrap().unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        }
    }
}
