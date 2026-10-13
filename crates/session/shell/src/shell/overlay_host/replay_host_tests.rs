use super::*;
use crate::shell::search::action_api::ActionArgs;
use crate::shell::search::{InvocationDisposition, ResourceKind, SearchContext, SearchProvider};
use automerge::transaction::Transactable as _;

fn replay_harness() -> Harness {
    let mut h = build_activity_harness();
    for index in 0..3 {
        let state = h.chrome.borrow();
        let mut tree = state.tree_mut();
        let tree = &mut *tree;
        tree.desktop.active_workspace_mut().unwrap().name = format!("Checkpoint {index}");
        tilo_desktop_model::desktop_doc::seed(&mut tree.doc, &tree.desktop);
        tilo_resource::history::commit_stamped(&mut tree.doc);
    }
    h.host
        .room_activity_mut()
        .set_view(crate::shell::room_activity::ActivityView::Replay);
    h.host.force_room_activity_settled();
    paint(&mut h);
    assert!(h.host.room_activity().replay_position().total >= 2);
    h
}

fn replay_rows(h: &Harness) -> Vec<crate::shell::search::SearchResult> {
    let state = h.chrome.borrow();
    let document = AutoCommit::new();
    crate::shell::search::providers::ActionsProvider::new()
        .query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "",
            wanted: &[ResourceKind::Action],
            chrome: &state,
            doc: &document,
            doc_root: &automerge::ROOT,
        })
        .into_iter()
        .filter(|row| row.id.starts_with("room.activity.replay."))
        .collect()
}

fn invoke(h: &mut Harness, row: &crate::shell::search::SearchResult) -> serde_json::Value {
    use crate::shell::search::action_api::{AllVisible, ResourceRef};
    let mut document = AutoCommit::new();
    let catalogue = {
        let state = h.chrome.borrow();
        h.host.exact_action_catalog(
            &row.provider_id,
            &row.id,
            &state,
            &document,
            &automerge::ROOT,
            &AllVisible,
        )
    };
    let selected = catalogue
        .select(&ResourceRef::new(&row.provider_id, &row.id))
        .unwrap();
    let InvocationDisposition::Pending(ticket) = selected
        .invoke_nullary(
            &mut ui::widget::Edits::lend(&mut document),
            &automerge::ROOT,
            &h.chrome,
        )
        .unwrap()
    else {
        panic!("replay must resolve through the native pane")
    };
    assert!(ticket().unwrap().is_none());
    paint(h);
    let receipt = ticket().unwrap().unwrap();
    let tilo_actions::InvocationUpdate::Succeeded { output, .. } = receipt.update else {
        panic!("replay did not complete")
    };
    output.value.unwrap()
}

#[test]
fn replay_registry_invokes_the_retained_pane_without_live_edits() {
    let mut h = replay_harness();
    let (active, heads, pending) = {
        let state = h.chrome.borrow();
        let tree = state.tree();
        (
            tree.desktop.active,
            tree.doc.base_heads(),
            tree.doc.pending_ops(),
        )
    };
    let rows = replay_rows(&h);
    let seek = rows
        .iter()
        .find(|row| row.id.starts_with("room.activity.replay.seek."))
        .unwrap();
    let selected = invoke(&mut h, seek);
    assert!(selected["selected"]["frontier"].is_array());
    let metadata: tilo_desktop::activity::view_actions::ReplayPosition =
        serde_json::from_value(selected.clone()).unwrap();
    assert_eq!(serde_json::to_value(metadata).unwrap(), selected);
    let play = rows
        .iter()
        .find(|row| row.id == "room.activity.replay.play")
        .unwrap();
    assert_eq!(invoke(&mut h, play)["playing"], true);
    assert_eq!(invoke(&mut h, play)["playing"], false);
    let next = rows
        .iter()
        .find(|row| row.id == "room.activity.replay.next")
        .unwrap();
    let previous = rows
        .iter()
        .find(|row| row.id == "room.activity.replay.previous")
        .unwrap();
    invoke(&mut h, next);
    assert_eq!(invoke(&mut h, previous)["selected"], selected["selected"]);
    let state = h.chrome.borrow();
    let tree = state.tree();
    assert_eq!(tree.desktop.active, active);
    assert_eq!(tree.doc.base_heads(), heads);
    assert_eq!(tree.doc.pending_ops(), pending);
    assert!(h.host.pending_launchpad_app.is_none());
    assert!(h.host.pending_launchpad_actions.is_empty());
}

#[test]
fn replay_slider_is_published_before_clicks_and_sets_the_same_selection() {
    let mut h = replay_harness();
    let targets = h.host.test_rects();
    assert!(
        targets
            .iter()
            .any(|(id, rect)| id == "activity-replay-timeline" && rect.area() > 0.0)
    );
    let node = h.host.accessibility().unwrap();
    let target = node
        .children
        .iter()
        .flat_map(|node| &node.children)
        .find_map(|node| {
            node.action_target
                .as_ref()
                .filter(|target| target.starts_with("activity-replay-value:"))
        })
        .unwrap()
        .clone();
    let (active, heads) = {
        let state = h.chrome.borrow();
        let tree = state.tree();
        (tree.desktop.active, tree.doc.base_heads())
    };
    let mut document = AutoCommit::new();
    assert!(h.host.perform_semantic_action(
        &target,
        &ui::a11y::Action::SetValue("1".into()),
        &mut ui::widget::Edits::lend(&mut document),
        &automerge::ROOT
    ));
    assert_eq!(h.host.room_activity().replay_position().position, Some(1));
    assert_eq!(h.chrome.borrow().tree().doc.base_heads(), heads);
    assert_eq!(h.chrome.borrow().tree().desktop.active, active);
    assert!(h.host.pending_launchpad_app.is_none());
    assert!(h.host.pending_launchpad_actions.is_empty());
    assert!(!h.host.room_activity().replay_is_playing());
}

#[test]
fn replay_admission_refuses_a_retained_action_after_workspace_change() {
    let h = replay_harness();
    let row = replay_rows(&h)
        .into_iter()
        .find(|row| row.id == "room.activity.replay.play")
        .unwrap();
    h.chrome
        .borrow()
        .tree_mut()
        .desktop
        .active_workspace_mut()
        .unwrap()
        .room = crate::shell::state::RoomId::new();
    assert!(replay_rows(&h).is_empty());
    let args = ActionArgs::bind(&row.params, std::iter::empty()).unwrap();
    let mut document = AutoCommit::new();
    assert!(
        row.on_begin_invoke.unwrap()(
            &mut ui::widget::Edits::lend(&mut document),
            &automerge::ROOT,
            &h.chrome,
            &args
        )
        .is_err()
    );
    assert!(!h.host.room_activity().replay_is_playing());
}
