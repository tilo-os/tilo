//! What Mail says once in its window, in the person's words: why a request to
//! a mail server failed, why an attachment cannot leave, and how a chosen file
//! ended up. The words come from the app's catalogue, so a window in another
//! language says them there, and each carries the way it reads.

use ui::i18n::ArgValue;
use ui::widget::BannerKind;

use crate::i18n::CATALOG;
use crate::state::{Intent, Notice, SyncCause, SyncFailure};

/// What the list says about an account whose check failed, and it says it for
/// as long as the failure lasts: what cannot be reached, what that means, and
/// the one control that helps.
pub(crate) struct SyncFailureText {
    pub kind: BannerKind,
    pub title: String,
    pub message: Option<String>,
    /// The control beside the words: check again, or open the account.
    pub action: Option<SyncOffer>,
}

/// The way out of a failed check: a word, and what pressing it asks the app
/// for.
pub(crate) struct SyncOffer {
    pub label: String,
    pub intent: Intent,
}

/// The account's failure in the window's language.
pub(crate) fn sync_failure(failure: &SyncFailure) -> SyncFailureText {
    let account = ArgValue::Text(failure.account.clone());
    match failure.cause {
        // Nothing reached the server: the account has no password here yet.
        SyncCause::NoPassword => SyncFailureText {
            kind: BannerKind::Warning,
            title: CATALOG.t_args("mail-sync-no-password", &[("account", account)]),
            message: Some(CATALOG.t("mail-sync-no-password-why")),
            action: Some(SyncOffer {
                label: CATALOG.t("mail-account-settings"),
                intent: Intent::AccountSettings,
            }),
        },
        // The session said why it kept the password back, in words made for the person.
        SyncCause::PasswordWithheld => SyncFailureText {
            kind: BannerKind::Warning,
            title: CATALOG.t_args("mail-sync-password-withheld", &[("account", account)]),
            message: failure.detail.clone(),
            action: Some(SyncOffer {
                label: CATALOG.t("mail-account-settings"),
                intent: Intent::AccountSettings,
            }),
        },
        // Somebody has to change the account, so the words lead there.
        SyncCause::SignedOut => SyncFailureText {
            kind: BannerKind::Warning,
            title: CATALOG.t_args("mail-sync-signed-out", &[("account", account)]),
            message: Some(CATALOG.t("mail-sync-signed-out-why")),
            action: Some(SyncOffer {
                label: CATALOG.t("mail-account-settings"),
                intent: Intent::AccountSettings,
            }),
        },
        // Nothing about the account is wrong, and Mail keeps trying: the
        // words say the state and offer nothing to press.
        SyncCause::Offline => SyncFailureText {
            kind: BannerKind::Warning,
            title: CATALOG.t("mail-sync-offline"),
            message: Some(CATALOG.t("mail-sync-offline-why")),
            action: None,
        },
        // Nothing is wrong with the account: what can reach its server is
        // not on, so the words say what the window shows meanwhile and what
        // brings new mail.
        SyncCause::NoDevice => SyncFailureText {
            kind: BannerKind::Warning,
            title: CATALOG.t_args("mail-sync-no-device", &[("account", account)]),
            message: Some(CATALOG.t("mail-sync-no-device-why")),
            action: Some(SyncOffer {
                label: CATALOG.t("mail-sync-check-again"),
                intent: Intent::CheckMail,
            }),
        },
        // Why it could not be reached (no such host, an untrusted certificate, no answer) is
        // what the person acts on.
        SyncCause::Unreachable => SyncFailureText {
            kind: BannerKind::Error,
            title: CATALOG.t_args("mail-sync-unreachable", &[("account", account)]),
            message: failure.detail.clone(),
            action: Some(SyncOffer {
                label: CATALOG.t("mail-sync-check-again"),
                intent: Intent::CheckMail,
            }),
        },
    }
}

