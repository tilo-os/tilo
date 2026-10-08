//! The existing compositor update pump must notice an attended request at rest.
use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::portal_dialog::PortalDialogQueue;
use crate::shell::search::providers::resource_selections::{PROVIDER_ID, tests::prompt};
use crate::shell::state::WorkspaceTree;
use tilo_actions::remote::PendingPoll;

#[test]
fn resource_selection_idle_update_opens_existing_palette_and_dismisses_same_waiter() {
    let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
    let room = chrome
        .borrow()
        .tree()
        .active_workspace()
        .expect("workspace")
        .room
        .to_string();
    let mut host = ShellOverlayHost::with_fixture_content(Box::new(ui::widget::Container::new()))
        .with_chrome(chrome);
    let queue = PortalDialogQueue::default();
    host.portal_dialogs = crate::shell::portal_dialog::PortalDialogController::new(queue.clone());
    host.search_registry = crate::shell::search::SearchRegistry::new();
    host.search_registry.register(std::rc::Rc::new(
        crate::shell::search::providers::resource_selections::ResourceSelectionsProvider {
            queue: queue.clone(),
            ranker: ui::search::ListIndex::default(),
        },
    ));
    let mut answer = queue
        .enqueue_resource_selection([19; 32], 481, prompt(&room))
        .expect("original attended request");
    let doc = AutoCommit::new();
    assert!(!host.palette_visible);
    assert!(
        host.update(&doc, &automerge::ROOT),
        "queued selection must dirty the production frame"
    );
    assert!(host.palette_visible);
    assert!(host.resource_selection_handle.is_some());
    assert!(
        !host.search_results.is_empty(),
        "palette rows must exist before the first layout"
    );
    assert!(
        host.search_results
            .iter()
            .all(|row| row.provider_id == PROVIDER_ID)
    );
    assert!(matches!(answer.poll_ready(), PendingPoll::Waiting));
    // Escape and the ordinary palette close retire the same deferred request.
    let mut handled = false;
    assert!(host.handle_shell_keyboard_event(
        &Key::Named(NamedKey::Escape),
        winit::event::Modifiers::default(),
        false,
        &doc,
        &automerge::ROOT,
        &mut handled,
    ));
    assert!(handled);
    assert!(!host.palette_visible);
    assert!(host.update(&doc, &automerge::ROOT));
    assert!(matches!(answer.poll_ready(), PendingPoll::Ready(None)));
    assert!(host.resource_selection_handle.is_none());
    drop(host);
    assert!(queue.resource_selection(&room).is_none());
}
