use super::*;
use crate::shell::build_tilo_overlay_host_with_chrome;
use crate::shell::state::WorkspaceTree;
use automerge::{ReadDoc as _, ScalarValue, Value, transaction::Transactable as _};
use taffy::AvailableSpace;

fn launchpad_labels(widget: &dyn Widget) -> Option<Vec<String>> {
    if widget.name() == "CollaborationLaunchpad" {
        return widget.accessibility().map(|node| {
            node.children
                .into_iter()
                .filter_map(|child| child.name)
                .collect()
        });
    }
    widget.children().into_iter().find_map(launchpad_labels)
}

fn launchpad_rect(widget: &dyn Widget) -> Option<Rect> {
    if widget.name() == "CollaborationLaunchpad" {
        return Some(widget.get_rect());
    }
    widget.children().into_iter().find_map(launchpad_rect)
}

/// The Applications provider with Chat, Notes and Calendar installed, whose
/// index resolves nothing for its first `cold_resolves` lookups, as on a
/// fresh boot. Opening an app records its package id instead of launching.
struct FakeApplications {
    kinds: Vec<crate::shell::search::ResourceKind>,
    cold_resolves: std::cell::Cell<usize>,
    opened: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
}

impl FakeApplications {
    fn install(host: &mut ShellOverlayHost, cold_resolves: usize) -> std::rc::Rc<Self> {
        let apps = std::rc::Rc::new(Self {
            kinds: vec![crate::shell::search::ResourceKind::Application],
            cold_resolves: std::cell::Cell::new(cold_resolves),
            opened: std::rc::Rc::default(),
        });
        host.search_registry
            .register_or_replace(std::rc::Rc::clone(&apps) as _);
        apps
    }
}

impl crate::shell::search::SearchProvider for FakeApplications {
    fn id(&self) -> &'static str {
        "applications"
    }

    fn name(&self) -> &'static str {
        "Applications"
    }

    fn resource_kinds(&self) -> &[crate::shell::search::ResourceKind] {
        &self.kinds
    }

    fn query(
        &self,
        _ctx: crate::shell::search::SearchContext<'_>,
    ) -> Vec<crate::shell::search::SearchResult> {
        Vec::new()
    }

    fn resolve(
        &self,
        id: &str,
        _ctx: crate::shell::search::SearchContext<'_>,
    ) -> Option<crate::shell::search::SearchResult> {
        if self.cold_resolves.get() > 0 {
            self.cold_resolves
                .set(self.cold_resolves.get().saturating_sub(1));
            return None;
        }
        let app = ["so.tilo.chat", "so.tilo.notes", "so.tilo.calendar"]
            .into_iter()
            .find(|app| *app == id)?;
        let opened = std::rc::Rc::clone(&self.opened);
        Some(crate::shell::search::SearchResult::nullary(
            crate::shell::search::ResourceKind::Application,
            "applications",
            app,
            format!("Open {app}"),
            None,
            0,
            move |_, _, _| opened.borrow_mut().push(app),
        ))
    }
}

fn lay_out(host: &mut ShellOverlayHost, width: f32, height: f32) {
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = host.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
    let measure_map = node_map.clone();
    let mut text = TextRenderer::new();
    taffy
        .compute_layout_with_measure(
            node,
            Size {
                width: AvailableSpace::Definite(width),
                height: AvailableSpace::Definite(height),
            },
            |known, available, node_id, _, _| {
                measure_map.get(&node_id).map_or(Size::ZERO, |&ptr| unsafe {
                    (*ptr).measure(known, available, &mut text)
                })
            },
        )
        .expect("empty workspace layout");
    host.apply_layout(&taffy, node, Point::ZERO);
}

