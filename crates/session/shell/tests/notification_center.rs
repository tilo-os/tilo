//! The Quick Settings notifications end to end: a click on a kept row
//! reaches the sending app as a D-Bus `ActionInvoked`, and the history is
//! restored from its private file after a restart, its app groups kept.

use shell::notifications::NotificationBridge;
use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::chrome_state::{Chrome, ChromeState};
use shell::shell::notification_history::NotificationHistoryStore;
use shell::shell::overlay_host::ShellOverlayHost;
use taffy::{NodeId, TaffyTree};
use tilo_desktop::notifications::Notification;
use tilo_quick_settings::QsLevel;
use vello::kurbo::Point;

use crate::support::host::{click, layout_host, one_workspace_tree};

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
    // The panel opens on a laid-out desktop, under its tray.
    let _ = layout_host(host);
    host.force_quick_settings_settled();
    layout_host(host)
}

/// The kept rows the panel lists, by their named targets.
fn notification_rows(host: &ShellOverlayHost) -> Vec<(String, vello::kurbo::Rect)> {
    let mut rows: Vec<_> = host
        .quick_settings_semantic_rects()
        .into_iter()
        .filter(|(name, _)| {
            name.strip_prefix("qs-notification-")
                .is_some_and(|id| id.chars().all(|c| c.is_ascii_digit()))
        })
        .collect();
    rows.sort_by(|a, b| a.1.y0.total_cmp(&b.1.y0));
    rows
}

#[test]
fn clicking_a_retained_notification_invokes_its_default_action_over_dbus() {
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&one_workspace_tree());
    let bridge = NotificationBridge::new();
    chrome.borrow_mut().set_notification_bridge(bridge.clone());
    let mut notification = from_app("org.mail", "Mail", "Invoice");
    notification.sender_id = Some(41);
    notification.actions = vec![("default".into(), "Open".into())];
    retain(&chrome, [notification]);
    let (taffy, node) = open_notification_centre(&mut host, &chrome);

    let row = notification_rows(&host)
        .first()
        .map(|(_, rect)| *rect)
        .expect("the kept row is a named target");

    click(
        &mut host,
        &taffy,
        node,
        Point::new(row.x0 + 24.0, row.center().y),
    );
    host.force_quick_settings_settled();
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
fn retained_history_is_restored_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tilo/notification-history.json");

    let first = ChromeState::new(one_workspace_tree());
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
    // The loop hands the write to the state writer; a restart reads what
    // it wrote.
    shell::shell::durable_writer::global().flush();

    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "the history file is private to the user"
        );
    }

    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&one_workspace_tree());
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
    assert!(
        host.quick_settings_target("qs-notifications-dismiss-org.mail\u{1f}Mail")
            .is_some(),
        "restored notifications keep their verified app group"
    );
    assert_eq!(notification_rows(&host).len(), 3);
}
