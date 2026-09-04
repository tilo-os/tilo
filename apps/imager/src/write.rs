//! The write / verify seam — the real, destructive work, safely gated.
//!
//! Writing a disk image onto a block device is **destructive** and needs
//! privileged raw access. This module is the single choke point where that
//! work happens. It ships a **real** streaming copy + read-back verify
//! engine ([`stream_copy`], [`verify_readback`], [`run_job_paths`]) — the
//! exact byte loop that flashes a USB stick — and three independent safety
//! gates in front of the block device:
//!
//! 1. **Typed confirmation.** A job only begins after the UI has collected an
//!    explicit, typed confirmation: the operator must type the target device
//!    name. [`begin_job`] re-checks that token against the target before it
//!    returns [`JobStart::Started`].
//!
//! 2. **Write-time privilege.** The wizard always runs as the signed-in user.
//!    Only after confirmation does it ask the host to authenticate a short-
//!    lived helper mode of this executable. The helper receives a secret-free
//!    request, repeats every target check, writes, verifies, and exits.
//!
//! 3. **Capacity + fit checks** ([`WriteRequest`] / [`TargetDrive::fits`]).
//!
//! The engine is exercised in tests against **temp files** (a real copy +
//! verify, byte-for-byte) — never a block device — so the production loop is
//! proven without any risk to hardware.

use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use crate::model::{
    Action, InstallProfile, JobPhase, Progress, Source, SourceIdentity, TargetDrive,
};
use sha2::{Digest, Sha256};

/// The chunk size of the streaming copy / verify loop (4 MiB) — large enough
/// to keep the device busy, small enough to report smooth progress.
pub const CHUNK: usize = 4 * 1024 * 1024;

/// Fixed block at the end of a flashed device carrying a secret-free profile.
/// Keeping it outside the image partitions makes the handoff identical on
/// Linux, macOS and Windows and lets the live medium read it before touching
/// any target disk.
pub const PROFILE_BLOCK_SIZE: usize = 64 * 1024;
pub const PROFILE_MAGIC: &str = "QOS_INSTALL_PROFILE_V1";

/// Outcome of asking to start a job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobStart {
    /// Pre-conditions satisfied; the real job may run.
    Started,
    /// Refused because privileged raw-device writes are not enabled.
    WriteDisabled,
    /// Refused — the confirmation token did not match the target device.
    /// This is the safety gate: a typo means no write.
    ConfirmationMismatch,
    /// Refused — no source or no target selected.
    Incomplete,
    /// Refused — the image is larger than the target drive.
    TooSmall,
    /// Refused because desktop imaging is restricted to external media. The
    /// booted qOS installer owns internal-disk and dual-boot installation.
    UnsafeTarget,
    /// Refused before any I/O because an installable image's embedded profile
    /// would be rejected by the live installer.
    InvalidProfile(String),
}

/// Which engine actually performs the write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteBackend {
    /// Raw-device effects are unavailable in this process (tests and shots).
    Disabled,
    /// Real raw write via a direct block-device open. Selecting this is itself
    /// gated (see [`WriteBackend::system`]); it is never the default.
    System,
    /// The unprivileged live UI will authenticate a short-lived platform
    /// helper at write time.
    PrivilegedHelper,
}

impl WriteBackend {
    /// Choose the backend for the running process. The allow gate is retained
    /// for deliberate root-side diagnostics; normal live UI uses the narrow
    /// helper, while tests and shots remain disabled.
    pub fn system() -> Self {
        if allow_write() {
            WriteBackend::System
        } else if cfg!(feature = "host-media-writer")
            || std::env::var("IMAGER_LIVE").as_deref() == Ok("1")
        {
            WriteBackend::PrivilegedHelper
        } else {
            WriteBackend::Disabled
        }
    }

    /// Whether this backend can perform a real write. Both the deliberately
    /// allow-gated direct backend and the normal live UI's short-lived helper
    /// are usable; tests and screenshot rendering remain disabled.
    pub fn is_available(self) -> bool {
        self != WriteBackend::Disabled
    }
}

/// Whether the running process has a real raw-media write path.
///
/// The live application normally returns `true` because it authenticates a
/// narrow privileged helper at write time. `QOS_IMAGER_ALLOW_WRITE` is only
/// the explicit diagnostic opt-in for writing directly in this process.
pub fn write_available() -> bool {
    WriteBackend::system().is_available()
}

/// The env allow-gate. `QOS_IMAGER_ALLOW_WRITE=1` is the single switch that
/// lets a real block-device write happen; unset (the default) keeps every
/// path unavailable. Modelled on the VPN app's `QOS_VPN_ALLOW_EXEC`.
pub fn allow_write() -> bool {
    std::env::var("QOS_IMAGER_ALLOW_WRITE").as_deref() == Ok("1")
}

/// A validated request to write `source` to `target` using `action`
/// (+ profile when pre-configuring). Construction does not start anything.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct WriteRequest {
    pub source: Source,
    pub target: TargetDrive,
    pub action: Action,
    pub profile: InstallProfile,
}

impl WriteRequest {
    /// Drop values which are intentionally never transported to installation
    /// media before this request crosses the privilege boundary.
    fn without_secrets(mut self) -> Self {
        self.profile.user.password.clear();
        self.profile.encryption_passphrase.clear();
        self
    }
}

impl WriteRequest {
    /// Validate that the typed confirmation token matches the target's device
    /// name (e.g. the operator typed `sdb` for `/dev/sdb`). This is the
    /// explicit typed gate required before any write.
    pub fn confirmation_matches(&self, typed: &str) -> bool {
        let typed = typed.trim();
        !typed.is_empty()
            && (typed == self.target.name
                || typed == self.target.dev_path()
                || typed == format!("/dev/{}", self.target.name))
    }
}

