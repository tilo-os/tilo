//! The pinned people overlay on the native desktop: pinned from the bar's
//! roster, kept pinned and at its edge in the person's settings as the desk
//! widgets keep theirs, and following from it is the host's one follow.

use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::state::{Collaborator, WorkspaceTree};
use settings_store::desk_widgets::{DockEdge, PEOPLE_OVERLAY};
use tilo_desktop::people_overlay::PeopleHit;

fn host() -> ShellOverlayHost {
    let mut tree = WorkspaceTree::bootstrap_demo();
    if let Some(workspace) = tree.desktop.active_workspace_mut() {
        workspace
            .collaborators
            .push(Collaborator::new("ada", "Ada"));
    }
    let chrome = ChromeState::new(tree);
    let mut host = ShellOverlayHost::with_fixture_content(Box::new(ui::widget::Container::new()))
        .with_chrome(chrome);
    host.rect = Rect::new(0.0, 0.0, 1600.0, 900.0);
    host
}

fn draw(host: &mut ShellOverlayHost) {
    host.draw_people_overlay(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        &PresenceCtx::empty(),
    );
}

/// Pinned from the roster, the overlay floats over the windows and stays
/// pinned; following from it follows; dragged to another edge, the edge is
/// kept where the desk widgets keep theirs.
#[test]
fn the_roster_pins_into_the_overlay_which_follows_and_keeps_its_edge() {
    let _settings = crate::shell::test_support::install_settings(std::sync::Arc::new(
        settings_store::HostDocument::new(serde_json::Map::new(), |_| Ok(())),
    ));
    let mut host = host();
    assert!(!host.people_overlay.is_visible(), "not pinned until asked");

    let mut handled = false;
    host.apply_collaborators_hit(CollaboratorsHit::Pin, &mut handled);
    assert!(handled);
    assert!(host.people_overlay.is_visible());
    assert!(PEOPLE_OVERLAY.get().pinned, "kept pinned");
    draw(&mut host);
    assert!(
        host.people_overlay.panel_rect().area() > 0.0,
        "it is laid out over the desktop"
    );
    assert!(
        host.people_overlay
            .shown_rows()
            .iter()
            .any(|row| row.id == "ada"),
        "it lists who the roster lists"
    );

    host.apply_people_hit(PeopleHit::ToggleFollow("ada".into()));
    assert_eq!(host.followed_peer.as_deref(), Some("ada"));
    host.apply_people_hit(PeopleHit::ToggleFollow("ada".into()));
    assert_eq!(host.followed_peer, None, "a second press stops following");

    host.apply_people_hit(PeopleHit::Moved(DockEdge::BottomLeft));
    assert_eq!(PEOPLE_OVERLAY.get().edge, DockEdge::BottomLeft);

    host.apply_people_hit(PeopleHit::Unpin);
    assert!(!PEOPLE_OVERLAY.get().pinned, "back in the bar, and kept so");
}
