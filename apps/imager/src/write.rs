//! The write / verify seam for destructive work, safely gated.
//!
//! Writing a disk image onto a block device is **destructive** and needs
//! privileged raw access. This module is the single choke point where that
//! work happens. It ships a streaming copy + read-back verify
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
//! The engine is exercised in tests against **temp files** (a copy +
//! verify, byte-for-byte) — never a block device — so the production loop is
//! proven without any risk to hardware.

use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;
#[cfg(any(test, target_os = "macos", target_os = "windows"))]
use ui::time::Instant;

use crate::model::{
    Action, InstallProfile, JobPhase, Progress, Source, SourceIdentity, TargetDrive,
};
use sha2::{Digest, Sha256};

/// The chunk size of the streaming copy / verify loop (4 MiB) — large enough
/// to keep the device busy, small enough to report smooth progress.
pub const CHUNK: usize = 4 * 1024 * 1024;

pub use crate::profile::{PROFILE_BLOCK_SIZE, PROFILE_MAGIC};

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
    /// booted Tilo installer owns internal-disk and dual-boot installation.
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
    /// Linux sandbox path: `UDisks2` authorizes and returns one exclusive raw
    /// device descriptor.  The Flatpak never sees `/dev` and cannot execute
    /// commands on the host.
    Udisks2,
}

impl WriteBackend {
    /// Choose the backend for the running process. The allow gate is retained
    /// for deliberate root-side diagnostics; normal live UI uses the narrow
    /// helper, while tests and shots remain disabled.
    pub fn system() -> Self {
        if cfg!(all(target_os = "linux", feature = "flatpak-media-writer"))
            && std::env::var("TILO_IMAGER_FLATPAK").as_deref() == Ok("1")
        {
            WriteBackend::Udisks2
        } else if allow_write() {
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
/// narrow privileged helper at write time. `TILO_IMAGER_ALLOW_WRITE` is only
/// the explicit diagnostic opt-in for writing directly in this process.
pub fn write_available() -> bool {
    WriteBackend::system().is_available()
}

/// The env allow-gate. `TILO_IMAGER_ALLOW_WRITE=1` is the single switch that
/// lets a real block-device write happen; unset (the default) keeps every
/// path unavailable. Modelled on the VPN app's `TILO_VPN_ALLOW_EXEC`.
pub fn allow_write() -> bool {
    std::env::var("TILO_IMAGER_ALLOW_WRITE").as_deref() == Ok("1")
}

/// A validated request to write `source` to `target` using `action`
/// (+ profile when pre-configuring). Construction does not start anything.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct WriteRequest {
    pub source: Source,
    pub target: TargetDrive,
    pub action: Action,
    pub profile: InstallProfile,
    /// Prepared etc/var tree with Wi-Fi and enrolled identity. Built on the
    /// setup machine before elevation; the privileged writer only copies it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_stage: Option<std::path::PathBuf>,
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

use crate::profile::write_profile_block_bound;
pub use crate::profile::{encode_profile_block, write_profile_block};

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

/// Peer-local control for one running write. The immutable write request stays
/// separately digest-bound; cancellation only narrows authority by asking the
/// worker/helper to stop at its next safe chunk boundary.
#[derive(Clone, Debug)]
pub struct JobControl {
    cancelled: Arc<AtomicBool>,
}

impl JobControl {
    pub(crate) fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

fn cancelled_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "image write cancelled")
}

/// A stable hardware identity is the rendezvous key shared by separately
/// elevated helpers, falling back to the canonical device path when probing
/// cannot provide one. The helper owns the lock, so the kernel releases it if
/// either the UI or helper crashes.
fn target_lock_name(target: &TargetDrive) -> String {
    let identity = target
        .stable_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .map_or_else(
            || format!("path:{}", target.dev_path()),
            |id| format!("id:{id}"),
        );
    hex::encode(Sha256::digest(identity.as_bytes()))
}

#[cfg(unix)]
type TargetWriteLock = advisory_lock::FileLock;

#[cfg(unix)]
fn try_lock_file(path: &Path) -> io::Result<TargetWriteLock> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    advisory_lock::FileLock::try_exclusive(file).map_err(|error| match error {
        advisory_lock::TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "another Tilo Imager helper is already using this drive",
        ),
        advisory_lock::TryLockError::Error(error) => error,
    })
}

#[cfg(unix)]
fn acquire_target_write_lock(target: &TargetDrive) -> io::Result<TargetWriteLock> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    #[cfg(target_os = "linux")]
    let directory = Path::new("/run/lock/tilo-imager");
    #[cfg(not(target_os = "linux"))]
    let directory = Path::new("/var/run/tilo-imager");

    if !directory.exists() {
        std::fs::create_dir(directory)?;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let metadata = directory.symlink_metadata()?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != 0
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the privileged Tilo Imager lock directory is not root-owned and private",
        ));
    }
    try_lock_file(&directory.join(target_lock_name(target)))
}

#[cfg(target_os = "windows")]
#[derive(Debug)]
struct TargetWriteLock(*mut std::ffi::c_void);

#[cfg(target_os = "windows")]
impl Drop for TargetWriteLock {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateMutexW(
        mutex_attributes: *const std::ffi::c_void,
        initial_owner: i32,
        name: *const u16,
    ) -> *mut std::ffi::c_void;
    fn GetLastError() -> u32;
    fn ReleaseMutex(handle: *mut std::ffi::c_void) -> i32;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
}

#[cfg(target_os = "windows")]
fn acquire_target_write_lock(target: &TargetDrive) -> io::Result<TargetWriteLock> {
    const ERROR_ALREADY_EXISTS: u32 = 183;
    let name = format!("Global\\Tilo-Imager-target-{}", target_lock_name(target));
    let wide = name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 1, wide.as_ptr()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            CloseHandle(handle);
        }
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "another Tilo Imager helper is already using this drive",
        ));
    }
    Ok(TargetWriteLock(handle))
}

#[cfg(not(any(unix, target_os = "windows")))]
struct TargetWriteLock;

