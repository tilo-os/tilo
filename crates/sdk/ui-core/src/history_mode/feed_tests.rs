use super::wire;
#[cfg(test)]
use super::{HistoryEntryId, listed};
use tilo_lock::MutexRecover as _;

use crate::tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending, ReceiptStatus,
};
use crate::tilo_actions::{InvocationId, InvocationUpdate};

/// The workspace's history as a session serves it for a window whose resources are a test's
/// documents: each of their changes, under its own id, named as its own history names its author,
/// newest first and a page at a time, and served again each time the session takes up a newer state
/// of the documents ([`Self::write`]).
pub(crate) struct ServedHistory {
    /// Every change as last read, newest first, under the sequence of that reading.
    feed: std::sync::Mutex<(u64, wire::Feed)>,
    /// How many of the newest changes each follower asked for ([`wire::FeedQuery::page`]).
    pages: std::sync::Mutex<std::collections::HashMap<InvocationId, usize>>,
    /// Followers waiting for the next reading.
    waiting: std::sync::Mutex<Vec<crate::tilo_actions::remote::Completer<()>>>,
}

impl ServedHistory {
    fn new<'a>(docs: impl IntoIterator<Item = &'a automerge::AutoCommit>) -> Self {
        Self {
            feed: std::sync::Mutex::new((1, read(docs))),
            pages: std::sync::Mutex::default(),
            waiting: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The session took up `doc` as it is now: every follower is served its history again.
    #[cfg(test)]
    pub(crate) fn write(&self, doc: &automerge::AutoCommit) {
        {
            let mut feed = self.feed.lock_recover();
            *feed = (feed.0 + 1, read([doc]));
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
        let page = match &request.argument {
            crate::tilo_actions::remote::InvokeArgument::Json(query) => {
                serde_json::from_value::<wire::FeedQuery>(serde_json::Value::Object(query.clone()))
                    .map_or(wire::FEED_PAGE, |query| query.page())
            }
            _ => wire::FEED_PAGE,
        };
        self.pages.lock_recover().insert(request.invocation, page);
        Pending::ready(Ok(InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Accepted,
        }))
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let page = self
            .pages
            .lock_recover()
            .get(id)
            .copied()
            .unwrap_or(wire::FEED_PAGE);
        let (sequence, feed) = &*self.feed.lock_recover();
        let mut paged = feed.clone();
        paged.entries.truncate(page);
        Some(InvocationState {
            update: InvocationUpdate::Item {
                invocation: *id,
                sequence: *sequence,
                value: serde_json::to_value(paged).ok()?,
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
#[cfg(test)]
pub(crate) fn serve(doc: &automerge::AutoCommit) {
    serve_all([doc]);
}

/// Serve the history of `docs`, the resources a window keeps, to every History this test opens,
/// as [`serve`] serves one's.
pub(crate) fn serve_all<'a>(docs: impl IntoIterator<Item = &'a automerge::AutoCommit>) {
    let served = std::sync::Arc::new(ServedHistory::new(docs));
    SERVED.with(|current| *current.borrow_mut() = Some(served.clone()));
    std::mem::forget(crate::remote_actions::RemoteActions::enter(
        served,
        Caller::local_app("so.tilo.test"),
    ));
}

/// The session took up `doc` as it is now: what this thread serves ([`serve`]) answers every
/// History that follows it again.
#[cfg(test)]
pub(crate) fn sync(doc: &automerge::AutoCommit) {
    let served = SERVED.with(|current| current.borrow().clone());
    served.expect("a history is served").write(doc);
}

/// The workspace's history of `docs` as the session reads it: their changes, newest first, and the
/// spans of them their members named.
fn read<'a>(docs: impl IntoIterator<Item = &'a automerge::AutoCommit>) -> wire::Feed {
    let mut feed = wire::Feed::default();
    let mut several = false;
    for (at, doc) in docs.into_iter().enumerate() {
        let (entries, groups) = read_one(doc);
        feed.entries.extend(entries);
        feed.groups.extend(groups);
        several = at > 0;
    }
    // One document's changes are in the order they were made; several interleave by time.
    if several {
        feed.entries
            .sort_by_key(|entry| std::cmp::Reverse(entry.at_ms));
    }
    feed.total = feed.entries.len();
    feed
}

fn read_one(doc: &automerge::AutoCommit) -> (Vec<wire::Entry>, Vec<wire::Group>) {
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
    (entries, groups)
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

/// A change whose action recorded its step reads in this app's own words, and software names
/// whom it acted for.
#[test]
fn a_row_says_its_step_in_the_apps_words_and_whom_software_acted_for() {
    crate::i18n::I18nManager::init("en-US");
    crate::i18n::I18nManager::add_resource(
        "en-US",
        "tasks-rename = Renamed { $item } to { $title }\n".into(),
    );
    let step = crate::tilo_actions::ActionContext::for_resource("issues", "tasks.issues", "Issues")
        .with_app("so.tilo.tasks", "Tasks")
        .with_described(
            crate::tilo_actions::Described::new(crate::tilo_actions::Did::verb(
                crate::tilo_actions::ActionKey::new("so.tilo.tasks", "tasks.issues", "rename"),
                [("title".to_owned(), "Final plan".into())],
            ))
            .on("i1", "Draft the plan"),
        );
    let mut served = entry(1, 1, "Planner", 0);
    served.author.relation = wire::Relation::Software;
    served.author.on_behalf_of = Some("Ben".into());
    served.summary = "Rename: Draft the plan".into();
    served.step = Some(step);
    let row = super::row(&served, HistoryEntryId::from_bytes([1; 16]));
    assert_eq!(row.summary, "Renamed Draft the plan to Final plan");
    assert_eq!(row.author.name, "Planner");
    assert_eq!(
        crate::history_source::sentence::acted_for(&row.author).as_deref(),
        Some("for Ben")
    );
    assert_eq!(
        row.context
            .and_then(|context| context.app)
            .map(|app| app.label)
            .as_deref(),
        Some("Notes"),
        "the app as the session names it"
    );
}
