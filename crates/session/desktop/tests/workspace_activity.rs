use automerge::{AutoCommit, ROOT, transaction::Transactable};
use tilo_actions::remote::workspace_activity as wire;
use tilo_desktop::activity::{ActivitySnapshot, WorkspaceActivityPane};
use tilo_desktop_model::projection::WORKSPACE_ID_KEY;
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};

/// The desktop of `workspace` alone, whose document is `doc`.
fn desktop(workspace: Workspace, doc: AutoCommit) -> ActivitySnapshot {
    ActivitySnapshot::new(
        Layout {
            active: workspace.id(),
            workspaces: vec![workspace],
            ..Default::default()
        },
        doc,
    )
    .unwrap()
}

/// The desktop of workspace "Design", whose document holds nothing yet.
fn design() -> ActivitySnapshot {
    let workspace = Workspace::new(WorkspaceId::from_u128(9), "Design");
    let mut doc = AutoCommit::new();
    doc.put(ROOT, WORKSPACE_ID_KEY, workspace.id().to_string())
        .unwrap();
    doc.commit();
    desktop(workspace, doc)
}

tilo_test_env::scenario! {
    fn workspace_activity_reads_the_admitted_workspace_document;

    phase workspace_activity_review_keeps_live_heads;
    // Removing causal replay or writing to the admitted document while reviewing must fail this
    // regression. The root schema is a real admitted workspace document.
    {
        let workspace = Workspace::new(WorkspaceId::from_u128(1), "Design");
        let mut doc = AutoCommit::new();
        doc.put(ROOT, WORKSPACE_ID_KEY, workspace.id().to_string())
            .unwrap();
        doc.put(ROOT, "name", "Design").unwrap();
        doc.commit();
        doc.put(ROOT, "name", "Design review").unwrap();
        doc.commit();
        let mut tree = desktop(workspace, doc);
        let before = tree.doc.get_heads();
        let mut pane = WorkspaceActivityPane::new();
        pane.begin_view(tree.active);
        pane.refresh(&mut tree);
        assert!(!pane.entries().is_empty(), "the rename is a feed change");
        // Reading the replay, like every review, is a read of the document.
        let replay = pane.serve_replay();
        assert!(!replay.pending, "the review read the history to its end");
        let filtered = pane.serve_feed(
            &wire::FeedQuery {
                author: Some("absent-author".into()),
                ..wire::FeedQuery::default()
            },
            &tree,
        );
        assert_eq!(filtered.total, 0, "a filter hides the rows nobody names");
        let all = pane.serve_feed(&wire::FeedQuery::default(), &tree);
        assert!(all.total > 0, "the unfiltered history lists the rename");
        pane.finish_view(tree.active);
        assert_eq!(tree.doc.get_heads(), before);
    }

    phase workspace_activity_names_workspace_events_and_leaves_bookkeeping_out;
    // A local desktop's workspace document, in the `contents` schema: its bookkeeping (the id counter,
    // the record index) listed as a change, a window opened left out of History, or any storage key
    // shown as a row's words, must fail this regression.
    {
        use automerge::{ObjType, transaction::CommitOptions};
        let workspace = Workspace::new(WorkspaceId::from_u128(3), "Reading group");
        let mut doc = AutoCommit::new();
        let stamp = |doc: &mut AutoCommit, at: i64| {
            doc.commit_with(CommitOptions::default().with_time(1_800_000_000 + at));
        };
        doc.put(ROOT, WORKSPACE_ID_KEY, workspace.id().to_string())
            .unwrap();
        doc.put(ROOT, "name", "Reading group").unwrap();
        doc.put(ROOT, "owner", "You").unwrap();
        doc.put(ROOT, "desktop_counter_v1", 3_u64).unwrap();
        let owners = doc
            .put_object(ROOT, "contents_owners_v1", ObjType::Map)
            .unwrap();
        doc.put(&owners, "0001", "device").unwrap();
        let contents = doc.put_object(ROOT, "contents", ObjType::Map).unwrap();
        let strip = doc
            .put_object(&contents, "0001000000000003", ObjType::Map)
            .unwrap();
        doc.put(&strip, "name", "The Odyssey").unwrap();
        doc.put(&strip, "order", 0_u64).unwrap();
        let groups = doc.put_object(&strip, "groups", ObjType::Map).unwrap();
        let open = |doc: &mut AutoCommit, group: &str, tab: &str, title: &str| {
            let group = doc.put_object(&groups, group, ObjType::Map).unwrap();
            doc.put(&group, "order", 0_u64).unwrap();
            let tabs = doc.put_object(&group, "tabs", ObjType::Map).unwrap();
            let tab = doc.put_object(&tabs, tab, ObjType::Map).unwrap();
            doc.put(&tab, "title", title).unwrap();
            doc.put(&tab, "order", 0_u64).unwrap();
        };
        open(
            &mut doc,
            "0001000000000001",
            "0001000000000002",
            "The Odyssey",
        );
        stamp(&mut doc, 0);
        doc.put(ROOT, "desktop_counter_v1", 5_u64).unwrap();
        let records = doc.put_object(ROOT, "records", ObjType::Map).unwrap();
        doc.put(&records, "uuid:notes", "word_processor.document")
            .unwrap();
        open(
            &mut doc,
            "0001000000000004",
            "0001000000000005",
            "Word Processor",
        );
        stamp(&mut doc, 1);
        doc.put(ROOT, "desktop_counter_v1", 6_u64).unwrap();
        doc.put(&records, "uuid:chat", "chat.workspace").unwrap();
        stamp(&mut doc, 2);
        doc.put(ROOT, "layout_hint_v2", "wide").unwrap();
        stamp(&mut doc, 3);
        doc.delete(&groups, "0001000000000004").unwrap();
        stamp(&mut doc, 4);
        let mut tree = desktop(workspace, doc);

        let mut pane = WorkspaceActivityPane::new();
        pane.refresh(&mut tree);
        let mut rows = pane
            .entries()
            .iter()
            .map(|row| (row.timestamp, row.summary.clone()))
            .collect::<Vec<_>>();
        rows.sort();
        let summaries = rows.into_iter().map(|(_, s)| s).collect::<Vec<_>>();
        assert_eq!(
            summaries.len(),
            3,
            "setup and bookkeeping stay out, a window opened is listed: {summaries:?}"
        );
        let [opened, updated, closed] = summaries.as_slice() else {
            return;
        };
        assert_eq!(opened, "Opened Word Processor");
        assert_eq!(updated, "Updated the workspace");
        assert_eq!(closed, "Closed 1 window");
        assert!(
            summaries.iter().all(|s| !s.starts_with("Created")),
            "the workspace's first write is not a change: {summaries:?}"
        );
        for summary in &summaries {
            assert!(
                !["_v1", "_v2", "contents", "counter", "records"]
                    .iter()
                    .any(|key| summary.contains(key)),
                "a storage key in a row: {summary}"
            );
        }
    }

    phase workspace_activity_detached_record_origins_do_not_collide;
    {
        use tilo_desktop::activity::*;
        use ui::resource::{DocumentCheckout, DocumentId, ResourceType};
        let mut tree = design();
        let mut resource = AutoCommit::new();
        resource.put(ROOT, "text", "shared causal hash").unwrap();
        resource.commit();
        let checkout = DocumentCheckout::new(DocumentId::for_tab(1), ResourceType::new("test"));
        let inputs = vec![
            DetachedRecordHistory {
                app: "app-a".into(),
                checkout: checkout.clone(),
                tree_chunks: Vec::new(),
                document: Some(resource.clone()),
            },
            DetachedRecordHistory {
                app: "app-b".into(),
                checkout: checkout.clone(),
                tree_chunks: Vec::new(),
                document: Some(resource),
            },
            DetachedRecordHistory {
                app: "missing".into(),
                checkout,
                tree_chunks: Vec::new(),
                document: None,
            },
        ];
        let mut pane = WorkspaceActivityPane::new();
        pane.refresh(&mut tree);
        pane.refresh_detached_records(&mut tree, inputs);
        let feed = pane.serve_feed(&wire::FeedQuery::default(), &tree);
        assert_eq!(feed.entries.len(), 2, "setup bookkeeping is not a feed row");
        assert_eq!(
            feed.entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            2,
            "the same causal hash in two apps is two rows"
        );
        let mut apps = Vec::new();
        for entry in &feed.entries {
            let detail = pane.serve_detail(
                &wire::DetailQuery {
                    range: wire::Range::one(&entry.id),
                    compare: true,
                },
                &mut tree,
            );
            if let Some(app) = detail
                .resources
                .first()
                .and_then(|resource| resource.app_id.clone())
            {
                apps.push(app);
                assert!(
                    detail.comparison.is_some(),
                    "a row compares what its change did"
                );
            }
        }
        apps.sort();
        assert_eq!(apps, ["app-a", "app-b"]);
    }

    phase workspace_activity_lists_content_and_filters_by_the_people_it_names;
    // Listing a read marker, or offering a filter for a member no row can be attributed to, must
    // fail this regression.
    {
        use automerge::{ActorId, transaction::CommitOptions};
        use tilo_actions::{ActionContext, ChangeKind};
        use tilo_desktop::activity::*;
        use ui::resource::{DocumentCheckout, DocumentId, ResourceType};
        let mut tree = design();
        let posted = ActionContext::for_resource("chat.channel:c", "chat.channel", "general")
            .with_app("so.tilo.chat", "Chat")
            .with_described(tilo_actions::Described::new(tilo_actions::Did::verb(
                tilo_actions::ActionKey::new("so.tilo.chat", "chat.conversation", "post-message"),
                [],
            )));
        let commit = |resource: &mut AutoCommit, context: &ActionContext, at: i64| {
            resource.commit_with(
                CommitOptions::default()
                    .with_time(at)
                    .with_message(context.change_message().unwrap()),
            );
        };
        let ada = ActorId::random();
        let mut resource = AutoCommit::new().with_actor(ada.clone());
        resource.put(ROOT, "message", "hello").unwrap();
        commit(&mut resource, &posted, 1_800_000_000);
        resource.put(ROOT, "read", 1).unwrap();
        let marker = ActionContext::for_resource("chat.channel:c", "chat.channel", "general")
            .with_change(ChangeKind::Housekeeping);
        commit(&mut resource, &marker, 1_800_000_001);
        resource.put(ROOT, "preview", "fetched").unwrap();
        commit(
            &mut resource,
            &posted.clone().with_change(ChangeKind::External),
            1_800_000_001,
        );
        resource.set_actor(ActorId::random());
        resource.put(ROOT, "message", "hi").unwrap();
        commit(&mut resource, &posted, 1_800_000_002);

        let mut pane = WorkspaceActivityPane::new();
        pane.authors_mut().register(
            &ada.to_hex_string(),
            ui::widget::HistoryAuthor::new("Ada", 120.0, "ada"),
        );
        pane.refresh(&mut tree);
        pane.refresh_detached_records(
            &mut tree,
            vec![DetachedRecordHistory {
                app: "so.tilo.chat".into(),
                checkout: DocumentCheckout::new(DocumentId::for_tab(1), ResourceType::new("chat")),
                tree_chunks: Vec::new(),
                document: Some(resource),
            }],
        );
        let feed = pane.serve_feed(&wire::FeedQuery::default(), &tree);
        let labels = feed
            .entries
            .iter()
            .map(|entry| entry.summary.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            ["Post message in general", "Post message in general"],
            "no read marker, nothing that came from outside"
        );
        assert!(
            feed.entries
                .iter()
                .any(|entry| entry.author.name == ui::history_source::UNKNOWN_AUTHOR_NAME),
            "a change history cannot name still shows under Everyone"
        );
        assert_eq!(feed.people, ["Ada"]);
    }

    phase workspace_activity_unsettled_refresh_retains_only_current_admission_rows;
    {
        use tilo_desktop::activity::*;
        use ui::resource::{DocumentCheckout, DocumentId, ResourceType};
        let mut tree = design();
        let mut resource = AutoCommit::new();
        resource.put(ROOT, "value", "settled").unwrap();
        resource.commit();
        let input = DetachedRecordHistory {
            app: "app".into(),
            checkout: DocumentCheckout::new(DocumentId::for_tab(1), ResourceType::new("test")),
            tree_chunks: Vec::new(),
            document: Some(resource),
        };
        let mut pane = WorkspaceActivityPane::new();
        pane.refresh(&mut tree);
        pane.refresh_detached_records(&mut tree, vec![input.clone()]);
        let settled = pane.entries().to_vec();
        pane.refresh_detached_records(
            &mut tree,
            vec![DetachedRecordHistory {
                tree_chunks: Vec::new(),
                document: None,
                ..input
            }],
        );
        assert_eq!(
            pane.entries(),
            settled,
            "a resource it could not read keeps the rows it read"
        );
        let feed = pane.serve_feed(&wire::FeedQuery::default(), &tree);
        assert_eq!(feed.entries.len(), settled.len());
        pane.refresh_detached_records(&mut tree, vec![]);
        assert_eq!(
            pane.entries().len(),
            0,
            "detached rows disappear and the workspace's first write is not a change"
        );
    }

    phase workspace_activity_reads_one_windows_resources;
    // A window's own history: Notes' text edits in the chain of pages it keeps beside its resource,
    // narrowed to one note when asked. Listing another app's change, another chain's, or naming a
    // person or lens of the workspace beyond them must fail this regression.
    {
        use automerge::{ActorId, transaction::CommitOptions};
        use tilo_actions::{ActionContext, ActionKey, Described, Did};
        use tilo_desktop::activity::*;
        use ui::resource::{DocumentCheckout, DocumentId, ResourceType};
        let mut tree = design();
        let did = |app: &str, kind: &str, action: &str, item: &str| {
            ActionContext::for_resource(kind, kind, kind)
                .with_app(app, app)
                .with_described(
                    Described::new(Did::verb(ActionKey::new(app, kind, action), [])).on(item, item),
                )
        };
        let resource = |actor: &ActorId, changes: &[(&str, ActionContext)]| {
            let mut resource = AutoCommit::new().with_actor(actor.clone());
            for (at, (text, context)) in (1_800_000_000..).zip(changes) {
                resource.put(ROOT, "text", *text).unwrap();
                resource.commit_with(
                    CommitOptions::default()
                        .with_time(at)
                        .with_message(context.change_message().unwrap()),
                );
            }
            resource
        };
        let (ada, bo) = (ActorId::random(), ActorId::random());
        let edit = |note: &str| did("so.tilo.notes", "notes.page", "edit-text", note);
        let pages = DocumentId::derived("pages", &DocumentId::workspace_default());
        let part = resource(&ada, &[("draft", edit("note-a")), ("plan", edit("note-b"))]);
        let elsewhere = resource(&ada, &[("other", edit("note-a"))]);
        let chat = resource(
            &bo,
            &[("hi", did("so.tilo.chat", "chat.conversation", "post-message", "general"))],
        );
        let mut pane = WorkspaceActivityPane::new();
        pane.authors_mut().register(
            &ada.to_hex_string(),
            ui::widget::HistoryAuthor::new("Ada", 120.0, "ada"),
        );
        pane.authors_mut().register(
            &bo.to_hex_string(),
            ui::widget::HistoryAuthor::new("Bo", 200.0, "bo"),
        );
        pane.refresh(&mut tree);
        pane.refresh_detached_records(
            &mut tree,
            vec![
                DetachedRecordHistory::new(
                    "so.tilo.notes",
                    DocumentCheckout::new(pages.part(0), ResourceType::new("notes.page")),
                    Some(part),
                ),
                DetachedRecordHistory::new(
                    "so.tilo.notes",
                    DocumentCheckout::new(
                        DocumentId::derived("pages", &DocumentId::for_tab(4)).part(0),
                        ResourceType::new("notes.page"),
                    ),
                    Some(elsewhere),
                ),
                DetachedRecordHistory::new(
                    "so.tilo.chat",
                    DocumentCheckout::new(pages.part(0), ResourceType::new("chat.conversation")),
                    Some(chat),
                ),
            ],
        );
        assert_eq!(pane.serve_feed(&wire::FeedQuery::default(), &tree).total, 4);
        let window = |item: Option<&str>| wire::FeedQuery {
            resources: Some(wire::WindowResources {
                app: "so.tilo.notes".into(),
                resources: vec![
                    DocumentId::workspace_default().as_str().into(),
                    pages.as_str().into(),
                ],
                item: item.map(Into::into),
            }),
            ..wire::FeedQuery::default()
        };
        let feed = pane.serve_feed(&window(None), &tree);
        assert_eq!(feed.total, 2, "the two edits in the window's chain: {:?}", feed.entries);
        assert!(
            feed.entries.iter().all(|entry| entry.author.name == "Ada"),
            "no change of Chat's, though it keeps a resource by the same id"
        );
        assert_eq!(feed.people, ["Ada"], "only who changed the window's resources");
        assert!(feed.lenses.is_empty(), "no lens onto the rest of the workspace");
        let note = pane.serve_feed(&window(Some("note-b")), &tree);
        assert_eq!(note.total, 1, "one note's history is its own edits");
        let all = pane.serve_feed(&wire::FeedQuery::default(), &tree);
        assert_eq!(all.total, 4, "the window's read leaves the workspace's as it was");
        assert_eq!(all.people.len(), 2);
    }

    phase workspace_activity_replay_starts_where_the_workspace_does;
    // Replay serving the workspace's first changes, made before the workspace stood in it, as
    // checkpoints with nothing to show must fail this regression.
    {
        use automerge::transaction::CommitOptions;
        use tilo_desktop_model::shared_resource::device;
        use tilo_desktop_model::{Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth};
        let workspace = Workspace::new(WorkspaceId::from_u128(4), "Planning");
        let mut doc = AutoCommit::new();
        let stamp = |doc: &mut AutoCommit, at: i64| {
            doc.commit_with(CommitOptions::default().with_time(1_800_000_000 + at));
        };
        doc.put(ROOT, WORKSPACE_ID_KEY, workspace.id().to_string())
            .unwrap();
        stamp(&mut doc, 0);
        doc.put(ROOT, "name", "Planning").unwrap();
        // Then a device lays the workspace out in its layout record, a window at a time.
        let mut layout = Workspace::new(workspace.id(), "Planning");
        let mut strip = Strip::new(StripId::from((1 << 48) | 3), "Planning");
        for (at, group, tab, title) in [
            (1, (1 << 48) | 1, (1 << 48) | 2, "Notes"),
            (2, (1 << 48) | 4, (1 << 48) | 5, "Sheets"),
        ] {
            strip.window_groups.push(WindowGroup {
                id: group,
                tabs: vec![Tab {
                    id: tab,
                    title: title.into(),
                    tile: TileContent::LocalApp {
                        app_id: format!("org.tilo.{title}"),
                        label: title.into(),
                        note: String::new(),
                    },
                    app_title: None,
                    view: String::new(),
                    pinned: false,
                }],
                active_tab: tab,
                width: WindowWidth::Wide,
                maximized: false,
                focus_history: Vec::new(),
            });
            layout.strips = vec![strip.clone()];
            device::write(
                &mut doc,
                workspace.id(),
                &layout,
                &device::Writer {
                    person: Some("ada"),
                    mine: &|id| id >> 48 == 1,
                    records: device::Writer::NOTHING_NOTED,
                },
            )
            .unwrap();
            stamp(&mut doc, at);
        }
        let mut tree = desktop(workspace, doc);
        let mut pane = WorkspaceActivityPane::new();
        pane.begin_view(tree.active);
        pane.refresh(&mut tree);
        let replay = pane.serve_replay();
        let first = replay
            .frames
            .first()
            .expect("the workspace's changes replay");
        assert!(
            !first.workspaces.is_empty(),
            "the first checkpoint shows the workspace: {:?}",
            replay
                .frames
                .iter()
                .map(|frame| &frame.summary)
                .collect::<Vec<_>>()
        );
    }
}