#[cfg(not(any(unix, target_os = "windows")))]
fn acquire_target_write_lock(_target: &TargetDrive) -> io::Result<TargetWriteLock> {
    Err(io::Error::other(
        "exclusive media writes are unsupported on this platform",
    ))
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
fn stream_copy_bound(
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
fn hash_source_exact(
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

fn verify_readback_bound(
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
    if should_cancel() {
        return Err(cancelled_error());
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
    run_job_paths_bound(src, dst, total, None, None, &|| false, &mut on_progress)
}

fn run_job_paths_bound(
    src: &Path,
    dst: &Path,
    total: u64,
    expected_identity: Option<SourceIdentity>,
    expected_sha256: Option<&str>,
    should_cancel: &impl Fn() -> bool,
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
        should_cancel,
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
        Err(e) if e.kind() == io::ErrorKind::Interrupted => return JobResult::Cancelled,
        Err(e) => return JobResult::WriteFailed(e.to_string()),
    };
    // Verify phase over exactly the bytes we wrote.
    let verify = verify_readback_bound(src, dst, written, should_cancel, |bytes| {
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
        Err(e) if e.kind() == io::ErrorKind::Interrupted => JobResult::Cancelled,
        Err(e) => JobResult::VerifyFailed(e.to_string()),
    }
}

/// Run the same exact write, digest, read-back and profile pipeline against an
/// already-authorized device descriptor. `UDisks2` uses this seam so raw-device
/// authority never has to be represented by a sandbox-visible path.
#[cfg(any(test, all(target_os = "linux", feature = "flatpak-media-writer")))]
#[allow(
    clippy::too_many_lines,
    reason = "the descriptor stays borrowed through one linear write, verify, and profile transaction; splitting ownership across helpers would obscure the destructive-I/O lifecycle"
)]
#[cfg(any(test, all(target_os = "linux", feature = "flatpak-media-writer")))]
fn run_job_file_bound(
    req: &WriteRequest,
    device: &mut std::fs::File,
    should_cancel: &impl Fn() -> bool,
    mut on_progress: impl FnMut(Progress),
) -> JobResult {
    let source_path = Path::new(&req.source.path);
    let mut source = match OpenOptions::new().read(true).open(source_path) {
        Ok(file) => file,
        Err(error) => return JobResult::WriteFailed(error.to_string()),
    };
    let metadata = match source.metadata() {
        Ok(metadata) => metadata,
        Err(error) => return JobResult::WriteFailed(error.to_string()),
    };
    if !metadata.is_file()
        || metadata.len() != req.source.size
        || req.source.identity.is_some_and(|expected| {
            SourceIdentity::from_file(&source).ok().flatten() != Some(expected)
        })
    {
        return JobResult::WriteFailed("source image changed after review".into());
    }
    if let Some(expected) = req.source.identity {
        match SourceIdentity::from_file(&source) {
            Ok(Some(actual)) if actual == expected => {}
            Ok(_) => return JobResult::WriteFailed("source image changed after review".into()),
            Err(error) => return JobResult::WriteFailed(error.to_string()),
        }
    }
    if let Some(expected) = req.source.expected_sha256.as_deref() {
        let actual = match hash_source_exact(&mut source, req.source.size, should_cancel) {
            Ok(digest) => digest,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                return JobResult::Cancelled;
            }
            Err(error) => return JobResult::WriteFailed(error.to_string()),
        };
        if !actual.eq_ignore_ascii_case(expected) {
            return JobResult::WriteFailed(
                "source image no longer matches its signed release catalog".into(),
            );
        }
    }
    if let Err(error) = source.seek(SeekFrom::Start(0)) {
        return JobResult::WriteFailed(error.to_string());
    }
    if let Err(error) = device.seek(SeekFrom::Start(0)) {
        return JobResult::WriteFailed(error.to_string());
    }

    let total = req.source.size.max(1);
    let mut buffer = vec![0_u8; CHUNK];
    let mut written = 0_u64;
    let mut digest = Sha256::new();
    while written < req.source.size {
        if should_cancel() {
            return JobResult::Cancelled;
        }
        let want = usize::try_from((req.source.size - written).min(CHUNK as u64)).unwrap_or(CHUNK);
        let Some(chunk) = buffer.get_mut(..want) else {
            return JobResult::WriteFailed("source chunk exceeds buffer".into());
        };
        if let Err(error) = read_exact_fill(&mut source, chunk) {
            return JobResult::WriteFailed(error.to_string());
        }
        if let Err(error) = device.write_all(chunk) {
            return JobResult::WriteFailed(error.to_string());
        }
        digest.update(&*chunk);
        written = written.saturating_add(want as u64);
        on_progress(Progress {
            phase: JobPhase::Writing,
            write_frac: (written as f64 / total as f64).min(1.0),
            verify_frac: 0.0,
            rate_bps: 0,
            error: None,
        });
    }
    if req
        .source
        .expected_sha256
        .as_deref()
        .is_some_and(|expected| !hex::encode(digest.finalize()).eq_ignore_ascii_case(expected))
    {
        return JobResult::WriteFailed(
            "source image no longer matches its signed release catalog".into(),
        );
    }
    // The descriptor path has already checked the reviewed metadata, but a
    // local source can still grow while the copy is in flight.  Keep this
    // path as strict as the pathname writer: accepting only the reviewed
    // prefix would make the two production backends disagree about what was
    // actually flashed.
    let mut extra = [0_u8; 1];
    match source.read(&mut extra) {
        Ok(0) => {}
        Ok(_) => return JobResult::WriteFailed("source image became larger after review".into()),
        Err(error) => return JobResult::WriteFailed(error.to_string()),
    }
    if let Err(error) = device.flush().and_then(|()| device.sync_all()) {
        return JobResult::WriteFailed(error.to_string());
    }
    if let Err(error) = source.seek(SeekFrom::Start(0)) {
        return JobResult::VerifyFailed(error.to_string());
    }
    if let Err(error) = device.seek(SeekFrom::Start(0)) {
        return JobResult::VerifyFailed(error.to_string());
    }
    let mut source_buffer = vec![0_u8; CHUNK];
    let mut target_buffer = vec![0_u8; CHUNK];
    let mut verified = 0_u64;
    while verified < req.source.size {
        if should_cancel() {
            return JobResult::Cancelled;
        }
        let want = usize::try_from((req.source.size - verified).min(CHUNK as u64)).unwrap_or(CHUNK);
        let Some(source_chunk) = source_buffer.get_mut(..want) else {
            return JobResult::VerifyFailed("source verify chunk exceeds buffer".into());
        };
        let Some(target_chunk) = target_buffer.get_mut(..want) else {
            return JobResult::VerifyFailed("target verify chunk exceeds buffer".into());
        };
        if let Err(error) = read_exact_fill(&mut source, source_chunk) {
            return JobResult::VerifyFailed(error.to_string());
        }
        if let Err(error) = read_exact_fill(device, target_chunk) {
            return JobResult::VerifyFailed(error.to_string());
        }
        if let Some(offset) = first_diff(source_chunk, target_chunk) {
            return JobResult::VerifyMismatch(verified.saturating_add(offset as u64));
        }
        verified = verified.saturating_add(want as u64);
        on_progress(Progress {
            phase: JobPhase::Verifying,
            write_frac: 1.0,
            verify_frac: (verified as f64 / total as f64).min(1.0),
            rate_bps: 0,
            error: None,
        });
    }
    if req.source.installable {
        let block = match encode_profile_block(&req.profile) {
            Ok(block) => block,
            Err(error) => return JobResult::VerifyFailed(error.to_string()),
        };
        let Some(offset) = req.target.size.checked_sub(PROFILE_BLOCK_SIZE as u64) else {
            return JobResult::VerifyFailed("target is too small for installation profile".into());
        };
        if let Err(error) = device
            .seek(SeekFrom::Start(offset))
            .and_then(|_| device.write_all(&block))
            .and_then(|()| device.flush())
            .and_then(|()| device.sync_all())
            .and_then(|()| device.seek(SeekFrom::Start(offset)).map(|_| ()))
        {
            return JobResult::VerifyFailed(error.to_string());
        }
        let mut actual = vec![0_u8; PROFILE_BLOCK_SIZE];
        if let Err(error) = read_exact_fill(device, &mut actual) {
            return JobResult::VerifyFailed(error.to_string());
        }
        if actual != block {
            return JobResult::VerifyFailed("installation profile read-back did not match".into());
        }
    }
    JobResult::Verified
}

/// Run the real job for `req` (source file → target device), streaming
/// [`Progress`] over `tx`. Only ever called on a worker thread when the
/// allow-gate selected [`WriteBackend::System`].
fn run_system_job(
    req: &WriteRequest,
    should_cancel: &impl Fn() -> bool,
    mut on_progress: impl FnMut(Progress),
) -> JobResult {
    let src = std::path::PathBuf::from(&req.source.path);
    let dst = std::path::PathBuf::from(req.target.dev_path());
    let mut prepared = false;
    let mut write_frac = 0.0;
    let mut verify_frac = 0.0;
    // Retain this guard until after finish_target. Separate elevated helpers
    // rendezvous here before either one unmounts, offlines, writes, verifies,
    // embeds a profile, or ejects the same physical target.
    let mut target_lock = None;
    let mut relay_progress = |progress: Progress| {
        write_frac = progress.write_frac;
        verify_frac = progress.verify_frac;
        on_progress(progress);
    };
    let mut result = if should_cancel() {
        JobResult::Cancelled
    } else if let Err(error) = revalidate_target(&req.target) {
        JobResult::WriteFailed(format!(
            "the selected drive changed or was disconnected: {error}"
        ))
    } else if let Err(error) = acquire_target_write_lock(&req.target).map(|lock| {
        target_lock = Some(lock);
    }) {
        JobResult::WriteFailed(format!("could not exclusively reserve the drive: {error}"))
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
            should_cancel,
            &mut relay_progress,
        )
    };
    if prepared {
        if result.is_success() && req.source.installable {
            // The profile is written through a second open because the
            // pathname backend's streaming verifier owns and then closes its
            // descriptor.  Revalidate immediately before that open so a
            // disconnect/replug cannot make the profile land on a replacement
            // device which happens to reuse the same path.
            if let Err(error) = revalidate_target(&req.target) {
                result = JobResult::VerifyFailed(format!(
                    "could not revalidate the drive before embedding the setup profile: {error}"
                ));
            } else if let Err(error) =
                write_profile_block_bound(&dst, req.target.size, &req.profile, should_cancel)
            {
                result = if error.kind() == io::ErrorKind::Interrupted {
                    JobResult::Cancelled
                } else {
                    JobResult::VerifyFailed(format!(
                        "could not embed and verify the setup profile: {error}"
                    ))
                };
            }
        }
        if result.is_success()
            && let Some(stage) = req.handoff_stage.as_deref()
        {
            if let Err(error) =
                apply_handoff_stage_to_device(&dst, stage, req.source.size, req.target.size)
            {
                result = JobResult::VerifyFailed(format!(
                    "could not apply Wi-Fi and identity onto the written disk: {error}"
                ));
            }
            let _ = std::fs::remove_dir_all(stage);
        }
        if result.is_success() && should_cancel() {
            result = JobResult::Cancelled;
        }
        finish_target(&req.target, &result);
    }
    // Keep the per-device exclusion through platform finalization, then make
    // its release point explicit before reporting the terminal event.
    drop(target_lock);
    let error = match &result {
        JobResult::Verified => None,
        JobResult::WriteFailed(message) => Some(format!("Could not write the drive: {message}")),
        JobResult::VerifyMismatch(offset) => Some(format!(
            "Verification found different data at byte {offset}. The drive may be faulty."
        )),
        JobResult::VerifyFailed(message) => {
            Some(format!("Could not verify the finished drive: {message}"))
        }
        JobResult::Cancelled => Some(
            "Image writing was cancelled. The drive may be incomplete; review and retry before using it."
                .into(),
        ),
    };
    on_progress(Progress {
        phase: if result.is_success() {
            JobPhase::Success
        } else {
            JobPhase::Failed
        },
        write_frac,
        verify_frac,
        rate_bps: 0,
        error,
    });
    result
}

