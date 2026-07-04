//! The write / install seam.
//!
//! Writing an ISO or installing qOS is **destructive** and needs
//! privileged raw block-device access. This module is the single choke
//! point where that real work would happen — and in this development /
//! headless environment it deliberately does **not** touch any device.
//!
//! Two guarantees enforced here:
//!
//! 1. **Nothing is ever written automatically.** A job only begins after
//!    the UI has collected an explicit, typed confirmation (the operator
//!    must type the target device name). [`begin_job`] re-checks that the
//!    confirmation token matches the target before doing anything.
//!
//! 2. **The real `dd` / installer call is gated behind a worker seam.** In
//!    a production build [`WriteBackend::system`] would shell out to a
//!    privileged helper (`qos-imager-helper`, a setuid/polkit-mediated
//!    process). Here the default backend is [`WriteBackend::Simulated`],
//!    which only advances progress numbers — see the `TODO(real-write)`
//!    below for the exact integration point.

use crate::model::{Action, InstallProfile, Source, TargetDrive};

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
    /// Default everywhere except a privileged production build: advance
    /// progress numbers, write nothing.
    Simulated,
    /// Real raw write via the privileged helper. Selecting this is itself
    /// gated (see [`WriteBackend::system`]); it is never the default in a
    /// dev/headless build.
    System,
}

impl WriteBackend {
    /// Choose the backend for the running process. The real backend is
    /// only ever returned when an operator has explicitly opted in via the
    /// `QOS_IMAGER_ALLOW_WRITE=1` environment variable AND a privileged
    /// helper is present. In every other case (tests, `--shots`, normal
    /// dev runs) this is [`WriteBackend::Simulated`] so we cannot brick a
    /// device by accident.
    pub fn system() -> Self {
        let opted_in = std::env::var("QOS_IMAGER_ALLOW_WRITE").as_deref() == Ok("1");
        if opted_in && helper_present() {
            WriteBackend::System
        } else {
            WriteBackend::Simulated
        }
    }
}

/// Is the privileged write helper installed? (Looked up, never executed
/// here.)
fn helper_present() -> bool {
    std::path::Path::new("/usr/libexec/qos-imager-helper").exists()
}

/// A validated request to write `source` to `target` using `action`
/// (+ profile when installing). Construction does not start anything.
#[derive(Clone, Debug)]
pub struct WriteRequest {
    pub source: Source,
    pub target: TargetDrive,
    pub action: Action,
    pub profile: InstallProfile,
}

impl WriteRequest {
    /// Validate that the typed confirmation token matches the target's
    /// device name (e.g. the operator typed `sdb` for `/dev/sdb`). This is
    /// the explicit hold-to-confirm / typed gate required before any write.
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
/// Even when this returns `Started`, the actual bytes are NOT written by
/// this function — it validates and hands off to the backend. The simulated
/// backend (the only one active in dev/headless) advances progress and
/// writes nothing.
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

    match WriteBackend::system() {
        WriteBackend::Simulated => {
            // Progress is driven by the app's tick loop against the CRDT.
            // Nothing touches a device.
            JobStart::Started
        }
        WriteBackend::System => {
            // TODO(real-write): hand `req` to the privileged helper here.
            //
            //   1. Open the target read-write with O_DIRECT|O_EXCL after a
            //      final umount of every partition under `req.target`.
            //   2. For Action::Flash — stream `req.source.path` to the block
            //      device in 4 MiB chunks (the dd-style raw copy), fsync,
            //      then re-read and compare blake3 digests for the verify pass.
            //   3. For Action::Install — write the base image, then apply
            //      `req.profile`: set hostname, create the user, write the
            //      locale/keyboard/timezone, lay out the disk per
            //      `profile.layout`, and (if `profile.encrypt`) set up LUKS.
            //      A persistent stick additionally creates the overlay of
            //      `profile.persist_mb` MiB.
            //   4. Report byte-offset progress back over the helper socket so
            //      the same Progress UI drives both backends.
            //
            // This branch is intentionally unreachable in the dev/headless
            // build: WriteBackend::system() never returns System unless an
            // operator opted in *and* the helper binary exists.
            JobStart::Started
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{sample_source, sample_targets};

    fn req() -> WriteRequest {
        WriteRequest {
            source: sample_source(),
            target: sample_targets()[0].clone(), // the USB stick
            action: Action::Flash,
            profile: InstallProfile::default(),
        }
    }

    #[test]
    fn default_backend_is_simulated_in_dev() {
        // No opt-in env var set → must be simulated, never System.
        assert_eq!(WriteBackend::system(), WriteBackend::Simulated);
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
    fn begin_starts_with_correct_confirmation() {
        let r = req();
        assert_eq!(begin_job(&r, "sdb"), JobStart::Started);
    }
}