#[test]
fn empty_workspace_cards_offer_accessible_collaboration_entry_points() {
    fn bootstrap_default_exposes_accessible_collaboration_choices() {
        let tree = WorkspaceTree::bootstrap_default();
        let (mut host, _) = build_tilo_overlay_host_with_chrome(&tree);
        lay_out(&mut host, 1600.0, 900.0);
        assert_eq!(
            launchpad_labels(host.content.as_ref()).expect("launchpad is mounted"),
            [
                "Discuss",
                "Notes",
                "Tasks",
                "Start a call",
                "Invite people",
                "Catch up",
                "Open an app"
            ],
            "first use presents collaboration jobs plus the explicit app escape hatch"
        );
        let rect = launchpad_rect(host.content.as_ref()).expect("launchpad is mounted");
        assert!(
            rect.width() >= 320.0 && rect.height() >= 160.0,
            "the first-use launchpad needs visible canvas space, rect={rect:?}"
        );
    }

    fn catch_up_card_opens_room_activity_and_consumes_its_signal() {
        let tree = WorkspaceTree::bootstrap_default();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let _ = doc.put(&root, crate::shell::EMPTY_WORKSPACE_ACTIVITY, true);

        host.sync(&doc, &root);
        host.settle(&mut ui::widget::Edits::lend(&mut doc), &root);

        assert!(
            chrome.borrow_mut().take_activity_open_request().is_some(),
            "the card opens the Activity app"
        );
        assert!(
            !chrome.borrow().overlays.room_activity,
            "Activity is an app's window, not transient overlay state"
        );
        assert!(matches!(
            doc.get(&root, crate::shell::EMPTY_WORKSPACE_ACTIVITY),
            Ok(Some((Value::Scalar(value), _)))
                if matches!(value.as_ref(), ScalarValue::Boolean(false))
        ));
    }

    fn invite_card_opens_the_existing_signed_invite_composer() {
        let tree = WorkspaceTree::bootstrap_default();
        let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
        // First boot's Home once it is placed on this device's host.
        let _room = super::tests::host_active_room(&chrome);
        host.set_peer_sync_enabled(true);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let _ = doc.put(&root, crate::shell::EMPTY_WORKSPACE_INVITE, true);

        host.sync(&doc, &root);
        host.settle(&mut ui::widget::Edits::lend(&mut doc), &root);

        assert!(host.peer_sync_card_visible());
        assert_eq!(
            host.peer_sync_snapshot().invite_name.as_deref(),
            Some(""),
            "the card opens the existing recipient-first invite flow"
        );
    }

    fn open_an_app_opens_the_app_launcher_and_consumes_its_signal() {
        let tree = WorkspaceTree::bootstrap_default();
        let (mut host, _) = build_tilo_overlay_host_with_chrome(&tree);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let _ = doc.put(
            &root,
            tilo_desktop::launchpad::EMPTY_WORKSPACE_OPEN_APP,
            true,
        );

        host.sync(&doc, &root);
        host.settle(&mut ui::widget::Edits::lend(&mut doc), &root);

        assert!(host.palette_visible(), "the button opens the launcher");
        assert!(matches!(
            doc.get(&root, tilo_desktop::launchpad::EMPTY_WORKSPACE_OPEN_APP),
            Ok(Some((Value::Scalar(value), _)))
                if matches!(value.as_ref(), ScalarValue::Boolean(false))
        ));
    }

    bootstrap_default_exposes_accessible_collaboration_choices();
    open_an_app_opens_the_app_launcher_and_consumes_its_signal();
    catch_up_card_opens_room_activity_and_consumes_its_signal();
    invite_card_opens_the_existing_signed_invite_composer();
}

#[test]
fn app_cards_open_their_app_directly() {
    /// Without an action host (as in this fixture) the Tasks and call cards
    /// still open their app; with one they invoke its action, below.
    fn app_cards_open_their_app_without_the_launcher() {
        use tilo_desktop::launchpad::LaunchpadAction;
        for (card, app) in [
            (LaunchpadAction::Chat, "so.tilo.chat"),
            (LaunchpadAction::Notes, "so.tilo.notes"),
            (LaunchpadAction::Tasks, "so.tilo.calendar"),
            (LaunchpadAction::Meet, "so.tilo.chat"),
        ] {
            let tree = WorkspaceTree::bootstrap_default();
            let (mut host, _) = build_tilo_overlay_host_with_chrome(&tree);
            let apps = FakeApplications::install(&mut host, 0);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let _ = doc.put(&root, card.path(), true);

            host.sync(&doc, &root);
            host.settle(&mut ui::widget::Edits::lend(&mut doc), &root);

            assert_eq!(*apps.opened.borrow(), [app], "{card:?} opens {app}");
            assert!(
                !host.palette_visible(),
                "{card:?} performs its action instead of pre-filling the launcher"
            );
            assert!(LaunchpadAction::take(&mut ui::widget::Edits::lend(&mut doc), &root).is_none());
        }
    }

    fn a_card_chosen_before_the_app_index_is_ready_opens_once_it_is() {
        let tree = WorkspaceTree::bootstrap_default();
        let (mut host, _) = build_tilo_overlay_host_with_chrome(&tree);
        let apps = FakeApplications::install(&mut host, 3);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let _ = doc.put(&root, crate::shell::EMPTY_WORKSPACE_NOTES, true);

        host.sync(&doc, &root);
        host.settle(&mut ui::widget::Edits::lend(&mut doc), &root);
        assert!(apps.opened.borrow().is_empty());
        assert!(
            host.next_wake_deadline().is_some(),
            "a waiting card keeps the frame clock asking the index"
        );
        for _ in 0..8 {
            if !apps.opened.borrow().is_empty() {
                break;
            }
            assert!(
                !host.palette_visible(),
                "a cold index is waited out, not handed to the person as a typed launcher"
            );
            host.update(&doc, &root);
            host.settle(&mut ui::widget::Edits::lend(&mut doc), &root);
        }
        assert_eq!(*apps.opened.borrow(), ["so.tilo.notes"]);
        assert!(!host.palette_visible());
        host.update(&doc, &root);
        host.settle(&mut ui::widget::Edits::lend(&mut doc), &root);
        assert_eq!(
            apps.opened.borrow().len(),
            1,
            "one card choice opens the app once"
        );
    }

    app_cards_open_their_app_without_the_launcher();
    a_card_chosen_before_the_app_index_is_ready_opens_once_it_is();
}

