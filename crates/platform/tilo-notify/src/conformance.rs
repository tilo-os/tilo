//! The contract every [`Notifier`] keeps, as checks a backend's tests (and a
//! host's own backend's tests) run, plus a [`Recording`] notifier.
//!
//! The checks send only requests the contract says must fail before the
//! platform sees them, so they run headless against the real system
//! backends without showing anything. They panic with the violated rule.
#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "a conformance check fails by panicking with the violated rule"
)]

use std::sync::Mutex;

use crate::{Feature, Notification, Notifier, NotifyError, Scheduled, SystemTime, availability};

/// Malformed requests are refused as [`NotifyError::Invalid`] (or the host is
/// [`NotifyError::Unavailable`] altogether) before the platform sees them.
pub fn check_contract(notifier: &dyn Notifier) {
    for (label, malformed) in [
        ("an empty tag", Notification::new("", "Title", "Body")),
        ("an empty title", Notification::new("tag", " ", "Body")),
        (
            "a control character",
            Notification::new("t\nag", "Title", ""),
        ),
    ] {
        assert!(
            matches!(
                notifier.show(&malformed),
                Err(NotifyError::Invalid(_) | NotifyError::Unavailable)
            ),
            "{label} must be refused before the platform"
        );
    }
    assert!(
        matches!(
            notifier.withdraw(""),
            Err(NotifyError::Invalid(_) | NotifyError::Unavailable)
        ),
        "withdrawing an empty tag must be refused"
    );
    assert!(
        notifier
            .schedule(&Notification::new("", "Title", ""), SystemTime::UNIX_EPOCH)
            .is_err(),
        "a malformed schedule must be refused"
    );
}

/// [`check_contract`] for this target's [`system`](crate::system) backend,
/// which also answers `Unsupported` for scheduling exactly where the
/// target's column says so.
pub fn check_system(notifier: &dyn Notifier) {
    check_contract(notifier);
    let unsupported = matches!(
        availability(Feature::Schedule),
        crate::Availability::Unsupported(_)
    );
    match notifier.schedule(&Notification::new("", "Title", ""), SystemTime::UNIX_EPOCH) {
        Err(NotifyError::Unsupported(_)) => {}
        Err(NotifyError::Invalid(_) | NotifyError::Unavailable) => assert!(
            !unsupported,
            "this target cannot schedule, so it must say Unsupported"
        ),
        other => panic!("a malformed schedule must be refused, got {other:?}"),
    }
}

/// A notifier that records what it was asked, for tests and for hosts that
/// observe an app without letting it reach the desktop.
#[derive(Default)]
pub struct Recording {
    log: Mutex<RecordingLog>,
}

#[derive(Default)]
struct RecordingLog {
    shown: Vec<Notification>,
    withdrawn: Vec<String>,
    scheduled: Vec<(Notification, SystemTime)>,
}

impl Recording {
    /// Every notification shown, in order.
    pub fn shown(&self) -> Vec<Notification> {
        self.log
            .lock()
            .map(|log| log.shown.clone())
            .unwrap_or_default()
    }

    /// Every tag withdrawn, in order.
    pub fn withdrawn(&self) -> Vec<String> {
        self.log
            .lock()
            .map(|log| log.withdrawn.clone())
            .unwrap_or_default()
    }
}

impl Notifier for Recording {
    fn show(&self, notification: &Notification) -> Result<(), NotifyError> {
        crate::validate(notification)?;
        if let Ok(mut log) = self.log.lock() {
            log.shown.push(notification.clone());
        }
        Ok(())
    }

    fn withdraw(&self, tag: &str) -> Result<(), NotifyError> {
        crate::validate_tag(tag)?;
        if let Ok(mut log) = self.log.lock() {
            log.withdrawn.push(tag.to_owned());
        }
        Ok(())
    }

    fn schedule(&self, notification: &Notification, at: SystemTime) -> Result<(), NotifyError> {
        crate::validate(notification)?;
        if let Ok(mut log) = self.log.lock() {
            log.scheduled
                .retain(|(held, _)| held.tag != notification.tag);
            log.scheduled.push((notification.clone(), at));
        }
        Ok(())
    }

    fn scheduled(&self) -> Result<Vec<Scheduled>, NotifyError> {
        Ok(self
            .log
            .lock()
            .map(|log| {
                log.scheduled
                    .iter()
                    .map(|(notification, at)| Scheduled {
                        tag: notification.tag.clone(),
                        at: *at,
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    fn cancel_scheduled(&self, tag: &str) -> Result<(), NotifyError> {
        if let Ok(mut log) = self.log.lock() {
            log.scheduled.retain(|(held, _)| held.tag != tag);
        }
        Ok(())
    }
}
