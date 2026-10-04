//! The native-input observer reads the app without touching its document.
//!
//! The snapshot a native audit asks for must describe the live state (the
//! selected category, the layout class, the document heads) without
//! committing the document's pending changes: the observer is a witness, not
//! a writer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::AutoCommit;
use ui::widget::Widget;

use crate::categories::Category;

fn root() -> automerge::ObjId {
    automerge::ROOT
}

#[test]
fn native_observer_does_not_commit_original_pending_changes() {
    use automerge::transaction::Transactable;
    let config = crate::prefs::test_env::ScratchConfig::new();
    let app = config.app();
    let mut doc = AutoCommit::new();
    doc.put(root(), "diagnostic_test", "pending").unwrap();
    let debug_before = format!("{doc:?}");
    let pending = doc.pending_ops();
    let a = app.native_input_snapshot(&doc, &root()).unwrap();
    let b = app.native_input_snapshot(&doc, &root()).unwrap();
    assert_eq!(a, b);
    assert_eq!(
        a["ready"], false,
        "an app that has not laid out is not ready"
    );
    assert_eq!(a["original_pending_ops"], pending);
    assert_eq!(format!("{doc:?}"), debug_before);
    assert_eq!(doc.pending_ops(), pending);
    assert!(!a["effective_document_heads"].as_array().unwrap().is_empty());
    assert!(a.get("document_content").is_none());
}

#[test]
fn native_observer_reports_the_selected_category_and_layout_class() {
    let config = crate::prefs::test_env::ScratchConfig::new();
    let app = config.app().with_category(Category::Appearance);
    let app = ui::headless::Framed::new(
        ui::State::new(),
        app,
        ui::headless::viewport((1200.0, 760.0)),
        ui::Theme::dark(),
    );
    let doc = AutoCommit::new();
    let snapshot = app.native_input_snapshot(&doc, &root()).unwrap();
    assert_eq!(snapshot["ready"], true);
    assert_eq!(snapshot["selected_category"], "appearance");
    assert_eq!(snapshot["narrow"], false);
    assert_eq!(snapshot["sidebar_open"], true);
    assert_eq!(snapshot["nav_id"], crate::sidebar::NAV_ID);
}
