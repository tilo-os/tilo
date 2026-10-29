//! Setup's statements that other code quotes: the shell's tests and its
//! review renderer find a page by them.

use crate::t;

/// Title and body for the first screen.
#[must_use]
pub fn intro_title() -> String {
    t("onboarding-intro-title")
}

#[must_use]
pub fn intro_body() -> String {
    t("onboarding-intro-body")
}

/// The specific things the identity does today, without promising that every
/// application operation already carries a signature.
#[must_use]
pub fn intro_points() -> Vec<String> {
    vec![
        t("onboarding-intro-point-collaboration"),
        t("onboarding-intro-point-local"),
        t("onboarding-intro-point-not-account"),
    ]
}

#[must_use]
pub fn host_desktop_intro_title() -> String {
    t("onboarding-host-intro-title")
}

#[must_use]
pub fn host_desktop_intro_body() -> String {
    t("onboarding-host-intro-body")
}

/// Heading for the consequence screen — stated as a fact, not a warning to be
/// dismissed.
#[must_use]
pub fn no_recovery_title() -> String {
    t("onboarding-no-recovery-title")
}

/// The plain truth, without making people read an implementation tour before
/// they can reach the backup step.
#[must_use]
pub fn no_recovery_points() -> Vec<String> {
    vec![
        t("onboarding-no-recovery-point-no-service"),
        t("onboarding-no-recovery-point-backup"),
    ]
}

/// The acknowledgement, phrased as the consequence rather than "I agree" —
/// the person confirms they understood a fact, not accepted terms.
#[must_use]
pub fn acknowledgement() -> String {
    t("onboarding-acknowledgement")
}

#[must_use]
pub fn done_title() -> String {
    t("onboarding-done-title")
}

#[must_use]
pub fn done_body() -> String {
    t("onboarding-done-body")
}

#[must_use]
pub fn codecs_title() -> String {
    t("onboarding-codecs-title")
}

/// The one short line under the offer: where the formats come from, and that
/// the choice can be undone.
#[must_use]
pub fn codecs_note() -> String {
    t("onboarding-codecs-note")
}

#[must_use]
pub fn codecs_background() -> String {
    t("onboarding-codecs-background")
}
