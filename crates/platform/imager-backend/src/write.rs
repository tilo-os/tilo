//! Exact, cancellable image copy and read-back verification on native files.
use crate::media_model::SourceIdentity;
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};

mod descriptor;
pub mod handoff;
pub mod helper;
pub mod job;
#[cfg(target_os = "linux")]
pub mod pending;
mod target;
pub mod transaction;
pub mod udisks;
pub use descriptor::*;
pub use target::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobResult {
    Verified,
    WriteFailed(String),
    VerifyMismatch(u64),
    VerifyFailed(String),
    /// The operator requested a cooperative stop; a partially written medium is never reported
    /// as usable.
    Cancelled,
}

impl JobResult {
    pub fn is_success(&self) -> bool {
        matches!(self, JobResult::Verified)
    }
}

pub const CHUNK: usize = 4 * 1024 * 1024;

pub fn cancelled_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "image write cancelled")
}

/// Streams `src` onto `dst` in [`CHUNK`] blocks, flushing and syncing at the end; `on_progress`
/// gets the cumulative bytes written after each chunk.
///
/// # Errors
///
/// Fails when either side cannot be opened, a chunk read or write fails, or the final sync fails.
pub fn stream_copy(src: &Path, dst: &Path, mut on_progress: impl FnMut(u64)) -> io::Result<u64> {
    let expected_size = src.metadata()?.len();
    stream_copy_bound(
        src,
        dst,
        expected_size,
        None,
        None,
        &|| false,
        &mut on_progress,
    )
}

/// Copies the source reviewed before elevation, identified from the opened handle and read to the
/// exact reviewed length with an EOF check; catalog media also carries a signed expected digest.
///
/// # Errors
///
/// Rejects changed source identity, size or digest, cancellation, and native I/O failures.
pub fn stream_copy_bound(
    src: &Path,
    dst: &Path,
    expected_size: u64,
    expected_identity: Option<SourceIdentity>,
    expected_sha256: Option<&str>,
    should_cancel: &impl Fn() -> bool,
    mut on_progress: impl FnMut(u64),
) -> io::Result<u64> {
    if should_cancel() {
        return Err(cancelled_error());
    }
    let mut reader = OpenOptions::new().read(true).open(src)?;
    let metadata = reader.metadata()?;
    if !metadata.is_file() || metadata.len() != expected_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source image size changed after review",
        ));
    }
    if let Some(expected) = expected_identity
        && SourceIdentity::from_file(&reader)? != Some(expected)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source image was replaced after review",
        ));
    }
    if expected_sha256.is_some_and(|digest| {
        digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source image has an invalid expected digest",
        ));
    }
    if let Some(expected) = expected_sha256 {
        let actual = hash_source_exact(&mut reader, expected_size, should_cancel)?;
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source image no longer matches its signed release catalog",
            ));
        }
        reader.seek(SeekFrom::Start(0))?;
    }
    // `truncate(true)` leaves no stale trailing bytes; it is a no-op on a block device, so this
    // suits both the temp-file tests and the device path.
    let regular_file = dst.metadata().map_or(true, |meta| meta.is_file());
    let mut options = OpenOptions::new();
    options.write(true);
    if regular_file {
        options.create(true).truncate(true);
    }
    let mut writer = options.open(dst)?;
    let mut buf = vec![0u8; CHUNK];
    let mut written: u64 = 0;
    let mut hash = Sha256::new();
    while written < expected_size {
        if should_cancel() {
            return Err(cancelled_error());
        }
        let remaining = usize::try_from(
            expected_size
                .checked_sub(written)
                .ok_or_else(|| io::Error::other("Source copy exceeds reviewed length"))?
                .min(CHUNK as u64),
        )
        .map_err(|_| io::Error::other("source chunk size is out of range"))?;
        let target = buf
            .get_mut(..remaining)
            .ok_or_else(|| io::Error::other("source chunk size exceeds buffer"))?;
        let n = reader.read(target)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "source image became shorter after review",
            ));
        }
        // A block-device write is exactly where to fail loudly instead of trusting the read
        // contract.
        let chunk = buf
            .get(..n)
            .ok_or_else(|| io::Error::other("read() returned a length past the buffer end"))?;
        writer.write_all(chunk)?;
        hash.update(chunk);
        written = written
            .checked_add(u64::try_from(n).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("Source copy length overflows"))?;
        on_progress(written);
    }
    let mut extra = [0_u8; 1];
    if reader.read(&mut extra)? != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source image became larger after review",
        ));
    }
    if let Some(expected) = expected_sha256 {
        let actual = hex::encode(hash.finalize());
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source image no longer matches its signed release catalog",
            ));
        }
    }
    writer.flush()?;
    writer.sync_all()?;
    if should_cancel() {
        return Err(cancelled_error());
    }
    Ok(written)
}