#[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
fn unmount_udisks_filesystems(
    udisks: &block_devices::udisks::Udisks,
    target: &TargetDrive,
) -> io::Result<()> {
    for name in std::iter::once(target.name.as_str()).chain(
        target
            .partitions
            .iter()
            .map(|partition| partition.name.as_str()),
    ) {
        let usage = udisks
            .id_usage(name)
            .map_err(|error| io::Error::other(format!("UDisks2 safety query failed: {error}")))?;
        if usage != "filesystem" {
            continue;
        }
        let mount_points = udisks.mount_points(name).map_err(|error| {
            io::Error::other(format!("UDisks2 mount-state query failed: {error}"))
        })?;
        if mount_points.is_empty() {
            continue;
        }
        udisks.unmount(name).map_err(|error| {
            io::Error::other(format!("UDisks2 could not unmount {name}: {error}"))
        })?;
    }
    Ok(())
}

/// Ask the host's root-owned `UDisks2` daemon for one descriptor to the exact
/// reviewed device. Polkit owns user authentication; `O_EXCL` prevents a
/// mounted or concurrently opened device from being written.
#[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
fn open_udisks_device(target: &TargetDrive) -> io::Result<std::fs::File> {
    use std::collections::HashMap;
    use zbus::zvariant::{OwnedFd, Value};

    revalidate_target(target)?;
    let udisks = block_devices::udisks::Udisks::system()
        .map_err(|error| io::Error::other(format!("could not reach UDisks2: {error}")))?;
    unmount_udisks_filesystems(&udisks, target)?;
    // Revalidate after unmounting: removable media can disappear while an
    // authorization prompt is on screen. UDisks2 then independently resolves
    // and exclusively opens the same object.
    revalidate_target(target)?;
    let proxy = udisks
        .block(&target.name)
        .map_err(|error| io::Error::other(format!("UDisks2 device lookup failed: {error}")))?;
    let flags = libc::O_EXCL | libc::O_SYNC | libc::O_CLOEXEC;
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("flags", Value::from(flags));
    let fd: OwnedFd = proxy
        .call("OpenDevice", &("rw", options))
        .map_err(|error| io::Error::other(format!("UDisks2 denied device access: {error}")))?;
    let std_fd: std::os::fd::OwnedFd = fd.into();
    Ok(std::fs::File::from(std_fd))
}

#[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
fn run_udisks_job(
    req: &WriteRequest,
    should_cancel: &impl Fn() -> bool,
    mut on_progress: impl FnMut(Progress),
) -> JobResult {
    let mut write_frac = 0.0;
    let mut verify_frac = 0.0;
    let mut relay = |progress: Progress| {
        write_frac = progress.write_frac;
        verify_frac = progress.verify_frac;
        on_progress(progress);
    };
    let mut device = match open_udisks_device(&req.target) {
        Ok(device) => device,
        Err(error) => {
            let result = JobResult::WriteFailed(error.to_string());
            on_progress(Progress {
                phase: JobPhase::Failed,
                error: Some(format!("Could not authorize this drive: {error}")),
                ..Progress::default()
            });
            return result;
        }
    };
    let mut result = run_job_file_bound(req, &mut device, should_cancel, &mut relay);
    drop(device);
    if result.is_success()
        && let Some(stage) = req.handoff_stage.as_deref()
    {
        if let Err(error) = apply_handoff_stage_to_device(
            std::path::Path::new(req.target.dev_path()),
            stage,
            req.source.size,
            req.target.size,
        ) {
            result = JobResult::VerifyFailed(format!(
                "could not apply Wi-Fi and identity onto the written disk: {error}"
            ));
        }
        let _ = std::fs::remove_dir_all(stage);
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
        JobResult::Cancelled => Some(
            "Image writing was cancelled. The drive may be incomplete; review and retry before using it."
                .into(),
        ),
    };
    on_progress(Progress {
        phase: if result.is_success() {
            JobPhase::Success
        } else {
            JobPhase::Failed
        },
        write_frac,
        verify_frac,
        rate_bps: 0,
        error,
    });
    result
}

const WRITE_HELPER_ARG: &str = "--tilo-write-helper";

/// Mount the written disk and copy the staged Wi-Fi / identity tree onto it.
#[cfg(target_os = "linux")]
fn apply_handoff_stage_to_device(
    device: &Path,
    staged: &Path,
    image_len: u64,
    device_len: u64,
) -> io::Result<()> {
    if !staged.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "setup handoff staging directory is missing",
        ));
    }
    let loop_dev = {
        let output = match std::process::Command::new("losetup")
            .args(["-fP", "--show"])
            .arg(device)
            .output()
        {
            Ok(output) if output.status.success() => output,
            _ => {
                return crate::handoff::embed_setup_handoff(device, image_len, device_len, staged);
            }
        };
        String::from_utf8(output.stdout)
            .map_err(io::Error::other)?
            .trim()
            .to_string()
    };
    let finish = |result: io::Result<()>| {
        let _ = std::process::Command::new("losetup")
            .args(["-d", &loop_dev])
            .status();
        result
    };
    let embed = || crate::handoff::embed_setup_handoff(device, image_len, device_len, staged);
    let Some(root_part) = find_tilo_root_partition(&loop_dev) else {
        return finish(embed());
    };
    let mnt = tempfile::Builder::new()
        .prefix("tilo-imager-root-")
        .tempdir()?;
    let mount = std::process::Command::new("mount")
        .args([&root_part, mnt.path().to_str().unwrap_or("/tmp")])
        .status();
    match mount {
        Ok(status) if status.success() => {}
        _ => return finish(embed()),
    }
    let applied = apply_handoff_stage_onto_mounted_root(mnt.path(), staged);
    let _ = std::process::Command::new("umount")
        .arg(mnt.path())
        .status();
    if applied
        .as_ref()
        .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
    {
        return finish(embed());
    }
    finish(applied)
}

/// Copy a staged handoff tree onto an already-mounted installed root.
///
/// Production mounts via losetup; tests exercise this with a fixture tree.
pub(crate) fn apply_handoff_stage_onto_mounted_root(mnt: &Path, staged: &Path) -> io::Result<()> {
    crate::handoff::install_staged_onto_mounted_root(mnt, staged)
}

#[cfg(not(target_os = "linux"))]
fn apply_handoff_stage_to_device(
    _device: &Path,
    _staged: &Path,
    _image_len: u64,
    _device_len: u64,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "applying Wi-Fi and identity onto a written disk requires Linux",
    ))
}