/// What stopped an account's check, from the words the engine failed with. Only a login the
/// server refused, or an OAuth grant it no longer takes, is the account's sign-in: a failure
/// before the login (no password here, a server that could not be reached or trusted) is said
/// as what it is.
pub(crate) fn sync_cause(message: &str) -> SyncCause {
    let lower = message.to_ascii_lowercase();
    if message.contains(mail_engine::engine::NO_PASSWORD) {
        SyncCause::NoPassword
    } else if mail_engine::validate::is_auth_failure(&lower) || lower.contains("oauth") {
        SyncCause::SignedOut
    } else if lower.contains("network unreachable") {
        SyncCause::Offline
    } else {
        SyncCause::Unreachable
    }
}

/// What a failed engine request says to the person: what did not happen, and
/// why in their terms, never the protocol step that failed. A check that fails
/// is not here: the list says it for as long as it lasts ([`sync_failure`]).
pub(crate) fn engine_error_text(context: &str, message: &str) -> Notice {
    let plain =
        |key: &str| crate::i18n::t_args(key, &[("error", ArgValue::Text(message.to_owned()))]);
    match context {
        "send" | "smtp" | mail_engine::engine::SMTP_UNREACHED => {
            Notice::error(plain("mail-status-send-failed"))
        }
        "oauth" => Notice::error(plain("mail-notice-sign-in-unfinished")),
        _ => Notice::error(plain("mail-notice-server-failed")),
    }
}

/// What Mail says when a message that left has no copy in its account's
/// Sent folder: that it was sent, and why its server kept none.
pub(crate) fn unfiled_notice(reason: String) -> Notice {
    Notice::warning(CATALOG.t_args(
        "mail-status-sent-unfiled",
        &[("error", ArgValue::Text(reason))],
    ))
}

/// How staging a chosen attachment ended, in the window's words.
pub(crate) fn staged_notice(name: String, result: Result<(), String>) -> Notice {
    let name = ArgValue::Text(name);
    match result {
        Ok(()) => Notice::success(CATALOG.t_args("mail-status-attached", &[("name", name)])),
        Err(error) => Notice::error(CATALOG.t_args(
            "mail-status-attach-failed",
            &[("name", name), ("error", ArgValue::Text(error))],
        )),
    }
}

/// Why an attachment cannot leave Mail. Each cause names itself from the
/// catalogue, so a German window says it in German whether it lands in the
/// window's notice or in the answer an agent reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AttachmentUnavailable {
    /// The message id its caller held is not a message id at all.
    UnknownMessage,
    /// The message, or that attachment of it, is not held here.
    NotLoaded,
    NotDownloaded,
    /// Kept on this device, but its bytes were freed since.
    NotKept(String),
    /// The bytes no longer match what the message said they were.
    Changed,
}

impl AttachmentUnavailable {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::UnknownMessage => CATALOG.t("mail-attachment-unknown-message"),
            Self::NotLoaded => CATALOG.t("mail-attachment-not-loaded"),
            Self::NotDownloaded => CATALOG.t("mail-attachment-not-downloaded"),
            Self::NotKept(file) => CATALOG.t_args(
                "mail-attachment-not-kept",
                &[("file", ArgValue::Text(file.clone()))],
            ),
            Self::Changed => CATALOG.t("mail-attachment-changed"),
        }
    }
}

#[cfg(test)]
mod cause_tests {
    use super::sync_cause;
    use crate::state::SyncCause;

    /// Only a login the server refused sends the person to the account's password: a failure
    /// before any login is said as what it is.
    #[test]
    fn a_failure_before_the_login_is_not_called_a_wrong_password() {
        assert_eq!(
            sync_cause(mail_engine::engine::NO_PASSWORD),
            SyncCause::NoPassword
        );
        for before_the_login in [
            "TLS/certificate error — the server's certificate wasn't trusted",
            "host not found — check the server address",
            "connection timed out — the server didn't respond",
            "connection failed: the system's trusted certificates are unreadable",
            "this imported account needs its sign-in before it can connect",
        ] {
            assert_eq!(
                sync_cause(before_the_login),
                SyncCause::Unreachable,
                "{before_the_login}"
            );
        }
        assert_eq!(
            sync_cause("authentication failed — check the username and password"),
            SyncCause::SignedOut
        );
        assert_eq!(
            sync_cause("the OAuth sign-in is no longer valid; sign in again"),
            SyncCause::SignedOut
        );
    }
}
