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
//! 2. **An env allow-gate.** Even with a matching confirmation, the *real*
//!    block-device backend is selected only when
//!    `QOS_IMAGER_ALLOW_WRITE=1` is set (mirroring the VPN app's
//!    `QOS_VPN_ALLOW_EXEC` precedent) **and** the target is a real device.
//!    Tests, `--shots`, and normal dev runs leave it unset, so starting a write
//!    is refused and no device is opened.
//!
//! 3. **Capacity + fit checks** ([`WriteRequest`] / [`TargetDrive::fits`]).
//!
//! The engine is exercised in tests against **temp files** (a real copy +
//! verify, byte-for-byte) — never a block device — so the production loop is
//! proven without any risk to hardware.

use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};

use crate::model::{Action, InstallProfile, JobPhase, Progress, Source, TargetDrive};
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
}

/// Which engine actually performs the write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteBackend {
    /// Raw-device effects are unavailable in this process.
    Disabled,
    /// Real raw write via a direct block-device open. Selecting this is itself
    /// gated (see [`WriteBackend::system`]); it is never the default.
    System,
}

impl WriteBackend {
    /// Choose the backend for the running process. The real backend is only
    /// ever returned when an operator has explicitly opted in via the
    /// `QOS_IMAGER_ALLOW_WRITE=1` environment variable. In every other case
    /// (tests, `--shots`, normal dev runs) this is [`WriteBackend::Disabled`].
    pub fn system() -> Self {
        if allow_write() {
            WriteBackend::System
        } else {
            WriteBackend::Disabled
        }
    }
}

/// The env allow-gate. `QOS_IMAGER_ALLOW_WRITE=1` is the single switch that
/// lets a real block-device write happen; unset (the default) keeps every
/// path unavailable. Modelled on the VPN app's `QOS_VPN_ALLOW_EXEC`.
pub fn allow_write() -> bool {
    std::env::var("QOS_IMAGER_ALLOW_WRITE").as_deref() == Ok("1")
}

/// A validated request to write `source` to `target` using `action`
/// (+ profile when pre-configuring). Construction does not start anything.
#[derive(Clone, Debug)]
pub struct WriteRequest {
    pub source: Source,
    pub target: TargetDrive,
    pub action: Action,
    pub profile: InstallProfile,
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
    let required = if req.source.installable {
        req.source.size.saturating_add(PROFILE_BLOCK_SIZE as u64)
    } else {
        req.source.size
    };
    if !req.target.fits(required) {
        return JobStart::TooSmall;
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
    let mut reader = OpenOptions::new().read(true).open(src)?;
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
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        // `n` is guaranteed <= buf.len() by the `Read::read` contract, but a
        // block-device write is exactly the place to fail loudly rather than
        // trust that contract silently.
        let chunk = buf
            .get(..n)
            .ok_or_else(|| io::Error::other("read() returned a length past the buffer end"))?;
        writer.write_all(chunk)?;
        written += n as u64;
        on_progress(written);
    }
    writer.flush()?;
    writer.sync_all()?;
    Ok(written)
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
    let total = total.max(1);
    // Write phase.
    let copied = stream_copy(src, dst, |bytes| {
        on_progress(Progress {
            phase: JobPhase::Writing,
            write_frac: (bytes as f64 / total as f64).min(1.0),
            verify_frac: 0.0,
            rate_bps: 0,
        });
    });
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
        });
    });
    match verify {
        Ok(None) => {
            on_progress(Progress {
                phase: JobPhase::Success,
                write_frac: 1.0,
                verify_frac: 1.0,
                rate_bps: 0,
            });
            JobResult::Verified
        }
        Ok(Some(off)) => JobResult::VerifyMismatch(off),
        Err(e) => JobResult::VerifyFailed(e.to_string()),
    }
}

/// Run the real job for `req` (source file → target device), streaming
/// [`Progress`] over `tx`. Only ever called on a worker thread when the
/// allow-gate selected [`WriteBackend::System`].
fn run_system_job(req: &WriteRequest, tx: &Sender<Progress>) -> JobResult {
    let src = std::path::PathBuf::from(&req.source.path);
    let dst = std::path::PathBuf::from(req.target.dev_path());
    if let Err(error) = revalidate_target(&req.target) {
        return JobResult::WriteFailed(format!("target changed since confirmation: {error}"));
    }
    if let Err(error) = prepare_target(&req.target) {
        return JobResult::WriteFailed(format!("could not safely unmount target: {error}"));
    }
    let mut result = run_job_paths(&src, &dst, req.source.size, |p| {
        let _ = tx.send(p);
    });
    if result.is_success()
        && req.source.installable
        && let Err(error) = write_profile_block(&dst, req.target.size, &req.profile)
    {
        result = JobResult::VerifyFailed(format!(
            "could not embed and verify installation profile: {error}"
        ));
        let _ = tx.send(Progress {
            phase: JobPhase::Failed,
            write_frac: 1.0,
            verify_frac: 1.0,
            rate_bps: 0,
        });
    }
    finish_target(&req.target);
    result
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
    status.success().then_some(()).ok_or_else(|| {
        io::Error::other("Windows could not take the disk offline; run qOS Imager as Administrator")
    })
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
/// * Allow-gate **off** (default) → the request is refused and no worker is
///   created.
/// * Allow-gate **on** → the *real* [`run_system_job`] streams a byte-for-byte
///   copy + read-back verify of the device.
pub fn spawn_job(req: WriteRequest, typed: &str) -> (JobStart, Option<Receiver<Progress>>) {
    let start = begin_job(&req, typed);
    if start != JobStart::Started {
        return (start, None);
    }
    if WriteBackend::system() == WriteBackend::Disabled {
        return (JobStart::WriteDisabled, None);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("imager-write".into())
        .spawn(move || {
            let _ = run_system_job(&req, &tx);
        });
    (JobStart::Started, Some(rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{sample_source, sample_targets};
    use std::sync::Mutex;

    /// Serialises the tests that read/write `QOS_IMAGER_ALLOW_WRITE`, since the
    /// process environment is shared across the parallel test threads.
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
    fn default_backend_is_disabled_without_gate() {
        let _g = ENV_LOCK.lock().unwrap();
        // No opt-in env var set → unavailable, never a fake success.
        unsafe { std::env::remove_var("QOS_IMAGER_ALLOW_WRITE") };
        assert_eq!(WriteBackend::system(), WriteBackend::Disabled);
        assert!(!allow_write());
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
        assert_eq!(WriteBackend::system(), WriteBackend::Disabled);
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
    fn spawn_refuses_instead_of_simulating_with_gate_off() {
        let _g = ENV_LOCK.lock().unwrap();
        // Validation remains pure, but the live start refuses rather than
        // reporting synthetic progress and success.
        unsafe { std::env::remove_var("QOS_IMAGER_ALLOW_WRITE") };
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
        // It really wrote the bytes and drove both phases to completion.
        assert_eq!(std::fs::read(dst.path()).unwrap(), data);
        assert_eq!(
            phases,
            vec![JobPhase::Writing, JobPhase::Verifying, JobPhase::Success]
        );
        assert!((last.write_frac - 1.0).abs() < 1e-9);
        assert!((last.verify_frac - 1.0).abs() < 1e-9);
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
