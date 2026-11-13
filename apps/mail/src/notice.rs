//! What Mail says once in its window, in the person's words: why a request to
//! a mail server failed, why an attachment cannot leave, and how a chosen file
//! ended up. The words come from the app's catalogue, so a window in another
//! language says them there, and each carries the way it reads.

use ui::i18n::ArgValue;

use crate::i18n::CATALOG;
use crate::state::Notice;

/// What a failed engine request says to the person: what did not happen,
/// and why in their terms, never the protocol step that failed.
pub(crate) fn engine_error_text(context: &str, message: &str, account: &str) -> Notice {
    let lower = message.to_ascii_lowercase();
    let signed_out = lower.contains("authentication")
        || lower.contains("password")
        || lower.contains("credential")
        || lower.contains("oauth")
        || lower.contains("sign in");
    let offline = lower.contains("network unreachable");
    // The account a failure is about, when the request that failed still
    // names it: a late answer arrives after its account was forgotten.
    let named = !account.is_empty();
    let with_account = |key: &str| {
        crate::i18n::t_args(
            key,
            &[
                ("account", ArgValue::Text(account.to_owned())),
                ("error", ArgValue::Text(message.to_owned())),
            ],
        )
    };
    let plain =
        |key: &str| crate::i18n::t_args(key, &[("error", ArgValue::Text(message.to_owned()))]);
    match context {
        "connect" | "fetch" if signed_out => Notice::error(if named {
            with_account("mail-notice-sign-in-failed-account")
        } else {
            crate::i18n::t("mail-notice-sign-in-failed")
        }),
        "connect" | "fetch" if offline => Notice::warning(crate::i18n::t("mail-notice-offline")),
        "connect" | "fetch" => Notice::error(if named {
            with_account("mail-notice-check-failed-account")
        } else {
            plain("mail-notice-check-failed")
        }),
        "send" | "smtp" | crate::engine::SMTP_UNREACHED => {
            Notice::error(plain("mail-status-send-failed"))
        }
        "oauth" => Notice::error(plain("mail-notice-sign-in-unfinished")),
        _ => Notice::error(plain("mail-notice-server-failed")),
    }
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
    TooLarge(String),
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
            Self::TooLarge(file) => CATALOG.t_args(
                "mail-attachment-too-large-save",
                &[("file", ArgValue::Text(file.clone()))],
            ),
            Self::Changed => CATALOG.t("mail-attachment-changed"),
        }
    }
}
