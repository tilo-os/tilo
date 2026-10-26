//! What went wrong, said to the person who asked.
//!
//! The network layer's errors are addressed to us: an `anyhow` chain with a
//! URL in it, `HTTP 503`, a socket that was refused. They are evidence, and
//! they go to the log; what a person reads is what they can do about it.
//!
//! The classes are the ones a person can act on differently — no connection,
//! a service that is having trouble, one that is limiting this device, and
//! the rest — so the sentence says which of those happened and not which
//! library reported it.

use ui::i18n::I18nManager;

/// The kinds of trouble worth telling apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Nothing reached the service: no connection, no name resolution.
    Unreachable,
    /// The service answered, slowly, or not at all.
    TimedOut,
    /// The service is limiting how often this device may ask.
    TooMany,
    /// The service itself is unwell.
    ServiceTrouble,
    /// Something the person's request said was not accepted.
    Refused,
    /// Anything else: still not the transport's own text.
    Other,
}

impl Kind {
    /// Which kind `error` (the transport's own words) is.
    pub(crate) fn of(error: &str) -> Self {
        let lowered = error.to_ascii_lowercase();
        let has = |needle: &str| lowered.contains(needle);
        if has("429") || has("too many requests") {
            Self::TooMany
        } else if has("timed out") || has("timeout") || has("deadline") {
            Self::TimedOut
        } else if has("connection refused")
            || has("connection reset")
            || has("dns")
            || has("resolve")
            || has("unreachable")
            || has("failed to connect")
            || has("network is down")
            || has("no route")
        {
            Self::Unreachable
        } else if has("http 5") || has("service unavailable") || has("bad gateway") {
            Self::ServiceTrouble
        } else if has("http 4") {
            Self::Refused
        } else {
            Self::Other
        }
    }

    /// The catalogue key this kind says.
    fn key(self) -> &'static str {
        match self {
            Self::Unreachable => "maps-failure-unreachable",
            Self::TimedOut => "maps-failure-timeout",
            Self::TooMany => "maps-failure-too-many",
            Self::ServiceTrouble => "maps-failure-service",
            Self::Refused => "maps-failure-refused",
            Self::Other => "maps-failure-other",
        }
    }
}

/// `error`, logged for us and said to the person as the sentence for what
/// happened: what was being done (`what`), and the kind of trouble.
pub(crate) fn say(what: &str, error: &str) -> String {
    tracing::warn!(
        what,
        error,
        "maps: a request failed; the person is told what they can do about it"
    );
    // The catalogue is loaded on first use on a thread no host loaded it on
    // (a test, a background reply), so the sentence is never a raw key.
    crate::CATALOG.ensure();
    let kind = Kind::of(error);
    let sentence = I18nManager::lookup_plain(kind.key());
    I18nManager::format(
        "maps-failure",
        &[("what", what.into()), ("reason", sentence.into())],
    )
}

/// What a person was doing, for [`say`].
pub(crate) const SEARCH: &str = "searching";
pub(crate) const ROUTE: &str = "planning the route";
pub(crate) const TRANSIT: &str = "looking up departures";
pub(crate) const NEARBY: &str = "loading nearby places";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_transports_own_words_are_read_not_shown() {
        for (error, kind) in [
            (
                "error sending request for url (https://router.project-osrm.org/route): connection refused",
                Kind::Unreachable,
            ),
            ("HTTP 503", Kind::ServiceTrouble),
            ("HTTP 429", Kind::TooMany),
            ("operation timed out", Kind::TimedOut),
            ("HTTP 400", Kind::Refused),
            ("something else entirely", Kind::Other),
        ] {
            assert_eq!(Kind::of(error), kind, "{error}");
        }
        // The sentence never carries the URL, the status or the library's name.
        let said = say(
            SEARCH,
            "error sending request for url (https://router.project-osrm.org/route): connection refused",
        );
        assert!(!said.contains("https://"), "{said}");
        assert!(!said.contains("router.project-osrm.org"), "{said}");
        assert!(!said.contains("connection refused"), "{said}");
        assert!(said.contains("searching"), "{said}");
    }
}

/// Look up `key`, with the catalogue loaded on a thread no host loaded it on.
pub(crate) fn t(key: &str) -> String {
    crate::CATALOG.ensure();
    I18nManager::lookup_plain(key)
}

/// Look up `key`, interpolating `args`.
pub(crate) fn t_args(key: &str, args: &[(&str, ui::i18n::ArgValue)]) -> String {
    crate::CATALOG.ensure();
    I18nManager::format(key, args)
}