/// A workspace the local person owns, a broker over it, and the live app
/// `app_id` publishing `actions` on `resource` in that workspace, with the
/// invocations the broker delivers to it.
fn host_with_live_app(
    app_id: &str,
    resource: &str,
    actions: &[&str],
    contract: &tilo_actions::ActionContract,
) -> (
    ShellOverlayHost,
    tokio::sync::mpsc::UnboundedReceiver<crate::shell::collab_broker_capnp::InvocationEnvelope>,
) {
    use crate::shell::collab_broker_capnp::{PublishedAction, PublishedInput, PublishedSurface};
    let mut tree = WorkspaceTree::bootstrap_default();
    tree.own_workspaces_for_test(crate::shell::state::local_user_id_for_room);
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let room = {
        let state = chrome.borrow();
        let tree = state.tree();
        let room = tree
            .desktop
            .active_workspace()
            .expect("an active workspace")
            .room()
            .to_string();
        drop(tree);
        room
    };
    let membership = chrome.borrow().tree_handle();
    let broker = crate::shell::broker_services::test_broker_with_membership(membership);
    broker.replace_published_surfaces_for_test(vec![PublishedSurface {
        peer_id: 9,
        app_id: app_id.into(),
        route: crate::shell::collab_broker::CrdtRoute::app(app_id, room),
        actions: actions
            .iter()
            .map(|action| PublishedAction {
                semantic_identity: None,
                resource: resource.into(),
                action: (*action).into(),
                resource_kind: resource.into(),
                resource_label: resource.into(),
                action_label: (*action).into(),
                description: None,
                operation_kind: tilo_actions::OperationKind::Effect,
                params: Vec::new(),
                contract: contract.clone(),
                input: PublishedInput::None,
                view: None,
            })
            .collect(),
        live: true,
    }]);
    let delivered = broker.capture_invocations_for_test(9);
    host.attach_shortcut_trigger_broker(broker.clone());
    drop(broker);
    (host, delivered)
}

