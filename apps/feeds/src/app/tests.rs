#![allow(clippy::float_cmp, reason = "tests assert exact values")]

use super::*;
use crate::model::{BlockKind, ContentBlock, Feed, Fetched};
use automerge::ReadDoc as _;
use ui::headless::Framed;
use ui::portal::PortalOutcome;
use ui::tilo_resource::Resource as _;
use ui::widget::Widget;
use ui::window_toolbar::{ToolbarAction, ToolbarEvent};
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

/// A new library's document: stamped with the storage version it is
/// written at, as the framework creates one.
fn new_library() -> AutoCommit {
    let mut doc = AutoCommit::new();
    ui::tilo_resource::evolution::write_version(&mut doc, Library::VERSION).unwrap();
    doc.commit();
    doc
}

fn synced(app: &mut FeedsApp) -> AutoCommit {
    sync_over(app, new_library())
}

/// Sync `app` over `doc`, writing what a component fixture writes.
fn sync_over(app: &mut FeedsApp, mut doc: AutoCommit) -> AutoCommit {
    app.attach_framework(ui::framework::Dispatcher::detached(
        "so.tilo.feeds",
        "component-test-author",
    ));
    // A component fixture writes the same field edits a capture applies;
    // the live app never seeds its document during sync.
    for edit in crate::resource::whole(&app.core().store) {
        edit.apply_local(&mut doc).unwrap();
    }
    app.sync(&doc, &automerge::ROOT);
    doc
}

fn loaded(doc: &AutoCommit) -> Library {
    Library::read(doc).unwrap()
}

/// Show the window what `doc` holds now, as the framework presents it.
fn present(app: &FeedsApp, doc: &AutoCommit) {
    app.present_library(Rc::new(loaded(doc)));
}

/// An article a refresh fetched, titled `title`.
fn fetched(title: &str, published: i64, blocks: Vec<ContentBlock>) -> Fetched {
    Fetched {
        title: title.into(),
        author: "Reporter".into(),
        url: format!("https://example.com/{}", title.replace(' ', "-")),
        published,
        summary: "A summary".into(),
        blocks,
    }
}

/// The id of the `n`th subscription the window shows.
fn nth_feed(app: &FeedsApp, n: usize) -> Id {
    app.core().store.feeds.ids().nth(n).unwrap().clone()
}

fn synth_device() -> winit::event::DeviceId {
    winit::event::DeviceId::dummy()
}

fn cursor_at(p: Point) -> WindowEvent {
    ui::synthetic_input::cursor_moved(p.x, p.y)
}

fn press(button: MouseButton) -> WindowEvent {
    ui::synthetic_input::mouse_input(ui::winit::event::ElementState::Pressed, button)
}

fn key_event(key: Key, text: Option<&str>) -> WindowEvent {
    ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        key,
        text.map(Into::into),
        ElementState::Pressed,
    ))
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
    let mut fresh = new_library();
    let created = fresh.get_heads();
    let mut doc = sync_over(&mut live, fresh);
    assert!(live.core().store.feeds.is_empty());
    assert!(
        doc.get(automerge::ROOT, "settings").unwrap().is_none(),
        "opening writes nothing: the default reading settings stand unwritten"
    );
    assert_eq!(
        doc.get_heads(),
        created,
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
            .article(&selected)
            .expect("the open article")
            .blocks
            .is_empty()
    );
    let doc = synced(&mut demo);
    assert_eq!(loaded(&doc).feeds.len(), demo.core().store.feeds.len());
}

/// The reader's commands as the window's bar holds them.
fn bar_actions() -> Vec<ToolbarAction> {
    ui::window_toolbar::declared()
        .map(|bar| bar.actions)
        .unwrap_or_default()
}

fn bar_action(name: &str) -> ToolbarAction {
    bar_actions()
        .into_iter()
        .find(|action| action.name == name)
        .unwrap_or_else(|| panic!("{name} is in the window bar: {:?}", bar_actions()))
}

/// The empty library disables what has nothing to act on and offers one
/// action in its empty state; the bar offers no commands for an article
/// that is not there.
#[test]
fn an_empty_library_disables_its_commands_and_offers_to_add_a_feed() {
    let mut mounted = framed(FeedsApp::new());
    Framed::harness_mut(&mut mounted).present_in_window_bar();
    settle(&mut mounted);
    // Disabled, Refresh says why rather than doing nothing on a press.
    let refresh = bar_action("There are no subscriptions to refresh");
    assert!(!refresh.enabled);
    assert!(
        bar_actions()
            .iter()
            .all(|action| action.name != "Refresh all feeds"),
        "with nothing subscribed, Refresh is not offered as if it could run"
    );
    assert!(!bar_action("Mark all as read").enabled);
    assert!(bar_action("Add subscription").enabled);
    for absent in ["Star article", "Open original article", "Reading settings"] {
        assert!(
            bar_actions().iter().all(|action| action.name != absent),
            "{absent} is not offered without an article"
        );
    }
    let controls = Framed::harness(&mounted).controls();
    let add = controls
        .iter()
        .find(|c| c.name.as_deref() == Some("Add feed"))
        .and_then(|c| c.bounds)
        .expect("the empty state offers Add feed")
        .center();
    Framed::harness_mut(&mut mounted).click_at(add.x, add.y);
    settle(&mut mounted);
    let (a, _) = Framed::split(&mut mounted);
    assert_eq!(
        a.modal.as_ref().map(|m| m.intent.clone()),
        Some(ModalIntent::AddSubscription),
        "the empty state's action opens the add dialog"
    );
}

