//! Behaviour of the reader window.

#![allow(clippy::float_cmp, reason = "tests assert exact values")]

use super::*;
use crate::doc::FeedsDoc;
use crate::model::{BlockKind, ContentBlock};
use automerge::ReadDoc as _;
use ui::headless::Framed;
use ui::widget::Widget;
use winit::event::MouseScrollDelta;

/// The sample library: the only place demo feeds come from.
fn app() -> FeedsApp {
    FeedsApp::new_demo()
}

/// Mount `app` over an empty document in the production frame host.
fn framed_at(mut app: FeedsApp, viewport: (u32, u32)) -> Framed<FeedsApp> {
    let doc = synced(&mut app);
    let mut state = ui::State::new();
    assert!(state.adopt_doc(doc));
    Framed::new(state, app, viewport, Theme::dark())
}

fn framed(app: FeedsApp) -> Framed<FeedsApp> {
    framed_at(app, (1280, 800))
}

/// Run frames until a staged scenario or a settled layout is in place.
fn settle(app: &mut Framed<FeedsApp>) {
    for _ in 0..(SETTLED_LAYOUTS + 3) {
        Framed::harness_mut(app).relayout();
    }
}

fn synced(app: &mut FeedsApp) -> AutoCommit {
    let mut doc = AutoCommit::new();
    app.attach_framework(ui::framework::Dispatcher::detached(
        "so.tilo.feeds",
        "component-test-author",
    ));
    // A component fixture presents the same typed actions a capture applies;
    // the live app never seeds its document during sync.
    let core = app.core();
    if !core.store.feeds.is_empty() || !core.store.folders.is_empty() {
        tilo_actions::Action::apply(
            &FeedsAction::ImportSubscriptions {
                folders: core.store.folders.clone(),
                feeds: core.store.feeds.clone(),
            },
            &mut doc,
        )
        .unwrap();
        tilo_actions::Action::apply(
            &FeedsAction::UpsertFetched {
                articles: core.store.articles.clone(),
                feeds: Vec::new(),
            },
            &mut doc,
        )
        .unwrap();
    }
    drop(core);
    app.sync(&doc, &automerge::ROOT);
    doc
}

fn article(feed_id: Id, title: &str, published: i64, blocks: Vec<ContentBlock>) -> Article {
    Article {
        id: Uuid::new_v4(),
        feed_id,
        title: title.into(),
        author: "Reporter".into(),
        url: format!("https://example.com/{}", Uuid::new_v4()),
        published,
        summary: "A summary".into(),
        blocks,
        read: false,
        starred: false,
        tags: Vec::new(),
    }
}

fn synth_device() -> winit::event::DeviceId {
    winit::event::DeviceId::dummy()
}

fn cursor_at(p: Point) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: synth_device(),
        position: winit::dpi::PhysicalPosition::new(p.x, p.y),
    }
}

fn press(button: MouseButton) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: synth_device(),
        state: ElementState::Pressed,
        button,
    }
}

fn key_event(key: Key, text: Option<&str>) -> WindowEvent {
    let ev = ui::synthetic_input::key_event(key, text.map(Into::into), ElementState::Pressed);
    WindowEvent::KeyboardInput {
        device_id: synth_device(),
        event: ev,
        is_synthetic: false,
    }
}

fn char_key(s: &str) -> WindowEvent {
    key_event(Key::Character(s.into()), Some(s))
}

fn named_key(k: NamedKey) -> WindowEvent {
    key_event(Key::Named(k), None)
}

/// Drive the root's real `handle_event` with explicit modifiers.
fn dispatch_mods(
    a: &mut FeedsApp,
    doc: &mut AutoCommit,
    event: &WindowEvent,
    mods: winit::event::Modifiers,
) -> bool {
    ui::testing::Dispatch::new()
        .modifiers(mods)
        .doc(doc)
        .send(a, event)
        .handled
}

fn dispatch(a: &mut FeedsApp, doc: &mut AutoCommit, event: &WindowEvent) -> bool {
    dispatch_mods(a, doc, event, winit::event::Modifiers::default())
}

fn has_target(node: &AccessNode, target: &str) -> bool {
    node.action_target.as_deref() == Some(target)
        || node.children.iter().any(|child| has_target(child, target))
}

fn find_role(node: &AccessNode, role: Role) -> Option<&AccessNode> {
    if node.role == role {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_role(child, role))
}

