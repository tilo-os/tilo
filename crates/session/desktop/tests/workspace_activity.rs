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
            ["Ran “Post message” in general", "Ran “Post message” in general"],
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

    phase workspace_activity_lists_a_change_after_what_it_builds_on_in_the_same_second;
    // A record's changes a second apart or less tie on their clock. Ordering the tie by anything
    // but the record's own causal order (its change hashes, say) lists an append below the note it
    // appended to, newest first, for about half of all pairs: each pair here must fail that.
    // Each reads in the words Notes publishes for what its action did, as the session learns them
    // from the apps' published actions: never the action's id, nor its name quoted.
    {
        use automerge::{ActorId, transaction::CommitOptions};
        use tilo_actions::published::{Offer, PublishedAction};
        use tilo_actions::{ActionContext, ActionKey, Described, Did};
        use tilo_desktop::activity::*;
        use ui::resource::{DocumentCheckout, DocumentId, ResourceType};
        let publish = |action: &str, label: &str, did: &str| {
            let mut published = PublishedAction::offered(
                &Offer {
                    resource: "notes.pages",
                    resource_kind: "notes.pages",
                    action,
                    label,
                    description: None,
                    kind: tilo_actions::ActionKind::Undoable,
                    operation: tilo_actions::OperationKind::Document,
                    confirm: false,
                    asks: None,
                    keywords: Vec::new(),
                    icon: None,
                },
                "Notes",
            );
            published.did = Some(did.to_owned());
            published
        };
        ui::history_source::sentence::learn_published(
            "so.tilo.notes",
            &[
                publish("new_note", "New note", "created the note"),
                publish("append-to-note", "Append to note", "added to the note"),
            ],
        );
        // As Notes records a verb: its kind and its declared id, the app the entry's.
        let step = |verb: &str| {
            ActionContext::for_resource("notes.pages", "notes.pages", "Notes")
                .with_app("so.tilo.notes", "Notes")
                .with_described(
                    Described::new(Did::verb(ActionKey::new("", "notes.pages", verb), []))
                        .on("notes.note:spring", "Spring plans"),
                )
        };
        for seed in 0..8_u8 {
            let mut tree = design();
            let mut note = AutoCommit::new().with_actor(ActorId::from([seed; 16].as_slice()));
            for (verb, text) in [("new_note", "Spring plans"), ("append_to_note", "- book the trip")] {
                note.put(ROOT, "text", text).unwrap();
                note.commit_with(
                    CommitOptions::default()
                        .with_time(1_800_000_000)
                        .with_message(step(verb).change_message().unwrap()),
                );
            }
            let mut pane = WorkspaceActivityPane::new();
            pane.refresh(&mut tree);
            pane.refresh_detached_records(
                &mut tree,
                vec![DetachedRecordHistory {
                    app: "so.tilo.notes".into(),
                    checkout: DocumentCheckout::new(
                        DocumentId::for_tab(1),
                        ResourceType::new("notes.note"),
                    ),
                    tree_chunks: Vec::new(),
                    document: Some(note),
                }],
            );
            let feed = pane.serve_feed(&wire::FeedQuery::default(), &tree);
            let summaries = feed
                .entries
                .iter()
                .map(|entry| entry.summary.as_str())
                .collect::<Vec<_>>();
            assert!(
                matches!(
                    summaries.as_slice(),
                    [appended, made] if appended == &"Added to the note “Spring plans”"
                        && made == &"Created the note “Spring plans”"
                ),
                "newest first, the append above the note it appended to: {summaries:?}"
            );
        }
    }

    phase workspace_activity_lists_two_records_changes_of_one_second_as_they_were_made;
    // A task made, then a note, in one second, in two records: a change keeps whole seconds, so
    // only the clock of the action that made each says which came first. Ordering them by which
    // record was read first lists the note below the task here, newest first.
    {
        use automerge::transaction::CommitOptions;
        use tilo_actions::{ActionContext, ActionKey, Described, Did, EncodedAction};
        use tilo_desktop::activity::*;
        use ui::resource::{DocumentCheckout, DocumentId, ResourceType};
        let made = |app: &str, kind: &str, verb: &str, title: &str, at_ms: u64| {
            let mut doc = AutoCommit::new();
            doc.put(ROOT, "title", title).unwrap();
            let mut context = ActionContext::for_resource(kind, kind, title)
                .with_app(app, app)
                .with_described(
                    Described::new(Did::verb(ActionKey::new(app, kind, verb), []))
                        .on(format!("{kind}:1"), title),
                );
            context.action = Some(EncodedAction {
                type_id: kind.to_owned(),
                payload: format!(r#"{{"op":{{"action":"{verb}"}},"at":{at_ms}}}"#).into_bytes(),
            });
            doc.commit_with(
                CommitOptions::default()
                    .with_time(1_800_000_000)
                    .with_message(context.change_message().unwrap()),
            );
            doc
        };
        let records = vec![
            ("so.tilo.notes", "notes.pages", "new_note", "Spring plans", 1_800_000_000_700),
            ("so.tilo.tasks", "tasks.issues", "create", "Check the budget", 1_800_000_000_300),
        ]
        .into_iter()
        .enumerate()
        .map(|(tab, (app, kind, verb, title, at))| DetachedRecordHistory {
            app: app.into(),
            checkout: DocumentCheckout::new(
                DocumentId::for_tab(u64::try_from(tab).unwrap() + 1),
                ResourceType::new(kind),
            ),
            tree_chunks: Vec::new(),
            document: Some(made(app, kind, verb, title, at)),
        })
        .collect();
        let mut tree = design();
        let mut pane = WorkspaceActivityPane::new();
        pane.refresh(&mut tree);
        pane.refresh_detached_records(&mut tree, records);
        let feed = pane.serve_feed(&wire::FeedQuery::default(), &tree);
        assert_eq!(
            feed.entries.len(),
            2,
            "both records' changes are listed: {:?}",
            feed.entries
        );
        let summaries = feed
            .entries
            .iter()
            .map(|entry| entry.summary.clone())
            .collect::<Vec<_>>();
        assert!(
            summaries[0].contains("Spring plans") && summaries[1].contains("Check the budget"),
            "newest first, the note made after the task above it: {summaries:?}"
        );
    }

    phase workspace_activity_names_setup_by_what_it_did_and_folds_it;
    // A workspace being set up by its devices: each action's receipt, what a device holds of it,
    // where a device that keeps it answers, the operation recording which rows a change wrote,
    // a device added (its row written twice) and two people added. Listing the bookkeeping, naming
    // a change by "Updated the workspace", or listing the device or the people once per change must
    // fail this regression.
    {
        use automerge::{ObjType, ScalarValue, transaction::CommitOptions};
        use std::rc::Rc;
        use tilo_desktop::activity::{GovernanceReader, MembershipWrite};

        /// The workspace engine's reading of the values below, which this test writes in the
        /// clear: an operation's bytes are "recorded" or the person it adds.
        struct Engine;
        impl GovernanceReader for Engine {
            fn membership(&self, op: &[u8]) -> Option<MembershipWrite> {
                let op = std::str::from_utf8(op).ok()?;
                Some(match op.strip_prefix("add:") {
                    Some(person) => MembershipWrite::Changes(vec![
                        tilo_action_policy::Change::AddMember {
                            person: person.to_owned(),
                            role: tilo_action_policy::Role::Editor,
                        },
                    ]),
                    None => MembershipWrite::Recorded,
                })
            }
            fn device_label(&self, row: &[u8]) -> Option<String> {
                std::str::from_utf8(row).ok().map(str::to_owned)
            }
        }

        let workspace = Workspace::new(WorkspaceId::from_u128(4), "Garden");
        let mut governance = AutoCommit::new();
        let mut at = 1_800_000_000;
        let mut commit = |doc: &mut AutoCommit| {
            doc.commit_with(CommitOptions::default().with_time(at));
            at += 1;
        };
        governance
            .put(ROOT, WORKSPACE_ID_KEY, workspace.id().to_string())
            .unwrap();
        let maps = [
            "effect_receipts_v1",
            "held_copies_v1",
            "keeping_devices_v1",
            "peers",
            "peer_identities_v1",
        ]
        .map(|key| governance.put_object(ROOT, key, ObjType::Map).unwrap());
        let [receipts, copies, keeping, peers, identities] = &maps;
        commit(&mut governance);
        let op = |doc: &mut AutoCommit, id: &str, says: &str| {
            doc.put(
                ROOT,
                format!("members_v3/{id}"),
                ScalarValue::Bytes(says.as_bytes().to_vec()),
            )
            .unwrap();
        };
        governance.put(keeping, "d1", "{}").unwrap();
        commit(&mut governance);
        op(&mut governance, "01", "recorded");
        commit(&mut governance);
        governance.put(copies, "d1", "{}").unwrap();
        commit(&mut governance);
        for written in ["02", "03"] {
            governance.put(peers, "d2", "").unwrap();
            governance
                .put(identities, "d2", ScalarValue::Bytes(b"Laptop".to_vec()))
                .unwrap();
            commit(&mut governance);
            op(&mut governance, written, "recorded");
            commit(&mut governance);
        }
        for person in ["ad", "cc"] {
            op(&mut governance, person, &format!("add:{}", person.repeat(32)));
            commit(&mut governance);
        }
        governance.put(receipts, "d2/1", "{}").unwrap();
        commit(&mut governance);

        let mut tree = tilo_desktop::activity::ActivitySnapshot::of_layout(
            Layout {
                active: workspace.id(),
                workspaces: vec![workspace.clone()],
                ..Default::default()
            },
            workspace.id(),
            AutoCommit::new(),
        )
        .governing(governance)
        .reading_governance(Rc::new(Engine));
        let mut pane = WorkspaceActivityPane::new();
        for (person, name) in [("ad", "Ada"), ("cc", "Claude Code")] {
            pane.authors_mut().register(
                &person.repeat(32),
                ui::widget::HistoryAuthor::new(name, 120.0, person),
            );
        }
        pane.begin_view(tree.active);
        pane.refresh(&mut tree);
        let feed = pane.serve_feed(&wire::FeedQuery::default(), &tree);
        let summaries = feed
            .entries
            .iter()
            .map(|entry| entry.summary.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            summaries,
            ["Added Ada and Claude Code", "Added the device “Laptop”"],
            "setup named by what it did, each run of it once, newest first"
        );
        pane.finish_view(tree.active);
    }

    phase workspace_activity_folds_the_windows_a_person_opened_one_after_another;
    // A person opening one window after another reads as one row naming each window, still
    // nobody's news; a change between the opens ends the run. A row per window opened, or a
    // folded row that names only the first two, must fail this regression.
    {
        use automerge::{ObjType, transaction::CommitOptions};
        let workspace = Workspace::new(WorkspaceId::from_u128(5), "Planning");
        let mut doc = AutoCommit::new();
        let mut at = 1_800_000_000;
        let mut stamp = |doc: &mut AutoCommit| {
            doc.commit_with(CommitOptions::default().with_time(at));
            at += 1;
        };
        doc.put(ROOT, WORKSPACE_ID_KEY, workspace.id().to_string())
            .unwrap();
        let contents = doc.put_object(ROOT, "contents", ObjType::Map).unwrap();
        let strip = doc
            .put_object(&contents, "0001000000000001", ObjType::Map)
            .unwrap();
        doc.put(&strip, "order", 0_u64).unwrap();
        let groups = doc.put_object(&strip, "groups", ObjType::Map).unwrap();
        stamp(&mut doc);
        let open = |doc: &mut AutoCommit, at: u64, titles: &[&str]| {
            let group = doc
                .put_object(&groups, format!("00020000000000{at:02}"), ObjType::Map)
                .unwrap();
            doc.put(&group, "order", at).unwrap();
            let tabs = doc.put_object(&group, "tabs", ObjType::Map).unwrap();
            for (n, title) in titles.iter().enumerate() {
                let tab = doc
                    .put_object(&tabs, format!("00030000000000{at:02}{n}"), ObjType::Map)
                    .unwrap();
                doc.put(&tab, "title", *title).unwrap();
                doc.put(&tab, "order", 0_u64).unwrap();
            }
        };
        open(&mut doc, 1, &["Files"]);
        stamp(&mut doc);
        doc.put(ROOT, "name", "Spring planning").unwrap();
        stamp(&mut doc);
        for (n, titles) in [&["Tasks"][..], &["Notes"], &["#chat", "Activity"], &["Chat"]]
            .into_iter()
            .enumerate()
        {
            open(&mut doc, 2 + u64::try_from(n).unwrap(), titles);
            stamp(&mut doc);
        }
        let mut tree = desktop(workspace, doc);
        let mut pane = WorkspaceActivityPane::new();
        pane.begin_view(tree.active);
        pane.refresh(&mut tree);
        let feed = pane.serve_feed(&wire::FeedQuery::default(), &tree);
        let summaries = feed
            .entries
            .iter()
            .map(|entry| entry.summary.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            summaries,
            [
                "Opened Tasks, Notes, #chat, Activity and Chat",
                "Named the workspace “Spring planning”",
                "Opened Files",
            ],
            "one person's opens one after another are one row naming the windows as they opened, rows newest first"
        );
        pane.finish_view(tree.active);
    }

    phase workspace_activity_names_where_a_change_happened_as_a_person_knows_it;
    // A change in an app's own collection (its messages, its folder) is placed in the app, by the
    // app's name; one in a record of its own (a channel) is placed in that record. Showing the
    // collection's kind ("Messages", "Folder") or an app's id as the place must fail this
    // regression.
    {
        use automerge::transaction::CommitOptions;
        use tilo_actions::{ActionContext, ActionKey, Described, Did};
        use tilo_desktop::activity::*;
        use ui::resource::{DocumentCheckout, DocumentId, ResourceType};
        let did = |app: &str, kind: &str, verb: &str| {
            Described::new(Did::verb(ActionKey::new(app, kind, verb), []))
        };
        let places = [
            ActionContext::for_resource(
                "chain:derived:messages@derived:default@tab:0000000000000000",
                "chat.messages",
                "Messages",
            )
            .with_app("so.tilo.chat", "Chat")
            .with_described(did("so.tilo.chat", "chat.messages", "post")),
            ActionContext::for_resource("files.folder", "files.folder", "Folder")
                .with_app("so.tilo.file-explorer", "so.tilo.file-explorer")
                .with_described(did("so.tilo.file-explorer", "files.folder", "write-file")),
            ActionContext::for_resource("chat.channel:c", "chat.channel", "general")
                .with_app("so.tilo.chat", "Chat")
                .with_described(did("so.tilo.chat", "chat.channel", "rename")),
        ];
        let mut tree = design();
        let mut records = Vec::new();
        for (tab, context) in (0u32..).zip(places.iter()) {
            let mut document = AutoCommit::new();
            document.put(ROOT, "value", i64::from(tab)).unwrap();
            document.commit_with(
                CommitOptions::default()
                    .with_time(1_800_000_000 + i64::from(tab))
                    .with_message(context.change_message().unwrap()),
            );
            records.push(DetachedRecordHistory {
                app: context.app.as_ref().unwrap().id.clone(),
                checkout: DocumentCheckout::new(
                    DocumentId::for_tab(u64::from(tab) + 1),
                    ResourceType::new(context.resource.as_ref().unwrap().kind.clone()),
                ),
                tree_chunks: Vec::new(),
                document: Some(document),
            });
        }
        let mut pane = WorkspaceActivityPane::new();
        pane.refresh(&mut tree);
        pane.refresh_detached_records(&mut tree, records);
        let feed = pane.serve_feed(&wire::FeedQuery::default(), &tree);
        let placed = feed
            .entries
            .iter()
            .map(|entry| (entry.app.as_deref(), entry.resource.as_deref()))
            .collect::<Vec<_>>();
        assert_eq!(
            placed,
            [
                (Some("Chat"), Some("general")),
                (Some("Files"), None),
                (Some("Chat"), None),
            ],
            "newest first, each placed where a person knows it"
        );
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
                    items: device::Writer::NOTHING_ITEMED,
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
