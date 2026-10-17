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

/// The terminal result of running a job to completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobResult {
    /// Written and verified: the read-back matched the source byte-for-byte.
    Verified,
    /// The write itself failed (I/O error, permission denied, short write).
    WriteFailed(String),
    /// The write finished but the read-back did not match at this byte offset.
    VerifyMismatch(u64),
    /// The verify pass could not be run (I/O error re-reading).
    VerifyFailed(String),
    /// The operator requested a cooperative stop. The partially written media
    /// is never reported as usable and must be rewritten before use.
    Cancelled,
}

impl JobResult {
    pub fn is_success(&self) -> bool {
        matches!(self, JobResult::Verified)
    }
}

/// Streaming buffer shared by copy, hash and read-back operations.
pub const CHUNK: usize = 4 * 1024 * 1024;

pub fn cancelled_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "image write cancelled")
}

// ---------------------------------------------------------------------------
// The real engine. `stream_copy` + `verify_readback` are the exact loops that
// flash a device; tests point them at temp files so the byte path is proven
// without touching hardware.
// ---------------------------------------------------------------------------

/// Stream `src` onto `dst` in [`CHUNK`]-sized blocks, flushing and syncing at
/// the end. `on_progress` is called with the cumulative bytes written after
/// each chunk (for the live progress bar). Returns the total bytes copied.
///
/// `dst` is opened for writing (created if absent — for a block device the
/// node already exists and is opened in place). This is the real copy loop:
/// in tests `dst` is a temp file; in a gated production run it is `/dev/sdX`.
///
/// # Errors
///
/// Returns an error if `src` cannot be opened for reading, `dst` cannot be
/// opened for writing, any chunk read or write fails, or the final flush or
/// sync to `dst` fails.
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

/// Copy exactly the source which was reviewed before elevation. The file is
/// identified from the opened handle (not from a separate path lookup), then
/// read to the exact reviewed length with an explicit EOF check. Catalog media
/// additionally carries a signed expected digest, calculated during the copy.
/// # Errors
/// Rejects changed source identity, size or signed digest, cancellation, and native source/destination I/O failures.
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
    // `truncate(true)`: a fresh, exact byte-for-byte copy — no stale trailing
    // bytes survive from whatever previously occupied `dst`. Truncation is a
    // no-op on a real block device (not a regular file), so this is safe for
    // both the temp-file test path and the gated device path.
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
        // `n` is guaranteed <= buf.len() by the `Read::read` contract, but a
        // block-device write is exactly the place to fail loudly rather than
        // trust that contract silently.
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

/// Hash exactly the reviewed source length and require EOF. This runs before
/// a catalog-backed write opens its destination, so a corrupt cached image
/// cannot cause any destructive I/O.
/// # Errors
/// Returns an error on cancellation, a short/long source relative to review, or a native read failure.
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

/// Re-read `dst` and compare it against `src`, [`CHUNK`] bytes at a time, over
/// the first `len` bytes (the region the image occupies). Returns `Ok(None)`
/// when every byte matches, or `Ok(Some(offset))` at the first differing byte.
/// `on_progress` reports cumulative bytes verified. This is a genuine
/// post-write read-back — the device is re-opened and its bytes are compared,
/// not merely re-hashed from memory.
///
/// # Errors
///
/// Returns an error if `src` or `dst` cannot be opened or seeked to the
/// start, or if a chunk read during the comparison fails (including a short
/// read / unexpected EOF).
pub fn verify_readback(
    src: &Path,
    dst: &Path,
    len: u64,
    mut on_progress: impl FnMut(u64),
) -> io::Result<Option<u64>> {
    verify_readback_bound(src, dst, len, &|| false, &mut on_progress)
}

/// # Errors
/// Returns an error on cancellation, native open/seek/read failures, or unexpectedly short source/destination content.
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
        // `want` <= CHUNK == ba.len() == bb.len() by construction; still fail
        // loudly instead of trusting that on a device read-back.
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

/// Read exactly `buf.len()` bytes, tolerating short reads by looping. A source
/// image file and a freshly written device should both yield full chunks; a
/// premature EOF surfaces as an error the caller reports as a verify failure.
/// # Errors
/// Returns the underlying reader error, unexpected EOF, or an invalid/overflowing reader byte count.
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