/// A live app starts empty and offers to add a feed; only the demo build
/// seeds and opens sample content.
#[test]
fn startup_is_empty_live_and_populated_only_in_demo() {
    let live = FeedsApp::new();
    {
        let core = live.core();
        assert!(core.store.feeds.is_empty() && core.store.articles.is_empty());
        assert!(core.selected.is_none());
    }
    assert_eq!(
        live.empty_onboarding_prompt(),
        Some(crate::list_pane::EMPTY_LIBRARY_TITLE)
    );

    // Syncing the live app over a fresh document writes nothing: the default
    // reading settings stand unwritten.
    let mut live = FeedsApp::new();
    let mut doc = synced(&mut live);
    assert!(live.core().store.feeds.is_empty());
    assert!(
        doc.get(automerge::ROOT, "settings").unwrap().is_none(),
        "opening writes nothing: the default reading settings stand unwritten"
    );
    assert!(
        doc.get_heads().is_empty(),
        "the untouched live record has no app writes"
    );

    // The demo opens a rich article and seeds its first document.
    let mut demo = app();
    assert!(demo.empty_onboarding_prompt().is_none());
    let selected = demo.selected_article().expect("the demo opens an article");
    assert!(
        !demo
            .core()
            .store
            .article(selected)
            .expect("the open article")
            .blocks
            .is_empty()
    );
    let doc = synced(&mut demo);
    assert_eq!(
        FeedsDoc::load(&doc).feeds.len(),
        demo.core().store.feeds.len()
    );
}

/// The empty library disables what has nothing to act on and offers one
/// action in its empty state; the reader shows nothing beside it.
#[test]
fn an_empty_library_disables_its_commands_and_offers_to_add_a_feed() {
    let mut mounted = framed(FeedsApp::new());
    settle(&mut mounted);
    let controls = Framed::harness(&mounted).controls();
    let control = |name: &str| {
        controls
            .iter()
            .find(|c| c.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("{name} is a control"))
    };
    assert!(control("Refresh all feeds").disabled);
    assert!(control("Mark all as read").disabled);
    assert!(!control("Add subscription").disabled);
    assert!(control("Star article").disabled);
    let add = control("Add feed").bounds.expect("placed").center();
    Framed::harness_mut(&mut mounted).click_at(add.x, add.y);
    settle(&mut mounted);
    let (a, _) = Framed::split(&mut mounted);
    assert_eq!(
        a.modal.as_ref().map(|m| m.intent),
        Some(ModalIntent::AddSubscription),
        "the empty state's action opens the add dialog"
    );
}

