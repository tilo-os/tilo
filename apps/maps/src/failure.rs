//! What went wrong, said as what the person can do about it. The network's own words go to the log;
//! the classes are the ones a person acts on differently: no connection, a service in trouble, one
//! limiting this device.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Unreachable,
    TimedOut,
    TooMany,
    ServiceTrouble,
    Refused,
    Other,
}

impl Kind {
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

pub(crate) fn say(what: &str, error: &str) -> String {
    tracing::warn!(
        what,
        error,
        "maps: a request failed; the person is told what they can do about it"
    );
    // Load the catalogue on first use, on whatever thread asks, so the sentence is never a raw key.
    crate::CATALOG.ensure();
    let kind = Kind::of(error);
    let sentence = crate::CATALOG.t(kind.key());
    crate::CATALOG.t_args(
        "maps-failure",
        &[("what", t(what).into()), ("reason", sentence.into())],
    )
}

/// What a person was doing, for [`say`]: the catalogue key of the phrase.
pub(crate) const SEARCH: &str = "maps-what-search";
pub(crate) const ROUTE: &str = "maps-what-route";
pub(crate) const TRANSIT: &str = "maps-what-transit";
pub(crate) const NEARBY: &str = "maps-what-nearby";

pub(crate) fn t(key: &str) -> String {
    crate::CATALOG.ensure();
    crate::CATALOG.t(key)
}

pub(crate) fn t_args(key: &str, args: &[(&str, ui::i18n::ArgValue)]) -> String {
    crate::CATALOG.ensure();
    crate::CATALOG.t_args(key, args)
}

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
