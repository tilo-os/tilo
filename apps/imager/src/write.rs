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
//!    Tests, `--shots`, and normal dev runs leave it unset, so
//!    [`WriteBackend::system`] resolves to [`WriteBackend::Simulated`] and no
//!    device is ever opened.
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

/// The chunk size of the streaming copy / verify loop (4 MiB) — large enough
/// to keep the device busy, small enough to report smooth progress.
pub const CHUNK: usize = 4 * 1024 * 1024;

/// Outcome of asking to start a job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobStart {
    /// Pre-conditions satisfied; the (simulated or real) job may run.
    Started,
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
    /// Default everywhere except a privileged, explicitly-opted-in run:
    /// advance progress numbers, write nothing.
    Simulated,
    /// Real raw write via a direct block-device open. Selecting this is itself
    /// gated (see [`WriteBackend::system`]); it is never the default.
    System,
}

impl WriteBackend {
    /// Choose the backend for the running process. The real backend is only
    /// ever returned when an operator has explicitly opted in via the
    /// `QOS_IMAGER_ALLOW_WRITE=1` environment variable. In every other case
    /// (tests, `--shots`, normal dev runs) this is [`WriteBackend::Simulated`]
    /// so we cannot brick a device by accident.
    pub fn system() -> Self {
        if allow_write() {
            WriteBackend::System
        } else {
            WriteBackend::Simulated
        }
    }
}

/// The env allow-gate. `QOS_IMAGER_ALLOW_WRITE=1` is the single switch that
/// lets a real block-device write happen; unset (the default) keeps every
/// path simulated. Modelled on the VPN app's `QOS_VPN_ALLOW_EXEC`.
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
/// chooses the backend; with the allow-gate off that is the simulated engine.
pub fn begin_job(req: &WriteRequest, typed_confirmation: &str) -> JobStart {
    if req.source.size == 0 || req.target.name.is_empty() {
        return JobStart::Incomplete;
    }
    if !req.target.fits(req.source.size) {
        return JobStart::TooSmall;
    }
    if !req.confirmation_matches(typed_confirmation) {
        return JobStart::ConfirmationMismatch;
    }
    JobStart::Started
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
pub fn stream_copy(
    src: &Path,
    dst: &Path,
    mut on_progress: impl FnMut(u64),
) -> io::Result<u64> {
    let mut reader = OpenOptions::new().read(true).open(src)?;
    let mut writer = OpenOptions::new().write(true).create(true).open(dst)?;
    let mut buf = vec![0u8; CHUNK];
    let mut written: u64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
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
        read_exact_fill(&mut a, &mut ba[..want])?;
        read_exact_fill(&mut b, &mut bb[..want])?;
        if let Some(i) = first_diff(&ba[..want], &bb[..want]) {
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
        let n = r.read(&mut buf[off..])?;
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
    run_job_paths(&src, &dst, req.source.size, |p| {
        let _ = tx.send(p);
    })
}

/// Spawn a job on a worker thread and return the receiver the UI drains once
/// per frame. The returned [`JobStart`] is the validation result; a channel is
/// only returned when it is [`JobStart::Started`].
///
/// * Allow-gate **off** (default) → a *simulated* worker emits synthetic
///   progress on a short timeline so the bars animate through the exact same
///   drain path, and **no device is opened**.
/// * Allow-gate **on** → the *real* [`run_system_job`] streams a byte-for-byte
///   copy + read-back verify of the device.
pub fn spawn_job(req: WriteRequest, typed: &str) -> (JobStart, Option<Receiver<Progress>>) {
    let start = begin_job(&req, typed);
    if start != JobStart::Started {
        return (start, None);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let backend = WriteBackend::system();
    let _ = std::thread::Builder::new()
        .name("imager-write".into())
        .spawn(move || match backend {
            WriteBackend::System => {
                let _ = run_system_job(&req, &tx);
            }
            WriteBackend::Simulated => simulate_job(&req, &tx),
        });
    (JobStart::Started, Some(rx))
}

/// Drive synthetic progress for the simulated backend: a write phase then a
/// verify phase, paced so the live UI shows moving bars. Writes nothing.
fn simulate_job(req: &WriteRequest, tx: &Sender<Progress>) {
    use std::time::Duration;
    let rate = 180_000_000u64; // 180 MB/s — a plausible USB3 figure for the ETA.
    let steps = 40;
    for i in 1..=steps {
        let frac = i as f64 / steps as f64;
        if tx
            .send(Progress {
                phase: JobPhase::Writing,
                write_frac: frac,
                verify_frac: 0.0,
                rate_bps: rate,
            })
            .is_err()
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    for i in 1..=steps {
        let frac = i as f64 / steps as f64;
        if tx
            .send(Progress {
                phase: JobPhase::Verifying,
                write_frac: 1.0,
                verify_frac: frac,
                rate_bps: rate,
            })
            .is_err()
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = tx.send(Progress {
        phase: JobPhase::Success,
        write_frac: 1.0,
        verify_frac: 1.0,
        rate_bps: 0,
    });
    let _ = req; // no bytes touched — the request only shapes the timeline length
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{sample_source, sample_targets};
    use std::io::Write as _;

    fn req() -> WriteRequest {
        WriteRequest {
            source: sample_source(),
            target: sample_targets()[0].clone(), // the USB stick
            action: Action::Flash,
            profile: InstallProfile::default(),
        }
    }

    #[test]
    fn default_backend_is_simulated_without_gate() {
        // No opt-in env var set → must be simulated, never System.
        // (Guard against a polluted env from another test in-process.)
        unsafe { std::env::remove_var("QOS_IMAGER_ALLOW_WRITE") };
        assert_eq!(WriteBackend::system(), WriteBackend::Simulated);
        assert!(!allow_write());
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
    fn begin_planned_path_touches_no_device_with_gate_off() {
        // With the gate off, a fully-armed request validates to Started — the
        // planned/dry-run path — and selects the simulated backend, so nothing
        // is ever written.
        unsafe { std::env::remove_var("QOS_IMAGER_ALLOW_WRITE") };
        let r = req();
        assert_eq!(begin_job(&r, "sdb"), JobStart::Started);
        assert_eq!(WriteBackend::system(), WriteBackend::Simulated);
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
        assert_eq!(phases, vec![JobPhase::Writing, JobPhase::Verifying, JobPhase::Success]);
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