/// Refresh runs on a worker, drains without blocking the frame, persists in
/// bounded steps and reports completion.
#[allow(
    clippy::too_many_lines,
    reason = "one scenario per phase of a refresh, kept together"
)]
#[test]
fn refresh_runs_off_thread_and_lands_through_the_store() {
    // --- phase: refresh only enqueues; the draw-time drain applies results ---
    {
        let mut a = app();
        let mut doc = synced(&mut a);
        let (worker, tx) = FetchWorker::for_test();
        a.core_mut().worker = Some(worker);
        // `refresh_all` only queues: it returns before any fetch reported.
        a.core_mut().refresh_all();
        assert!(a.core().refresh.in_progress());
        assert_eq!(a.core().refresh.done, 0, "refresh_all waited for a fetch");
        assert!(
            a.next_wake_deadline().is_some(),
            "outstanding fetches wake the loop without animation"
        );

        let feed_id = a.core().store.feeds[0].id;
        let fresh = article(feed_id, "Fresh from the worker", a.core().now, vec![]);
        let before = a.core().store.articles.len();
        tx.send(FetchResult::Articles {
            feed_id,
            title: None,
            articles: vec![fresh.clone()],
            certificate_pin: None,
        })
        .unwrap();
        assert!(a.core_mut().drain_fetches_live());
        assert_eq!(a.core().store.articles.len(), before + 1);
        assert_eq!(a.core().refresh.done, 1);
        assert_eq!(a.core().refresh.new_items, 1);
        assert!(
            !a.core().pending_doc_writes.is_empty(),
            "the write is queued"
        );
        a.core_mut()
            .flush_doc_writes(&mut ui::widget::Edits::lend(&mut doc));
        assert!(a.core().pending_doc_writes.is_empty());
        assert!(
            FeedsDoc::load(&doc)
                .articles
                .iter()
                .any(|x| x.id == fresh.id)
        );

        tx.send(FetchResult::Failed {
            feed_id: Some(a.core().store.feeds[1].id),
            message: "unreachable".into(),
        })
        .unwrap();
        a.core_mut().drain_fetches_live();
        assert_eq!(a.core().refresh.failed_feeds, 1);
        assert!(
            a.needs_animation_frame(),
            "frames flow until the batch ends"
        );
    }

    // --- phase: a feed pins the certificate of its first fetch, and only that ---
    {
        let mut a = app();
        let mut doc = synced(&mut a);
        let (worker, tx) = FetchWorker::for_test();
        a.core_mut().worker = Some(worker);
        let feed_id = a.core().store.feeds[0].id;
        assert_eq!(a.core().store.feeds[0].certificate_pin, None);
        for presented in ["first", "impostor"] {
            tx.send(FetchResult::Articles {
                feed_id,
                title: None,
                articles: vec![],
                certificate_pin: Some(presented.into()),
            })
            .unwrap();
            a.core_mut().drain_fetches_live();
        }
        a.core_mut()
            .flush_doc_writes(&mut ui::widget::Edits::lend(&mut doc));
        let pinned = |feeds: &[Feed]| {
            feeds
                .iter()
                .find(|f| f.id == feed_id)
                .and_then(|f| f.certificate_pin.clone())
        };
        assert_eq!(pinned(&a.core().store.feeds).as_deref(), Some("first"));
        assert_eq!(
            pinned(&FeedsDoc::load(&doc).feeds).as_deref(),
            Some("first"),
            "the pin syncs with the subscription"
        );
    }

    // --- phase: a large batch persists at most FLUSH_BUDGET writes a frame ---
    {
        let mut a = app();
        let mut doc = synced(&mut a);
        let (worker, tx) = FetchWorker::for_test();
        a.core_mut().worker = Some(worker);
        a.core_mut().refresh_all();
        let feed_ids: Vec<Id> = a.core().store.feeds.iter().map(|f| f.id).collect();
        let now = a.core().now;
        let mut total = 0usize;
        for fid in &feed_ids {
            let articles: Vec<Article> = (0..100)
                .map(|i| {
                    article(
                        *fid,
                        &format!("Article {i}"),
                        now - i64::from(i) * 60,
                        vec![ContentBlock::new(BlockKind::Paragraph, "Body.")],
                    )
                })
                .collect();
            total += articles.len();
            tx.send(FetchResult::Articles {
                feed_id: *fid,
                title: None,
                articles,
                certificate_pin: None,
            })
            .unwrap();
        }
        let heads = doc.get_heads();
        a.core_mut().drain_fetches_live();
        assert_eq!(doc.get_heads(), heads, "the drain persists nothing");
        let before = a.core().pending_doc_writes.len();
        assert!(before >= total);
        a.core_mut()
            .flush_doc_writes(&mut ui::widget::Edits::lend(&mut doc));
        assert!(before - a.core().pending_doc_writes.len() <= FLUSH_BUDGET);
        let mut frames = 0;
        while !a.core().pending_doc_writes.is_empty() && frames < 5000 {
            a.core_mut()
                .flush_doc_writes(&mut ui::widget::Edits::lend(&mut doc));
            frames += 1;
        }
        assert!(FeedsDoc::load(&doc).articles.len() >= total);
    }

    // --- phase: progress finishes when every feed reported back ---
    {
        let mut a = app();
        let doc = synced(&mut a);
        let (worker, tx) = FetchWorker::for_test();
        a.core_mut().worker = Some(worker);
        a.core_mut().refresh_all();
        let total = a.core().refresh.total;
        assert_eq!(total, a.core().store.feeds.len());
        assert!(a.core().refresh.status().starts_with("Refreshing 0 of"));
        let feeds = a.core().store.feeds.clone();
        for (i, f) in feeds.iter().enumerate() {
            let result = if i % 2 == 0 {
                FetchResult::Articles {
                    feed_id: f.id,
                    title: None,
                    articles: vec![],
                    certificate_pin: None,
                }
            } else {
                FetchResult::Failed {
                    feed_id: Some(f.id),
                    message: "x".into(),
                }
            };
            tx.send(result).unwrap();
        }
        a.core_mut().drain_fetches_live();
        assert!(!a.core().refresh.in_progress());
        assert!(a.core().refresh.visible(), "the summary lingers");
        assert!(
            a.core().refresh.status().contains("could not be refreshed"),
            "{}",
            a.core().refresh.status()
        );
        a.sync(&doc, &automerge::ROOT);
        assert!(a.core().next_wake_deadline().is_none());
    }

    // --- phase: the progress shows above the list in the window ---
    {
        let mut a = app();
        a.force_refresh_in_progress();
        let mounted = framed(a);
        let tree = mounted.accessibility().expect("a semantic window");
        let progress = find_role(&tree, Role::ProgressBar).expect("refresh progress");
        assert_eq!(progress.name.as_deref(), Some("Refreshing 3 of 7 feeds"));
        let core = mounted.core();
        assert!(
            core.list.viewport.y0 > core.list.rect.y0,
            "the rows start below the progress"
        );
    }
}

/// Reading settings, read and star state and places persist through the
/// document.
#[test]
fn reader_state_edits_persist_through_the_document() {
    let mut a = app();
    let mut doc = synced(&mut a);
    assert_eq!(a.core().settings.theme, ReadTheme::System);
    for request in [
        Request::SetFont(ReadFont::Sans),
        Request::SetPaper(ReadTheme::Sepia),
        Request::SetScale(120.0),
    ] {
        a.core_mut()
            .apply(request, &mut ui::widget::Edits::lend(&mut doc));
    }
    let loaded = a.read_settings_for_test();
    assert_eq!(loaded.font, ReadFont::Sans);
    assert_eq!(loaded.theme, ReadTheme::Sepia);
    assert!((loaded.font_scale - 1.2).abs() < 0.001);

    let unread = a
        .core()
        .store
        .articles_for(Filter::All)
        .into_iter()
        .find(|x| !x.read)
        .map(|x| x.id)
        .expect("an unread article");
    a.core_mut()
        .select_article(unread, &mut ui::widget::Edits::lend(&mut doc));
    assert!(FeedsDoc::load(&doc).article(unread).is_some_and(|x| x.read));

    let id = a.core().store.articles[0].id;
    let before = a.core().store.article(id).is_some_and(|x| x.starred);
    a.core_mut()
        .toggle_star(id, &mut ui::widget::Edits::lend(&mut doc));
    assert_eq!(
        FeedsDoc::load(&doc).article(id).map(|x| x.starred),
        Some(!before)
    );

    a.core_mut().set_filter(Filter::Starred);
    let selected = a.selected_article();
    assert!(
        a.core()
            .store
            .articles_for(Filter::Starred)
            .iter()
            .any(|x| Some(x.id) == selected)
    );

    a.core_mut().set_filter(Filter::All);
    a.core_mut()
        .mark_all_read(&mut ui::widget::Edits::lend(&mut doc));
    assert_eq!(a.core().store.total_unread(), 0);
}