#[cfg(target_os = "linux")]
fn find_tilo_root_partition(loop_dev: &str) -> Option<String> {
    // `losetup -P` creates the partition nodes before udev has identified
    // their filesystems. Giving up on the first empty probe embeds the
    // handoff in slack the installed system never reads.
    for attempt in 0..8 {
        if let Ok(output) = std::process::Command::new("lsblk")
            .args(["-ln", "-o", "PATH,FSTYPE"])
            .arg(loop_dev)
            .output()
            && output.status.success()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(part) = select_tilo_root_partition(loop_dev, &text) {
                return Some(part);
            }
        }
        if attempt + 1 < 8 {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
    None
}

/// Prefer a child partition with a Linux root filesystem; fall back to the
/// loop device itself when the image is a single filesystem.
pub(crate) fn select_tilo_root_partition(loop_dev: &str, lsblk_text: &str) -> Option<String> {
    let mut fallback = None;
    for line in lsblk_text.lines() {
        let mut parts = line.split_whitespace();
        let Some(path) = parts.next() else {
            continue;
        };
        let Some(fstype) = parts.next() else {
            continue;
        };
        if matches!(fstype, "btrfs" | "ext4" | "xfs") {
            if path != loop_dev {
                return Some(path.to_string());
            }
            fallback = Some(path.to_string());
        }
    }
    fallback
}

// `Start-Process` flattens its `ArgumentList` into a native command line. A
// literal double quote must therefore surround each path so spaces remain in
// the same argv element when the elevated process starts. PowerShell's
// single-quoted `'"'` is exactly one double-quote character; `\"` would be a
// backslash followed by a quote and would not group the path.
#[cfg(any(test, target_os = "windows"))]
const WINDOWS_ELEVATION_SCRIPT: &str = r#"$p = Start-Process -FilePath $args[0] -Verb RunAs -Wait -PassThru -ArgumentList @('--tilo-write-helper', ('"' + $args[1] + '"'), ('"' + $args[2] + '"'), $args[3], ('"' + $args[4] + '"')); exit $p.ExitCode"#;

/// Execute the privileged helper mode when `args` requests it. This is called
/// before the windowing runtime is initialized.
///
/// The request is deserialized from a user-private temporary file, then fully
/// validated and its live target identity is checked again by
/// `run_system_job`. Progress is append-only JSON so the unprivileged window
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
    let cancel_path = args
        .get(at + 4)
        .ok_or("write helper cancellation path is missing")?;
    if args.get(at + 5).is_some() {
        return Err("write helper received unexpected arguments".into());
    }
    validate_helper_paths(
        Path::new(request_path),
        Path::new(progress_path),
        Path::new(cancel_path),
    )?;
    #[cfg(target_os = "linux")]
    let (request_bytes, caller_uid) = read_linux_helper_request(Path::new(request_path))?;
    #[cfg(not(target_os = "linux"))]
    let request_bytes = read_helper_request(Path::new(request_path))?;
    if request_bytes.len() > 1024 * 1024 {
        return Err("write helper request is unexpectedly large".into());
    }
    let actual_digest = hex::encode(Sha256::digest(&request_bytes));
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
    #[cfg(target_os = "linux")]
    let request = {
        let mut request = request;
        bind_linux_helper_source(&mut request, caller_uid)?;
        request
    };
    // The privileged process creates the progress inode itself. `create_new`
    // refuses a pre-planted symlink instead of letting a user-writable path
    // redirect root's append into an arbitrary file.
    let mut progress_file = create_progress_output(Path::new(progress_path))?;
    let mut terminal_sent = false;
    let result = run_system_job(&request, &|| Path::new(cancel_path).exists(), |progress| {
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

#[cfg(not(target_os = "linux"))]
fn read_helper_request(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = OpenOptions::new().read(true).open(path)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(target_os = "linux")]
fn read_linux_helper_request(request_path: &Path) -> io::Result<(Vec<u8>, u32)> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let caller_uid = std::env::var("PKEXEC_UID")
        .map_err(|_| io::Error::other("write helper is missing PKEXEC_UID"))?
        .parse::<u32>()
        .map_err(|_| io::Error::other("write helper received an invalid PKEXEC_UID"))?;
    let directory = request_path
        .parent()
        .ok_or_else(|| io::Error::other("write helper request has no parent directory"))?;
    let directory_file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory)?;
    let directory_metadata = directory_file.metadata()?;
    if !directory_metadata.is_dir()
        || directory_metadata.uid() != caller_uid
        || directory_metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "write helper job directory is not private to the authenticated user",
        ));
    }
    let name = CString::new("request.json").expect("fixed request filename has no NUL");
    // SAFETY: the held directory descriptor remains live for this call, the
    // filename is a valid C string, and a successful descriptor is moved into
    // exactly one `File` owner below.
    let request_fd = unsafe {
        libc::openat(
            directory_file.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if request_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a new owned descriptor and ownership is
    // transferred once to this `File`.
    let mut request_file = unsafe { std::fs::File::from_raw_fd(request_fd) };
    let request_metadata = request_file.metadata()?;
    if !request_metadata.is_file()
        || request_metadata.uid() != caller_uid
        || request_metadata.permissions().mode() & 0o022 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "write helper request is not owned safely by the authenticated user",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut request_file)
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    Ok((bytes, caller_uid))
}

fn validate_helper_paths(request: &Path, progress: &Path, cancel: &Path) -> io::Result<()> {
    let request_parent = request
        .parent()
        .ok_or_else(|| io::Error::other("write helper request has no parent directory"))?;
    if progress.parent() != Some(request_parent)
        || cancel.parent() != Some(request_parent)
        || request.file_name().and_then(|name| name.to_str()) != Some("request.json")
        || progress.file_name().and_then(|name| name.to_str()) != Some("progress.jsonl")
        || cancel.file_name().and_then(|name| name.to_str()) != Some("cancel")
        || !request_parent
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("tilo-imager-write-"))
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

/// Bind a root helper to authority the authenticated desktop process already
/// possessed. `pkexec` preserves stdin, which the unprivileged process opens
/// on the reviewed image before elevation. The helper therefore never opens a
/// caller-named source path with root authority.
#[cfg(target_os = "linux")]
fn bind_linux_helper_source(request: &mut WriteRequest, _caller_uid: u32) -> io::Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "write helper must run through pkexec",
        ));
    }
    let expected_identity = request.source.identity.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "write helper source has no reviewed file identity",
        )
    })?;
    let inherited_source = std::fs::File::open("/proc/self/fd/0")?;
    let metadata = inherited_source.metadata()?;
    if !metadata.is_file()
        || metadata.len() != request.source.size
        || SourceIdentity::from_file(&inherited_source)? != Some(expected_identity)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "inherited source image does not match the reviewed file",
        ));
    }
    request.source.path = "/proc/self/fd/0".into();
    Ok(())
}

fn create_progress_output(path: &Path) -> io::Result<std::fs::File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

fn spawn_privileged_helper(req: WriteRequest, control: &JobControl, tx: &Sender<Progress>) {
    let result = run_privileged_helper(req, control, tx);
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

fn run_privileged_helper(
    req: WriteRequest,
    control: &JobControl,
    tx: &Sender<Progress>,
) -> io::Result<()> {
    let source_file = OpenOptions::new().read(true).open(&req.source.path)?;
    let source_metadata = source_file.metadata()?;
    let expected_identity = req.source.identity.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "reviewed source has no stable file identity",
        )
    })?;
    if !source_metadata.is_file()
        || source_metadata.len() != req.source.size
        || SourceIdentity::from_file(&source_file)? != Some(expected_identity)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source image changed after review",
        ));
    }
    let work = tempfile::Builder::new()
        .prefix("tilo-imager-write-")
        .tempdir()?;
    let request_path = work.path().join("request.json");
    let progress_path = work.path().join("progress.jsonl");
    let cancel_path = work.path().join("cancel");
    let mut request_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&request_path)?;
    let request_bytes = serde_json::to_vec(&req.without_secrets())?;
    request_file.write_all(&request_bytes)?;
    request_file.flush()?;
    request_file.sync_all()?;
    drop(request_file);
    let request_digest = hex::encode(Sha256::digest(&request_bytes));
    let executable = helper_executable()?;
    let mut child = elevated_helper_command(
        &executable,
        &request_path,
        &progress_path,
        &request_digest,
        &cancel_path,
        source_file,
    )?
    .spawn()?;
    relay_helper_progress(&mut child, &progress_path, &cancel_path, control, tx)
}