#[test]
fn an_open_article_offers_its_commands() {
    let mut mounted = framed(FeedsApp::new_demo());
    Framed::harness_mut(&mut mounted).present_in_window_bar();
    settle(&mut mounted);
    let star = bar_actions()
        .into_iter()
        .find(|action| action.name == "Star article" || action.name == "Remove star")
        .expect("the open article can be starred");
    assert!(star.enabled);
}

/// New folder heads the sidebar's Feeds, where folders are made, not the
/// window bar, and opens its dialog from there.
#[test]
fn new_folder_heads_the_sidebars_feeds_and_opens_its_dialog() {
    let mut mounted = framed(FeedsApp::new_demo());
    Framed::harness_mut(&mut mounted).present_in_window_bar();
    settle(&mut mounted);
    let names: Vec<String> = bar_actions()
        .iter()
        .map(|action| action.name.clone())
        .collect();
    let drawn = Framed::harness(&mounted).controls();
    assert!(
        names.iter().all(|name| *name != "New folder"),
        "New folder is the sidebar's, not the bar's: {names:?}"
    );
    let new_folder = drawn
        .iter()
        .find(|c| c.name.as_deref() == Some("New folder Feeds"))
        .expect("the sidebar's Feeds heading holds New folder")
        .id
        .clone();
    Framed::harness_mut(&mut mounted)
        .operate_control(&new_folder, &ui::headless::ControlInput::Press)
        .expect("New folder takes a press");
    settle(&mut mounted);
    assert_eq!(
        mounted.modal.as_ref().map(|m| m.intent.clone()),
        Some(ModalIntent::AddFolder),
        "the sidebar's New folder opens its dialog"
    );
}