/// The window's semantics: the sidebar list, the article rows with their
/// star, the reader document; operating them runs the same actions.
#[test]
fn accessibility_mirrors_and_operates_the_reader() {
    let mut mounted = framed(app());
    settle(&mut mounted);
    let window = mounted.accessibility().expect("a semantic window");
    assert_eq!(window.role, Role::Window);
    let document = mounted.core().reader_accessibility();
    assert_eq!(document.role, Role::Document);
    assert!(find_role(&document, Role::Heading { level: 1 }).is_some());
    let list = mounted.core().list_accessibility();
    assert_eq!(list.role, Role::List);
    assert_eq!(list.name.as_deref(), Some("All articles"));
    let (a, st) = Framed::split(&mut mounted);
    let id = a.core().list.rows[0].id;
    assert!(has_target(&list, &format!("feeds.article:{id}")));
    assert!(has_target(&list, &format!("feeds.star:{id}")));
    let before = a.core().store.article(id).is_some_and(|x| x.starred);
    assert!(a.perform_semantic_action(
        &format!("feeds.star:{id}"),
        &A11yAction::ToggleCheck,
        &mut ui::widget::Edits::lend(&mut st.doc),
        &st.root,
    ));
    assert_ne!(
        a.core().store.article(id).is_some_and(|x| x.starred),
        before
    );
    assert!(a.perform_semantic_action(
        &format!("feeds.article:{id}"),
        &A11yAction::Activate,
        &mut ui::widget::Edits::lend(&mut st.doc),
        &st.root,
    ));
    assert_eq!(a.selected_article(), Some(id));

    // A tag of the open article narrows the list to that tag.
    let tagged = a
        .core()
        .store
        .articles
        .iter()
        .find(|x| !x.tags.is_empty())
        .map(|x| (x.id, crate::model::tag_key(&x.tags[0])))
        .expect("a tagged sample article");
    a.core_mut().selected = Some(tagged.0);
    settle(&mut mounted);
    let (a, st) = Framed::split(&mut mounted);
    assert!(a.perform_semantic_action(
        &format!("feeds.reader-tag:{}", tagged.1),
        &A11yAction::Activate,
        &mut ui::widget::Edits::lend(&mut st.doc),
        &st.root,
    ));
    assert_eq!(a.active_filter(), Filter::Tag(tagged.1));
}

/// The modal's field is a real native control: accessibility edits it and
/// the footer submits a real subscription.
#[test]
fn the_add_dialog_field_is_native_and_submits_a_subscription() {
    check_add_dialog_native_accessibility_sets_the_field();

    check_add_dialog_typing_goes_to_the_field_escape_cancels();

    check_add_dialog_enter_subscribes();

    check_add_dialog_an_empty_required_field_keeps_the_dialog_open();
}

/// Tags are edited through the canonical action and may be cleared.
#[test]
fn the_tag_editor_uses_the_canonical_action() {
    let mut a = app();
    let mut doc = synced(&mut a);
    let id = a.selected_article().expect("an open article");
    a.stage_tag_editor();
    a.take_pending_modal();
    assert_eq!(
        a.modal.as_ref().map(|m| m.intent),
        Some(ModalIntent::EditArticleTags { article_id: id })
    );
    a.modal
        .as_mut()
        .unwrap()
        .dialog
        .content_mut()
        .set_value("Design, Later, design");
    a.submit_modal(&mut ui::widget::Edits::lend(&mut doc));
    assert_eq!(
        FeedsDoc::load(&doc).article(id).unwrap().tags,
        ["design", "later"]
    );
    a.stage_tag_editor();
    a.take_pending_modal();
    a.modal.as_mut().unwrap().dialog.content_mut().set_value("");
    a.submit_modal(&mut ui::widget::Edits::lend(&mut doc));
    assert!(FeedsDoc::load(&doc).article(id).unwrap().tags.is_empty());
}