fn helper_executable() -> io::Result<std::path::PathBuf> {
    #[cfg(target_os = "linux")]
    if let Some(appimage) = std::env::var_os("TILO_IMAGER_APPIMAGE") {
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
    cancel_path: &Path,
    control: &JobControl,
    tx: &Sender<Progress>,
) -> io::Result<()> {
    let result = relay_helper_progress_inner(child, progress_path, cancel_path, control, tx);
    if result.is_err() {
        // Never abandon an elevated raw-device writer merely because its
        // unprivileged progress channel failed. Keeping the private job
        // directory alive and waiting for the child prevents the UI from
        // claiming the operation stopped while destructive I/O may continue.
        let _ = child.wait();
    }
    result
}

fn relay_helper_progress_inner(
    child: &mut Child,
    progress_path: &Path,
    cancel_path: &Path,
    control: &JobControl,
    tx: &Sender<Progress>,
) -> io::Result<()> {
    let mut cancel_sent = false;
    let file = loop {
        // A transient filesystem error must not detach supervision from a
        // privileged writer. Leave `cancel_sent` false and retry on the next
        // poll; the UI conservatively rejects a racing Success event too.
        let _ = relay_cancellation(control, cancel_path, &mut cancel_sent);
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
        let _ = relay_cancellation(control, cancel_path, &mut cancel_sent);
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

fn relay_cancellation(control: &JobControl, cancel_path: &Path, sent: &mut bool) -> io::Result<()> {
    if control.is_cancelled() && !*sent {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(cancel_path)
        {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        *sent = true;
    }
    Ok(())
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
    cancel: &Path,
    source: std::fs::File,
) -> io::Result<Command> {
    let mut command = Command::new("pkexec");
    command
        .arg(exe)
        .arg(WRITE_HELPER_ARG)
        .arg(request)
        .arg(progress)
        .arg(digest)
        .arg(cancel)
        .stdin(Stdio::from(source))
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
    cancel: &Path,
    source: std::fs::File,
) -> io::Result<Command> {
    drop(source);
    const SCRIPT: &str = r#"on run argv
set helperCommand to quoted form of item 1 of argv & " --tilo-write-helper " & quoted form of item 2 of argv & " " & quoted form of item 3 of argv & " " & quoted form of item 4 of argv & " " & quoted form of item 5 of argv
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
        .arg(cancel)
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
    cancel: &Path,
    source: std::fs::File,
) -> io::Result<Command> {
    drop(source);
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
        .arg(cancel)
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
    _cancel: &Path,
    _source: std::fs::File,
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
    const PREPARE_OUTPUT_LIMIT: usize = 1024 * 1024;
    const PREPARE_TIMEOUT: Duration = Duration::from_secs(10);
    let device_path = target.dev_path();
    let out = bounded_command::run(
        "lsblk",
        &["-lnpo", "NAME,MOUNTPOINT", &device_path],
        None,
        PREPARE_OUTPUT_LIMIT,
        PREPARE_TIMEOUT,
    )
    .map_err(|error| io::Error::other(format!("lsblk probe failed: {error}")))?;
    if !out.status.success() {
        return Err(io::Error::other("lsblk could not inspect mount state"));
    }
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut fields = line.split_whitespace();
        let Some(device) = fields.next() else {
            continue;
        };
        if fields.next().is_some() {
            let out = bounded_command::run(
                "umount",
                &[device],
                None,
                PREPARE_OUTPUT_LIMIT,
                PREPARE_TIMEOUT,
            )
            .map_err(|error| io::Error::other(format!("unmount failed: {error}")))?;
            if !out.status.success() {
                return Err(io::Error::other(format!("failed to unmount {device}")));
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn prepare_target(target: &TargetDrive) -> io::Result<()> {
    let mut command = Command::new("diskutil");
    command.args(["unmountDisk", &format!("/dev/{}", target.name)]);
    let status = run_finite_status(command, Duration::from_secs(10))?;
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
    let mut command = Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    let status = run_finite_status(command, Duration::from_secs(10))?;
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
fn finish_target(target: &TargetDrive, result: &JobResult) {
    // Keep a failed target attached so the user can retry without physically
    // reconnecting it. It remains unmounted from `prepare_target`, which is
    // also the safe state for another raw-write attempt. Successful media is
    // ejected so it can be removed immediately.
    if macos_should_eject(result) {
        let mut command = Command::new("diskutil");
        command.args(["eject", &format!("/dev/{}", target.name)]);
        let _ = run_finite_status(command, Duration::from_secs(10));
    }
}

#[cfg(any(test, target_os = "macos"))]
fn macos_should_eject(result: &JobResult) -> bool {
    result.is_success()
}

#[cfg(target_os = "windows")]
fn finish_target(target: &TargetDrive, _result: &JobResult) {
    // Set-Disk took the device offline before opening its raw handle. Always
    // restore it, including after a failed attempt, so it remains usable.
    if let Some(number) = target.name.strip_prefix("PhysicalDrive") {
        let script =
            format!("Set-Disk -Number {number} -IsOffline $false -ErrorAction SilentlyContinue");
        let mut command = Command::new("powershell.exe");
        command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
        let _ = run_finite_status(command, Duration::from_secs(10));
    }
}

#[cfg(any(test, target_os = "macos", target_os = "windows"))]
fn run_finite_status(
    mut command: Command,
    timeout: Duration,
) -> io::Result<std::process::ExitStatus> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn()?;
    let deadline = ui::time::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if ui::time::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "platform media command timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn finish_target(_target: &TargetDrive, _result: &JobResult) {}

/// Spawn a job on a worker thread and return the receiver the UI drains once
/// per frame. The returned [`JobStart`] is the validation result; a channel is
/// only returned when it is [`JobStart::Started`].
///
/// * Tests/shots → the request is refused and no worker is created.
/// * Live UI → platform authentication starts the narrow helper.
/// * Deliberate allow-gated root diagnostics → `run_system_job` runs in the
///   current process.
pub fn spawn_job(
    req: WriteRequest,
    typed: &str,
) -> (JobStart, Option<Receiver<Progress>>, Option<JobControl>) {
    let start = begin_job(&req, typed);
    if start != JobStart::Started {
        return (start, None, None);
    }
    let backend = WriteBackend::system();
    if backend == WriteBackend::Disabled {
        return (JobStart::WriteDisabled, None, None);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let control = JobControl::new();
    let worker_control = control.clone();
    let _ = std::thread::Builder::new()
        .name("imager-write".into())
        .spawn(move || match backend {
            WriteBackend::System => {
                let _ = run_system_job(&req, &|| worker_control.is_cancelled(), |p| {
                    let _ = tx.send(p);
                });
            }
            WriteBackend::PrivilegedHelper => {
                spawn_privileged_helper(req, &worker_control, &tx);
            }
            WriteBackend::Udisks2 => {
                #[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
                let _ = run_udisks_job(&req, &|| worker_control.is_cancelled(), |p| {
                    let _ = tx.send(p);
                });
                #[cfg(not(all(target_os = "linux", feature = "flatpak-media-writer")))]
                let _ = tx.send(Progress {
                    phase: JobPhase::Failed,
                    error: Some("UDisks2 media writing is unavailable in this build.".into()),
                    ..Progress::default()
                });
            }
            WriteBackend::Disabled => {}
        });
    (JobStart::Started, Some(rx), Some(control))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{sample_source, sample_targets};

    /// Target identity, confirmation and job admission.
    #[test]
    fn begin_refuses_mismatched_oversized_forbidden_or_invalid_jobs() {
        // Phase `target_lock_prefers_stable_hardware_identity_then_falls_back_to_path`.
        {
            let mut original = sample_targets().remove(0);
            original.stable_id = Some("usb-serial-123".into());
            let mut renumbered = original.clone();
            renumbered.name = "renumbered-device".into();
            assert_eq!(target_lock_name(&original), target_lock_name(&renumbered));

            original.stable_id = None;
            renumbered.stable_id = None;
            assert_ne!(target_lock_name(&original), target_lock_name(&renumbered));
        }

        // Phase `confirmation_must_match_target`.
        {
            let r = req();
            assert!(r.confirmation_matches("sdb"));
            assert!(r.confirmation_matches("/dev/sdb"));
            assert!(!r.confirmation_matches("sda"));
            assert!(!r.confirmation_matches(""));
            assert!(!r.confirmation_matches("nvme0n1"));
        }

        // Phase `begin_refuses_on_mismatched_confirmation`.
        {
            let r = req();
            assert_eq!(begin_job(&r, "sda"), JobStart::ConfirmationMismatch);
            assert_eq!(begin_job(&r, ""), JobStart::ConfirmationMismatch);
        }

        // Phase `begin_refuses_when_image_too_big`.
        {
            let mut r = req();
            r.source.size = 40_000_000_000; // bigger than the 30 GB stick
            assert_eq!(begin_job(&r, "sdb"), JobStart::TooSmall);
        }

        // Phase `begin_refuses_internal_and_system_targets`.
        {
            let mut r = req();
            r.target.removable = false;
            r.target.tran = Some("sata".into());
            assert_eq!(begin_job(&r, "sdb"), JobStart::UnsafeTarget);
            r.target.removable = true;
            r.target.is_system = true;
            assert_eq!(begin_job(&r, "sdb"), JobStart::UnsafeTarget);
        }

        // Phase `begin_refuses_invalid_profile_before_worker_start`.
        {
            let mut r = req();
            r.profile.keyboard = "not-a-layout".into();
            assert!(matches!(
                begin_job(&r, "sdb"),
                JobStart::InvalidProfile(message) if message.contains("Keyboard")
            ));
        }

        // Phase `target_revalidation_rejects_reused_device_name`.
        {
            let selected = req().target;
            let mut replacement = selected.clone();
            assert!(target_identity_matches(&selected, &replacement));
            replacement.stable_id = Some("another-device".into());
            assert!(!target_identity_matches(&selected, &replacement));
            replacement = selected.clone();
            replacement.is_system = true;
            assert!(!target_identity_matches(&selected, &replacement));
        }
    }

    #[cfg(unix)]
    #[test]
    fn target_lock_excludes_another_helper_handle_until_the_owner_releases_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target.lock");
        let first = try_lock_file(&path).expect("first helper reserves target");
        let contention = try_lock_file(&path).expect_err("second helper must be excluded");
        assert_eq!(contention.kind(), io::ErrorKind::WouldBlock);

        drop(first);
        let _reacquired = try_lock_file(&path).expect("kernel releases lock with its owner");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn target_lock_excludes_another_helper_handle_until_the_owner_releases_it() {
        let target = sample_targets().remove(0);
        let first = acquire_target_write_lock(&target).expect("first helper reserves target");
        let contention =
            acquire_target_write_lock(&target).expect_err("second helper must be excluded");
        assert_eq!(contention.kind(), io::ErrorKind::WouldBlock);

        drop(first);
        acquire_target_write_lock(&target).expect("kernel releases lock with its owner");
    }

    fn req() -> WriteRequest {
        WriteRequest {
            source: sample_source(),
            target: sample_targets()[0].clone(), // the USB stick
            action: Action::Flash,
            profile: InstallProfile::default(),
            handoff_stage: None,
        }
    }

    /// Backend gating across builds and platforms.
    #[test]
    fn backend_selection_is_gated_and_never_simulates_writes() {
        // Phase `default_backend_is_disabled_without_gate`.
        #[cfg(not(feature = "host-media-writer"))]
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            // No opt-in env var set → unavailable, never a fake success.
            env.remove("TILO_IMAGER_ALLOW_WRITE").remove("IMAGER_LIVE");
            assert_eq!(WriteBackend::system(), WriteBackend::Disabled);
            assert!(!allow_write());
        }

        // Phase `packaged_host_build_enables_the_short_lived_helper_without_an_environment_flag`.
        #[cfg(feature = "host-media-writer")]
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.remove("TILO_IMAGER_ALLOW_WRITE").remove("IMAGER_LIVE");
            assert_eq!(WriteBackend::system(), WriteBackend::PrivilegedHelper);
            assert!(write_available());
        }

        // Phase `live_ui_uses_short_lived_privileged_helper`.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.remove("TILO_IMAGER_ALLOW_WRITE")
                .set("IMAGER_LIVE", "1");
            assert_eq!(WriteBackend::system(), WriteBackend::PrivilegedHelper);
            assert!(write_available());
        }

        // Phase `flatpak_selects_udisks_without_enabling_direct_device_writes`.
        #[cfg(all(target_os = "linux", feature = "flatpak-media-writer"))]
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.remove("TILO_IMAGER_ALLOW_WRITE")
                .remove("IMAGER_LIVE")
                .set("TILO_IMAGER_FLATPAK", "1");
            assert_eq!(WriteBackend::system(), WriteBackend::Udisks2);
            assert!(!allow_write(), "the sandbox never gains direct /dev access");
        }

        // Phase `only_disabled_backend_is_unavailable`.
        {
            assert!(!WriteBackend::Disabled.is_available());
            assert!(WriteBackend::System.is_available());
            assert!(WriteBackend::PrivilegedHelper.is_available());
            assert!(WriteBackend::Udisks2.is_available());
        }

        // Phase `system_backend_selected_only_with_gate_on`.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            // The real block-device backend is chosen ONLY when the operator opts
            // in explicitly; flipping the gate is the single switch between the
            // disabled path and the real device write.
            env.set("TILO_IMAGER_ALLOW_WRITE", "1");
            assert!(allow_write());
            assert_eq!(WriteBackend::system(), WriteBackend::System);
            env.remove("TILO_IMAGER_ALLOW_WRITE");
            let expected = if cfg!(feature = "host-media-writer") {
                WriteBackend::PrivilegedHelper
            } else {
                WriteBackend::Disabled
            };
            assert_eq!(WriteBackend::system(), expected);
        }

        // Phase `spawn_refuses_instead_of_simulating_with_gate_off`.
        #[cfg(not(feature = "host-media-writer"))]
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            // Validation remains pure, but the live start refuses rather than
            // reporting synthetic progress and success.
            env.remove("TILO_IMAGER_ALLOW_WRITE").remove("IMAGER_LIVE");
            let r = req();
            assert_eq!(begin_job(&r, "sdb"), JobStart::Started);
            let (start, rx, control) = spawn_job(r, "sdb");
            assert_eq!(start, JobStart::WriteDisabled);
            assert!(rx.is_none());
            assert!(control.is_none());
        }
    }

    /// Helper request integrity and elevation.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn privileged_helper_requests_are_secret_free_private_and_exact() {
        // Phase `privileged_request_never_contains_user_or_encryption_secrets`.
        {
            let mut request = req();
            request.profile.user.password = "account-secret".into();
            request.profile.encryption_passphrase = "encryption-secret".into();
            let encoded = serde_json::to_string(&request.without_secrets()).unwrap();
            assert!(!encoded.contains("account-secret"));
            assert!(!encoded.contains("encryption-secret"));
            assert!(encoded.contains("\"password\":\"\""));
            assert!(encoded.contains("\"encryption_passphrase\":\"\""));
        }

        // Phase `helper_rejects_a_request_changed_after_authorization`.
        {
            #[cfg(target_os = "linux")]
            use std::os::unix::fs::PermissionsExt;

            #[cfg(target_os = "linux")]
            let mut env = tilo_test_env::ScopedEnv::new();
            #[cfg(target_os = "linux")]
            env.set("PKEXEC_UID", unsafe { libc::geteuid() }.to_string());
            let work = tempfile::Builder::new()
                .prefix("tilo-imager-write-")
                .tempdir()
                .unwrap();
            let request_path = work.path().join("request.json");
            let progress_path = work.path().join("progress.jsonl");
            let cancel_path = work.path().join("cancel");
            std::fs::write(&request_path, serde_json::to_vec(&req()).unwrap()).unwrap();
            std::fs::write(&progress_path, []).unwrap();
            #[cfg(target_os = "linux")]
            {
                std::fs::set_permissions(work.path(), std::fs::Permissions::from_mode(0o700))
                    .unwrap();
                std::fs::set_permissions(&request_path, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
            }
            let args = vec![
                "imager".into(),
                WRITE_HELPER_ARG.into(),
                request_path.display().to_string(),
                progress_path.display().to_string(),
                "0".repeat(64),
                cancel_path.display().to_string(),
            ];
            let error = maybe_run_write_helper(&args).unwrap_err();
            #[cfg(target_os = "linux")]
            drop(env);
            assert!(error.to_string().contains("changed after authorization"));
            assert!(std::fs::read(progress_path).unwrap().is_empty());
        }

        // Phase `helper_rejects_a_non_private_job_directory`.
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut env = tilo_test_env::ScopedEnv::new();
            let work = tempfile::Builder::new()
                .prefix("tilo-imager-write-")
                .tempdir()
                .unwrap();
            let request_path = work.path().join("request.json");
            std::fs::write(&request_path, b"{}").unwrap();
            std::fs::set_permissions(work.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
            env.set("PKEXEC_UID", unsafe { libc::geteuid() }.to_string());

            let error = read_linux_helper_request(&request_path).unwrap_err();

            drop(env);
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert!(error.to_string().contains("not private"));
        }

        // Phase `helper_request_reader_never_follows_a_symlink`.
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            use std::os::unix::fs::symlink;

            let mut env = tilo_test_env::ScopedEnv::new();
            let work = tempfile::tempdir().unwrap();
            let outside = work.path().join("outside");
            let request = work.path().join("request.json");
            std::fs::write(&outside, b"root-only payload").unwrap();
            symlink(&outside, &request).unwrap();
            std::fs::set_permissions(work.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            env.set("PKEXEC_UID", unsafe { libc::geteuid() }.to_string());

            let error = read_linux_helper_request(&request).unwrap_err();

            drop(env);
            assert_eq!(error.raw_os_error(), Some(libc::ELOOP));
        }

        // Phase `helper_never_follows_or_overwrites_a_planted_progress_path`.
        {
            let work = tempfile::Builder::new()
                .prefix("tilo-imager-write-")
                .tempdir()
                .unwrap();
            let progress_path = work.path().join("progress.jsonl");
            std::fs::write(&progress_path, b"keep-me").unwrap();
            let error = create_progress_output(&progress_path).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(std::fs::read(progress_path).unwrap(), b"keep-me");
        }

        // Phase `linux_elevates_only_the_hidden_helper_mode`.
        #[cfg(target_os = "linux")]
        {
            let source = tempfile::tempfile().unwrap();
            let command = elevated_helper_command(
                Path::new("/opt/tilo-imager-bin"),
                Path::new("/tmp/request"),
                Path::new("/tmp/progress"),
                &"a".repeat(64),
                Path::new("/tmp/cancel"),
                source,
            )
            .unwrap();
            assert_eq!(command.get_program(), "pkexec");
            let args: Vec<_> = command.get_args().collect();
            assert_eq!(args[0], "/opt/tilo-imager-bin");
            assert_eq!(args[1], WRITE_HELPER_ARG);
            assert_eq!(args.len(), 6);
        }

        // Phase `windows_elevation_script_native_quotes_paths_with_spaces`.
        {
            // PowerShell single-quoted strings do not treat backslash as an
            // escape. The wrapper must concatenate a literal `"`, not `\"`,
            // around both paths before Start-Process flattens ArgumentList.
            assert!(WINDOWS_ELEVATION_SCRIPT.contains("('\"' + $args[1] + '\"')"));
            assert!(WINDOWS_ELEVATION_SCRIPT.contains("('\"' + $args[2] + '\"')"));
            assert!(WINDOWS_ELEVATION_SCRIPT.contains("('\"' + $args[4] + '\"')"));
            assert!(!WINDOWS_ELEVATION_SCRIPT.contains(r#"('\"' +"#));
        }
    }

    /// Cancellation markers, relay errors and command deadlines.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn helper_cancellation_and_child_processes_are_bounded_and_retryable() {
        // Phase `cancellation_marker_is_separate_from_digest_bound_request`.
        {
            let work = tempfile::Builder::new()
                .prefix("tilo-imager-write-")
                .tempdir()
                .unwrap();
            let request_path = work.path().join("request.json");
            let cancel_path = work.path().join("cancel");
            let request_bytes = serde_json::to_vec(&req()).unwrap();
            std::fs::write(&request_path, &request_bytes).unwrap();
            let digest_before = Sha256::digest(&request_bytes);
            let control = JobControl::new();
            control.cancel();
            let mut sent = false;

            relay_cancellation(&control, &cancel_path, &mut sent).unwrap();

            assert!(sent);
            assert!(cancel_path.is_file());
            assert_eq!(
                Sha256::digest(std::fs::read(request_path).unwrap()),
                digest_before
            );
        }

        // Phase `failed_cancellation_delivery_remains_retryable`.
        {
            let work = tempfile::tempdir().unwrap();
            let job_dir = work.path().join("tilo-imager-write-retry");
            let cancel_path = job_dir.join("cancel");
            let control = JobControl::new();
            control.cancel();
            let mut sent = false;

            assert!(relay_cancellation(&control, &cancel_path, &mut sent).is_err());
            assert!(!sent, "a failed marker write must be retried");
            std::fs::create_dir(&job_dir).unwrap();
            relay_cancellation(&control, &cancel_path, &mut sent).unwrap();

            assert!(sent);
            assert!(cancel_path.is_file());
        }

        // Phase `relay_error_waits_until_the_privileged_child_has_exited`.
        #[cfg(unix)]
        {
            let work = tempfile::tempdir().unwrap();
            let mut child = Command::new("sh")
                .args(["-c", "sleep 0.05"])
                .spawn()
                .unwrap();
            let (tx, _rx) = std::sync::mpsc::channel();
            let control = JobControl::new();

            // Opening a directory as the JSONL progress file fails immediately.
            let error = relay_helper_progress(
                &mut child,
                work.path(),
                &work.path().join("cancel"),
                &control,
                &tx,
            )
            .unwrap_err();

            assert_ne!(error.kind(), io::ErrorKind::NotFound);
            assert!(child.try_wait().unwrap().is_some());
        }

        // Phase `finite_platform_command_is_reaped_at_its_deadline`.
        #[cfg(unix)]
        {
            let mut command = Command::new("sh");
            command.args(["-c", "sleep 10"]);
            let started = ui::time::now();
            let error = run_finite_status(command, Duration::from_millis(20)).unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert!(started.elapsed() < Duration::from_secs(1));
        }

        // Phase `helper_rejects_cancellation_marker_outside_private_job_directory`.
        {
            let work = tempfile::Builder::new()
                .prefix("tilo-imager-write-")
                .tempdir()
                .unwrap();
            let outside = tempfile::tempdir().unwrap();
            let request_path = work.path().join("request.json");
            std::fs::write(&request_path, b"{}").unwrap();

            let error = validate_helper_paths(
                &request_path,
                &work.path().join("progress.jsonl"),
                &outside.path().join("cancel"),
            )
            .unwrap_err();

            assert!(
                error
                    .to_string()
                    .contains("outside its private job directory")
            );
        }
    }

    /// Profile block reservation, encoding, extraction and cancel.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn media_profile_block_is_reserved_secret_free_and_verified() {
        // Phase `q_os_media_reserves_room_for_the_profile`.
        {
            let mut r = req();
            r.source.installable = true;
            r.source.size = r.target.size - PROFILE_BLOCK_SIZE as u64 + 1;
            assert_eq!(begin_job(&r, "sdb"), JobStart::TooSmall);
        }

        // Phase `media_profile_is_secret_free_and_read_back_verified`.
        {
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

        // Phase `invalid_media_profile_is_rejected_before_a_block_is_encoded`.
        {
            let profile = InstallProfile {
                hostname: "bad host".into(),
                ..InstallProfile::default()
            };
            let error = encode_profile_block(&profile).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(error.to_string().contains("Hostname"));
        }

        // Phase `written_profile_is_consumed_by_the_live_extractor`.
        // Exercise the real Rust encoder/tail writer against the exact live-media
        // extractor instead of maintaining two merely similar format fixtures.
        #[cfg(target_os = "linux")]
        {
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
                .join("../../os/usr/libexec/tilo-installer-profile-extract");
            let status = std::process::Command::new(extractor)
                .env("TILO_INSTALLER_PROFILE_DEVICE", &media)
                .env("TILO_INSTALLER_PROFILE_ALLOW_FILE", "1")
                .env("TILO_INSTALLER_PROFILE_OUT_DIR", &out)
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

        // Phase `profile_write_honours_cancel_before_touching_the_tail`.
        {
            let target = tmp_with(b"leave this target unchanged");
            let before = std::fs::read(target.path()).unwrap();
            let error = write_profile_block_bound(
                target.path(),
                before.len() as u64,
                &InstallProfile::default(),
                &|| true,
            )
            .unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
            assert_eq!(std::fs::read(target.path()).unwrap(), before);
        }
    }

    /// Stream copy, readback verification, job paths and eject.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn write_engine_copies_verifies_and_reports_end_to_end() {
        // Phase `stream_copy_reproduces_source_byte_for_byte`.
        {
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

        // Phase `verify_readback_passes_on_identical_and_flags_the_diff`.
        {
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

        // Phase `run_job_paths_writes_and_verifies_end_to_end`.
        {
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

        // Phase `authorized_descriptor_path_writes_verifies_and_embeds_profile`.
        {
            let data = sample_bytes(CHUNK + 917);
            let source = tmp_with(&data);
            let mut target = tempfile::tempfile().unwrap();
            let target_size = data.len() as u64 + PROFILE_BLOCK_SIZE as u64 + 4096;
            target.set_len(target_size).unwrap();
            let mut request = req();
            request.source.path = source.path().to_string_lossy().into_owned();
            request.source.size = data.len() as u64;
            request.source.identity = SourceIdentity::from_file(source.as_file()).unwrap();
            request.source.installable = true;
            request.source.expected_sha256 = None;
            request.target.size = target_size;

            let mut phases = Vec::new();
            let result = run_job_file_bound(&request, &mut target, &|| false, |progress| {
                if phases.last() != Some(&progress.phase) {
                    phases.push(progress.phase);
                }
            });

            assert_eq!(result, JobResult::Verified);
            assert_eq!(phases, vec![JobPhase::Writing, JobPhase::Verifying]);
            target.seek(SeekFrom::Start(0)).unwrap();
            let mut written = vec![0_u8; data.len()];
            target.read_exact(&mut written).unwrap();
            assert_eq!(written, data);
            target
                .seek(SeekFrom::Start(target_size - PROFILE_BLOCK_SIZE as u64))
                .unwrap();
            let mut profile = vec![0_u8; PROFILE_BLOCK_SIZE];
            target.read_exact(&mut profile).unwrap();
            assert!(profile.starts_with(PROFILE_MAGIC.as_bytes()));
        }

        // Phase `run_job_paths_reports_write_failure_on_bad_dest`.
        {
            let data = sample_bytes(1024);
            let src = tmp_with(&data);
            // A destination under a path that cannot exist → open fails → WriteFailed.
            let bad = std::path::Path::new("/proc/definitely/not/writable/imager.img");
            let result = run_job_paths(src.path(), bad, data.len() as u64, |_| {});
            assert!(matches!(result, JobResult::WriteFailed(_)));
        }

        // Phase `macos_ejects_only_after_a_fully_verified_job`.
        {
            assert!(macos_should_eject(&JobResult::Verified));
            assert!(!macos_should_eject(&JobResult::WriteFailed(
                "device disconnected".into()
            )));
            assert!(!macos_should_eject(&JobResult::VerifyMismatch(42)));
            assert!(!macos_should_eject(&JobResult::VerifyFailed(
                "read-back failed".into()
            )));
        }
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

    /// Cancellation during write and readback, and revalidation failure.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn cooperative_cancel_and_revalidation_failures_never_claim_success() {
        // Phase `cooperative_cancel_stops_between_write_chunks_and_never_verifies`.
        {
            let data = sample_bytes(CHUNK * 3);
            let src = tmp_with(&data);
            let dst = tempfile::NamedTempFile::new().unwrap();
            let control = JobControl::new();
            let signal = control.clone();
            let mut phases = Vec::new();

            let result = run_job_paths_bound(
                src.path(),
                dst.path(),
                data.len() as u64,
                None,
                None,
                &|| control.is_cancelled(),
                |progress| {
                    phases.push(progress.phase);
                    signal.cancel();
                },
            );

            assert_eq!(result, JobResult::Cancelled);
            assert_eq!(phases, vec![JobPhase::Writing]);
            assert_eq!(std::fs::metadata(dst.path()).unwrap().len(), CHUNK as u64);
        }

        // Phase `cooperative_cancel_stops_during_readback_without_claiming_success`.
        {
            let data = sample_bytes(CHUNK * 3);
            let src = tmp_with(&data);
            let dst = tempfile::NamedTempFile::new().unwrap();
            let control = JobControl::new();
            let signal = control.clone();
            let mut saw_verify = false;

            let result = run_job_paths_bound(
                src.path(),
                dst.path(),
                data.len() as u64,
                None,
                None,
                &|| control.is_cancelled(),
                |progress| {
                    if progress.phase == JobPhase::Verifying {
                        saw_verify = true;
                        signal.cancel();
                    }
                },
            );

            assert_eq!(result, JobResult::Cancelled);
            assert!(saw_verify);
        }

        // Phase `target_revalidation_failure_still_reports_an_actionable_terminal_event`.
        {
            let mut request = req();
            request.target.name = "tilo-test-device-that-cannot-exist".into();
            let mut updates = Vec::new();

            let result = run_system_job(&request, &|| false, |progress| updates.push(progress));

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
    }

    /// Source size, replacement, identity and digest checks during the write.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn write_source_must_stay_the_reviewed_file_and_digest() {
        // Phase `reviewed_source_size_fails_closed_after_truncation_or_growth`.
        {
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

        // Phase `replaced_source_is_rejected_before_destination_is_opened`.
        {
            let data = sample_bytes(4096);
            let root = tempfile::tempdir().unwrap();
            let selected = root.path().join("selected.img");
            std::fs::write(&selected, &data).unwrap();
            let held = std::fs::File::open(&selected).unwrap();
            let identity = SourceIdentity::from_file(&held)
                .unwrap()
                .expect("test platform exposes file identity");
            std::fs::rename(&selected, root.path().join("reviewed.img")).unwrap();
            std::fs::write(&selected, &data).unwrap();
            let dst = tmp_with(b"leave destination untouched");

            let result = run_job_paths_bound(
                &selected,
                dst.path(),
                data.len() as u64,
                Some(identity),
                None,
                &|| false,
                |_| panic!("a replaced source must not produce write progress"),
            );
            assert!(
                matches!(result, JobResult::WriteFailed(ref message) if message.contains("replaced"))
            );
            assert_eq!(
                std::fs::read(dst.path()).unwrap(),
                b"leave destination untouched"
            );

            let absent_destination = root.path().join("not-created.img");
            let result = run_job_paths_bound(
                &selected,
                &absent_destination,
                data.len() as u64,
                Some(identity),
                None,
                &|| false,
                |_| panic!("a replaced source must not produce write progress"),
            );
            assert!(matches!(result, JobResult::WriteFailed(_)));
            assert!(!absent_destination.exists());

            // The held identity still authorizes the original file at its new path.
            let result = run_job_paths_bound(
                &root.path().join("reviewed.img"),
                dst.path(),
                data.len() as u64,
                Some(identity),
                None,
                &|| false,
                |_| {},
            );
            assert_eq!(result, JobResult::Verified);
            assert_eq!(std::fs::read(dst.path()).unwrap(), data);
        }

        // Phase `descriptor_writer_rejects_another_file_with_identical_bytes_before_mutation`.
        {
            let data = sample_bytes(4096);
            let reviewed = tmp_with(&data);
            let replacement = tmp_with(&data);
            let mut target = tmp_with(b"leave destination untouched");
            target.as_file_mut().seek(SeekFrom::Start(7)).unwrap();
            let mut request = req();
            request.source.path = replacement.path().to_string_lossy().into_owned();
            request.source.size = data.len() as u64;
            request.source.identity = Some(
                SourceIdentity::from_file(reviewed.as_file())
                    .unwrap()
                    .expect("test platform exposes file identity"),
            );
            request.source.installable = false;

            let result = run_job_file_bound(&request, target.as_file_mut(), &|| false, |_| {
                panic!("a replaced source must not produce write progress");
            });

            assert!(matches!(result, JobResult::WriteFailed(_)));
            assert_eq!(target.as_file_mut().stream_position().unwrap(), 7);
            assert_eq!(
                std::fs::read(target.path()).unwrap(),
                b"leave destination untouched"
            );
        }

        // Phase `catalog_digest_is_checked_during_the_write`.
        {
            let data = sample_bytes(4096);
            let src = tmp_with(&data);
            let dst = tmp_with(b"leave destination untouched");
            let wrong_digest = "0".repeat(64);

            let result = run_job_paths_bound(
                src.path(),
                dst.path(),
                data.len() as u64,
                SourceIdentity::from_file(src.as_file()).unwrap(),
                Some(&wrong_digest),
                &|| false,
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
    }

    #[test]
    fn handoff_apply_selects_root_partition_and_copies_onto_a_mounted_fixture() {
        // Phase `select_tilo_root_partition_prefers_child_linux_fs`.
        {
            let lsblk = "\
/dev/loop7\n\
/dev/loop7p1 vfat\n\
/dev/loop7p2 btrfs\n\
/dev/loop7p3 swap\n";
            assert_eq!(
                select_tilo_root_partition("/dev/loop7", lsblk).as_deref(),
                Some("/dev/loop7p2")
            );
        }

        // Phase `select_tilo_root_partition_falls_back_to_whole_device`.
        {
            let lsblk = "/dev/loop9 ext4\n";
            assert_eq!(
                select_tilo_root_partition("/dev/loop9", lsblk).as_deref(),
                Some("/dev/loop9")
            );
            assert_eq!(
                select_tilo_root_partition("/dev/loop9", "/dev/loop9\n"),
                None
            );
        }

        // Phase `apply_handoff_stage_onto_mounted_root_copies_etc_and_var`.
        {
            let staged = tempfile::tempdir().unwrap();
            let conn = staged.path().join("etc/NetworkManager/system-connections");
            std::fs::create_dir_all(&conn).unwrap();
            std::fs::write(conn.join("tilo-setup.nmconnection"), b"ssid=studio\n").unwrap();
            std::fs::create_dir_all(staged.path().join("var/lib/tilo")).unwrap();
            std::fs::write(staged.path().join("var/lib/tilo/device.key"), b"secret").unwrap();

            let mnt = tempfile::tempdir().unwrap();
            let etc = mnt.path().join("ostree/deploy/tilo/deploy/abcd.0/etc");
            let var = mnt.path().join("ostree/deploy/tilo/var");
            std::fs::create_dir_all(&etc).unwrap();
            std::fs::create_dir_all(&var).unwrap();

            apply_handoff_stage_onto_mounted_root(mnt.path(), staged.path()).unwrap();
            assert_eq!(
                std::fs::read(
                    etc.join("NetworkManager/system-connections/tilo-setup.nmconnection")
                )
                .unwrap(),
                b"ssid=studio\n"
            );
            assert_eq!(
                std::fs::read(var.join("lib/tilo/device.key")).unwrap(),
                b"secret"
            );
        }
    }
}
