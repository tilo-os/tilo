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
                "Plan a meeting",
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

        host.sync(&mut doc, &root);

        assert!(
            crate::shell::tab_binding::active_workspace_activity_tab(&chrome.borrow().tree())
                .is_some()
        );
        assert!(
            !chrome.borrow().overlays.room_activity,
            "the recap is a tiled window, not transient overlay state"
        );
        assert!(matches!(
            doc.get(&root, crate::shell::EMPTY_WORKSPACE_ACTIVITY),
            Ok(Some((Value::Scalar(value), _)))
                if matches!(value.as_ref(), ScalarValue::Boolean(false))
        ));
    }

    fn invite_card_opens_the_existing_signed_invite_composer() {
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.adopt_ownerless_workspaces_with(crate::shell::state::local_user_id_for_room);
        let (mut host, _) = build_tilo_overlay_host_with_chrome(&tree);
        host.set_peer_sync_enabled(true);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let _ = doc.put(&root, crate::shell::EMPTY_WORKSPACE_INVITE, true);

        host.sync(&mut doc, &root);

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

        host.sync(&mut doc, &root);

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

            host.sync(&mut doc, &root);

            assert_eq!(*apps.opened.borrow(), [app], "{card:?} opens {app}");
            assert!(
                !host.palette_visible(),
                "{card:?} performs its action instead of pre-filling the launcher"
            );
            assert!(LaunchpadAction::take(&mut doc, &root).is_none());
        }
    }

    fn a_card_chosen_before_the_app_index_is_ready_opens_once_it_is() {
        let tree = WorkspaceTree::bootstrap_default();
        let (mut host, _) = build_tilo_overlay_host_with_chrome(&tree);
        let apps = FakeApplications::install(&mut host, 3);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let _ = doc.put(&root, crate::shell::EMPTY_WORKSPACE_NOTES, true);

        host.sync(&mut doc, &root);
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
            host.update(&mut doc, &root);
        }
        assert_eq!(*apps.opened.borrow(), ["so.tilo.notes"]);
        assert!(!host.palette_visible());
        host.update(&mut doc, &root);
        assert_eq!(
            apps.opened.borrow().len(),
            1,
            "one card choice opens the app once"
        );
    }

    app_cards_open_their_app_without_the_launcher();
    a_card_chosen_before_the_app_index_is_ready_opens_once_it_is();
}