/// Attempt to start the job. Returns [`JobStart::Started`] only after every
/// safety pre-condition is met; otherwise a specific refusal the UI surfaces.
///
/// This is the pure *planning / dry-run* entry: it validates and never opens a
/// device. The live app hands a `Started` request to [`spawn_job`], which
/// chooses the backend; with the allow-gate off the request is refused.
pub fn begin_job(req: &WriteRequest, typed_confirmation: &str) -> JobStart {
    if req.source.size == 0 || req.target.name.is_empty() {
        return JobStart::Incomplete;
    }
    if !req.target.is_media_target() {
        return JobStart::UnsafeTarget;
    }
    let required = if req.source.installable {
        req.source.size.saturating_add(PROFILE_BLOCK_SIZE as u64)
    } else {
        req.source.size
    };
    if !req.target.fits(required) {
        return JobStart::TooSmall;
    }
    if req.source.installable
        && let Err(message) = req.profile.validate_media_profile()
    {
        return JobStart::InvalidProfile(message);
    }
    if !req.confirmation_matches(typed_confirmation) {
        return JobStart::ConfirmationMismatch;
    }
    JobStart::Started
}

/// Encode the on-media profile block. The compact JSON contains no secrets;
/// SHA-256 detects incomplete writes and accidental corruption. It is not an
/// authenticity claim: the installer treats every field as untrusted input,
/// validates it, and displays the result again on Review.
///
/// # Errors
///
/// Returns an error if the profile cannot be serialized or does not fit in the
/// reserved block.
pub fn encode_profile_block(profile: &InstallProfile) -> io::Result<Vec<u8>> {
    profile
        .validate_media_profile()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let payload = profile
        .to_media_profile_json()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let digest = format!("{:x}", Sha256::digest(&payload));
    let header = format!("{PROFILE_MAGIC}\n{digest}\n{}\n", payload.len());
    if header.len() + payload.len() > PROFILE_BLOCK_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "installation profile exceeds reserved media block",
        ));
    }
    let mut block = vec![0u8; PROFILE_BLOCK_SIZE];
    let payload_at = header.len();
    block
        .get_mut(..payload_at)
        .ok_or_else(|| io::Error::other("installation profile header is out of bounds"))?
        .copy_from_slice(header.as_bytes());
    block
        .get_mut(payload_at..payload_at + payload.len())
        .ok_or_else(|| io::Error::other("installation profile payload is out of bounds"))?
        .copy_from_slice(&payload);
    Ok(block)
}

/// Write and read back the complete profile block at the device tail.
///
/// # Errors
///
/// Returns an error if the target is too small, cannot be opened or written,
/// or if the complete read-back differs from the encoded profile block.
pub fn write_profile_block(
    dst: &Path,
    target_size: u64,
    profile: &InstallProfile,
) -> io::Result<()> {
    let block = encode_profile_block(profile)?;
    let offset = target_size
        .checked_sub(PROFILE_BLOCK_SIZE as u64)
        .ok_or_else(|| io::Error::other("target is too small for installation profile"))?;
    let mut device = OpenOptions::new().read(true).write(true).open(dst)?;
    device.seek(SeekFrom::Start(offset))?;
    device.write_all(&block)?;
    device.flush()?;
    device.sync_all()?;
    let mut actual = vec![0u8; PROFILE_BLOCK_SIZE];
    device.seek(SeekFrom::Start(offset))?;
    read_exact_fill(&mut device, &mut actual)?;
    if actual != block {
        return Err(io::Error::other(
            "installation profile read-back did not match",
        ));
    }
    Ok(())
}

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
}

impl JobResult {
    pub fn is_success(&self) -> bool {
        matches!(self, JobResult::Verified)
    }
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
    stream_copy_bound(src, dst, expected_size, None, None, &mut on_progress)
}

/// Copy exactly the source which was reviewed before elevation. The file is
/// identified from the opened handle (not from a separate path lookup), then
/// read to the exact reviewed length with an explicit EOF check. Catalog media
/// additionally carries a signed expected digest, calculated during the copy.
fn stream_copy_bound(
    src: &Path,
    dst: &Path,
    expected_size: u64,
    expected_identity: Option<SourceIdentity>,
    expected_sha256: Option<&str>,
    mut on_progress: impl FnMut(u64),
) -> io::Result<u64> {
    let mut reader = OpenOptions::new().read(true).open(src)?;
    let metadata = reader.metadata()?;
    if !metadata.is_file() || metadata.len() != expected_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source image size changed after review",
        ));
    }
    if let Some(expected) = expected_identity
        && SourceIdentity::from_metadata(&metadata) != Some(expected)
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
        let actual = hash_source_exact(&mut reader, expected_size)?;
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
        let remaining = usize::try_from((expected_size - written).min(CHUNK as u64))
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
        written += n as u64;
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
        let actual = format!("{:x}", hash.finalize());
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source image no longer matches its signed release catalog",
            ));
        }
    }
    writer.flush()?;
    writer.sync_all()?;
    Ok(written)
}

/// Hash exactly the reviewed source length and require EOF. This runs before
/// a catalog-backed write opens its destination, so a corrupt cached image
/// cannot cause any destructive I/O.
fn hash_source_exact(reader: &mut impl Read, expected_size: u64) -> io::Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; CHUNK];
    let mut read_total = 0_u64;
    while read_total < expected_size {
        let want = usize::try_from((expected_size - read_total).min(CHUNK as u64))
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
        read_total += want as u64;
    }
    let mut extra = [0_u8; 1];
    if reader.read(&mut extra)? != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source image became larger after review",
        ));
    }
    Ok(format!("{:x}", hash.finalize()))
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
    let mut a = OpenOptions::new().read(true).open(src)?;
    let mut b = OpenOptions::new().read(true).open(dst)?;
    a.seek(SeekFrom::Start(0))?;
    b.seek(SeekFrom::Start(0))?;
    let mut ba = vec![0u8; CHUNK];
    let mut bb = vec![0u8; CHUNK];
    let mut done: u64 = 0;
    while done < len {
        let want = CHUNK.min((len - done) as usize);
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
            return Ok(Some(done + i as u64));
        }
        done += want as u64;
        on_progress(done);
    }
    Ok(None)
}

/// Read exactly `buf.len()` bytes, tolerating short reads by looping. A source
/// image file and a freshly written device should both yield full chunks; a
/// premature EOF surfaces as an error the caller reports as a verify failure.
fn read_exact_fill(r: &mut impl Read, buf: &mut [u8]) -> io::Result<()> {
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
        off += n;
    }
    Ok(())
}

fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter().zip(b.iter()).position(|(x, y)| x != y)
}

/// Run a full write + verify against explicit paths, reporting a [`Progress`]
/// snapshot after every chunk through `on_progress`. Split from device
/// selection so tests drive the real loop against temp files.
pub fn run_job_paths(
    src: &Path,
    dst: &Path,
    total: u64,
    mut on_progress: impl FnMut(Progress),
) -> JobResult {
    run_job_paths_bound(src, dst, total, None, None, &mut on_progress)
}

fn run_job_paths_bound(
    src: &Path,
    dst: &Path,
    total: u64,
    expected_identity: Option<SourceIdentity>,
    expected_sha256: Option<&str>,
    mut on_progress: impl FnMut(Progress),
) -> JobResult {
    let total = total.max(1);
    // Write phase.
    let copied = stream_copy_bound(
        src,
        dst,
        total,
        expected_identity,
        expected_sha256,
        |bytes| {
            on_progress(Progress {
                phase: JobPhase::Writing,
                write_frac: (bytes as f64 / total as f64).min(1.0),
                verify_frac: 0.0,
                rate_bps: 0,
                error: None,
            });
        },
    );
    let written = match copied {
        Ok(n) => n,
        Err(e) => return JobResult::WriteFailed(e.to_string()),
    };
    // Verify phase over exactly the bytes we wrote.
    let verify = verify_readback(src, dst, written, |bytes| {
        on_progress(Progress {
            phase: JobPhase::Verifying,
            write_frac: 1.0,
            verify_frac: (bytes as f64 / written.max(1) as f64).min(1.0),
            rate_bps: 0,
            error: None,
        });
    });
    match verify {
        // This is not yet the user-visible terminal success: installable media
        // still needs its profile block written and read back, and the device
        // still needs platform finalisation. `run_system_job` publishes the
        // single terminal event only after all of those operations finish.
        Ok(None) => JobResult::Verified,
        Ok(Some(off)) => JobResult::VerifyMismatch(off),
        Err(e) => JobResult::VerifyFailed(e.to_string()),
    }
}

/// Run the real job for `req` (source file → target device), streaming
/// [`Progress`] over `tx`. Only ever called on a worker thread when the
/// allow-gate selected [`WriteBackend::System`].
fn run_system_job(req: &WriteRequest, mut on_progress: impl FnMut(Progress)) -> JobResult {
    let src = std::path::PathBuf::from(&req.source.path);
    let dst = std::path::PathBuf::from(req.target.dev_path());
    let mut prepared = false;
    let mut result = if let Err(error) = revalidate_target(&req.target) {
        JobResult::WriteFailed(format!(
            "the selected drive changed or was disconnected: {error}"
        ))
    } else if let Err(error) = prepare_target(&req.target) {
        JobResult::WriteFailed(format!(
            "the drive is still in use and could not be prepared: {error}"
        ))
    } else {
        prepared = true;
        run_job_paths_bound(
            &src,
            &dst,
            req.source.size,
            req.source.identity,
            req.source.expected_sha256.as_deref(),
            &mut on_progress,
        )
    };
    if prepared {
        if result.is_success()
            && req.source.installable
            && let Err(error) = write_profile_block(&dst, req.target.size, &req.profile)
        {
            result = JobResult::VerifyFailed(format!(
                "could not embed and verify the setup profile: {error}"
            ));
        }
        finish_target(&req.target);
    }
    let error = match &result {
        JobResult::Verified => None,
        JobResult::WriteFailed(message) => Some(format!("Could not write the drive: {message}")),
        JobResult::VerifyMismatch(offset) => Some(format!(
            "Verification found different data at byte {offset}. The drive may be faulty."
        )),
        JobResult::VerifyFailed(message) => {
            Some(format!("Could not verify the finished drive: {message}"))
        }
    };
    on_progress(Progress {
        phase: if result.is_success() {
            JobPhase::Success
        } else {
            JobPhase::Failed
        },
        write_frac: 1.0,
        verify_frac: if matches!(result, JobResult::WriteFailed(_)) {
            0.0
        } else {
            1.0
        },
        rate_bps: 0,
        error,
    });
    result
}

const WRITE_HELPER_ARG: &str = "--qos-write-helper";

// `Start-Process` flattens its `ArgumentList` into a native command line. A
// literal double quote must therefore surround each path so spaces remain in
// the same argv element when the elevated process starts. PowerShell's
// single-quoted `'"'` is exactly one double-quote character; `\"` would be a
// backslash followed by a quote and would not group the path.
#[cfg(any(test, target_os = "windows"))]
const WINDOWS_ELEVATION_SCRIPT: &str = r#"$p = Start-Process -FilePath $args[0] -Verb RunAs -Wait -PassThru -ArgumentList @('--qos-write-helper', ('"' + $args[1] + '"'), ('"' + $args[2] + '"'), $args[3]); exit $p.ExitCode"#;