/// The shell docks the sidebar in a wide window, floats it as a drawer in
/// a narrower one and pushes the article in a narrow one.
#[test]
fn the_window_shell_adapts_to_its_width() {
    // Wide: the sidebar, the list and the article side by side.
    let mut mounted = framed(app());
    settle(&mut mounted);
    let geometry = mounted.shell().geometry().clone();
    assert_eq!(geometry.sidebar, SidebarPlacement::Docked);
    assert!(geometry.side_by_side);
    assert!(geometry.stack.is_empty());

    // 800 wide: the sidebar becomes a drawer; the list and article stay.
    let mut mounted = framed_at(app(), (800, 600));
    settle(&mut mounted);
    assert_eq!(mounted.shell().geometry().sidebar, SidebarPlacement::Drawer);
    assert!(!mounted.shell().is_sidebar_open(), "it opens closed");
    mounted.stage("sidebar-open");
    settle(&mut mounted);
    assert!(mounted.shell().is_sidebar_open());
    // Choosing a place closes the drawer.
    {
        let (a, st) = Framed::split(&mut mounted);
        a.requests.borrow_mut().push(Request::Show(Filter::Unread));
        a.settle(&mut ui::widget::Edits::lend(&mut st.doc), &st.root);
    }
    assert!(!mounted.shell().is_sidebar_open());

    // Narrow: list and article are push screens; choosing an article shows it.
    let mut mounted = framed_at(app(), (520, 820));
    settle(&mut mounted);
    assert_eq!(
        mounted.shell().geometry().stack,
        vec![NavPane::Overview, NavPane::Content]
    );
    {
        let (a, st) = Framed::split(&mut mounted);
        a.shell_mut().show(NavPane::Overview);
        a.shell_mut().snap();
        let id = a.core().store.articles_for(Filter::All)[1].id;
        a.core_mut()
            .select_article(id, &mut ui::widget::Edits::lend(&mut st.doc));
        a.settle(&mut ui::widget::Edits::lend(&mut st.doc), &st.root);
        assert_eq!(a.shell().front_pane(), NavPane::Content);
    }
}

