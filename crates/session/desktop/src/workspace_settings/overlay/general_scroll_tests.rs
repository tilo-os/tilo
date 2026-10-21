use super::*;
#[test]
fn compact_general_body_scroll_reaches_summary_and_keeps_hit_tests_in_clip() {
    let mut overlay = WorkspaceSettingsOverlay::default();
    overlay.layout(Rect::new(0.0, 0.0, 960.0, 540.0));
    let name = overlay.name_rect();
    assert!(overlay.general_body_rect().contains(name.center()));
    // Cards follow the summary (devices, public), so scrolling to the end
    // passes it: scroll until its bottom meets the body's.
    let below = overlay.general_summary_rect().y1 - overlay.general_body_rect().y1;
    assert!(below > 0.0, "a compact body starts with the summary below");
    assert!(overlay.scroll_general(below));
    let summary = overlay.general_summary_rect();
    assert!(summary.y1 <= overlay.general_body_rect().y1);
    assert!(summary.y0 >= overlay.general_body_rect().y0);
    assert_eq!(
        overlay.hit_test(name.center(), WsTab::General),
        EventOutcome::CommitName
    );
    assert!(overlay.scroll_general(-1000.0));
    assert_eq!(overlay.name_rect(), name);
}
/// The devices card scrolls fully into view and its controls answer as
/// the device toggles, the moves and Delete.
/// Archived workspaces are listed under the Public card, each with
/// Restore and Delete, and scroll into view; nothing is listed while
/// none is archived.
#[test]
fn archived_workspaces_are_listed_with_restore_and_delete() {
    let mut overlay = WorkspaceSettingsOverlay::default();
    overlay.layout(Rect::new(0.0, 0.0, 960.0, 540.0));
    assert_eq!(overlay.archived_card_rect().height(), 0.0);
    overlay.archived_rows = 2;
    let card = overlay.archived_card_rect();
    assert!(
        card.y0 > overlay.public_card_rect().y1,
        "below the Public card"
    );
    assert!(
        overlay.scroll_general(f64::MAX),
        "the card scrolls into view"
    );
    let card = overlay.archived_card_rect();
    assert!(card.y1 <= overlay.general_body_rect().y1 + 0.5);
    let row = |index: f64| {
        let y = card.y0 + 48.0 + index * 40.0;
        [
            Rect::new(card.x1 - 216.0, y, card.x1 - 120.0, y + 32.0),
            Rect::new(card.x1 - 112.0, y, card.x1 - 16.0, y + 32.0),
        ]
    };
    overlay.archived_rects = vec![row(0.0), row(1.0)];
    assert_eq!(
        overlay.hit_test(overlay.archived_rects[1][0].center(), WsTab::General),
        EventOutcome::RestoreArchived(1)
    );
    assert_eq!(
        overlay.hit_test(overlay.archived_rects[0][1].center(), WsTab::General),
        EventOutcome::DeleteArchived(0)
    );
}

#[test]
fn the_devices_card_scrolls_into_view_and_its_controls_answer() {
    let mut overlay = WorkspaceSettingsOverlay::default();
    overlay.layout(Rect::new(0.0, 0.0, 960.0, 540.0));
    overlay.device_rows = 3;
    // The public card follows, so scroll until this card's bottom meets
    // the body's rather than to the end.
    let below = overlay.devices_card_rect().y1 - overlay.general_body_rect().y1;
    overlay.scroll_general(below);
    let card = overlay.devices_card_rect();
    assert!(card.y1 <= overlay.general_body_rect().y1 + 0.5);
    assert!(
        card.y0 >= overlay.general_body_rect().y0,
        "the whole card is in view"
    );
    let toggle = Rect::new(
        card.x1 - 112.0,
        card.y0 + 48.0,
        card.x1 - 16.0,
        card.y0 + 80.0,
    );
    overlay.device_toggle_rects = vec![toggle];
    overlay.move_rects = [
        Rect::new(
            card.x0 + 16.0,
            card.y1 - 48.0,
            card.x0 + 112.0,
            card.y1 - 16.0,
        ),
        Rect::ZERO,
    ];
    overlay.delete_rect = Rect::new(
        card.x1 - 136.0,
        card.y1 - 48.0,
        card.x1 - 16.0,
        card.y1 - 16.0,
    );
    assert_eq!(
        overlay.hit_test(toggle.center(), WsTab::General),
        EventOutcome::ToggleDevice(0)
    );
    assert_eq!(
        overlay.hit_test(overlay.move_rects[0].center(), WsTab::General),
        EventOutcome::MoveWorkspace(-1)
    );
    assert_eq!(
        overlay.hit_test(overlay.delete_rect.center(), WsTab::General),
        EventOutcome::DeleteWorkspace
    );
}
