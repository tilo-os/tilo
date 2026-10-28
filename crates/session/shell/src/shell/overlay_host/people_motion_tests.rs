//! Who is here (the people popover under the bar's faces) arrives and
//! leaves on the standard motion (`ui::motion::Appear`), as the bar's
//! workspace menu and profile card do.

use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::state::WorkspaceTree;
use ui::time::testing::with_monotonic_time;
use ui::tokens::MotionSource;

fn host() -> ShellOverlayHost {
    let tree = WorkspaceTree::bootstrap_demo();
    let chrome = ChromeState::new(tree);
    {
        let state = chrome.borrow();
        state
            .tree_mut()
            .claim_own_unhosted_rooms(crate::shell::state::local_user_id_for_room);
    }
    chrome.borrow_mut().peer_sync_enabled = true;
    let mut host = ShellOverlayHost::with_fixture_content(Box::new(ui::widget::Container::new()))
        .with_chrome(chrome);
    host.rect = Rect::new(0.0, 0.0, 1600.0, 900.0);
    host
}

fn set_open(host: &ShellOverlayHost, open: bool) {
    if let Some(chrome) = host.chrome.as_ref() {
        chrome.borrow_mut().overlays.collaborators = open;
    }
}

fn draw_at(host: &mut ShellOverlayHost, at: ui::time::Instant) {
    with_monotonic_time(at, || {
        let flags = host.overlay_flags();
        host.draw_collaborators_dropdown(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::empty(),
            flags,
        );
    });
}

fn progress(host: &ShellOverlayHost, at: ui::time::Instant) -> f64 {
    with_monotonic_time(at, || host.collaborators_appearing.progress())
}

fn drawn(host: &ShellOverlayHost, at: ui::time::Instant) -> bool {
    with_monotonic_time(at, || host.collaborators_appearing.is_drawn())
}

/// Opened and closed by a click it moves; once closed it is no target while
/// it leaves; a key lands it at once.
#[test]
fn people_arrive_and_leave_on_the_appear_motion_from_a_click_and_land_from_a_key() {
    let mut host = host();
    let start = ui::time::now();
    let open = start + std::time::Duration::from_secs(1);
    let gone = open + std::time::Duration::from_secs(1);

    ui::motion::note_source(MotionSource::Pointer);
    set_open(&host, true);
    draw_at(&mut host, start);
    let opening = progress(&host, start);
    assert!(
        opening < 0.5,
        "a click opens it on a motion, not in one frame: {opening}"
    );
    assert!(
        host.collaborators_overlay.panel_rect().area() > 0.0,
        "it is laid out, and takes input, from the start"
    );
    draw_at(&mut host, open);
    assert!(
        (progress(&host, open) - 1.0).abs() < f64::EPSILON,
        "and comes to rest"
    );
    let panel = host.collaborators_painted_panel;
    assert!(panel.area() > 0.0);

    set_open(&host, false);
    draw_at(&mut host, open);
    assert!(
        drawn(&host, open),
        "a click closes it on a motion: it is drawn while it leaves"
    );
    assert_eq!(host.collaborators_painted_panel, panel, "where it was");
    assert_eq!(
        host.collaborators_overlay.panel_rect(),
        Rect::ZERO,
        "but nothing of it is a target any more"
    );
    assert_eq!(host.collaborators_invite_rect_for_tests(), Rect::ZERO);
    draw_at(&mut host, gone);
    assert!(!drawn(&host, gone));
    assert_eq!(
        host.collaborators_painted_panel,
        Rect::ZERO,
        "then it is let go"
    );

    ui::motion::note_source(MotionSource::Keyboard);
    set_open(&host, true);
    draw_at(&mut host, gone);
    assert!(
        (progress(&host, gone) - 1.0).abs() < f64::EPSILON,
        "a key lands it at once"
    );
}
