//! The Quick Settings notification centre end to end: retained
//! notifications group by verified app in the accessibility tree, a click on
//! a retained row reaches the sending app as a D-Bus `ActionInvoked`, and
//! the history is restored from its private file after a restart.

use std::collections::HashMap;

use shell::notifications::NotificationBridge;
use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::chrome_state::{Chrome, ChromeState};
use shell::shell::notification_history::NotificationHistoryStore;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::quick_settings::QsLevel;
use shell::shell::state::{FreshId as _, WorkspaceId, WorkspaceTree};
use taffy::prelude::TaffyMaxContent;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use tilo_desktop::notifications::Notification;
use ui::a11y::AccessNode;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, KeyLocation, NamedKey, PhysicalKey};

fn tree() -> WorkspaceTree {
    let ws = shell::shell::state::Workspace::new(WorkspaceId::new(), "W");
    let active = ws.id;
    WorkspaceTree {
        desktop: shell::shell::state::DesktopTree {
            workspaces: vec![ws],
            active,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn from_app(app_id: &str, label: &str, title: &str) -> Notification {
    let mut notification = Notification::new(title, format!("{title} body"));
    notification.app_id = Some(app_id.to_string());
    notification.app_name = Some(label.to_string());
    notification
}

/// Retire notifications straight into history the way Do-Not-Disturb does
/// on the production push path.
fn retain(chrome: &Chrome, notifications: impl IntoIterator<Item = Notification>) {
    let mut state = chrome.borrow_mut();
    state.set_dnd(true);
    for notification in notifications {
        state.push_notification_full(notification);
    }
    state.set_dnd(false);
}

fn layout_host(host: &mut ShellOverlayHost) -> (TaffyTree<()>, NodeId) {
    use taffy::{Dimension, Style};
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let host_node = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(1600.0),
                    height: Dimension::length(900.0),
                },
                ..Default::default()
            },
            &[host_node],
        )
        .unwrap();
    let map_for_measure = node_map.clone();
    let mut tr = ui::text_render::TextRenderer::new();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(1600.0),
                height: AvailableSpace::Definite(900.0),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = map_for_measure.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .or_else(|_| taffy.compute_layout(root, taffy::Size::MAX_CONTENT))
        .expect("layout must compute");
    host.apply_layout(&taffy, host_node, Point::ZERO);
    (taffy, host_node)
}

fn open_notification_centre(
    host: &mut ShellOverlayHost,
    chrome: &Chrome,
) -> (TaffyTree<()>, NodeId) {
    {
        let mut state = chrome.borrow_mut();
        state.overlays.quick_settings = true;
        state.quick_settings.push_level(QsLevel::Notifications);
        state.bump();
    }
    host.force_quick_settings_settled();
    layout_host(host)
}

fn deliver(host: &mut ShellOverlayHost, taffy: &TaffyTree<()>, node: NodeId, event: &WindowEvent) {
    let mut state = automerge::AutoCommit::new();
    let mut handled = false;
    let mut focus_target = None;
    host.handle_event(
        event,
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &Modifiers::default(),
            state: &mut state,
            path: &automerge::ROOT,
            handled: &mut handled,
            focus_target: &mut focus_target,
            node_id: node,
            taffy,
        },
    );
}

fn press(host: &mut ShellOverlayHost, taffy: &TaffyTree<()>, node: NodeId, key: NamedKey) {
    let mut key_event: KeyEvent = unsafe { std::mem::zeroed() };
    key_event.logical_key = Key::Named(key);
    key_event.physical_key =
        PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    key_event.location = KeyLocation::Standard;
    key_event.state = ElementState::Pressed;
    key_event.repeat = false;
    key_event.text = None;
    deliver(
        host,
        taffy,
        node,
        &WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: key_event,
            is_synthetic: false,
        },
    );
}

fn click(host: &mut ShellOverlayHost, taffy: &TaffyTree<()>, node: NodeId, at: Point) {
    deliver(
        host,
        taffy,
        node,
        &WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: PhysicalPosition::new(at.x, at.y),
        },
    );
    for state in [ElementState::Pressed, ElementState::Released] {
        deliver(
            host,
            taffy,
            node,
            &WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state,
                button: MouseButton::Left,
            },
        );
    }
}

/// The rows of the notification centre's list, as a screen reader sees them.
fn retained_rows(host: &ShellOverlayHost) -> Vec<AccessNode> {
    let root = host.accessibility().expect("shell semantics");
    let centre = root
        .children
        .iter()
        .find(|node| node.name.as_deref() == Some("Notifications"))
        .expect("the notification centre is published as a dialog");
    centre
        .children
        .iter()
        .find(|node| node.name.as_deref() == Some("Retained notifications"))
        .expect("retained notifications are a list")
        .children
        .clone()
}

fn group_rows(rows: &[AccessNode]) -> Vec<(String, bool)> {
    rows.iter()
        .filter(|row| row.state.expanded.is_some())
        .map(|row| {
            (
                row.name.clone().unwrap_or_default(),
                row.state.expanded.unwrap_or_default(),
            )
        })
        .collect()
}