/// OPML import persists every new feed once; the export and import portals
/// never report success on cancel or failure.
#[allow(
    clippy::too_many_lines,
    reason = "one scenario per portal outcome, kept together"
)]
#[test]
fn opml_portal_import_and_export_report_honestly() {
    fn export_with(a: &FeedsApp, outcome: PortalOutcome) {
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(outcome);
        ui::host::scoped(&services, || a.core_mut().export_opml());
        let picks = recorder.pick_requests();
        assert_eq!(picks.len(), 1);
        assert_eq!(picks[0].title(), "Export OPML");
    }
    fn import_with(a: &FeedsApp, outcome: PortalOutcome) {
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(outcome);
        ui::host::scoped(&services, || a.core_mut().request_import());
        let picks = recorder.pick_requests();
        assert_eq!(picks.len(), 1);
        assert_eq!(picks[0].title(), "Import OPML");
    }

    // --- phase: import adds every feed and persists; re-import adds none ---
    {
        let mut a = app();
        let mut doc = synced(&mut a);
        let before = a.core().store.feeds.len();
        let new_ids = a
            .core_mut()
            .apply_opml_import(opml::sample_opml(), &mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(new_ids.len(), 3);
        assert_eq!(a.core().store.feeds.len(), before + 3);
        let loaded = FeedsDoc::load(&doc);
        assert!(
            loaded
                .feeds
                .iter()
                .any(|f| f.url == "https://jvns.ca/atom.xml")
        );
        assert!(loaded.folders.iter().any(|f| f.name == "Reading"));
        assert!(
            a.core_mut()
                .apply_opml_import(opml::sample_opml(), &mut ui::widget::Edits::lend(&mut doc))
                .is_empty()
        );
    }

    // --- phase: export writes the snapshot to the chosen path ---
    {
        let mut a = app();
        let doc = synced(&mut a);
        let directory = std::env::temp_dir().join(format!("feeds-export-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("Subscriptions Grün 🌍.opml");
        let xml = opml::export(&a.core().store);
        export_with(&a, PortalOutcome::Opened(path.clone()));
        assert!(a.next_wake_deadline().is_some());
        a.sync(&doc, &automerge::ROOT);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), xml);
        assert!(a.core().export_rx.is_none());
        assert!(
            a.core()
                .toast_visible()
                .is_some_and(|t| t.starts_with("Exported subscriptions"))
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    // --- phase: cancel and failure are reported as such ---
    for outcome in [
        PortalOutcome::Cancelled,
        PortalOutcome::Failed("test denial".into()),
    ] {
        let mut a = app();
        let doc = synced(&mut a);
        export_with(&a, outcome.clone());
        a.sync(&doc, &automerge::ROOT);
        let message = a.core().toast_visible().map(str::to_owned).unwrap();
        match outcome {
            PortalOutcome::Cancelled => assert_eq!(message, "Export cancelled"),
            PortalOutcome::Failed(reason) => {
                assert_eq!(message, format!("Export failed: {reason}"));
            }
            PortalOutcome::Opened(_) => unreachable!(),
        }
        let tree = a.accessibility().unwrap();
        assert!(
            tree.children.iter().any(|node| {
                node.name.as_deref() == Some(message.as_str())
                    && node.live == Some(Politeness::Polite)
            }),
            "the outcome is announced"
        );
    }

    // --- phase: import reads the chosen file; cancel changes nothing ---
    {
        let mut a = app();
        let mut doc = synced(&mut a);
        let (worker, _tx) = FetchWorker::for_test();
        a.core_mut().worker = Some(worker);
        let dir = std::env::temp_dir().join(format!("feeds-opml-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("subs.opml");
        std::fs::write(&path, opml::sample_opml()).unwrap();
        let before = a.core().store.feeds.len();
        import_with(&a, PortalOutcome::Opened(path));
        a.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        a.sync(&doc, &automerge::ROOT);
        assert!(a.core().import_rx.is_none());
        assert_eq!(a.core().store.feeds.len(), before + 3);
        std::fs::remove_dir_all(&dir).ok();
    }
    {
        let mut a = app();
        let mut doc = synced(&mut a);
        let before = a.core().store.feeds.len();
        import_with(&a, PortalOutcome::Cancelled);
        assert!(
            a.core().next_wake_deadline().is_some(),
            "an open chooser wakes the loop"
        );
        a.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        a.sync(&doc, &automerge::ROOT);
        assert!(a.core().import_rx.is_none());
        assert_eq!(a.core().store.feeds.len(), before);
        assert!(a.core().next_wake_deadline().is_none());
    }
}

/// A right click on a feed opens its menu on that row; the menu owns the
/// keyboard and pointer while open and runs its actions.
#[allow(
    clippy::too_many_lines,
    reason = "one scenario per menu interaction, kept together"
)]
#[test]
fn the_feed_menu_opens_on_its_row_and_runs_its_actions() {
    let mut mounted = framed(app());
    settle(&mut mounted);
    let (feed, row) = mounted.core().first_feed_row.expect("a painted feed row");

    // Right click on the row.
    let harness = Framed::harness_mut(&mut mounted);
    harness.dispatch_to_root(&cursor_at(row.center()));
    harness.dispatch_to_root(&press(MouseButton::Right));
    harness.relayout();
    {
        let core = mounted.core();
        let open = core.menu.as_ref().expect("the right click opened the menu");
        assert_eq!(open.kind, MenuKind::Feed(feed));
        assert_eq!(
            open.actions,
            vec![
                MenuAction::MarkFeedRead(feed),
                MenuAction::RenameFeed(feed),
                MenuAction::Unsubscribe(feed),
            ]
        );
    }

    // The menu owns article shortcuts while it is open.
    let (a, st) = Framed::split(&mut mounted);
    let doc = &mut st.doc;
    let before: Vec<_> = a
        .core()
        .store
        .articles
        .iter()
        .map(|x| (x.id, x.read, x.starred))
        .collect();
    for key in ["m", "s", "j", "k"] {
        dispatch(a, doc, &char_key(key));
    }
    let after: Vec<_> = a
        .core()
        .store
        .articles
        .iter()
        .map(|x| (x.id, x.read, x.starred))
        .collect();
    assert_eq!(before, after, "the menu captures article shortcuts");
    assert!(a.core().menu.is_some());

    // Arrow down to Rename and Enter opens the rename dialog.
    dispatch(a, doc, &named_key(NamedKey::ArrowDown));
    dispatch(a, doc, &named_key(NamedKey::ArrowDown));
    dispatch(a, doc, &named_key(NamedKey::Enter));
    assert!(a.core().menu.is_none());
    assert_eq!(
        a.modal.as_ref().map(|m| m.intent),
        Some(ModalIntent::RenameFeed { feed_id: feed })
    );
    a.modal
        .as_mut()
        .unwrap()
        .dialog
        .content_mut()
        .set_value("My feed");
    a.submit_modal(&mut ui::widget::Edits::lend(doc));
    assert_eq!(FeedsDoc::load(doc).feed(feed).unwrap().title, "My feed");

    // Losing focus dismisses an open menu.
    a.core_mut().open_feed_menu(feed, row);
    assert!(dispatch(a, doc, &WindowEvent::Focused(false)) || a.core().menu.is_none());
    assert!(a.core().menu.is_none());

    // Mark all as read marks the feed's articles and persists.
    a.core_mut().apply_menu_action(
        MenuAction::MarkFeedRead(feed),
        &mut ui::widget::Edits::lend(doc),
    );
    assert_eq!(a.core().store.unread_for_feed(feed), 0);
    assert!(
        FeedsDoc::load(doc)
            .articles
            .iter()
            .filter(|x| x.feed_id == feed)
            .all(|x| x.read)
    );

    // Unsubscribe asks first, as a destructive confirmation.
    a.core_mut().apply_menu_action(
        MenuAction::Unsubscribe(feed),
        &mut ui::widget::Edits::lend(doc),
    );
    a.take_pending_modal();
    assert_eq!(
        a.modal.as_ref().map(|m| m.intent),
        Some(ModalIntent::Unsubscribe { feed_id: feed })
    );
    assert!(a.core().store.feed(feed).is_some(), "not until confirmed");
    a.submit_modal(&mut ui::widget::Edits::lend(doc));
    assert!(a.core().store.feed(feed).is_none());
    let loaded = FeedsDoc::load(doc);
    assert!(!loaded.feeds.iter().any(|f| f.id == feed));
    assert!(!loaded.articles.iter().any(|x| x.feed_id == feed));

    // The menu stays inside a shrinking window and captures host undo.
    let other = a.core().store.feeds[0].id;
    a.core_mut()
        .open_feed_menu(other, Rect::new(900.0, 700.0, 1200.0, 740.0));
    assert!(ui::undo::handle_local_undo_request(
        a,
        ui::undo::UndoRequest::Undo
    ));
}

/// The sidebar's counts are the unread articles of each place, the same
/// metric the list emphasises; the titles follow the chosen place.
#[test]
fn sidebar_counts_and_titles_follow_the_library() {
    let mut a = app();
    let mut doc = synced(&mut a);
    let feed = a.core().store.feeds[1].id;
    let sections = a.core().nav_sections();
    let library = &sections[0].items;
    assert_eq!(
        library[0].count, None,
        "Unread already counts every new article"
    );
    assert_eq!(
        library[1].count,
        Some(ui::chrome::NavCount::Neutral(
            u32::try_from(a.core().store.total_unread()).unwrap()
        ))
    );
    let feeds = sections
        .iter()
        .find(|s| s.id == "feeds")
        .expect("a feeds section");
    let item = feeds
        .items
        .iter()
        .flat_map(|i| std::iter::once(i).chain(i.children.iter()))
        .find(|i| i.id == crate::sidebar::nav_id(Filter::Feed(feed)))
        .expect("the feed's row");
    let unread = a.core().store.unread_for_feed(feed);
    assert_eq!(
        item.count,
        Some(ui::chrome::NavCount::Neutral(
            u32::try_from(unread).unwrap()
        ))
    );

    a.requests
        .borrow_mut()
        .push(Request::Show(Filter::Feed(feed)));
    a.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
    assert_eq!(a.active_filter(), Filter::Feed(feed));
    let title = a.core().store.feed(feed).unwrap().title.clone();
    assert_eq!(a.titles.0, title);
}

/// The reading settings open under their button and their controls change
/// the reader.
#[test]
fn reading_settings_open_under_their_button() {
    let mut a = app();
    a.stage("reader-settings");
    let mut mounted = framed(a);
    settle(&mut mounted);
    assert!(mounted.settings_panel_open());
    let tree = Framed::harness(&mounted).controls();
    assert!(
        tree.iter().any(|c| c.name.as_deref() == Some("Text size")),
        "the text size field is a control"
    );
    let (a, st) = Framed::split(&mut mounted);
    a.requests
        .borrow_mut()
        .push(Request::SetPaper(ReadTheme::Dark));
    a.settle(&mut ui::widget::Edits::lend(&mut st.doc), &st.root);
    assert_eq!(a.core().settings.theme, ReadTheme::Dark);
    // Escape closes the panel.
    dispatch(a, &mut st.doc, &named_key(NamedKey::Escape));
    assert!(!a.settings_panel_open());
}

/// Lists scroll to their content and keep the open article in view.
#[test]
fn the_list_scrolls_and_keeps_the_open_article_in_view() {
    let a = app();
    // Enough articles to overflow the window.
    {
        let mut core = a.core_mut();
        let feed = core.store.feeds[0].id;
        let now = core.now;
        for i in 0..40 {
            core.store.insert_article_dedup(article(
                feed,
                &format!("Archive {i}"),
                now - 86_400 * 30 - i64::from(i),
                vec![],
            ));
        }
    }
    let mut mounted = framed_at(a, (1280, 600));
    settle(&mut mounted);
    let (rect, max) = {
        let core = mounted.core();
        (
            core.list.viewport,
            core.list.content - core.list.viewport.height(),
        )
    };
    assert!(max > 0.0, "the list overflows");
    let harness = Framed::harness_mut(&mut mounted);
    harness.dispatch_to_root(&cursor_at(rect.center()));
    harness.dispatch_to_root(&WindowEvent::MouseWheel {
        device_id: synth_device(),
        delta: MouseScrollDelta::LineDelta(0.0, -1000.0),
        phase: winit::event::TouchPhase::Moved,
    });
    harness.relayout();
    let scroll = mounted.core().list.scroll();
    assert!(
        (scroll - max).abs() < 0.5,
        "scrolled to the end: {scroll} of {max}"
    );

    // Stepping to the first article brings it back into view.
    let (a, st) = Framed::split(&mut mounted);
    let first = a.core().store.articles_for(Filter::All)[0].id;
    a.core_mut()
        .select_article(first, &mut ui::widget::Edits::lend(&mut st.doc));
    settle(&mut mounted);
    assert_eq!(mounted.core().list.scroll(), 0.0);
}

/// Links open only web, Gemini and mail addresses.
#[test]
fn article_links_reject_local_files() {
    let reader = FeedsApp::new();
    reader
        .core_mut()
        .open_url("file:///does-not-exist-tilo-link-proof");
    assert_eq!(
        reader.core().toast_visible(),
        Some(ui::i18n!("feeds-open-link-invalid").as_str())
    );
}

fn check_add_dialog_native_accessibility_sets_the_field() {
    let mut reader = FeedsApp::new();
    reader.core_mut().open_add_subscription();
    reader.take_pending_modal();
    let mut app = ui::App::new(ui::State::new(), Box::new(reader));
    app.init_layout();
    let field = app
        .build_a11y_tree_update(1.0)
        .nodes
        .iter()
        .find(|(_, node)| node.label() == Some("Feed URL"))
        .map(|(id, _)| *id)
        .expect("the field is exported");
    app.dispatch_a11y_action(
        field,
        &A11yAction::SetValue("https://example.invalid/rss.xml".into()),
    );
    let after = app.build_a11y_tree_update(1.0);
    let field = after
        .nodes
        .iter()
        .find(|(_, node)| node.label() == Some("Feed URL"))
        .expect("still exported");
    assert_eq!(field.1.value(), Some("https://example.invalid/rss.xml"));
}

fn check_add_dialog_typing_goes_to_the_field_escape_cancels() {
    let mut mounted = framed(FeedsApp::new());
    mounted.core_mut().open_add_subscription();
    settle(&mut mounted);
    assert!(mounted.modal.is_some());
    let harness = Framed::harness_mut(&mut mounted);
    harness.type_text("hi");
    harness.relayout();
    assert_eq!(
        mounted.modal.as_ref().unwrap().dialog.content().value(),
        "hi"
    );
    let harness = Framed::harness_mut(&mut mounted);
    harness.key("Escape");
    harness.relayout();
    assert!(mounted.modal.is_none(), "Escape closes the dialog");
    assert!(
        mounted.core().store.feeds.is_empty(),
        "and subscribes to nothing"
    );
}

fn check_add_dialog_enter_subscribes() {
    let mut a = FeedsApp::new();
    let mut doc = synced(&mut a);
    let (worker, _tx) = FetchWorker::for_test();
    a.core_mut().worker = Some(worker);
    a.core_mut().open_add_subscription();
    a.take_pending_modal();
    assert!(a.take_structure_dirty(), "opening rebuilds the tree");
    let url = "https://example.invalid/feeds.xml";
    a.modal
        .as_mut()
        .unwrap()
        .dialog
        .content_mut()
        .set_value(url);
    assert!(dispatch(&mut a, &mut doc, &named_key(NamedKey::Enter)));
    assert!(a.modal.is_none());
    assert!(FeedsDoc::load(&doc).feeds.iter().any(|f| f.url == url));
    assert_eq!(
        a.core().toast_visible(),
        Some(format!("Subscribing to {url}").as_str())
    );
}

fn check_add_dialog_an_empty_required_field_keeps_the_dialog_open() {
    let mut a = FeedsApp::new();
    let mut doc = synced(&mut a);
    a.core_mut().open_add_folder();
    a.take_pending_modal();
    a.submit_modal(&mut ui::widget::Edits::lend(&mut doc));
    assert!(a.modal.is_some(), "nothing to create yet");
    a.modal
        .as_mut()
        .unwrap()
        .dialog
        .content_mut()
        .set_value("Later");
    a.submit_modal(&mut ui::widget::Edits::lend(&mut doc));
    assert!(a.modal.is_none());
    let folder = a
        .core()
        .store
        .folders
        .iter()
        .find(|f| f.name == "Later")
        .map(|f| f.id)
        .expect("created folder");
    assert_eq!(a.active_filter(), Filter::Folder(folder));
    // The managed record owns Undo; a detached view has no writer history.
    let mut host = ui::prelude::TestApp::<crate::resource::FeedsShell>::new().unwrap();
    let versions_before = host.versions().len();
    host.with_widget(|app: &mut FeedsApp| {
        app.core_mut().open_add_folder();
        app.take_pending_modal();
    })
    .unwrap();
    let field = host.harness().a11y_node_named("Folder name").unwrap();
    host.harness()
        .perform_a11y_action(field, &A11yAction::SetValue("Later".into()))
        .unwrap();
    let create = host.harness().a11y_node_named("Create folder").unwrap();
    host.harness()
        .perform_a11y_action(create, &A11yAction::Activate)
        .unwrap();
    host.settle();
    assert!(
        host.state()
            .store
            .folders
            .iter()
            .any(|folder| folder.name == "Later")
    );
    assert_eq!(
        host.versions().len(),
        versions_before + 1,
        "the dialog submits one canonical action"
    );
    assert!(host.undo(), "the record owns the undo step");
    assert!(host.state().store.folders.is_empty());
}
