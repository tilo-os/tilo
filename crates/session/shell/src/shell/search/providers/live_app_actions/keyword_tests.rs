//! An app's action is found by the other words it publishes for itself.

use super::{PublishedAction, PublishedInput, PublishedSurface, index_actions};
use crate::shell::collab_broker::CrdtRoute;

fn action(label: &str, id: &str, keywords: &[&str]) -> PublishedAction {
    PublishedAction {
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        resource: format!("resource:{id}"),
        action: id.into(),
        resource_kind: "fixture".into(),
        resource_label: "Fixture".into(),
        action_label: label.into(),
        keywords: keywords.iter().map(|word| (*word).to_owned()).collect(),
        icon: None,
        description: None,
        operation_kind: tilo_actions::OperationKind::View,
        params: Vec::new(),
        contract: tilo_actions::ActionContract::view(),
        view: None,
        input: PublishedInput::None,
    }
}

/// "mkdir" finds New folder as "new folder" does, and a word no action
/// declares finds none.
#[test]
fn a_published_action_is_found_by_the_words_it_declares() {
    let surface = PublishedSurface {
        peer_id: 7,
        app_id: "so.tilo.fixture".into(),
        route: CrdtRoute::app("so.tilo.fixture", "room"),
        actions: Vec::new(),
        live: true,
    };
    let entries = vec![
        (surface.clone(), action("Rename", "rename", &[])),
        (
            surface,
            action("New folder", "new-folder", &["mkdir", "make directory"]),
        ),
    ];
    let index = index_actions(&entries, false);
    let page = tilo_search::Page {
        offset: 0,
        limit: 8,
    };
    for query in ["mkdir", "make directory", "new folder"] {
        let keys: Vec<u64> = index
            .search(query, page)
            .hits
            .iter()
            .map(|hit| hit.key)
            .collect();
        assert_eq!(
            keys.first(),
            Some(&1),
            "{query} finds New folder first: {keys:?}"
        );
    }
    assert!(index.search("rmdir", page).hits.is_empty());
}