#[test]
fn tasks_and_call_cards_invoke_their_apps_action() {
    use tilo_desktop::launchpad::LaunchpadAction;
    for (card, app, resource, action) in [
        (
            LaunchpadAction::Tasks,
            "so.tilo.calendar",
            "calendar.calendar",
            "show-tasks",
        ),
        (
            LaunchpadAction::Meet,
            "so.tilo.chat",
            "chat.workspace",
            "start-call",
        ),
    ] {
        let (mut host, mut delivered) = host_with_live_app(
            app,
            resource,
            &[action, "something-else"],
            &tilo_actions::ActionContract::default(),
        );
        let apps = FakeApplications::install(&mut host, 0);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let _ = doc.put(&root, card.path(), true);

        host.sync(&doc, &root);
        host.settle(&mut ui::widget::Edits::lend(&mut doc), &root);

        let deadline = Instant::now() + tilo_test_env::hang_bound();
        let invocation = loop {
            match delivered.try_recv() {
                Ok(invocation) => break invocation,
                Err(_) if Instant::now() < deadline => {
                    host.update(&doc, &root);
                    host.settle(&mut ui::widget::Edits::lend(&mut doc), &root);
                    std::thread::yield_now();
                }
                Err(error) => panic!("{card:?} delivered nothing to {app}: {error}"),
            }
        };
        assert_eq!(
            (invocation.resource.as_str(), invocation.action.as_str()),
            (resource, action),
            "{card:?} invokes {app}'s {action}"
        );
        assert_eq!(
            invocation.actor,
            crate::shell::state::local_user_id_for_room(
                &host
                    .chrome
                    .as_ref()
                    .expect("chrome")
                    .borrow()
                    .tree()
                    .desktop
                    .active_workspace()
                    .expect("an active workspace")
                    .room()
            ),
            "the card acts as the person this device acts for"
        );
        host.update(&doc, &root);
        host.settle(&mut ui::widget::Edits::lend(&mut doc), &root);
        assert!(
            delivered.try_recv().is_err(),
            "{card:?} invokes exactly that action, once"
        );
        assert!(
            apps.opened.borrow().is_empty(),
            "{card:?} does not open a second copy of {app} beside its action"
        );
        assert!(!host.palette_visible(), "{card:?} needs no launcher step");
        assert!(LaunchpadAction::take(&mut ui::widget::Edits::lend(&mut doc), &root).is_none());
    }
}

/// A card whose action the app refuses before anything runs says so,
/// naming the card and the reason, rather than doing nothing.
#[test]
fn a_refused_card_action_says_why() {
    let (mut host, mut delivered) = host_with_live_app(
        "so.tilo.chat",
        "chat.workspace",
        &["start-call"],
        &tilo_actions::ActionContract {
            confirmation: tilo_actions::ConfirmationPolicy::Confirm,
            ..tilo_actions::ActionContract::default()
        },
    );
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    let _ = doc.put(&root, crate::shell::EMPTY_WORKSPACE_MEET, true);

    host.sync(&doc, &root);
    host.settle(&mut ui::widget::Edits::lend(&mut doc), &root);
    let deadline = Instant::now() + tilo_test_env::hang_bound();
    while !host.pending_launchpad_actions.is_empty() && Instant::now() < deadline {
        host.update(&doc, &root);
        host.settle(&mut ui::widget::Edits::lend(&mut doc), &root);
        std::thread::yield_now();
    }

    assert!(
        host.pending_launchpad_actions.is_empty(),
        "the refusal arrived"
    );
    assert!(delivered.try_recv().is_err(), "nothing ran");
    let chrome = host.chrome.clone().expect("chrome");
    let chrome = chrome.borrow();
    let warning = chrome
        .notifications()
        .iter()
        .find(|notification| notification.title.contains("Start a call"))
        .expect("the card names itself in a warning");
    assert!(
        !warning.body.trim().is_empty(),
        "the warning says why: {:?}",
        warning.body
    );
}