/// Runs before a catalog-backed write opens its destination, so a corrupt cached image cannot
/// cause destructive I/O.
pub fn hash_source_exact(
    reader: &mut impl Read,
    expected_size: u64,
    should_cancel: &impl Fn() -> bool,
) -> io::Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; CHUNK];
    let mut read_total = 0_u64;
    while read_total < expected_size {
        if should_cancel() {
            return Err(cancelled_error());
        }
        let want = usize::try_from(
            expected_size
                .checked_sub(read_total)
                .ok_or_else(|| io::Error::other("Source hash exceeds reviewed length"))?
                .min(CHUNK as u64),
        )
        .map_err(|_| io::Error::other("source hash chunk size is out of range"))?;
        let chunk = buffer
            .get_mut(..want)
            .ok_or_else(|| io::Error::other("source hash chunk size exceeds buffer"))?;
        read_exact_fill(reader, chunk).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("source image became shorter after review: {error}"),
            )
        })?;
        hash.update(chunk);
        read_total = read_total
            .checked_add(u64::try_from(want).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("Source hash length overflows"))?;
    }
    let mut extra = [0_u8; 1];
    if reader.read(&mut extra)? != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source image became larger after review",
        ));
    }
    Ok(hex::encode(hash.finalize()))
}

/// Re-opens `dst` and compares the first `len` bytes against `src` [`CHUNK`] at a time: `Ok(None)`
/// when all match, else the first differing offset. `on_progress` reports cumulative bytes read.
pub fn verify_readback(
    src: &Path,
    dst: &Path,
    len: u64,
    mut on_progress: impl FnMut(u64),
) -> io::Result<Option<u64>> {
    verify_readback_bound(src, dst, len, &|| false, &mut on_progress)
}

pub fn verify_readback_bound(
    src: &Path,
    dst: &Path,
    len: u64,
    should_cancel: &impl Fn() -> bool,
    mut on_progress: impl FnMut(u64),
) -> io::Result<Option<u64>> {
    if should_cancel() {
        return Err(cancelled_error());
    }
    let mut a = OpenOptions::new().read(true).open(src)?;
    let mut b = OpenOptions::new().read(true).open(dst)?;
    a.seek(SeekFrom::Start(0))?;
    b.seek(SeekFrom::Start(0))?;
    let mut ba = vec![0u8; CHUNK];
    let mut bb = vec![0u8; CHUNK];
    let mut done: u64 = 0;
    while done < len {
        if should_cancel() {
            return Err(cancelled_error());
        }
        let want = usize::try_from(
            len.checked_sub(done)
                .ok_or_else(|| io::Error::other("Read-back exceeds reviewed length"))?
                .min(CHUNK as u64),
        )
        .map_err(io::Error::other)?;
        // `want` fits both buffers by construction; still fail loudly on a device read-back.
        let chunk_a = ba
            .get_mut(..want)
            .ok_or_else(|| io::Error::other("verify chunk size exceeds buffer"))?;
        read_exact_fill(&mut a, chunk_a)?;
        let chunk_b = bb
            .get_mut(..want)
            .ok_or_else(|| io::Error::other("verify chunk size exceeds buffer"))?;
        read_exact_fill(&mut b, chunk_b)?;
        let cmp_a = ba
            .get(..want)
            .ok_or_else(|| io::Error::other("verify chunk size exceeds buffer"))?;
        let cmp_b = bb
            .get(..want)
            .ok_or_else(|| io::Error::other("verify chunk size exceeds buffer"))?;
        if let Some(i) = first_diff(cmp_a, cmp_b) {
            return done
                .checked_add(u64::try_from(i).map_err(io::Error::other)?)
                .map(Some)
                .ok_or_else(|| io::Error::other("Read-back mismatch offset overflows"));
        }
        done = done
            .checked_add(u64::try_from(want).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("Read-back length overflows"))?;
        on_progress(done);
    }
    if should_cancel() {
        return Err(cancelled_error());
    }
    Ok(None)
}

/// Tolerates short reads by looping; a premature EOF surfaces as an error the caller reports as a
/// verify failure.
pub fn read_exact_fill(r: &mut impl Read, buf: &mut [u8]) -> io::Result<()> {
    let mut off = 0;
    while off < buf.len() {
        let rest = buf
            .get_mut(off..)
            .ok_or_else(|| io::Error::other("read offset past buffer end"))?;
        let n = r.read(rest)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "short read during verify",
            ));
        }
        off = off
            .checked_add(n)
            .ok_or_else(|| io::Error::other("Read offset overflows"))?;
    }
    Ok(())
}

pub fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter().zip(b.iter()).position(|(x, y)| x != y)
}