/// Execute the privileged helper mode when `args` requests it. This is called
/// before the windowing runtime is initialized.
///
/// The request is deserialized from a user-private temporary file, then fully
/// validated and its live target identity is checked again by
/// [`run_system_job`]. Progress is append-only JSON so the unprivileged window
/// can remain responsive while the platform authentication broker waits.
///
/// # Errors
///
/// Returns a descriptive error for malformed arguments/request data or an I/O
/// failure in the helper protocol.
pub fn maybe_run_write_helper(args: &[String]) -> Result<Option<i32>, Box<dyn std::error::Error>> {
    let Some(at) = args.iter().position(|arg| arg == WRITE_HELPER_ARG) else {
        return Ok(None);
    };
    let request_path = args
        .get(at + 1)
        .ok_or("write helper request path is missing")?;
    let progress_path = args
        .get(at + 2)
        .ok_or("write helper progress path is missing")?;
    let expected_digest = args
        .get(at + 3)
        .ok_or("write helper request digest is missing")?;
    if args.get(at + 4).is_some() {
        return Err("write helper received unexpected arguments".into());
    }
    validate_helper_paths(Path::new(request_path), Path::new(progress_path))?;
    let request_bytes = std::fs::read(request_path)?;
    if request_bytes.len() > 1024 * 1024 {
        return Err("write helper request is unexpectedly large".into());
    }
    let actual_digest = format!("{:x}", Sha256::digest(&request_bytes));
    if expected_digest.len() != 64
        || !expected_digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !actual_digest.eq_ignore_ascii_case(expected_digest)
    {
        return Err("write helper request changed after authorization".into());
    }
    let request: WriteRequest = serde_json::from_slice(&request_bytes)?;
    if begin_job(&request, &request.target.dev_path()) != JobStart::Started {
        return Err("write helper rejected an invalid or unsafe request".into());
    }
    // The privileged process creates the progress inode itself. `create_new`
    // refuses a pre-planted symlink instead of letting a user-writable path
    // redirect root's append into an arbitrary file.
    let mut progress_file = create_progress_output(Path::new(progress_path))?;
    let mut terminal_sent = false;
    let result = run_system_job(&request, |progress| {
        terminal_sent |= matches!(progress.phase, JobPhase::Success | JobPhase::Failed);
        if serde_json::to_writer(&mut progress_file, &progress).is_ok() {
            let _ = progress_file.write_all(b"\n");
            let _ = progress_file.flush();
        }
    });
    if !terminal_sent {
        serde_json::to_writer(
            &mut progress_file,
            &Progress {
                phase: JobPhase::Failed,
                ..Progress::default()
            },
        )?;
        progress_file.write_all(b"\n")?;
        progress_file.flush()?;
    }
    Ok(Some(i32::from(!result.is_success())))
}

fn validate_helper_paths(request: &Path, progress: &Path) -> io::Result<()> {
    let request_parent = request
        .parent()
        .ok_or_else(|| io::Error::other("write helper request has no parent directory"))?;
    if progress.parent() != Some(request_parent)
        || request.file_name().and_then(|name| name.to_str()) != Some("request.json")
        || progress.file_name().and_then(|name| name.to_str()) != Some("progress.jsonl")
        || !request_parent
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("qos-imager-write-"))
    {
        return Err(io::Error::other(
            "write helper paths are outside its private job directory",
        ));
    }
    let metadata = request.symlink_metadata()?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::other(
            "write helper request is not a regular file",
        ));
    }
    Ok(())
}

fn create_progress_output(path: &Path) -> io::Result<std::fs::File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

fn spawn_privileged_helper(req: WriteRequest, tx: &Sender<Progress>) {
    let result = run_privileged_helper(req, tx);
    if let Err(error) = result {
        let _ = tx.send(Progress {
            phase: JobPhase::Failed,
            error: Some(format!(
                "Could not start the authorized writer: {error}. Check the authorization prompt and try again."
            )),
            ..Progress::default()
        });
    }
}

fn run_privileged_helper(req: WriteRequest, tx: &Sender<Progress>) -> io::Result<()> {
    let work = tempfile::Builder::new()
        .prefix("qos-imager-write-")
        .tempdir()?;
    let request_path = work.path().join("request.json");
    let progress_path = work.path().join("progress.jsonl");
    let mut request_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&request_path)?;
    let request_bytes = serde_json::to_vec(&req.without_secrets())?;
    request_file.write_all(&request_bytes)?;
    request_file.flush()?;
    request_file.sync_all()?;
    drop(request_file);
    let request_digest = format!("{:x}", Sha256::digest(&request_bytes));
    let executable = helper_executable()?;
    let mut child =
        elevated_helper_command(&executable, &request_path, &progress_path, &request_digest)?
            .spawn()?;
    relay_helper_progress(&mut child, &progress_path, tx)
}

fn helper_executable() -> io::Result<std::path::PathBuf> {
    #[cfg(target_os = "linux")]
    if let Some(appimage) = std::env::var_os("QOS_IMAGER_APPIMAGE") {
        let path = std::path::PathBuf::from(appimage);
        if path.is_absolute() && path.is_file() {
            return Ok(path);
        }
    }
    std::env::current_exe()
}

fn relay_helper_progress(
    child: &mut Child,
    progress_path: &Path,
    tx: &Sender<Progress>,
) -> io::Result<()> {
    let file = loop {
        match OpenOptions::new().read(true).open(progress_path) {
            Ok(file) => break file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if let Some(status) = child.try_wait()? {
                    let _ = tx.send(Progress {
                        phase: JobPhase::Failed,
                        error: Some(if status.success() {
                            "The writer stopped before reporting progress. Please try again."
                                .into()
                        } else {
                            "Authorization was cancelled or the writer could not start. Please try again."
                                .into()
                        }),
                        ..Progress::default()
                    });
                    return if status.success() {
                        Err(io::Error::other(
                            "writer exited without creating its progress channel",
                        ))
                    } else {
                        Ok(())
                    };
                }
                std::thread::sleep(Duration::from_millis(80));
            }
            Err(error) => return Err(error),
        }
    };
    let mut reader = BufReader::new(file);
    let mut terminal = false;
    loop {
        let mut line = String::new();
        while reader.read_line(&mut line)? != 0 {
            if let Ok(progress) = serde_json::from_str::<Progress>(line.trim_end()) {
                terminal |= matches!(progress.phase, JobPhase::Success | JobPhase::Failed);
                let _ = tx.send(progress);
            }
            line.clear();
        }
        if let Some(status) = child.try_wait()? {
            // Consume bytes written between the last EOF observation and exit.
            while reader.read_line(&mut line)? != 0 {
                if let Ok(progress) = serde_json::from_str::<Progress>(line.trim_end()) {
                    terminal |= matches!(progress.phase, JobPhase::Success | JobPhase::Failed);
                    let _ = tx.send(progress);
                }
                line.clear();
            }
            // A helper that reported a detailed Failed terminal event exits
            // non-zero by design. Do not overwrite that useful explanation
            // with a second generic event merely because of its exit status.
            if !terminal {
                let _ = tx.send(Progress {
                    phase: JobPhase::Failed,
                    error: Some(if status.success() {
                        "The writer stopped before reporting completion. Please try again.".into()
                    } else {
                        "Authorization was cancelled or the writer stopped unexpectedly. Please try again."
                            .into()
                    }),
                    ..Progress::default()
                });
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(80));
    }
}

