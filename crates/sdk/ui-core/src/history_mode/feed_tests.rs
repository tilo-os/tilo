use super::*;
use crate::tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending, ReceiptStatus,
};
use crate::tilo_actions::{ActionOutput, InvocationId};

/// The workspace's history as a session serves it for a window whose resource is `doc`: each of
/// its changes, under its own id, named as its own history names its author.
struct DocumentHistory(wire::Feed);

impl ActionProvider for DocumentHistory {
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
            status: ReceiptStatus::Completed {
                output: ActionOutput {
                    value: serde_json::to_value(&self.0).ok(),
                    resources: Vec::new(),
                },
            },
        }))
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Ok(()))
    }
}

/// Serve the history of `doc` to every History this test opens: each test is a process of its
/// own, so the provider stays entered until it ends.
pub(crate) fn serve(doc: &automerge::AutoCommit) {
    std::mem::forget(serving(doc));
}

/// Serve the history of `doc` to every History this thread opens while the guard lives.
pub(crate) fn serving(doc: &automerge::AutoCommit) -> crate::remote_actions::RemoteActionsGuard {
    let mut doc = doc.clone();
    let entries = crate::history_source::HistorySource::new()
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
    let feed = wire::Feed {
        total: entries.len(),
        entries,
        ..wire::Feed::default()
    };
    crate::remote_actions::RemoteActions::enter(
        std::sync::Arc::new(DocumentHistory(feed)),
        Caller::local_app("so.tilo.test"),
    )
}

fn hex(byte: u8) -> String {
    wire::entry_hex(&[byte; 16])
}

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
