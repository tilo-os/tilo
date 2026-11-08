//! The wait for the clock to admit this device's certificate, in front of the desk: the identity on
//! disk is intact and its certificate is outside its validity at the clock's reading, so the screen
//! says what the clock must reach and goes away by itself once it does. This is never first-use
//! setup or a restore: nothing here replaces an identity, and the desk takes the wait's place as
//! soon as the clock admits the certificate.
//! Nothing here polls. The wait is the identity host's policy clock
//! (`tilo_identity_host::policy_change`): an absolute `CLOCK_REALTIME` timerfd armed at the instant
//! the certificate becomes valid and cancelled by any step of the clock, plus inotify on the
//! identity directories, so a clock that network time steps forward, or a certificate written anew,
//! ends the wait at once. The identity is verified again on a worker, which wakes the session's
//! loop with what it found.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::task::Waker;

use tilo_identity_host::policy_change;
use tilo_onboarding_host::identity::IdentityProblem;
use tilo_onboarding_host::view::OnboardingView;
use tilo_user_identity::{CertificateClockError, DeviceCertError};
use ui::format::UnixSecs;
use ui::icons::Icon;
use ui::widget::{Container, EmptyState};

use crate::i18n::{t, t_args};
use crate::startup::{self, Failure};

/// What a verification of the identity found once the wait looked again.
#[derive(Debug)]
enum Check {
    /// The identity and its certificate verify at the clock's reading.
    Verified,
    /// The certificate is still outside its validity at the clock's reading.
    Clock(CertificateClockError),
    /// The stored identity no longer verifies: owner recovery.
    Recovery(IdentityProblem),
    /// Something that proves nothing about the identity (a store or keyring this account cannot
    /// read): try again at the next change.
    Failed(String),
}

/// What [`ClockWait::pump`] found this pass.
pub(crate) enum ClockWaitStep {
    /// Still waiting for the clock, or for a check to finish.
    Waiting,
    /// The identity verifies: `Some` is the setup this device still needs,
    /// `None` opens the desk.
    Verified(Option<OnboardingView>),
    /// The identity stopped verifying: its owner restores it.
    Recovery(OnboardingView),
}

/// The wait for the clock to admit this device's certificate.
pub struct ClockWait {
    /// The directory the identity this device holds lives in.
    identity_dir: PathBuf,
    /// Whether this session is an operating system Tilo does not control,
    /// which chooses the setup its identity is recovered through.
    host_desktop: bool,
    /// What the clock refused, and the certificate it read.
    refusal: CertificateClockError,
    /// Wakes the session's loop when the clock, the identity's files or a finished check change
    /// what the wait answers.
    waiter: Waker,
    /// The identity host's announcements, kept for as long as the wait runs.
    watch: Option<policy_change::Watch>,
    /// The policy epoch the last check (or the start) read.
    checked: u64,
    /// Where [`Self::pump`] learns that a check finished.
    check: Option<Receiver<Check>>,
    /// The desk takes over: the identity verified and no setup is due.
    done: bool,
}

impl ClockWait {
    /// Wait for the clock to admit `refusal` for the identity in
    /// `identity_dir`. Nothing listens yet: [`Self::listening`] arms the wait,
    /// which happens as the surface showing it is mounted.
    pub(crate) fn new(
        identity_dir: PathBuf,
        host_desktop: bool,
        refusal: CertificateClockError,
    ) -> Self {
        Self {
            identity_dir,
            host_desktop,
            refusal,
            waiter: Waker::noop().clone(),
            watch: None,
            checked: 0,
            check: None,
            done: false,
        }
    }

    /// Arm the wait: the policy clock announces the instant the certificate becomes valid, or any
    /// step of the clock, and the identity directories announce a certificate written anew.
    /// `waiter` wakes the session's loop for each of them.
    pub(crate) fn listening(mut self, waiter: Waker) -> Self {
        let waking = waiter.clone();
        let watch = policy_change::Watch::new(move || waking.wake_by_ref());
        self.checked = watch.arm();
        // An expired certificate has no instant a forward-running clock reaches: the timer is armed
        // as far out as it goes, so only a step of the clock ends that wait.
        policy_change::wake_at(self.refusal.valid_at_ns().unwrap_or(u64::MAX));
        // Starts the identity directories' inotify watch: a certificate written anew ends the wait
        // as well.
        let _ = policy_change::identity_files_watched();
        self.waiter = waiter;
        self.watch = Some(watch);
        self
    }

    /// Whether the desk takes the wait's place.
    pub(crate) fn done(&self) -> bool {
        self.done
    }

    /// Advance the wait: collect a finished check, or start one when the clock or the identity's
    /// files changed since the last.
    pub(crate) fn pump(&mut self) -> ClockWaitStep {
        let Some(watch) = self.watch.as_ref() else {
            return ClockWaitStep::Waiting;
        };
        let answered = match self.check.as_ref().map(Receiver::try_recv) {
            None => None,
            Some(Ok(outcome)) => Some(outcome),
            Some(Err(TryRecvError::Empty)) => return ClockWaitStep::Waiting,
            Some(Err(TryRecvError::Disconnected)) => {
                Some(Check::Failed("the check did not answer".to_owned()))
            }
        };
        if let Some(outcome) = answered {
            self.check = None;
            match outcome {
                Check::Verified => {
                    return ClockWaitStep::Verified(startup::setup_due_now(self.host_desktop));
                }
                Check::Clock(refusal) => {
                    tracing::info!(
                        valid_from_ns = refusal.valid_from_ns,
                        clock_ns = refusal.clock_ns,
                        "this device's certificate still waits for the clock"
                    );
                    policy_change::wake_at(refusal.valid_at_ns().unwrap_or(u64::MAX));
                }
                Check::Recovery(problem) => {
                    tracing::warn!(
                        problem = problem.as_str(),
                        "existing Tilo identity needs recovery; opening trusted recovery"
                    );
                    return ClockWaitStep::Recovery(recovery_view(
                        &self.identity_dir,
                        self.host_desktop,
                    ));
                }
                Check::Failed(said) => {
                    tracing::warn!(
                        %said,
                        "could not verify the identity after the clock changed; waiting for \
                         the next change"
                    );
                }
            }
        }
        if policy_change::epoch() == self.checked {
            return ClockWaitStep::Waiting;
        }
        // Armed before the check reads anything, so a change during it wakes the loop for one more.
        self.checked = watch.arm();
        self.start_check();
        ClockWaitStep::Waiting
    }

