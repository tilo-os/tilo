use super::*;
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
    pub(crate) fn write(&self, doc: &automerge::AutoCommit) {
        {
            let mut feed = self.feed.lock().unwrap();
            *feed = (feed.0 + 1, read(doc));
        }
        let waiting = std::mem::take(&mut *self.waiting.lock().unwrap());
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
        let (sequence, feed) = &*self.feed.lock().unwrap();
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
        self.waiting.lock().unwrap().push(changed);
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
pub fn sync(doc: &automerge::AutoCommit) {
    let served = SERVED.with(|current| current.borrow().clone());
    served.expect("a history is served").write(doc);
}

/// The workspace's history of `doc` as the session reads it: its changes, newest first, and the spans
/// of them its members named. Each change is served as the session serves it
/// (`desktop-surface`'s `serve::wire_entry`): what it did under `step`, the app and the resource as
/// a person names them, and its kind — the feed a window follows is read through exactly those
/// fields. A resource's genesis is left out, as the session's history source leaves it out: nobody
/// made it.
fn read(doc: &automerge::AutoCommit) -> wire::Feed {
    let mut doc = doc.clone();
    let source = crate::history_source::HistorySource::new().without_resource_genesis();
    let entries = source
        .entries(&mut doc)
        .iter()
        .rev()
        .map(|entry| {
            let (red, green, blue) = entry.author.rgb();
            let context = entry.context.as_ref();
            wire::Entry {
                id: wire::entry_hex(entry.id.as_bytes()),
                author: wire::Author {
                    name: entry.author.name.clone(),
                    rgb: [red, green, blue],
                    identity: entry.author.identity_short.clone(),
                    relation: match entry.author.relation {
                        AuthorRelation::Reader => wire::Relation::Reader,
                        AuthorRelation::Person => wire::Relation::Person,
                        AuthorRelation::Software => wire::Relation::Software,
                    },
                    on_behalf_of: entry
                        .author
                        .attribution
                        .as_ref()
                        .and_then(|who| who.on_behalf_of_name.clone()),
                    for_reader: entry.author.for_reader,
                    task: entry.author.task.clone(),
                },
                kind: match entry.entry_kind {
                    EntryKind::Text => wire::Kind::Text,
                    EntryKind::Move => wire::Kind::Move,
                    EntryKind::Details => wire::Kind::Details,
                    EntryKind::Comment => wire::Kind::Comment,
                    EntryKind::Delete => wire::Kind::Delete,
                    EntryKind::Restore => wire::Kind::Restore,
                    EntryKind::Access => wire::Kind::Access,
                    EntryKind::Compaction => wire::Kind::Compaction,
                    EntryKind::Redaction => wire::Kind::Redaction,
                },
                at_ms: entry
                    .timestamp
                    .duration_since(crate::time::SystemTime::UNIX_EPOCH)
                    .map_or(0, |since| {
                        i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
                    }),
                summary: entry.summary.clone(),
                step: context
                    .filter(|context| context.what.is_some())
                    .map(|context| crate::tilo_actions::ActionContext {
                        app: context.app.clone(),
                        resource: context.resource.clone(),
                        item: context.item.clone(),
                        what: context.what.clone(),
                        ..crate::tilo_actions::ActionContext::default()
                    }),
                app: context
                    .and_then(|context| context.app.as_ref())
                    .map(|app| app.label.clone())
                    .filter(|label| !label.is_empty()),
                resource: context
                    .and_then(|context| context.resource.as_ref())
                    .and_then(|resource| {
                        crate::history_source::detail::human_label(&resource.label, &resource.id)
                    })
                    .map(str::to_owned),
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

/// A change a window's History lists says what it did in the words of the step its action
/// recorded, names the app and the resource as a person reads them, and a resource's own genesis,
/// which nobody made, is not served to a window at all — the session's feed carries exactly these
/// fields ([`crate::session`]'s `serve::wire_entry`) and leaves the genesis out.
#[test]
fn a_served_change_carries_the_step_its_action_recorded_and_no_genesis() {
    use automerge::{AutoCommit, ROOT};

    use crate::resource::{DocumentCheckout, DocumentId, ResourceOwners, ResourceType};
    use crate::tilo_actions::{ActionKey, Described, Did};

    /// A fixed clock so the change's date is deterministic.
    const T0: i64 = 1_780_000_000;

    crate::i18n::I18nManager::init("en-US");
    crate::i18n::I18nManager::add_resource(
        "en-US",
        "tasks-rename = Renamed { $item } to { $title }\n".into(),
    );

    let cache = crate::portable_storage::PortableStorage::new("profile", "device").unwrap();
    let store = ResourceOwners::portable(cache)
        .open("test", "workspace")
        .unwrap();
    let checkout = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("issues"));
    store.create_resource(&checkout).unwrap();
    let mut doc = store.read(&checkout.id, AutoCommit::clone).unwrap();
    assert!(
        read(&doc).entries.is_empty(),
        "a resource's genesis names no one, so it is not served to a window"
    );

    let step = ActionContext::for_resource("i1", "tasks.issue", "Draft the plan")
        .with_app("so.tilo.tasks", "Tasks")
        .with_described(
            Described::new(Did::verb(
                ActionKey::new("so.tilo.tasks", "tasks.issues", "rename"),
                [("title".to_owned(), "Final plan".into())],
            ))
            .on("i1", "Draft the plan"),
        );
    // The change is typed text, as a person's edit is: a splice into a block, not a
    // metadata put, so the history classes it as text.
    let (c, mut ids) = {
        let c = tilo_doc::model::init(&mut doc, &ROOT);
        let ids = tilo_doc::IdGen::new(doc.get_actor().to_hex_string());
        (c, ids)
    };
    {
        let mut dm = tilo_doc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        let block = dm.insert_block(None, "paragraph");
        dm.splice_text(&block, 0, 0, "Draft the plan");
    }
    tilo_resource::history::commit_stamped_at_with_message(&mut doc, T0, step.change_message());

    let feed = read(&doc);
    assert_eq!(feed.entries.len(), 1, "the one change someone made");
    let served = &feed.entries[0];
    assert_eq!(
        served.app.as_deref(),
        Some("Tasks"),
        "the app as a person names it"
    );
    assert_eq!(served.resource.as_deref(), Some("Draft the plan"));
    assert_eq!(served.kind, wire::Kind::Text);
    assert_eq!(
        served.step.as_ref().and_then(|step| step.what.as_ref()),
        step.what.as_ref(),
        "what the change did, under the step the feed carries"
    );

    let id = wire::entry_id(&served.id).expect("a history id");
    let row = super::row(served, HistoryEntryId::from_bytes(id));
    assert_eq!(
        row.summary, "Renamed Draft the plan to Final plan",
        "read in the app's words, from the step the change recorded"
    );
}