#[cfg(target_os = "linux")]
#[allow(
    clippy::unnecessary_wraps,
    reason = "all platform implementations share one fallible command-builder signature"
)]
fn elevated_helper_command(
    exe: &Path,
    request: &Path,
    progress: &Path,
    digest: &str,
) -> io::Result<Command> {
    let mut command = Command::new("pkexec");
    command
        .arg(exe)
        .arg(WRITE_HELPER_ARG)
        .arg(request)
        .arg(progress)
        .arg(digest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

#[cfg(target_os = "macos")]
#[allow(
    clippy::unnecessary_wraps,
    reason = "all platform implementations share one fallible command-builder signature"
)]
fn elevated_helper_command(
    exe: &Path,
    request: &Path,
    progress: &Path,
    digest: &str,
) -> io::Result<Command> {
    const SCRIPT: &str = r#"on run argv
set helperCommand to quoted form of item 1 of argv & " --qos-write-helper " & quoted form of item 2 of argv & " " & quoted form of item 3 of argv & " " & quoted form of item 4 of argv
do shell script helperCommand with administrator privileges
end run"#;
    let mut command = Command::new("osascript");
    command
        .arg("-e")
        .arg(SCRIPT)
        .arg(exe)
        .arg(request)
        .arg(progress)
        .arg(digest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

#[cfg(target_os = "windows")]
#[allow(
    clippy::unnecessary_wraps,
    reason = "all platform implementations share one fallible command-builder signature"
)]
fn elevated_helper_command(
    exe: &Path,
    request: &Path,
    progress: &Path,
    digest: &str,
) -> io::Result<Command> {
    // Values are positional PowerShell arguments, never interpolated into the
    // script. ArgumentList receives individually quoted native arguments.
    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            WINDOWS_ELEVATION_SCRIPT,
        ])
        .arg(exe)
        .arg(request)
        .arg(progress)
        .arg(digest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn elevated_helper_command(
    _exe: &Path,
    _request: &Path,
    _progress: &Path,
    _digest: &str,
) -> io::Result<Command> {
    Err(io::Error::other(
        "raw media writing is unsupported on this platform",
    ))
}

fn revalidate_target(selected: &TargetDrive) -> io::Result<()> {
    let current = crate::probe::probe()
        .and_then(|drives| drives.into_iter().find(|drive| drive.name == selected.name))
        .ok_or_else(|| io::Error::other("the selected device is no longer present"))?;
    if !target_identity_matches(selected, &current) {
        return Err(io::Error::other(
            "device identity, capacity, or system-disk status changed",
        ));
    }
    Ok(())
}

fn target_identity_matches(selected: &TargetDrive, current: &TargetDrive) -> bool {
    let stable_id_changed = match (&selected.stable_id, &current.stable_id) {
        (Some(expected), Some(actual)) => expected != actual,
        (Some(_), None) => true,
        (None, _) => false,
    };
    !stable_id_changed
        && current.name == selected.name
        && current.size == selected.size
        && current.model == selected.model
        && current.removable == selected.removable
        && (selected.is_system || !current.is_system)
}

#[cfg(target_os = "linux")]
fn prepare_target(target: &TargetDrive) -> io::Result<()> {
    let out = std::process::Command::new("lsblk")
        .args(["-lnpo", "NAME,MOUNTPOINT", &target.dev_path()])
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("lsblk could not inspect mount state"));
    }
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut fields = line.split_whitespace();
        let Some(device) = fields.next() else {
            continue;
        };
        if fields.next().is_some() {
            let status = std::process::Command::new("umount").arg(device).status()?;
            if !status.success() {
                return Err(io::Error::other(format!("failed to unmount {device}")));
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn prepare_target(target: &TargetDrive) -> io::Result<()> {
    let status = std::process::Command::new("diskutil")
        .args(["unmountDisk", &format!("/dev/{}", target.name)])
        .status()?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| io::Error::other("diskutil unmountDisk failed"))
}

#[cfg(target_os = "windows")]
fn prepare_target(target: &TargetDrive) -> io::Result<()> {
    let number = target
        .name
        .strip_prefix("PhysicalDrive")
        .ok_or_else(|| io::Error::other("invalid Windows physical-drive name"))?;
    let script = format!("Set-Disk -Number {number} -IsOffline $true -ErrorAction Stop");
    let status = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| io::Error::other("Windows could not take the selected disk offline"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn prepare_target(_target: &TargetDrive) -> io::Result<()> {
    Err(io::Error::other(
        "raw media writing is unsupported on this platform",
    ))
}

#[cfg(target_os = "macos")]
fn finish_target(target: &TargetDrive) {
    let _ = std::process::Command::new("diskutil")
        .args(["eject", &format!("/dev/{}", target.name)])
        .status();
}

#[cfg(target_os = "windows")]
fn finish_target(target: &TargetDrive) {
    if let Some(number) = target.name.strip_prefix("PhysicalDrive") {
        let script =
            format!("Set-Disk -Number {number} -IsOffline $false -ErrorAction SilentlyContinue");
        let _ = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status();
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn finish_target(_target: &TargetDrive) {}

/// Spawn a job on a worker thread and return the receiver the UI drains once
/// per frame. The returned [`JobStart`] is the validation result; a channel is
/// only returned when it is [`JobStart::Started`].
///
/// * Tests/shots → the request is refused and no worker is created.
/// * Live UI → platform authentication starts the narrow helper.
/// * Deliberate allow-gated root diagnostics → [`run_system_job`] runs in the
///   current process.
pub fn spawn_job(req: WriteRequest, typed: &str) -> (JobStart, Option<Receiver<Progress>>) {
    let start = begin_job(&req, typed);
    if start != JobStart::Started {
        return (start, None);
    }
    let backend = WriteBackend::system();
    if backend == WriteBackend::Disabled {
        return (JobStart::WriteDisabled, None);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("imager-write".into())
        .spawn(move || match backend {
            WriteBackend::System => {
                let _ = run_system_job(&req, |p| {
                    let _ = tx.send(p);
                });
            }
            WriteBackend::PrivilegedHelper => spawn_privileged_helper(req, &tx),
            WriteBackend::Disabled => {}
        });
    (JobStart::Started, Some(rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{sample_source, sample_targets};
    use std::sync::Mutex;

    /// Serialises tests which change process environment, since it is shared
    /// across parallel test threads.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn req() -> WriteRequest {
        WriteRequest {
            source: sample_source(),
            target: sample_targets()[0].clone(), // the USB stick
            action: Action::Flash,
            profile: InstallProfile::default(),
        }
    }

    #[test]
    #[cfg(not(feature = "host-media-writer"))]
    fn default_backend_is_disabled_without_gate() {
        let _g = ENV_LOCK.lock().unwrap();
        // No opt-in env var set → unavailable, never a fake success.
        unsafe { std::env::remove_var("QOS_IMAGER_ALLOW_WRITE") };
        unsafe { std::env::remove_var("IMAGER_LIVE") };
        assert_eq!(WriteBackend::system(), WriteBackend::Disabled);
        assert!(!allow_write());
    }

    #[test]
    #[cfg(feature = "host-media-writer")]
    fn packaged_host_build_enables_the_short_lived_helper_without_an_environment_flag() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("QOS_IMAGER_ALLOW_WRITE") };
        unsafe { std::env::remove_var("IMAGER_LIVE") };
        assert_eq!(WriteBackend::system(), WriteBackend::PrivilegedHelper);
        assert!(write_available());
    }

    #[test]
    fn live_ui_uses_short_lived_privileged_helper() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("QOS_IMAGER_ALLOW_WRITE") };
        unsafe { std::env::set_var("IMAGER_LIVE", "1") };
        assert_eq!(WriteBackend::system(), WriteBackend::PrivilegedHelper);
        assert!(write_available());
        unsafe { std::env::remove_var("IMAGER_LIVE") };
    }

    #[test]
    fn only_disabled_backend_is_unavailable() {
        assert!(!WriteBackend::Disabled.is_available());
        assert!(WriteBackend::System.is_available());
        assert!(WriteBackend::PrivilegedHelper.is_available());
    }

    #[test]
    fn system_backend_selected_only_with_gate_on() {
        let _g = ENV_LOCK.lock().unwrap();
        // The real block-device backend is chosen ONLY when the operator opts
        // in explicitly; flipping the gate is the single switch between the
        // disabled path and the real device write.
        unsafe { std::env::set_var("QOS_IMAGER_ALLOW_WRITE", "1") };
        assert!(allow_write());
        assert_eq!(WriteBackend::system(), WriteBackend::System);
        unsafe { std::env::remove_var("QOS_IMAGER_ALLOW_WRITE") };
        let expected = if cfg!(feature = "host-media-writer") {
            WriteBackend::PrivilegedHelper
        } else {
            WriteBackend::Disabled
        };
        assert_eq!(WriteBackend::system(), expected);
    }

    #[test]
    fn privileged_request_never_contains_user_or_encryption_secrets() {
        let mut request = req();
        request.profile.user.password = "account-secret".into();
        request.profile.encryption_passphrase = "encryption-secret".into();
        let encoded = serde_json::to_string(&request.without_secrets()).unwrap();
        assert!(!encoded.contains("account-secret"));
        assert!(!encoded.contains("encryption-secret"));
        assert!(encoded.contains("\"password\":\"\""));
        assert!(encoded.contains("\"encryption_passphrase\":\"\""));
    }

    #[test]
    fn helper_rejects_a_request_changed_after_authorization() {
        let work = tempfile::Builder::new()
            .prefix("qos-imager-write-")
            .tempdir()
            .unwrap();
        let request_path = work.path().join("request.json");
        let progress_path = work.path().join("progress.jsonl");
        std::fs::write(&request_path, serde_json::to_vec(&req()).unwrap()).unwrap();
        std::fs::write(&progress_path, []).unwrap();
        let args = vec![
            "imager".into(),
            WRITE_HELPER_ARG.into(),
            request_path.display().to_string(),
            progress_path.display().to_string(),
            "0".repeat(64),
        ];
        let error = maybe_run_write_helper(&args).unwrap_err();
        assert!(error.to_string().contains("changed after authorization"));
        assert!(std::fs::read(progress_path).unwrap().is_empty());
    }

    #[test]
    fn helper_never_follows_or_overwrites_a_planted_progress_path() {
        let work = tempfile::Builder::new()
            .prefix("qos-imager-write-")
            .tempdir()
            .unwrap();
        let progress_path = work.path().join("progress.jsonl");
        std::fs::write(&progress_path, b"keep-me").unwrap();
        let error = create_progress_output(&progress_path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(progress_path).unwrap(), b"keep-me");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_elevates_only_the_hidden_helper_mode() {
        let command = elevated_helper_command(
            Path::new("/opt/qos-imager-bin"),
            Path::new("/tmp/request"),
            Path::new("/tmp/progress"),
            &"a".repeat(64),
        )
        .unwrap();
        assert_eq!(command.get_program(), "pkexec");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args[0], "/opt/qos-imager-bin");
        assert_eq!(args[1], WRITE_HELPER_ARG);
        assert_eq!(args.len(), 5);
    }

    #[test]
    fn windows_elevation_script_native_quotes_paths_with_spaces() {
        // PowerShell single-quoted strings do not treat backslash as an
        // escape. The wrapper must concatenate a literal `"`, not `\"`,
        // around both paths before Start-Process flattens ArgumentList.
        assert!(WINDOWS_ELEVATION_SCRIPT.contains("('\"' + $args[1] + '\"')"));
        assert!(WINDOWS_ELEVATION_SCRIPT.contains("('\"' + $args[2] + '\"')"));
        assert!(!WINDOWS_ELEVATION_SCRIPT.contains(r#"('\"' +"#));
    }

    #[test]
    fn confirmation_must_match_target() {
        let r = req();
        assert!(r.confirmation_matches("sdb"));
        assert!(r.confirmation_matches("/dev/sdb"));
        assert!(!r.confirmation_matches("sda"));
        assert!(!r.confirmation_matches(""));
        assert!(!r.confirmation_matches("nvme0n1"));
    }

    #[test]
    fn begin_refuses_on_mismatched_confirmation() {
        let r = req();
        assert_eq!(begin_job(&r, "sda"), JobStart::ConfirmationMismatch);
        assert_eq!(begin_job(&r, ""), JobStart::ConfirmationMismatch);
    }

    #[test]
    fn begin_refuses_when_image_too_big() {
        let mut r = req();
        r.source.size = 40_000_000_000; // bigger than the 30 GB stick
        assert_eq!(begin_job(&r, "sdb"), JobStart::TooSmall);
    }

    #[test]
    fn begin_refuses_internal_and_system_targets() {
        let mut r = req();
        r.target.removable = false;
        r.target.tran = Some("sata".into());
        assert_eq!(begin_job(&r, "sdb"), JobStart::UnsafeTarget);
        r.target.removable = true;
        r.target.is_system = true;
        assert_eq!(begin_job(&r, "sdb"), JobStart::UnsafeTarget);
    }

    #[test]
    fn begin_refuses_invalid_profile_before_worker_start() {
        let mut r = req();
        r.profile.keyboard = "not-a-layout".into();
        assert!(matches!(
            begin_job(&r, "sdb"),
            JobStart::InvalidProfile(message) if message.contains("Keyboard")
        ));
    }

    #[test]
    fn q_os_media_reserves_room_for_the_profile() {
        let mut r = req();
        r.source.installable = true;
        r.source.size = r.target.size - PROFILE_BLOCK_SIZE as u64 + 1;
        assert_eq!(begin_job(&r, "sdb"), JobStart::TooSmall);
    }

    #[test]
    fn media_profile_is_secret_free_and_read_back_verified() {
        let mut profile = InstallProfile {
            hostname: "studio".into(),
            ..InstallProfile::default()
        };
        profile.user.username = "ada".into();
        profile.user.password = "account-secret".into();
        profile.encryption_passphrase = "disk-secret".into();
        let block = encode_profile_block(&profile).unwrap();
        let text = String::from_utf8_lossy(&block);
        assert!(text.contains(PROFILE_MAGIC));
        assert!(text.contains("studio"));
        assert!(!text.contains("account-secret"));
        assert!(!text.contains("disk-secret"));

        let target = tempfile::NamedTempFile::new().unwrap();
        target
            .as_file()
            .set_len(PROFILE_BLOCK_SIZE as u64 * 2)
            .unwrap();
        write_profile_block(target.path(), PROFILE_BLOCK_SIZE as u64 * 2, &profile).unwrap();
        let bytes = std::fs::read(target.path()).unwrap();
        assert_eq!(&bytes[PROFILE_BLOCK_SIZE..], block.as_slice());
    }

    #[test]
    fn invalid_media_profile_is_rejected_before_a_block_is_encoded() {
        let profile = InstallProfile {
            hostname: "bad host".into(),
            ..InstallProfile::default()
        };
        let error = encode_profile_block(&profile).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("Hostname"));
    }

    /// Exercise the real Rust encoder/tail writer against the exact live-media
    /// extractor instead of maintaining two merely similar format fixtures.
    #[cfg(target_os = "linux")]
    #[test]
    fn written_profile_is_consumed_by_the_live_extractor() {
        let mut profile = InstallProfile {
            hostname: "studio".into(),
            keyboard: "de".into(),
            timezone: "Europe/Berlin".into(),
            ..InstallProfile::default()
        };
        profile.user.username = "ada".into();
        profile.user.full_name = "ada Example".into();
        profile.user.password = "must-not-cross".into();

        let work = tempfile::tempdir().unwrap();
        let media = work.path().join("media.img");
        let out = work.path().join("extracted");
        std::fs::File::create(&media)
            .unwrap()
            .set_len(PROFILE_BLOCK_SIZE as u64 * 2)
            .unwrap();
        write_profile_block(&media, PROFILE_BLOCK_SIZE as u64 * 2, &profile).unwrap();

        let extractor = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../os/usr/libexec/qos-installer-profile-extract");
        let status = std::process::Command::new(extractor)
            .env("QOS_INSTALLER_PROFILE_DEVICE", &media)
            .env("QOS_INSTALLER_PROFILE_ALLOW_FILE", "1")
            .env("QOS_INSTALLER_PROFILE_OUT_DIR", &out)
            .status()
            .unwrap();
        assert!(status.success());

        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(out.join("profile.json")).unwrap()).unwrap();
        assert_eq!(value["hostname"], "studio");
        assert_eq!(value["username"], "ada");
        assert_eq!(value["keyboard"], "de");
        assert!(value.get("password").is_none());
        assert!(
            !String::from_utf8_lossy(&std::fs::read(media).unwrap()).contains("must-not-cross")
        );
    }

    #[test]
    #[cfg(not(feature = "host-media-writer"))]
    fn spawn_refuses_instead_of_simulating_with_gate_off() {
        let _g = ENV_LOCK.lock().unwrap();
        // Validation remains pure, but the live start refuses rather than
        // reporting synthetic progress and success.
        unsafe { std::env::remove_var("QOS_IMAGER_ALLOW_WRITE") };
        unsafe { std::env::remove_var("IMAGER_LIVE") };
        let r = req();
        assert_eq!(begin_job(&r, "sdb"), JobStart::Started);
        let (start, rx) = spawn_job(r, "sdb");
        assert_eq!(start, JobStart::WriteDisabled);
        assert!(rx.is_none());
    }

    #[test]
    fn target_revalidation_rejects_reused_device_name() {
        let selected = req().target;
        let mut replacement = selected.clone();
        assert!(target_identity_matches(&selected, &replacement));
        replacement.stable_id = Some("another-device".into());
        assert!(!target_identity_matches(&selected, &replacement));
        replacement = selected.clone();
        replacement.is_system = true;
        assert!(!target_identity_matches(&selected, &replacement));
    }

    // ---- the real engine, proven against temp files (never a device) -------

    fn tmp_with(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().expect("temp");
        f.write_all(bytes).expect("write");
        f.flush().expect("flush");
        f
    }

    fn sample_bytes(n: usize) -> Vec<u8> {
        // A deterministic, non-trivial pattern spanning several chunks.
        (0..n).map(|i| ((i * 31 + 7) % 251) as u8).collect()
    }

    #[test]
    fn stream_copy_reproduces_source_byte_for_byte() {
        let data = sample_bytes(CHUNK * 2 + 12345); // multi-chunk + a ragged tail
        let src = tmp_with(&data);
        let dst = tempfile::NamedTempFile::new().unwrap();
        let mut last = 0u64;
        let n = stream_copy(src.path(), dst.path(), |b| last = b).unwrap();
        assert_eq!(n as usize, data.len());
        assert_eq!(last, n, "final progress equals total bytes");
        let back = std::fs::read(dst.path()).unwrap();
        assert_eq!(back, data, "written bytes match the source exactly");
    }

    #[test]
    fn verify_readback_passes_on_identical_and_flags_the_diff() {
        let data = sample_bytes(CHUNK + 999);
        let src = tmp_with(&data);
        let dst = tmp_with(&data);
        assert_eq!(
            verify_readback(src.path(), dst.path(), data.len() as u64, |_| {}).unwrap(),
            None,
            "identical files verify clean"
        );
        // Corrupt one byte deep in the second chunk and confirm it is caught.
        let mut corrupt = data.clone();
        let at = CHUNK + 100;
        corrupt[at] ^= 0xFF;
        let bad = tmp_with(&corrupt);
        assert_eq!(
            verify_readback(src.path(), bad.path(), data.len() as u64, |_| {}).unwrap(),
            Some(at as u64),
            "the first differing byte offset is reported"
        );
    }

    #[test]
    fn run_job_paths_writes_and_verifies_end_to_end() {
        let data = sample_bytes(CHUNK * 3 + 4096);
        let src = tmp_with(&data);
        let dst = tempfile::NamedTempFile::new().unwrap();
        let mut phases = Vec::new();
        let mut last = Progress::default();
        let result = run_job_paths(src.path(), dst.path(), data.len() as u64, |p| {
            if phases.last() != Some(&p.phase) {
                phases.push(p.phase);
            }
            last = p;
        });
        assert_eq!(result, JobResult::Verified);
        assert!(result.is_success());
        // It really wrote the bytes and drove both non-terminal phases to
        // completion. Only `run_system_job` may announce terminal success,
        // after the optional profile write and device finalisation.
        assert_eq!(std::fs::read(dst.path()).unwrap(), data);
        assert_eq!(phases, vec![JobPhase::Writing, JobPhase::Verifying]);
        assert!((last.write_frac - 1.0).abs() < 1e-9);
        assert!((last.verify_frac - 1.0).abs() < 1e-9);
    }

    #[test]
    fn target_revalidation_failure_still_reports_an_actionable_terminal_event() {
        let mut request = req();
        request.target.name = "qos-test-device-that-cannot-exist".into();
        let mut updates = Vec::new();

        let result = run_system_job(&request, |progress| updates.push(progress));

        assert!(matches!(result, JobResult::WriteFailed(_)));
        let terminal = updates.last().expect("terminal progress event");
        assert_eq!(terminal.phase, JobPhase::Failed);
        assert!(
            terminal.error.as_deref().is_some_and(|message| {
                message.contains("changed or was disconnected")
                    && message.contains("Could not write")
            }),
            "the UI receives a useful cause even when writing never starts"
        );
    }

    #[test]
    fn reviewed_source_size_fails_closed_after_truncation_or_growth() {
        let data = sample_bytes(4096);

        let truncated = tmp_with(&data[..2048]);
        let truncated_dst = tempfile::NamedTempFile::new().unwrap();
        let result = run_job_paths(truncated.path(), truncated_dst.path(), 4096, |_| {});
        assert!(
            matches!(result, JobResult::WriteFailed(ref message) if message.contains("size changed"))
        );

        let grown = tmp_with(&data);
        let grown_dst = tempfile::NamedTempFile::new().unwrap();
        let result = run_job_paths(grown.path(), grown_dst.path(), 2048, |_| {});
        assert!(
            matches!(result, JobResult::WriteFailed(ref message) if message.contains("size changed"))
        );
    }

    #[test]
    fn replaced_source_is_rejected_before_destination_is_opened() {
        let data = sample_bytes(4096);
        let src = tmp_with(&data);
        let mut wrong_identity = SourceIdentity::from_metadata(&src.as_file().metadata().unwrap())
            .expect("test platform exposes file identity");
        wrong_identity.file = wrong_identity.file.wrapping_add(1);
        let dst = tmp_with(b"leave destination untouched");

        let result = run_job_paths_bound(
            src.path(),
            dst.path(),
            data.len() as u64,
            Some(wrong_identity),
            None,
            |_| {},
        );
        assert!(
            matches!(result, JobResult::WriteFailed(ref message) if message.contains("replaced"))
        );
        assert_eq!(
            std::fs::read(dst.path()).unwrap(),
            b"leave destination untouched"
        );
    }

    #[test]
    fn catalog_digest_is_checked_during_the_write() {
        let data = sample_bytes(4096);
        let src = tmp_with(&data);
        let dst = tmp_with(b"leave destination untouched");
        let wrong_digest = "0".repeat(64);

        let result = run_job_paths_bound(
            src.path(),
            dst.path(),
            data.len() as u64,
            SourceIdentity::from_metadata(&src.as_file().metadata().unwrap()),
            Some(&wrong_digest),
            |_| {},
        );
        assert!(
            matches!(result, JobResult::WriteFailed(ref message) if message.contains("signed release catalog"))
        );
        assert_eq!(
            std::fs::read(dst.path()).unwrap(),
            b"leave destination untouched"
        );
    }

    #[test]
    fn run_job_paths_reports_write_failure_on_bad_dest() {
        let data = sample_bytes(1024);
        let src = tmp_with(&data);
        // A destination under a path that cannot exist → open fails → WriteFailed.
        let bad = std::path::Path::new("/proc/definitely/not/writable/imager.img");
        let result = run_job_paths(src.path(), bad, data.len() as u64, |_| {});
        assert!(matches!(result, JobResult::WriteFailed(_)));
    }
}