// Actual native Search journey: no application callback or device action runs.
fn launchpad_journey_target(widget: &dyn Widget) -> Option<Rect> {
    if let Some(pad) = widget
        .as_any()
        .and_then(|any| any.downcast_ref::<tilo_desktop::launchpad::CollaborationLaunchpad>())
    {
        return pad
            .targets()
            .into_iter()
            .find(|(id, _)| id == "launchpad:open-app")
            .map(|(_, rect)| rect);
    }
    widget
        .children()
        .into_iter()
        .find_map(launchpad_journey_target)
}
fn launchpad_journey_layout(host: &mut ShellOverlayHost, theme: &Theme) -> (TaffyTree<()>, NodeId) {
    let mut taffy = TaffyTree::new();
    let mut nodes = HashMap::new();
    let node = host.build_layout(&mut taffy, theme, &mut nodes);
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    taffy
        .compute_layout_with_measure(
            node,
            Size {
                width: AvailableSpace::Definite(1280.0),
                height: AvailableSpace::Definite(800.0),
            },
            |known, available, id, _, _| {
                nodes.get(&id).map_or(Size::ZERO, |&ptr| unsafe {
                    (*ptr).measure(known, available, &mut text)
                })
            },
        )
        .expect("production native layout");
    host.apply_layout(&taffy, node, Point::ZERO);
    (taffy, node)
}
fn launchpad_journey_deliver(
    host: &mut ShellOverlayHost,
    doc: &mut AutoCommit,
    event: &WindowEvent,
    tree: &TaffyTree<()>,
    node: NodeId,
) {
    ui::testing::Dispatch::new()
        .doc(doc)
        .layout(tree, node)
        .send(host, event);
    host.settle(&mut ui::widget::Edits::lend(doc), &automerge::ROOT);
}
fn launchpad_journey_escape() -> WindowEvent {
    let key = winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape);
    ui::synthetic_input::keyboard_input(winit::event::KeyEvent::synthetic(
        winit::event::RawKeyEvent {
            physical_key: winit::keyboard::PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified,
            ),
            state: ElementState::Pressed,
        },
        key.clone(),
        None,
        winit::keyboard::KeyLocation::Standard,
        false,
        None,
        key,
    ))
}
fn launchpad_journey_paint(host: &mut ShellOverlayHost, theme: &Theme, name: &str) {
    host.force_palette_settled();
    let _ = launchpad_journey_layout(host, theme);
    let mut scene = vello::Scene::new();
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    host.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        theme,
        &PresenceCtx::default(),
    );
    if let Some(out) = std::env::var_os("TILO_LAUNCHPAD_JOURNEY_SHOTS") {
        let out = std::path::PathBuf::from(out);
        std::fs::create_dir_all(&out).expect("requested capture directory");
        ui::offscreen::OffscreenRenderer::new(1280, 800).render_png(
            &scene,
            theme.tokens.colors.surfaces.base,
            out.join(format!("{name}.png")),
        );
    }
}

#[test]
fn app_opener_pointer_press_waits_and_drag_cancel_keeps_native_search_closed() {
    let (mut host, _) = build_tilo_overlay_host_with_chrome(&WorkspaceTree::bootstrap_default());
    let mut doc = AutoCommit::new();
    let theme = match std::env::var("TILO_LAUNCHPAD_JOURNEY_VARIANT").as_deref() {
        Ok("light") => Theme::light(),
        Ok("large-text") => Theme::dark().with_text_scale(2.0),
        Ok("scaled") => Theme::dark().with_ui_scale(1.5).with_text_scale(1.3),
        _ => Theme::dark(),
    }
    .with_reduce_motion(true);
    let (taffy, node) = launchpad_journey_layout(&mut host, &theme);
    launchpad_journey_paint(&mut host, &theme, "idle");
    let button = launchpad_journey_target(host.content.as_ref())
        .expect("production app opener must be mounted")
        .center();
    let moved = |point: Point| ui::synthetic_input::cursor_moved(point.x, point.y);
    let mouse = |state| ui::synthetic_input::mouse_input(state, winit::event::MouseButton::Left);
    launchpad_journey_deliver(&mut host, &mut doc, &moved(button), &taffy, node);
    launchpad_journey_deliver(
        &mut host,
        &mut doc,
        &mouse(ElementState::Pressed),
        &taffy,
        node,
    );
    launchpad_journey_paint(&mut host, &theme, "pointer-down");
    assert!(
        !host.palette_visible(),
        "native Search must stay closed until release"
    );
    launchpad_journey_deliver(
        &mut host,
        &mut doc,
        &moved(Point::new(5.0, 200.0)),
        &taffy,
        node,
    );
    launchpad_journey_deliver(
        &mut host,
        &mut doc,
        &mouse(ElementState::Released),
        &taffy,
        node,
    );
    launchpad_journey_paint(&mut host, &theme, "cancelled");
    assert!(
        !host.palette_visible(),
        "cancelled click must not open native Search"
    );
    launchpad_journey_deliver(&mut host, &mut doc, &moved(button), &taffy, node);
    launchpad_journey_deliver(
        &mut host,
        &mut doc,
        &mouse(ElementState::Pressed),
        &taffy,
        node,
    );
    launchpad_journey_deliver(
        &mut host,
        &mut doc,
        &mouse(ElementState::Released),
        &taffy,
        node,
    );
    launchpad_journey_paint(&mut host, &theme, "released");
    assert!(
        host.palette_visible(),
        "completed click must open the real native palette"
    );
    let escape = launchpad_journey_escape();
    launchpad_journey_deliver(&mut host, &mut doc, &escape, &taffy, node);
    launchpad_journey_paint(&mut host, &theme, "search-closed");
    assert!(
        !host.palette_visible(),
        "Escape closes native Search after a completed click"
    );
}
