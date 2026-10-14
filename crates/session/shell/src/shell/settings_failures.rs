//! What the shell does when a setting it changed could not be stored.
//!
//! The compositor thread's settings writes return before they reach the disk
//! (`settings_store::defer_writes_on_this_thread`). One that cannot be stored
//! is reverted by the settings worker, and nothing returns an error to the
//! caller that has gone on. The loop learns of it from one atomic load per
//! tick, writes a line to the log naming the sections and the disk's reason,
//! and tells the person once that a change did not stick.

use settings_store::DeferredFailure;

/// The log line for one failure: the sections it reverted and why.
#[must_use]
pub fn log_line(failure: &DeferredFailure) -> String {
    format!(
        "settings writes to {} were not stored and went back to their previous values: {}",
        failure.sections.join(", "),
        failure.error
    )
}

/// Report every failure: one log line each, and a single notice for all of
/// them, so a run of failing writes is one message, not one per write.
pub fn report(
    failures: &[DeferredFailure],
    mut log: impl FnMut(String),
    mut notify: impl FnMut(String, String),
) {
    if failures.is_empty() {
        return;
    }
    for failure in failures {
        log(log_line(failure));
    }
    notify(
        crate::shell::i18n::t("settings-not-saved-title"),
        crate::shell::i18n::t("settings-not-saved-body"),
    );
}

/// Take the failures the settings worker recorded and report them. Cheap when
/// there are none: one atomic load.
pub fn drain(chrome: &mut crate::shell::chrome_state::ChromeState) {
    if !settings_store::deferred_failure_pending() {
        return;
    }
    report(
        &settings_store::take_deferred_failures(),
        |line| tracing::warn!(target: "tilo::settings", "{line}"),
        |title, body| {
            chrome.push_error_notification(title, body);
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(sections: &[&str]) -> DeferredFailure {
        DeferredFailure {
            sections: sections.iter().map(|s| (*s).to_owned()).collect(),
            error: "No space left on device".to_owned(),
        }
    }

    #[test]
    fn a_failed_write_is_logged_once_and_the_person_told_once() {
        let mut lines = Vec::new();
        let mut notices = Vec::new();
        report(
            &[failure(&["appearance"])],
            |line| lines.push(line),
            |title, body| notices.push((title, body)),
        );
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("appearance") && lines[0].contains("No space left"));
        assert_eq!(notices.len(), 1);
        assert!(!notices[0].0.is_empty() && !notices[0].1.is_empty());
        assert_ne!(
            notices[0].0, "settings-not-saved-title",
            "the string is in the catalogue"
        );
    }

    #[test]
    fn several_failures_are_one_notice_and_nothing_failed_is_silent() {
        let mut lines = 0;
        let mut notices = 0;
        report(
            &[failure(&["appearance"]), failure(&["night_light", "power"])],
            |_| lines += 1,
            |_, _| notices += 1,
        );
        assert_eq!((lines, notices), (2, 1));
        report(&[], |_| lines += 1, |_, _| notices += 1);
        assert_eq!((lines, notices), (2, 1), "a second drain finds nothing");
    }
}
