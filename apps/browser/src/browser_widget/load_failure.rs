//! What a person reads when an address does not open: what happened and
//! what to do next, in their words. The engine's own message (a network
//! error name such as `ERR_NAME_NOT_RESOLVED`) goes to the log.

use super::http_fallback::error_name;
use crate::resource::CATALOG;

/// Why a typed address was not opened, before it reached the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AddressProblem {
    /// Nothing was typed.
    Empty,
    /// It is too long, holds control characters or does not parse.
    Invalid,
    /// It names a file, an internal page or another non-web scheme.
    NotWeb,
}

impl AddressProblem {
    /// What the person reads, and what an action that asked for it is told.
    pub(super) fn message(self) -> String {
        CATALOG.t(match self {
            Self::Empty => "browser-address-empty",
            Self::Invalid => "browser-address-invalid",
            Self::NotWeb => "browser-address-not-web",
        })
    }
}

/// The page's heading when an address does not open.
pub(super) fn address_failed_title() -> String {
    CATALOG.t("browser-address-failed")
}

/// The page's heading when a page does not load.
pub(super) fn page_failed_title() -> String {
    CATALOG.t("browser-page-failed")
}

/// What the person reads for a page that failed to load with the engine's
/// `error`. A message the engine wrote for people (its runtime missing or
/// not starting) is shown as it is.
pub(super) fn page_failed_message(error: &str) -> String {
    use ui::webview::chromium::runtime::{FAILED_MESSAGE, NOT_INSTALLED_MESSAGE};
    if error == FAILED_MESSAGE || error == NOT_INSTALLED_MESSAGE {
        return error.to_owned();
    }
    let name = error_name(error);
    let key = match name {
        "ERR_NAME_NOT_RESOLVED" | "ERR_NAME_RESOLUTION_FAILED" => "browser-page-failed-not-found",
        "ERR_INTERNET_DISCONNECTED" | "ERR_NETWORK_CHANGED" | "ERR_PROXY_CONNECTION_FAILED" => {
            "browser-page-failed-offline"
        }
        "ERR_CONNECTION_REFUSED"
        | "ERR_CONNECTION_RESET"
        | "ERR_CONNECTION_CLOSED"
        | "ERR_CONNECTION_FAILED"
        | "ERR_CONNECTION_TIMED_OUT"
        | "ERR_TIMED_OUT"
        | "ERR_EMPTY_RESPONSE"
        | "ERR_ADDRESS_UNREACHABLE" => "browser-page-failed-no-answer",
        "ERR_UNSAFE_PORT"
        | "ERR_BLOCKED_BY_CLIENT"
        | "ERR_BLOCKED_BY_ADMINISTRATOR"
        | "ERR_BLOCKED_BY_RESPONSE" => "browser-page-failed-blocked",
        name if name.starts_with("ERR_CERT_") || name.starts_with("ERR_SSL_") => {
            "browser-page-failed-not-secure"
        }
        _ => "browser-page-failed-retry",
    };
    CATALOG.t(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_page_says_what_happened_not_the_engine_error() {
        for (error, key) in [
            ("ERR_NAME_NOT_RESOLVED", "browser-page-failed-not-found"),
            (
                "net::ERR_INTERNET_DISCONNECTED",
                "browser-page-failed-offline",
            ),
            ("ERR_CONNECTION_REFUSED", "browser-page-failed-no-answer"),
            ("ERR_CERT_DATE_INVALID", "browser-page-failed-not-secure"),
            ("ERR_UNSAFE_PORT", "browser-page-failed-blocked"),
            ("Chromium engine exited", "browser-page-failed-retry"),
        ] {
            let message = page_failed_message(error);
            assert_eq!(message, CATALOG.t(key), "{error}");
            assert!(!message.contains("ERR_"), "{message}");
            assert!(
                !message.is_empty() && message != key,
                "{key} is in the catalogue"
            );
        }
        let missing = ui::webview::chromium::runtime::NOT_INSTALLED_MESSAGE;
        assert_eq!(page_failed_message(missing), missing);
        for problem in [
            AddressProblem::Empty,
            AddressProblem::Invalid,
            AddressProblem::NotWeb,
        ] {
            assert!(!problem.message().starts_with("browser-"), "{problem:?}");
        }
        assert!(!address_failed_title().starts_with("browser-"));
        assert!(!page_failed_title().starts_with("browser-"));
    }
}