#[test]
fn notifications_group_by_verified_app_in_the_accessibility_tree() {
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree());
    retain(
        &chrome,
        [
            from_app("org.mail", "Mail", "Invoice"),
            from_app("org.chat", "Chat", "Hello"),
            from_app("org.calendar", "Calendar", "Standup"),
            from_app("org.mail", "Mail", "Receipt"),
            from_app("org.chat", "Chat", "Lunch?"),
            from_app("org.calendar", "Calendar", "Review"),
        ],
    );
    let (taffy, node) = open_notification_centre(&mut host, &chrome);

    let rows = retained_rows(&host);
    let first = rows.first().expect("a visible row");
    assert_eq!(first.name.as_deref(), Some("Calendar"), "newest app first");
    assert_eq!(first.state.expanded, Some(true));
    assert_eq!(first.level, Some(1));
    assert!(
        first
            .description
            .as_deref()
            .is_some_and(|d| d.starts_with("2 notifications")),
        "a group header announces its count: {:?}",
        first.description
    );
    let members: Vec<_> = rows
        .iter()
        .skip(1)
        .take(2)
        .map(|row| (row.name.clone().unwrap_or_default(), row.level))
        .collect();
    assert_eq!(
        members,
        [
            ("Review".to_string(), Some(2)),
            ("Standup".to_string(), Some(2))
        ],
        "a group's notifications follow its header, newest first"
    );
    let member = rows.get(1).unwrap();
    let controls: Vec<_> = member
        .children
        .iter()
        .filter_map(|control| control.name.as_deref())
        .collect();
    assert_eq!(
        controls,
        ["Dismiss"],
        "with no agent in the workspace, Hand to agent is not offered"
    );

    // Fold every group from the keyboard: select a header, Enter, next.
    for _ in 0..3 {
        press(&mut host, &taffy, node, NamedKey::ArrowDown);
        press(&mut host, &taffy, node, NamedKey::Enter);
    }
    let rows = retained_rows(&host);
    assert_eq!(
        group_rows(&rows),
        [
            ("Calendar".to_string(), false),
            ("Chat".to_string(), false),
            ("Mail".to_string(), false),
        ],
        "three folded app groups remain, one row each"
    );
    assert_eq!(rows.len(), 3);
}

#[test]
fn clicking_a_retained_notification_invokes_its_default_action_over_dbus() {
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree());
    let bridge = NotificationBridge::new();
    chrome.borrow_mut().set_notification_bridge(bridge.clone());
    let mut notification = from_app("org.mail", "Mail", "Invoice");
    notification.sender_id = Some(41);
    notification.actions = vec![("default".into(), "Open".into())];
    retain(&chrome, [notification]);
    let (taffy, node) = open_notification_centre(&mut host, &chrome);

    let rects = host.quick_settings_semantic_rects();
    let row = rects
        .iter()
        .find(|(name, _)| name == "quick_settings_notification_row[0]")
        .map(|(_, rect)| *rect)
        .expect("the retained row is a named target");
    let dismiss = rects
        .iter()
        .find(|(name, _)| name == "quick_settings_notification_dismiss[0]")
        .map(|(_, rect)| *rect)
        .expect("each row carries a dismiss control");
    assert!(dismiss.x0 > row.x0 + 40.0 && row.contains(dismiss.center()));

    click(
        &mut host,
        &taffy,
        node,
        Point::new(row.x0 + 24.0, row.center().y),
    );
    assert_eq!(
        bridge.drain_action_events(),
        vec![(41, "default".to_string())],
        "the click reaches the sending app as ActionInvoked for its default action"
    );
    assert!(
        chrome.borrow().notification_history().is_empty(),
        "an acted-on notification leaves the history"
    );
}

#[test]
fn delete_on_the_selected_row_dismisses_only_that_notification() {
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree());
    retain(
        &chrome,
        [
            Notification::new("Older", ""),
            Notification::new("Newer", ""),
        ],
    );
    let (taffy, node) = open_notification_centre(&mut host, &chrome);
    press(&mut host, &taffy, node, NamedKey::ArrowDown);
    press(&mut host, &taffy, node, NamedKey::Delete);
    let titles: Vec<_> = chrome
        .borrow()
        .notification_history()
        .iter()
        .map(|n| n.title.clone())
        .collect();
    assert_eq!(
        titles,
        ["Older"],
        "Delete removes the selected (newest) row"
    );
    let rows = retained_rows(&host);
    assert!(
        rows.first().is_some_and(|row| row.state.selected),
        "the selection moves to the row that took its place"
    );
}

#[test]
fn retained_history_is_restored_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tilo/notification-history.json");

    let first = ChromeState::new(tree());
    first
        .borrow_mut()
        .attach_notification_history_store(NotificationHistoryStore::at(&path));
    retain(
        &first,
        [
            from_app("org.mail", "Mail", "Invoice"),
            from_app("org.mail", "Mail", "Receipt"),
            Notification::new("Backup finished", ""),
        ],
    );
    first.borrow_mut().flush_notification_history();
    drop(first);

    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "the history file is private to the user"
        );
    }

    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree());
    chrome
        .borrow_mut()
        .attach_notification_history_store(NotificationHistoryStore::at(&path));
    let titles: Vec<_> = chrome
        .borrow()
        .notification_history()
        .iter()
        .map(|n| n.title.clone())
        .collect();
    assert_eq!(titles, ["Invoice", "Receipt", "Backup finished"]);
    let _layout = open_notification_centre(&mut host, &chrome);
    let rows = retained_rows(&host);
    assert_eq!(
        group_rows(&rows),
        [("Mail".to_string(), true)],
        "restored notifications keep their verified app group"
    );
}
