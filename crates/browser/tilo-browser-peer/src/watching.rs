//! Which records' channel keys a watching page asks the host for.
//!
//! A page that only watches a public room ([`crate::session::PeerConfig::watching`])
//! is no member of the room's channels: it never asks for access over a
//! channel, which would be a write the room refuses it. It takes a record's
//! content keys from the room's steward over HTTPS instead, when a sealed
//! document of the record arrives that its keys cannot open, or when the page
//! opens the record. One request per record is in flight at a time, and a
//! refused record is asked for again only after [`RETRY_MS`], so a private
//! room's stream of sealed documents costs one refusal per interval, not one
//! per document.

use std::collections::{BTreeMap, BTreeSet};

use crate::document::DocumentRoute;

/// How long a refused or failed record waits before it is asked for again.
pub(crate) const RETRY_MS: u64 = 30_000;

/// The records a watching page wants keys for, asks for, and was refused.
#[derive(Default)]
pub(crate) struct WatchedKeys {
    wanted: BTreeSet<DocumentRoute>,
    asking: BTreeSet<DocumentRoute>,
    refused_until: BTreeMap<DocumentRoute, u64>,
}

impl WatchedKeys {
    /// Want `route`'s keys: a sealed document of it is waiting for them.
    pub(crate) fn want(&mut self, route: DocumentRoute) {
        self.wanted.insert(route);
    }

    /// The wanted records to ask for at `now_ms`, now marked in flight:
    /// only those the room holds a seat for this page in (`seated`), as the
    /// steward hands keys to a seated spectator only.
    pub(crate) fn due(
        &mut self,
        now_ms: u64,
        seated: impl Fn(&DocumentRoute) -> bool,
    ) -> Vec<DocumentRoute> {
        self.refused_until.retain(|_, until| *until > now_ms);
        let due: Vec<DocumentRoute> = self
            .wanted
            .iter()
            .filter(|route| {
                seated(route)
                    && !self.asking.contains(*route)
                    && !self.refused_until.contains_key(*route)
            })
            .cloned()
            .collect();
        for route in &due {
            self.wanted.remove(route);
            self.asking.insert(route.clone());
        }
        due
    }

    /// `route`'s request was answered at `now_ms`: with its keys, or not.
    pub(crate) fn answered(&mut self, route: &DocumentRoute, granted: bool, now_ms: u64) {
        self.asking.remove(route);
        if !granted {
            self.refused_until
                .insert(route.clone(), now_ms.saturating_add(RETRY_MS));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(record: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: "so.tilo.chat".into(),
            cluster_id: "room".into(),
            record: record.into(),
        }
    }

    #[test]
    fn a_record_is_asked_for_once_at_a_time_and_a_refusal_waits() {
        let mut keys = WatchedKeys::default();
        keys.want(route("a"));
        keys.want(route("a"));
        assert!(keys.due(0, |_| false).is_empty(), "no seat yet");
        assert_eq!(keys.due(0, |_| true), vec![route("a")]);
        // More sealed documents while the request is out ask nothing more.
        keys.want(route("a"));
        assert!(keys.due(1, |_| true).is_empty());
        keys.answered(&route("a"), false, 1);
        assert!(keys.due(2, |_| true).is_empty(), "a refused record waits");
        assert_eq!(keys.due(1 + RETRY_MS, |_| true), vec![route("a")]);
        keys.answered(&route("a"), true, 1 + RETRY_MS);
        // A newer epoch's document asks again at once.
        keys.want(route("a"));
        keys.want(route("b"));
        assert_eq!(
            keys.due(2 + RETRY_MS, |_| true),
            vec![route("a"), route("b")]
        );
    }
}
