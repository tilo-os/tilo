use super::wire;
#[cfg(test)]
use super::{HistoryEntryId, listed};
use tilo_lock::MutexRecover as _;

use crate::tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending, ReceiptStatus,
};
use crate::tilo_actions::{InvocationId, InvocationUpdate};

/// The workspace's history as a session serves it for a window whose resource is a test's
/// document: each of its changes, under its own id, named as its own history names its author, and
/// served again each time the session takes up a newer state of the document ([`Self::write`]).
pub struct ServedHistory {
    /// The feed as last read, under the sequence of that reading.
    feed: std::sync::Mutex<(u64, wire::Feed)>,
    /// Followers waiting for the next reading.
    waiting: std::sync::Mutex<Vec<crate::tilo_actions::remote::Completer<()>>>,
}

impl ServedHistory {
    fn new(doc: &automerge::AutoCommit) -> Self {
        Self {
            feed: std::sync::Mutex::new((1, read(doc))),
            waiting: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The session took up `doc` as it is now: every follower is served its history again.
    #[cfg(test)]
    pub(crate) fn write(&self, doc: &automerge::AutoCommit) {
        {
            let mut feed = self.feed.lock_recover();
            *feed = (feed.0 + 1, read(doc));
        }
        let waiting = std::mem::take(&mut *self.waiting.lock_recover());
        for waiter in waiting {
            waiter.complete(());
        }
    }
}

impl ActionProvider for ServedHistory {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::default()))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if request.target != wire::target(wire::FEED) {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        Pending::ready(Ok(InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Accepted,
        }))
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let (sequence, feed) = &*self.feed.lock_recover();
        Some(InvocationState {
            update: InvocationUpdate::Item {
                invocation: *id,
                sequence: *sequence,
                value: serde_json::to_value(feed).ok()?,
            },
            untrusted: false,
            left_at: None,
            link: None,
        })
    }

    fn next_change(&self, _caller: &Caller, _id: &InvocationId) -> Option<Pending<()>> {
        let (changed, next) = crate::tilo_actions::remote::pending();
        self.waiting.lock_recover().push(changed);
        Some(next)
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Ok(()))
    }
}

thread_local! {
    static SERVED: std::cell::RefCell<Option<std::sync::Arc<ServedHistory>>> =
        const { std::cell::RefCell::new(None) };
}

/// Serve the history of `doc` to every History this test opens: each test is a process of its
/// own, so the provider stays entered until it ends.
pub fn serve(doc: &automerge::AutoCommit) {
    let served = std::sync::Arc::new(ServedHistory::new(doc));
    SERVED.with(|current| *current.borrow_mut() = Some(served.clone()));
    std::mem::forget(crate::remote_actions::RemoteActions::enter(
        served,
        Caller::local_app("so.tilo.test"),
    ));
}

/// The session took up `doc` as it is now: what this thread serves ([`serve`]) answers every
/// History that follows it again.
#[cfg(test)]
pub fn sync(doc: &automerge::AutoCommit) {
    let served = SERVED.with(|current| current.borrow().clone());
    served.expect("a history is served").write(doc);
}

/// The workspace's history of `doc` as the session reads it: its changes, newest first, and the spans
/// of them its members named.
fn read(doc: &automerge::AutoCommit) -> wire::Feed {
    let mut doc = doc.clone();
    let source = crate::history_source::HistorySource::new();
    let entries = source
        .entries(&mut doc)
        .iter()
        .rev()
        .map(|entry| {
            let (red, green, blue) = entry.author.rgb();
            wire::Entry {
                id: wire::entry_hex(entry.id.as_bytes()),
                author: wire::Author {
                    name: entry.author.name.clone(),
                    rgb: [red, green, blue],
                    identity: entry.author.identity_short.clone(),
                    ..wire::Author::default()
                },
                at_ms: entry
                    .timestamp
                    .duration_since(crate::time::SystemTime::UNIX_EPOCH)
                    .map_or(0, |since| {
                        i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
                    }),
                summary: entry.summary.clone(),
                ..wire::Entry::default()
            }
        })
        .collect::<Vec<_>>();
    let groups = crate::history_source::groups::read_named_ranges(&doc, source.authors())
        .into_iter()
        .map(|named| wire::Group {
            id: named.id,
            label: named.label,
            start: wire::entry_hex(named.start.as_bytes()),
            end: named.end.map(|end| wire::entry_hex(end.as_bytes())),
            named_by: named.named_by.map(|author| author.name),
        })
        .collect();
    wire::Feed {
        total: entries.len(),
        entries,
        groups,
        ..wire::Feed::default()
    }
}

#[cfg(test)]
fn hex(byte: u8) -> String {
    wire::entry_hex(&[byte; 16])
}

#[cfg(test)]
fn entry(listed: u8, change: u8, name: &str, at_ms: i64) -> wire::Entry {
    wire::Entry {
        id: hex(listed),
        change: Some(hex(change)),
        author: wire::Author {
            name: name.into(),
            rgb: [10, 20, 30],
            identity: "ab12".into(),
            relation: wire::Relation::Person,
            ..wire::Author::default()
        },
        at_ms,
        summary: format!("{name} edited the note"),
        app: Some("Notes".into()),
        ..wire::Entry::default()
    }
}

tilo_test_env::scenario! {
    fn a_windows_history_lists_what_the_workspaces_history_serves;

    phase rows_carry_the_change_in_the_windows_resource_and_the_name_the_feed_gave;
    {
        let feed = wire::Feed {
            entries: vec![entry(2, 12, "Ada", 2_000), entry(1, 11, "Ben", 1_000)],
            groups: vec![wire::Group {
                id: "group".into(),
                label: "Draft".into(),
                start: hex(1),
                end: Some(hex(2)),
                named_by: Some("Ada".into()),
            }],
            total: 3,
            ..wire::Feed::default()
        };
        let listed = listed(&feed);
        let ids: Vec<HistoryEntryId> = listed.entries.iter().map(|entry| entry.id).collect();
        assert_eq!(
            ids,
            [HistoryEntryId::from_bytes([11; 16]), HistoryEntryId::from_bytes([12; 16])],
            "oldest first, each under its id in the window's own resource"
        );
        assert_eq!(listed.entries[1].author.name, "Ada");
        assert_eq!(listed.entries[1].author.rgb(), (10, 20, 30));
        assert_eq!(listed.entries[1].summary, "Ada edited the note");
        assert_eq!(listed.named.len(), 1);
        assert_eq!(listed.named[0].start, HistoryEntryId::from_bytes([11; 16]));
        assert_eq!(listed.named[0].end, Some(HistoryEntryId::from_bytes([12; 16])));
        assert!(listed.has_earlier, "the feed holds one change more than it listed");
    }
}