/// The reader's commands are the window bar's: the window draws no header
/// row of its own, the open article's commands lead, the library's follow
/// in the bar's menu, and each runs from there.
#[test]
fn the_readers_commands_are_the_window_bars_and_run_from_it() {
    let mut mounted = framed(FeedsApp::new_demo());
    Framed::harness_mut(&mut mounted).present_in_window_bar();
    settle(&mut mounted);
    let names: Vec<String> = bar_actions()
        .iter()
        .map(|action| action.name.clone())
        .collect();
    let row: Vec<String> = bar_actions()
        .into_iter()
        .filter(|action| !action.menu)
        .map(|action| action.name)
        .collect();
    let starred = mounted.core().open_article().is_some_and(|a| a.starred);
    let star = if starred {
        "Remove star"
    } else {
        "Star article"
    };
    assert_eq!(
        row,
        [star, "Open original article", "Reading settings"]
            .map(str::to_owned)
            .to_vec(),
        "the open article's commands lead: {names:?}"
    );
    for in_menu in [
        "Mark as read",
        "Edit tags…",
        "Refresh all feeds",
        "Mark all as read",
        "Add subscription",
        "Import OPML…",
        "Export OPML…",
    ] {
        let action = bar_action(in_menu);
        assert!(action.menu, "{in_menu} is in the bar's menu");
    }
    assert!(
        names.iter().all(|name| !name.contains("Search")),
        "the bar's own search is the only one: {names:?}"
    );
    let drawn = Framed::harness(&mounted).controls();
    for command in [
        "Star article",
        "Reading settings",
        "Mark all as read",
        "More",
    ] {
        assert!(
            drawn.iter().all(|c| c.name.as_deref() != Some(command)),
            "the window draws no {command} of its own"
        );
    }

    // Reading settings opens its panel from the bar.
    Framed::harness_mut(&mut mounted).press_window_slot(ToolbarEvent::Action {
        id: bar_action("Reading settings").id,
    });
    settle(&mut mounted);
    assert!(
        mounted.settings_panel_open(),
        "the bar opened the reading settings"
    );

    // Star flips the open article and the bar names what a press does now.
    let starred = mounted.core().open_article().is_some_and(|a| a.starred);
    let star = if starred {
        "Remove star"
    } else {
        "Star article"
    };
    Framed::harness_mut(&mut mounted).press_window_slot(ToolbarEvent::Action {
        id: bar_action(star).id,
    });
    settle(&mut mounted);
    // The library presents the edit the press made, as it does each one.
    let (a, st) = Framed::split(&mut mounted);
    present(a, &st.doc);
    settle(&mut mounted);
    assert_eq!(
        mounted.core().open_article().map(|a| a.starred),
        Some(!starred),
        "the bar's star flipped the article"
    );
    let now = if starred {
        "Star article"
    } else {
        "Remove star"
    };
    assert_eq!(bar_action(now).on, !starred);
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

        let feed_id = nth_feed(&a, 0);
        let fresh = fetched("Fresh from the worker", a.core().now, vec![]);
        let before = a.core().store.articles.len();
        tx.send(FetchResult::Articles {
            feed_id: feed_id.clone(),
            title: None,
            articles: vec![fresh.clone()],
            certificate_pin: None,
        })
        .unwrap();
        assert!(a.core_mut().drain_fetches_live());
        assert_eq!(a.core().refresh.done, 1);
        assert_eq!(a.core().refresh.new_items, 1);
        assert!(
            !a.core().pending_landings.is_empty(),
            "the landing is queued"
        );
        a.core_mut()
            .flush_doc_writes(&mut ui::widget::Edits::lend(&mut doc));
        assert!(a.core().pending_landings.is_empty());
        present(&a, &doc);
        assert_eq!(a.core().store.articles.len(), before + 1);
        assert!(
            loaded(&doc)
                .articles
                .values()
                .any(|x| x.title == fresh.title && x.feed_id == feed_id)
        );

        tx.send(FetchResult::Failed {
            feed_id: Some(nth_feed(&a, 1)),
            reason: RefreshFailure::Unreachable,
            message: "request: connection refused".into(),
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
        let feed_id = nth_feed(&a, 0);
        assert_eq!(a.core().store.feed(&feed_id).unwrap().certificate_pin, None);
        for presented in ["first", "impostor"] {
            tx.send(FetchResult::Articles {
                feed_id: feed_id.clone(),
                title: None,
                articles: vec![],
                certificate_pin: Some(presented.into()),
            })
            .unwrap();
            a.core_mut().drain_fetches_live();
            a.core_mut()
                .flush_doc_writes(&mut ui::widget::Edits::lend(&mut doc));
            present(&a, &doc);
        }
        let pinned = |library: &Library| {
            library
                .feeds
                .get(&feed_id)
                .and_then(|f: &Feed| f.certificate_pin.clone())
        };
        assert_eq!(pinned(&a.core().store).as_deref(), Some("first"));
        assert_eq!(
            pinned(&loaded(&doc)).as_deref(),
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
        let feed_ids: Vec<Id> = a.core().store.feeds.ids().cloned().collect();
        let now = a.core().now;
        let mut total = 0usize;
        for fid in &feed_ids {
            let articles: Vec<Fetched> = (0..100)
                .map(|i| {
                    fetched(
                        &format!("Article {fid} {i}"),
                        now - i64::from(i) * 60,
                        vec![ContentBlock::new(BlockKind::Paragraph, "Body.")],
                    )
                })
                .collect();
            total += articles.len();
            tx.send(FetchResult::Articles {
                feed_id: fid.clone(),
                title: None,
                articles,
                certificate_pin: None,
            })
            .unwrap();
        }
        let heads = doc.get_heads();
        a.core_mut().drain_fetches_live();
        assert_eq!(doc.get_heads(), heads, "the drain persists nothing");
        let queued = |a: &FeedsApp| -> usize {
            a.core()
                .pending_landings
                .iter()
                .map(|landing| landing.fetched.articles.len())
                .sum()
        };
        let before = queued(&a);
        assert!(before >= total);
        let articles = loaded(&doc).articles.len();
        a.core_mut()
            .flush_doc_writes(&mut ui::widget::Edits::lend(&mut doc));
        assert!(before - queued(&a) <= FLUSH_BUDGET);
        assert!(loaded(&doc).articles.len() - articles <= FLUSH_BUDGET);
        let mut frames = 0;
        while !a.core().pending_landings.is_empty() && frames < 5000 {
            a.core_mut()
                .flush_doc_writes(&mut ui::widget::Edits::lend(&mut doc));
            frames += 1;
        }
        assert!(loaded(&doc).articles.len() >= total);
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
        let feeds: Vec<Id> = a.core().store.feeds.ids().cloned().collect();
        for (i, f) in feeds.iter().enumerate() {
            let result = if i % 2 == 0 {
                FetchResult::Articles {
                    feed_id: f.clone(),
                    title: None,
                    articles: vec![],
                    certificate_pin: None,
                }
            } else {
                FetchResult::Failed {
                    feed_id: Some(f.clone()),
                    reason: RefreshFailure::Refused { status: Some(503) },
                    message: "HTTP 503".into(),
                }
            };
            tx.send(result).unwrap();
        }
        // The monotonic clock is pinned, so the summary cannot age out
        // between finishing and the check.
        ui::time::testing::with_monotonic_time(ui::time::now(), || {
            a.core_mut().drain_fetches_live();
            assert!(!a.core().refresh.in_progress());
            assert!(a.core().refresh.visible(), "the summary lingers");
        });
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
        let a = app();
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

/// A refresh asked for while one runs fetches nothing twice and keeps its
/// count, and a subscription added meanwhile joins it: the batch ends, and
/// an agent's refresh is answered, only once every feed in flight is back.
#[test]
fn a_running_refresh_waits_for_every_feed_it_fetches() {
    let mut a = app();
    let mut doc = synced(&mut a);
    let (worker, tx) = FetchWorker::for_test();
    a.core_mut().worker = Some(worker);
    let pending = |a: &FeedsApp| a.core().worker.as_ref().map_or(0, FetchWorker::pending);
    let feeds: Vec<Id> = a.core().store.feeds.ids().cloned().collect();

    assert!(dispatch(&mut a, &mut doc, &char_key("r")));
    assert!(a.core().refresh.in_progress());
    assert_eq!(pending(&a), feeds.len());
    assert!(dispatch(&mut a, &mut doc, &char_key("r")));
    assert_eq!(
        pending(&a),
        feeds.len(),
        "no feed in flight is fetched again"
    );
    assert_eq!(a.core().refresh.total, feeds.len());

    let url = "https://example.invalid/added-meanwhile.xml";
    a.core_mut()
        .subscribe(url, &mut ui::widget::Edits::lend(&mut doc));
    present(&a, &doc);
    let added = a.core().store.feed_at(url).expect("subscribed").id.clone();
    assert_eq!(pending(&a), feeds.len() + 1, "the new feed is fetched");
    assert_eq!(a.core().refresh.total, feeds.len() + 1);

    let answered = |feed: &Id| FetchResult::Articles {
        feed_id: feed.clone(),
        title: None,
        articles: vec![],
        certificate_pin: None,
    };
    tx.send(answered(&added)).unwrap();
    a.core_mut().drain_fetches_live();
    assert!(
        a.core().refresh.in_progress(),
        "{}",
        a.core().refresh.status()
    );
    for feed in &feeds {
        tx.send(answered(feed)).unwrap();
    }
    a.core_mut().drain_fetches_live();
    assert!(!a.core().refresh.in_progress());
    assert_eq!(a.core().refresh.done, feeds.len() + 1);
    assert_eq!(pending(&a), 0);
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
    let settings = a.read_settings_for_test();
    assert_eq!(settings.font, ReadFont::Sans);
    assert_eq!(settings.theme, ReadTheme::Sepia);
    assert!((settings.font_scale - 1.2).abs() < 0.001);

    let unread = a
        .core()
        .store
        .articles_for(&Filter::All)
        .into_iter()
        .find(|x| !x.read)
        .map(|x| x.id.clone())
        .expect("an unread article");
    a.core_mut()
        .select_article(&unread, &mut ui::widget::Edits::lend(&mut doc));
    assert!(loaded(&doc).article(&unread).is_some_and(|x| x.read));

    let id = nth_article(&a, 0);
    let before = a.core().store.article(&id).is_some_and(|x| x.starred);
    a.core_mut()
        .toggle_star(&id, &mut ui::widget::Edits::lend(&mut doc));
    assert_eq!(loaded(&doc).article(&id).map(|x| x.starred), Some(!before));

    a.core_mut().set_filter(Filter::Starred);
    let selected = a.selected_article();
    assert!(
        a.core()
            .store
            .articles_for(&Filter::Starred)
            .iter()
            .any(|x| Some(x.id) == selected.as_ref())
    );

    a.core_mut().set_filter(Filter::All);
    a.core_mut()
        .mark_all_read(&mut ui::widget::Edits::lend(&mut doc));
    present(&a, &doc);
    assert_eq!(a.core().store.total_unread(), 0);
    assert_eq!(loaded(&doc).total_unread(), 0, "marked read in the library");
}

/// The id of the `n`th article the library holds.
fn nth_article(app: &FeedsApp, n: usize) -> Id {
    app.core().store.articles.ids().nth(n).unwrap().clone()
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
    let id = a.core().list.rows[0].id.clone();
    assert!(has_target(&list, &format!("feeds.article:{id}")));
    assert!(has_target(&list, &format!("feeds.star:{id}")));
    let before = a.core().store.article(&id).is_some_and(|x| x.starred);
    assert!(a.perform_semantic_action(
        &format!("feeds.star:{id}"),
        &A11yAction::ToggleCheck,
        &mut ui::widget::Edits::lend(&mut st.doc),
        &st.root,
    ));
    present(a, &st.doc);
    assert_ne!(
        a.core().store.article(&id).is_some_and(|x| x.starred),
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
        .article_rows()
        .find(|x| !x.tags.is_empty())
        .map(|x| (x.id.clone(), crate::model::tag_key(&x.tag_list()[0])))
        .expect("a tagged sample article");
    a.core_mut().selected = Some(tagged.0.clone());
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

#[test]
fn the_tag_editor_uses_the_canonical_action() {
    let mut a = app();
    let mut doc = synced(&mut a);
    let id = a.selected_article().expect("an open article");
    a.stage_tag_editor();
    a.take_pending_modal();
    assert_eq!(
        a.modal.as_ref().map(|m| m.intent.clone()),
        Some(ModalIntent::EditArticleTags {
            article_id: id.clone()
        })
    );
    a.modal
        .as_mut()
        .unwrap()
        .dialog
        .content_mut()
        .set_value("Design, Later, design");
    a.submit_modal(&mut ui::widget::Edits::lend(&mut doc));
    assert_eq!(
        loaded(&doc).article(&id).unwrap().tag_list(),
        ["design", "later"]
    );
    present(&a, &doc);
    a.stage_tag_editor();
    a.take_pending_modal();
    a.modal.as_mut().unwrap().dialog.content_mut().set_value("");
    a.submit_modal(&mut ui::widget::Edits::lend(&mut doc));
    assert!(loaded(&doc).article(&id).unwrap().tags.is_empty());
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

    // 800 wide: the sidebar no longer docks beside the list and a reading
    // column, so it becomes a screen of its own; the list and article stay
    // side by side. Feeds reads, it is not a document: no floating drawer.
    let mut mounted = framed_at(app(), (800, 600));
    settle(&mut mounted);
    let geometry = mounted.shell().geometry().clone();
    assert_eq!(geometry.sidebar, SidebarPlacement::Screen);
    assert!(geometry.side_by_side);
    assert_eq!(geometry.stack, vec![NavPane::Sidebar, NavPane::Content]);
    mounted.stage("sidebar-open");
    settle(&mut mounted);
    assert!(mounted.shell().is_sidebar_open());
    // Choosing a place moves on to its articles.
    {
        let (a, st) = Framed::split(&mut mounted);
        a.requests.borrow_mut().push(Request::Show(Filter::Unread));
        a.settle(&mut ui::widget::Edits::lend(&mut st.doc), &st.root);
    }
    assert!(!mounted.shell().is_sidebar_open());

    // Narrow: sidebar, list and article are push screens; choosing an
    // article shows it.
    let mut mounted = framed_at(app(), (520, 820));
    settle(&mut mounted);
    assert_eq!(
        mounted.shell().geometry().stack,
        vec![NavPane::Sidebar, NavPane::Overview, NavPane::Content]
    );
    {
        let (a, st) = Framed::split(&mut mounted);
        a.shell_mut().show(NavPane::Overview);
        a.shell_mut().snap();
        let id = a.core().store.articles_for(&Filter::All)[1].id.clone();
        a.core_mut()
            .select_article(&id, &mut ui::widget::Edits::lend(&mut st.doc));
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
    // The host reads and writes the chosen file as background work; pump it
    // so its answer is there when the app next looks.
    let work = ui::background::Pumped::new();
    let _work = work.background().enter();
    let export_with = |a: &FeedsApp, outcome: PortalOutcome| {
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(outcome);
        ui::host::scoped(&services, || a.core_mut().export_opml());
        work.run_pending();
        let picks = recorder.pick_requests();
        assert_eq!(picks.len(), 1);
        assert_eq!(picks[0].title(), "Export OPML");
    };
    let import_with = |a: &FeedsApp, outcome: PortalOutcome| {
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(outcome);
        ui::host::scoped(&services, || a.core_mut().request_import());
        work.run_pending();
        let picks = recorder.pick_requests();
        assert_eq!(picks.len(), 1);
        assert_eq!(picks[0].title(), "Import OPML");
    };

    // --- phase: import adds every feed and persists; re-import adds none ---
    {
        let mut a = app();
        let mut doc = synced(&mut a);
        let before = a.core().store.feeds.len();
        let added = a
            .core_mut()
            .apply_opml_import(opml::sample_opml(), &mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(added.len(), 3);
        present(&a, &doc);
        assert_eq!(a.core().store.feeds.len(), before + 3);
        let library = loaded(&doc);
        assert!(library.feed_at("https://jvns.ca/atom.xml").is_some());
        assert!(library.folders.values().any(|f| f.name == "Reading"));
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
        let directory = std::env::temp_dir().join(format!("feeds-export-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("Subscriptions Grün 🌍.opml");
        let xml = opml::export(&a.core().store);
        export_with(&a, PortalOutcome::Opened(path.clone()));
        assert!(a.next_wake_deadline().is_some());
        a.sync(&doc, &automerge::ROOT);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), xml);
        assert!(a.core().export_rx.is_none());
        assert!(
            a.shell()
                .toast_text()
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
        let message = a.shell().toast_text().map(str::to_owned).unwrap();
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
        let dir = std::env::temp_dir().join(format!("feeds-opml-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("subs.opml");
        std::fs::write(&path, opml::sample_opml()).unwrap();
        let before = a.core().store.feeds.len();
        import_with(&a, PortalOutcome::Opened(path));
        a.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        a.sync(&doc, &automerge::ROOT);
        assert!(a.core().import_rx.is_none());
        present(&a, &doc);
        assert_eq!(a.core().store.feeds.len(), before + 3);
        assert_eq!(
            a.core().refresh.total,
            3,
            "the imported subscriptions are refreshed once the library holds them"
        );
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
#[test]
fn the_feed_menu_opens_on_its_row_and_runs_its_actions() {
    let mut mounted = framed(app());
    settle(&mut mounted);
    let (feed, row) = mounted
        .core()
        .first_feed_row
        .clone()
        .expect("a painted feed row");

    // Right click on the row.
    let harness = Framed::harness_mut(&mut mounted);
    harness.dispatch_to_root(&cursor_at(row.center()));
    harness.dispatch_to_root(&press(MouseButton::Right));
    harness.relayout();
    {
        let core = mounted.core();
        let open = core.menu.as_ref().expect("the right click opened the menu");
        assert_eq!(open.kind, MenuKind::Feed(feed.clone()));
        assert_eq!(
            open.actions,
            vec![
                MenuAction::MarkFeedRead(feed.clone()),
                MenuAction::RenameFeed(feed.clone()),
                MenuAction::Unsubscribe(feed.clone()),
            ]
        );
    }

    // The menu owns article shortcuts while it is open.
    let (a, st) = Framed::split(&mut mounted);
    let doc = &mut st.doc;
    let before: Vec<_> = a
        .core()
        .store
        .article_rows()
        .map(|x| (x.id.clone(), x.read, x.starred))
        .collect();
    for key in ["m", "s", "j", "k"] {
        dispatch(a, doc, &char_key(key));
    }
    let after: Vec<_> = a
        .core()
        .store
        .article_rows()
        .map(|x| (x.id.clone(), x.read, x.starred))
        .collect();
    assert_eq!(before, after, "the menu captures article shortcuts");
    assert!(a.core().menu.is_some());

    // Arrow down to Rename and Enter opens the rename dialog.
    dispatch(a, doc, &named_key(NamedKey::ArrowDown));
    dispatch(a, doc, &named_key(NamedKey::ArrowDown));
    dispatch(a, doc, &named_key(NamedKey::Enter));
    assert!(a.core().menu.is_none());
    assert_eq!(
        a.modal.as_ref().map(|m| m.intent.clone()),
        Some(ModalIntent::RenameFeed {
            feed_id: feed.clone()
        })
    );
    a.modal
        .as_mut()
        .unwrap()
        .dialog
        .content_mut()
        .set_value("My feed");
    a.submit_modal(&mut ui::widget::Edits::lend(doc));
    assert_eq!(loaded(doc).feed(&feed).unwrap().title, "My feed");

    // Losing focus dismisses an open menu.
    a.core_mut().open_feed_menu(feed.clone(), row);
    assert!(dispatch(a, doc, &WindowEvent::Focused(false)) || a.core().menu.is_none());
    assert!(a.core().menu.is_none());

    a.core_mut().apply_menu_action(
        MenuAction::MarkFeedRead(feed.clone()),
        &mut ui::widget::Edits::lend(doc),
    );
    present(a, doc);
    assert_eq!(a.core().store.unread_for_feed(&feed), 0);
    assert_eq!(loaded(doc).unread_for_feed(&feed), 0);

    // Undo takes an unsubscription back, so it runs at once: no dialog of
    // the window's own and nothing asked by the host (the resource tests
    // drive it through the host, Undo included).
    a.core_mut().apply_menu_action(
        MenuAction::Unsubscribe(feed),
        &mut ui::widget::Edits::lend(doc),
    );
    a.take_pending_modal();
    assert!(a.modal.is_none(), "the window asks nothing itself");

    // The menu stays inside a shrinking window and captures host undo.
    let other = nth_feed(a, 0);
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
    let feed = nth_feed(&a, 1);
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
        .find(|i| i.id == crate::sidebar::nav_id(&Filter::Feed(feed.clone())))
        .expect("the feed's row");
    let unread = a.core().store.unread_for_feed(&feed);
    assert_eq!(
        item.count,
        Some(ui::chrome::NavCount::Neutral(
            u32::try_from(unread).unwrap()
        ))
    );

    a.requests
        .borrow_mut()
        .push(Request::Show(Filter::Feed(feed.clone())));
    a.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
    assert_eq!(a.active_filter(), Filter::Feed(feed.clone()));
    let title = a.core().store.feed(&feed).unwrap().title.clone();
    assert_eq!(
        ui::window_toolbar::declared()
            .and_then(|bar| bar.location)
            .map(|location| location.label),
        Some(title),
        "the window bar names the chosen feed"
    );
}

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

#[test]
fn the_list_scrolls_and_keeps_the_open_article_in_view() {
    let a = app();
    // Enough articles to overflow the window.
    {
        let mut core = a.core_mut();
        let feed = core.store.feeds.ids().next().unwrap().clone();
        let now = core.now;
        for i in 0..40 {
            let archived = Article::fetched(
                feed.clone(),
                fetched(
                    &format!("Archive {i}"),
                    now - 86_400 * 30 - i64::from(i),
                    vec![],
                ),
            );
            Rc::make_mut(&mut core.store)
                .articles
                .push(Id::from_raw(format!("archive-{i}")), archived);
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
    let first = a.core().store.articles_for(&Filter::All)[0].id.clone();
    a.core_mut()
        .select_article(&first, &mut ui::widget::Edits::lend(&mut st.doc));
    settle(&mut mounted);
    assert_eq!(mounted.core().list.scroll(), 0.0);
}

/// Links open only web, Gemini and mail addresses.
#[test]
fn article_links_reject_local_files() {
    let mut reader = FeedsApp::new();
    reader
        .core_mut()
        .open_url("file:///does-not-exist-tilo-link-proof");
    reader.take_toast();
    assert_eq!(
        reader.shell().toast_text(),
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
    assert!(loaded(&doc).feed_at(url).is_some());
    assert_eq!(
        a.shell().toast_text(),
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
    present(&a, &doc);
    let folder = a
        .core()
        .store
        .folders
        .iter()
        .find(|(_, f)| f.name == "Later")
        .map(|(id, _)| id.clone())
        .expect("created folder");
    assert_eq!(
        a.active_filter(),
        Filter::Folder(folder),
        "the new folder is shown once the library holds it"
    );
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
            .folders
            .values()
            .any(|folder| folder.name == "Later")
    );
    assert_eq!(
        host.versions().len(),
        versions_before + 1,
        "the dialog submits one canonical action"
    );
    assert!(host.undo(), "the record owns the undo step");
    assert!(host.state().folders.is_empty());
}

/// Draw frames the way a window's host does after input: the frame the
/// input asked for, then one more only while the app asks for it.
fn pump_frames(host: &mut ui::prelude::TestApp<crate::resource::FeedsShell>) {
    host.harness().relayout();
    for _ in 0..32 {
        let schedule = host.harness().frame_schedule();
        if !(schedule.needs_animation_frame || schedule.queued_work) {
            return;
        }
        host.harness().relayout();
    }
}

/// How wide the sidebar stands on screen, as the shell last laid it out.
fn sidebar_width(host: &mut ui::prelude::TestApp<crate::resource::FeedsShell>) -> f64 {
    host.with_widget(|app: &mut FeedsApp| app.shell().geometry().sidebar_column.width())
        .unwrap()
}

#[test]
fn the_collapse_button_collapses_and_expands_the_sidebar() {
    // Every motion reads as arrived: a frame that comes after the slide's
    // whole time (a busy device), where the collapse lands in one tick.
    let _arrived = ui::motion::testing::settled();
    let mut host = ui::prelude::TestApp::<crate::resource::FeedsShell>::new().unwrap();
    host.harness().present_in_window_bar();
    pump_frames(&mut host);
    let open = sidebar_width(&mut host);
    assert!(open > 0.0, "a wide window docks its sidebar");
    assert_eq!(
        ui::window_toolbar::declared().and_then(|bar| bar.sidebar),
        Some(true),
        "the window bar offers Hide sidebar"
    );
    host.harness()
        .press_window_slot(ToolbarEvent::ToggleSidebar);
    pump_frames(&mut host);
    assert!(
        sidebar_width(&mut host) < 0.5,
        "the bar's Hide sidebar collapsed the sidebar on screen"
    );
    assert_eq!(
        ui::window_toolbar::declared().and_then(|bar| bar.sidebar),
        Some(false),
        "the window bar offers Show sidebar"
    );
    host.harness()
        .press_window_slot(ToolbarEvent::ToggleSidebar);
    pump_frames(&mut host);
    assert!(
        (sidebar_width(&mut host) - open).abs() < 0.5,
        "the bar's Show sidebar expanded it again"
    );
}

#[test]
fn the_sidebar_shortcut_collapses_and_expands_the_sidebar() {
    let _arrived = ui::motion::testing::settled();
    let mut host = ui::prelude::TestApp::<crate::resource::FeedsShell>::new().unwrap();
    host.harness().present_in_window_bar();
    pump_frames(&mut host);
    let open = sidebar_width(&mut host);
    assert!(open > 0.0);
    host.harness()
        .press_window_slot(ToolbarEvent::ToggleSidebar);
    pump_frames(&mut host);
    // The sidebar's shortcut is Command+Shift+L: Cmd on macOS, Ctrl elsewhere.
    host.harness()
        .set_modifiers(&["command".to_owned(), "shift".to_owned()]);
    // Collapsed from the bar; the shortcut brings it back and takes it away.
    host.harness().key("L");
    pump_frames(&mut host);
    assert!(
        (sidebar_width(&mut host) - open).abs() < 0.5,
        "the shortcut expanded it"
    );
    host.harness().key("L");
    pump_frames(&mut host);
    assert!(sidebar_width(&mut host) < 0.5, "the shortcut collapsed it");
}

/// In a half-width window, where the sidebar is a screen of its own, the
/// window bar's Show sidebar brings it up from the article list: the places
/// are not reachable only through Back.
#[test]
fn show_sidebar_brings_up_the_places_in_a_half_width_window() {
    let libraries: [(fn() -> FeedsApp, u32); 2] = [(FeedsApp::new, 640), (app, 640)];
    for (library, width) in libraries {
        let mut mounted = framed_at(library(), (width, 800));
        Framed::harness_mut(&mut mounted).present_in_window_bar();
        settle(&mut mounted);
        assert_eq!(
            mounted.shell().geometry().sidebar,
            SidebarPlacement::Screen,
            "{width} wide"
        );
        assert!(!mounted.shell().is_sidebar_open(), "{width} wide");
        assert_eq!(
            ui::window_toolbar::declared().and_then(|bar| bar.sidebar),
            Some(false),
            "the bar offers Show sidebar {width} wide"
        );
        Framed::harness_mut(&mut mounted)
            .press_window_slot(ui::window_toolbar::ToolbarEvent::ToggleSidebar);
        settle(&mut mounted);
        assert!(
            mounted.shell().is_sidebar_open(),
            "Show sidebar showed the places {width} wide"
        );
        assert_eq!(
            ui::window_toolbar::declared().and_then(|bar| bar.sidebar),
            Some(true),
            "the bar's toggle now hides it {width} wide"
        );
    }
}

/// The sidebar row for `feed`, with its own status and second line.
fn feed_row(core: &Core, feed: &Id) -> ui::widget::NavItem {
    fn find(items: &[ui::widget::NavItem], id: &str) -> Option<ui::widget::NavItem> {
        for item in items {
            if item.id == id {
                return Some(item.clone());
            }
            if let Some(found) = find(&item.children, id) {
                return Some(found);
            }
        }
        None
    }
    let id = crate::sidebar::nav_id(&Filter::Feed(feed.clone()));
    core.nav_sections()
        .iter()
        .find_map(|section| find(&section.items, &id))
        .unwrap_or_else(|| panic!("the sidebar holds {id}"))
}

/// A failed refresh of one subscription marks that row with an honest reason
/// and leaves the others alone; its next success clears the mark. The batch
/// summary stays a count.
#[test]
fn a_failed_subscription_says_so_in_its_own_row_until_it_refreshes() {
    let mut a = app();
    let _doc = synced(&mut a);
    let (worker, tx) = FetchWorker::for_test();
    a.core_mut().worker = Some(worker);
    let failing = nth_feed(&a, 0);
    let healthy = nth_feed(&a, 1);
    // Neither row says anything before a refresh has failed.
    assert_eq!(feed_row(&a.core(), &failing).status, None);
    assert_eq!(feed_row(&a.core(), &healthy).status, None);

    a.core_mut()
        .refresh_feeds(&[failing.clone(), healthy.clone()]);
    tx.send(FetchResult::Failed {
        feed_id: Some(failing.clone()),
        reason: RefreshFailure::Refused { status: Some(403) },
        message: "HTTP 403".into(),
    })
    .unwrap();
    tx.send(FetchResult::Articles {
        feed_id: healthy.clone(),
        title: None,
        articles: vec![],
        certificate_pin: None,
    })
    .unwrap();
    a.core_mut().drain_fetches_live();

    let marked = feed_row(&a.core(), &failing);
    assert_eq!(
        marked.status.as_ref().map(|status| status.row_status()),
        Some(ui::widget::RowStatus::Error),
        "the subscription that failed is marked"
    );
    assert_eq!(
        marked.status_label, None,
        "the reason itself is the second line, not a word beside the name"
    );
    assert_eq!(
        marked.secondary.as_deref(),
        Some("The site refused the request (HTTP 403)"),
        "the row says why, and a refusal is not a network fault"
    );
    let untouched = feed_row(&a.core(), &healthy);
    assert_eq!(
        untouched.status, None,
        "the subscription that answered is not marked"
    );
    assert_eq!(untouched.secondary, None);
    // The batch summary is a count of the batch, and stays that.
    assert_eq!(a.core().refresh.failed_feeds, 1);
    assert!(
        a.core().refresh.status().contains("could not be refreshed"),
        "{}",
        a.core().refresh.status()
    );

    // The next successful refresh of that subscription clears the mark.
    a.core_mut().refresh_feeds(std::slice::from_ref(&failing));
    tx.send(FetchResult::Articles {
        feed_id: failing.clone(),
        title: None,
        articles: vec![],
        certificate_pin: None,
    })
    .unwrap();
    a.core_mut().drain_fetches_live();
    let cleared = feed_row(&a.core(), &failing);
    assert_eq!(cleared.status, None);
    assert_eq!(cleared.secondary, None);
    assert!(a.core().feed_failures.is_empty());
}

/// The failing subscription's row says why to assistive technology too: the
/// list folds the row's second line and its error status into the description
/// a screen reader hears.
#[test]
fn the_failing_rows_reason_reaches_assistive_technology() {
    let mut a = app();
    let _doc = synced(&mut a);
    let (worker, tx) = FetchWorker::for_test();
    a.core_mut().worker = Some(worker);
    let failing = nth_feed(&a, 0);
    let title = a
        .core()
        .store
        .feed(&failing)
        .map(|feed| feed.shown_title())
        .expect("a bundled subscription");
    a.core_mut().refresh_feeds(std::slice::from_ref(&failing));
    tx.send(FetchResult::Failed {
        feed_id: Some(failing),
        reason: RefreshFailure::Unreachable,
        message: "request: connection refused".into(),
    })
    .unwrap();
    a.core_mut().drain_fetches_live();
    // The sidebar's list is built from exactly these sections.
    let list = ui::widget::NavList::new(a.core().nav_sections())
        .with_id("feeds.sidebar")
        .with_accessible_name("Subscriptions");
    let tree = Widget::accessibility(&list).expect("the subscriptions are semantics");
    let row = named_node(&tree, &title).unwrap_or_else(|| panic!("the row for {title:?}"));
    let description = row.description.clone().unwrap_or_default();
    assert!(
        description.contains("Couldn't reach the site"),
        "the row reads out why it failed: {description:?}"
    );
    assert!(
        description.contains("Error"),
        "and its status: {description:?}"
    );
}

/// The first semantic node named `name`, at any depth.
fn named_node<'a>(node: &'a AccessNode, name: &str) -> Option<&'a AccessNode> {
    if node.name.as_deref() == Some(name) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| named_node(child, name))
}