    /// Verify the identity on a worker and wake the loop with what it found.
    fn start_check(&mut self) {
        let identity_dir = self.identity_dir.clone();
        let waiter = self.waiter.clone();
        let (sender, check) = channel();
        let spawned = std::thread::Builder::new()
            .name("tilo-identity-clock-wait".into())
            .spawn(move || {
                let outcome = verify(&identity_dir);
                // The receiver may be gone with the session: nothing to tell.
                let _ = sender.send(outcome);
                waiter.wake_by_ref();
            });
        match spawned {
            Ok(_) => self.check = Some(check),
            Err(error) => {
                tracing::warn!(%error, "could not verify the identity after the clock changed");
            }
        }
    }

    /// Show the desk in the wait's place.
    pub(crate) fn finish(&mut self) {
        self.done = true;
    }

    /// What the wait shows: what the clock reads, what the certificate needs instead, and that the
    /// desk opens by itself.
    pub(crate) fn screen(&self) -> Container {
        let now = ui::format::timestamp(&UnixSecs(secs(self.refusal.clock_ns)));
        let body = match self.refusal.reason {
            DeviceCertError::NotYetValid => {
                let valid = ui::format::timestamp(&UnixSecs(secs(self.refusal.valid_from_ns)));
                t_args(
                    "identity-clock-wait-not-yet-valid",
                    &[("now", now.into()), ("valid", valid.into())],
                )
            }
            DeviceCertError::Expired => {
                let until = ui::format::timestamp(&UnixSecs(secs(
                    self.refusal.valid_until_ns.unwrap_or(self.refusal.clock_ns),
                )));
                t_args(
                    "identity-clock-wait-expired",
                    &[("now", now.into()), ("until", until.into())],
                )
            }
            _ => t("identity-clock-wait-body"),
        };
        Container::new()
            .filling()
            .with_background()
            .with_centered_max_width(560.0)
            .with_child(Box::new(
                EmptyState::new(t("identity-clock-wait-title"))
                    .with_message(body)
                    .with_icon(Icon::CircleAlert),
            ))
    }
}

/// The setup an identity that stopped verifying is restored through, for a session on Tilo's own
/// image or on an operating system it does not control.
fn recovery_view(identity_dir: &std::path::Path, host_desktop: bool) -> OnboardingView {
    if host_desktop {
        OnboardingView::new_for_host_desktop_recovery(identity_dir.to_path_buf())
    } else {
        OnboardingView::new_for_recovery(identity_dir.to_path_buf())
    }
}

/// Verify the identity `identity_dir` holds and settle what a verified one leaves behind.
fn verify(identity_dir: &std::path::Path) -> Check {
    match startup::verify_now(identity_dir) {
        Ok(()) => match startup::settle(identity_dir) {
            Ok(()) => Check::Verified,
            Err(said) => Check::Failed(said),
        },
        Err(error) => match startup::failure(&error) {
            Failure::WaitForClock(refusal) => Check::Clock(refusal),
            Failure::Recovery(problem) => Check::Recovery(problem),
            Failure::Refused => Check::Failed(error.to_string()),
        },
    }
}

/// `unix_ns` as the seconds a formatted instant is read from.
fn secs(unix_ns: u64) -> i64 {
    i64::try_from(unix_ns / 1_000_000_000).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(reason: DeviceCertError) -> CertificateClockError {
        CertificateClockError {
            reason,
            valid_from_ns: 2_000_000_000,
            valid_until_ns: Some(3_000_000_000),
            clock_ns: 1_000_000_000,
        }
    }

    /// A wait with nothing to listen to still shows why the desk is off, and does not end on its
    /// own.
    #[test]
    fn the_wait_holds_the_desk_until_the_clock_is_admitted() {
        let mut wait = ClockWait::new(
            PathBuf::from("/nonexistent"),
            false,
            refusal(DeviceCertError::NotYetValid),
        );
        assert!(!wait.done(), "the desk waits for the clock");
        assert!(matches!(wait.pump(), ClockWaitStep::Waiting));
        assert!(!wait.done(), "nothing verified the identity");
        wait.finish();
        assert!(wait.done(), "the desk takes the wait's place");
    }

    /// A certificate the clock reads as not yet valid becomes valid when the clock reaches its
    /// instant, and one it reads as expired only when the clock is moved back.
    #[test]
    fn only_a_forward_running_clock_admits_a_certificate_dated_ahead() {
        assert_eq!(
            refusal(DeviceCertError::NotYetValid).valid_at_ns(),
            Some(2_000_000_000)
        );
        assert_eq!(
            refusal(DeviceCertError::Expired).valid_at_ns(),
            None,
            "an expired certificate has no instant a forward-running clock reaches"
        );
    }
}
