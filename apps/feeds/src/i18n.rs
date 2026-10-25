//! Feeds' own copy: the words the reader shows, through the toolkit's Fluent
//! catalogues.
//!
//! A subscription that failed to refresh says why in its sidebar row, so the
//! reason has to be a sentence a person reads in their own language rather
//! than the transport's words. The catalogue is embedded with `include_str!`,
//! so a live window, a `--shots` capture and a unit test resolve the same
//! strings with no file I/O.

use ui::i18n::ArgValue;

use crate::fetch::RefreshFailure;

ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

/// One named variable for [`t_args`], as `(name, value)`.
pub fn arg(name: &str, value: impl Into<ArgValue>) -> (&str, ArgValue) {
    (name, value.into())
}

/// Why a subscription's last refresh failed, as its row says it: short enough
/// for a row's caption line, and naming the site's refusal as a refusal rather
/// than as a connection problem.
pub fn failure_sentence(reason: RefreshFailure) -> String {
    match reason {
        RefreshFailure::Unreachable => t("feeds-refresh-unreachable"),
        RefreshFailure::Refused { status: None } => t("feeds-refresh-refused"),
        RefreshFailure::Refused {
            status: Some(status),
        } => t_args(
            "feeds-refresh-refused-status",
            &[arg("status", u32::from(status))],
        ),
        RefreshFailure::NotAFeed => t("feeds-refresh-not-a-feed"),
        RefreshFailure::Untrusted => t("feeds-refresh-untrusted"),
        RefreshFailure::Unavailable => t("feeds-refresh-unavailable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_is_not_a_network_error() {
        let refused = failure_sentence(RefreshFailure::Refused { status: Some(403) });
        let unreachable = failure_sentence(RefreshFailure::Unreachable);
        assert_eq!(refused, "The site refused the request (HTTP 403)");
        assert_eq!(unreachable, "Couldn't reach the site");
        assert_ne!(refused, unreachable);
        // A gemini server refuses without an HTTP code.
        assert_eq!(
            failure_sentence(RefreshFailure::Refused { status: None }),
            "The site refused the request"
        );
    }

    #[test]
    fn every_reason_has_its_own_honest_sentence() {
        let all = [
            RefreshFailure::Unreachable,
            RefreshFailure::Refused { status: Some(404) },
            RefreshFailure::Refused { status: None },
            RefreshFailure::NotAFeed,
            RefreshFailure::Untrusted,
            RefreshFailure::Unavailable,
        ];
        let mut said: Vec<String> = all.iter().map(|reason| failure_sentence(*reason)).collect();
        // No key leaked unresolved, and no two reasons read the same.
        assert!(said.iter().all(|sentence| !sentence.contains("feeds-")));
        said.sort();
        let count = said.len();
        said.dedup();
        assert_eq!(said.len(), count);
    }
}
