//! A session whose identity is intact but whose device certificate the clock
//! refuses: it starts, says so, and verifies the identity again when the
//! clock changes, then starts what a verified identity starts.
//!
//! Nothing here polls. The wait is the identity host's policy clock
//! (`tilo_identity_host::policy_change`): an absolute `CLOCK_REALTIME`
//! timerfd armed at the instant the certificate becomes valid and cancelled
//! by any step of the clock, plus inotify on the identity directories, so a
//! clock that network time steps forward, or a certificate written anew, ends
//! the wait at once.

use std::path::PathBuf;
use std::sync::Arc;

use tilo_identity_host::policy_change;
use tilo_user_identity::CertificateClockError;

use crate::session_runtime::{PendingStart, Wake};

/// What a verification of the existing identity found.
#[derive(Debug)]
enum Verification {
    /// The identity and its certificate verify at the clock's reading.
    Verified,
    /// The certificate is still outside its validity at the clock's reading.
    Clock(CertificateClockError),
    /// The stored identity is malformed or mismatched: owner recovery.
    Broken(String),
    /// Something that proves nothing about the identity (storage, keyring).
    Failed(String),
}

/// What [`IdentityClockWait::pump`] found this turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockWaitStep {
    /// Still waiting for the clock, or for a check to finish.
    Waiting,
    /// The identity verifies: start the session's services.
    Verified,
    /// The identity itself is broken: open owner recovery.
    NeedsRecovery,
}

/// The wait for the clock to admit this device's certificate.
pub struct IdentityClockWait {
    identity_dir: PathBuf,
    watch: policy_change::Watch,
    /// The policy epoch the last check (or the start) read.
    checked: u64,
    check: Option<PendingStart<Verification>>,
    wake: Wake,
}

impl IdentityClockWait {
    /// Wait for the clock to admit the certificate `refusal` describes, for
    /// the identity in `identity_dir`. `wake` wakes the compositor loop,
    /// which then calls [`Self::pump`].
    pub fn start(refusal: &CertificateClockError, identity_dir: PathBuf, wake: Wake) -> Self {
        let notify = Arc::clone(&wake);
        let watch = policy_change::Watch::new(move || notify());
        let checked = watch.arm();
        expect_clock(refusal);
        // Starts the identity directories' inotify watch: a certificate
        // written anew ends the wait as well.
        let _ = policy_change::identity_files_watched();
        Self {
            identity_dir,
            watch,
            checked,
            check: None,
            wake,
        }
    }

    /// Advance the wait: collect a finished check, or start one when the
    /// clock or the identity files changed since the last.
    pub fn pump(&mut self) -> ClockWaitStep {
        if let Some(check) = self.check.as_ref() {
            let Some(outcome) = check.poll() else {
                return ClockWaitStep::Waiting;
            };
            self.check = None;
            match outcome {
                Ok(Verification::Verified) => return ClockWaitStep::Verified,
                Ok(Verification::Clock(refusal)) => {
                    tracing::info!(
                        valid_from_ns = refusal.valid_from_ns,
                        clock_ns = refusal.clock_ns,
                        "this device's certificate still waits for the clock"
                    );
                    expect_clock(&refusal);
                }
                Ok(Verification::Broken(error)) => {
                    tracing::warn!(%error, "existing Tilo identity needs recovery; opening trusted recovery");
                    return ClockWaitStep::NeedsRecovery;
                }
                Ok(Verification::Failed(error)) | Err(error) => {
                    tracing::warn!(%error, "could not verify the identity after the clock changed; waiting for the next change");
                }
            }
        }
        if policy_change::epoch() == self.checked {
            return ClockWaitStep::Waiting;
        }
        // Armed before the check reads anything, so a change during it
        // wakes the loop for one more.
        self.checked = self.watch.arm();
        let identity_dir = self.identity_dir.clone();
        let job = move || Ok(verify(&identity_dir));
        self.check = Some(match crate::session_runtime::runtime() {
            Ok(runtime) => crate::session_runtime::start_blocking(
                runtime.handle(),
                Arc::clone(&self.wake),
                job,
            ),
            Err(error) => PendingStart::finished(Err(error)),
        });
        ClockWaitStep::Waiting
    }
}

/// Have the policy clock announce when the certificate becomes valid, or,
/// for an expired one that only a clock moved back can admit, keep its timer
/// armed so that any step of the clock is announced.
fn expect_clock(refusal: &CertificateClockError) {
    policy_change::wake_at(refusal.valid_at_ns().unwrap_or(u64::MAX));
}

fn verify(identity_dir: &std::path::Path) -> Verification {
    classify(crate::firstboot::verify_existing_collaboration_identity(
        identity_dir,
        &crate::firstboot::collaboration_identity_paths(),
    ))
}

fn classify<T>(result: std::io::Result<T>) -> Verification {
    match result {
        Ok(_) => Verification::Verified,
        Err(error) => {
            if let Some(refusal) = CertificateClockError::in_io(&error) {
                Verification::Clock(refusal.clone())
            } else if error.kind() == std::io::ErrorKind::InvalidData {
                Verification::Broken(error.to_string())
            } else {
                Verification::Failed(error.to_string())
            }
        }
    }
}

impl crate::shell::chrome_state::ChromeState {
    /// Show, or take back, that the identity waits for the clock: its
    /// certificate is outside its validity at the clock's reading. A state of
    /// its own, never first-use setup or a restore: the identity on disk is
    /// intact and becomes usable again when the clock is right.
    pub fn set_identity_waits_for_clock(&mut self, waits: bool) {
        match (waits, self.identity_clock_notice) {
            (true, None) => {
                let mut notice = tilo_desktop::notifications::Notification::new(
                    crate::shell::i18n::t("identity-clock-wrong-title"),
                    crate::shell::i18n::t("identity-clock-wrong-body"),
                )
                .with_severity(tilo_desktop::notifications::NotificationSeverity::Warning)
                .with_icon_kind(tilo_desktop::notifications::NotificationIconKind::Generic)
                .machine_wide();
                notice.dismiss_after = tilo_desktop::notifications::PERSISTENT_DISMISS;
                notice.bypass_dnd = true;
                self.identity_clock_notice = Some(self.push_notification_full(notice));
            }
            (false, Some(id)) => {
                self.identity_clock_notice = None;
                self.dismiss_notification(id);
            }
            _ => {}
        }
    }

    /// Whether the identity waits for the clock
    /// ([`Self::set_identity_waits_for_clock`]).
    pub fn identity_waits_for_clock(&self) -> bool {
        self.identity_clock_notice.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clock_refusal_keeps_waiting_and_only_a_broken_identity_asks_for_recovery() {
        let refusal = CertificateClockError {
            reason: tilo_user_identity::DeviceCertError::NotYetValid,
            valid_from_ns: 2_000,
            valid_until_ns: None,
            clock_ns: 1_000,
        };
        assert!(matches!(
            classify::<()>(Err(refusal.clone().into_io())),
            Verification::Clock(found) if found == refusal
        ));
        assert!(matches!(classify(Ok(())), Verification::Verified));
        assert!(matches!(
            classify::<()>(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "certified device key does not match the persisted device secret",
            ))),
            Verification::Broken(_)
        ));
        assert!(matches!(
            classify::<()>(Err(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied
            ))),
            Verification::Failed(_)
        ));
    }
}
