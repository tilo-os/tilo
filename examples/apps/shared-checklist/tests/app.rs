//! Two replicas of one checklist, edited concurrently and then synced, as
//! two collaborators' machines would be.

use example_shared_checklist::{App, Checklist};
use ui::a11y::Action;
use ui::headless::SemanticExpectation;
use ui::prelude::{Id, Replica, TestApp};

#[expect(clippy::expect_used, reason = "a test helper fails the test loudly")]
fn add(list: &mut Replica<Checklist>, title: &str) -> Id {
    list.apply(Checklist::SET_DRAFT.with(title.into()))
        .expect("draft");
    list.apply(Checklist::ADD.invoke()).expect("add");
    let state = list.state().expect("checklist");
    state
        .items
        .ids()
        .find(|id| state.items.get(id).is_some_and(|item| item.title == title))
        .cloned()
        .expect("the added item")
}

#[expect(clippy::expect_used, reason = "a test helper fails the test loudly")]
fn titles(list: &Replica<Checklist>) -> Vec<String> {
    let mut titles = list
        .state()
        .expect("checklist")
        .items
        .values()
        .map(|item| item.title.clone())
        .collect::<Vec<_>>();
    titles.sort();
    titles
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Items have stable ids, so two people adding at once keep both items.
#[test]
fn concurrent_adds_keep_both_items() {
    let mut ada = Replica::<Checklist>::new().as_author("ada");
    let mut grace = ada.fork().as_author("grace");
    add(&mut ada, "Milk");
    add(&mut grace, "Bread");
    ada.sync_with(&mut grace).expect("sync");
    assert_eq!(titles(&ada), ["Bread", "Milk"]);
    assert_eq!(ada.state().expect("ada"), grace.state().expect("grace"));
}

/// Titles are collaborative text: typing in the same title merges.
#[test]
fn concurrent_typing_in_one_title_merges() {
    let mut ada = Replica::<Checklist>::new().as_author("ada");
    let milk = add(&mut ada, "Buy milk");
    let mut grace = ada.fork().as_author("grace");
    ada.apply(Checklist::RETITLE.with((milk.clone(), "Buy oat milk".into())))
        .expect("ada types");
    grace
        .apply(Checklist::RETITLE.with((milk, "Buy milk today".into())))
        .expect("grace types");
    ada.sync_with(&mut grace).expect("sync");
    assert_eq!(titles(&ada), ["Buy oat milk today"]);
    assert_eq!(titles(&grace), ["Buy oat milk today"]);
}

/// Different fields of one item never conflict, and a register written on
/// both sides settles on the same value everywhere.
#[test]
fn concurrent_edits_to_one_item_converge() {
    let mut ada = Replica::<Checklist>::new().as_author("ada");
    let milk = add(&mut ada, "Milk");
    let mut grace = ada.fork().as_author("grace");
    ada.apply(Checklist::SET_DONE.with((milk.clone(), true)))
        .expect("ada checks it");
    grace
        .apply(Checklist::RETITLE.with((milk.clone(), "Oat milk".into())))
        .expect("grace retitles it");
    ada.sync_with(&mut grace).expect("sync");
    let merged = ada.state().expect("ada");
    let item = merged.items.get(&milk).expect("the item");
    assert!(item.done);
    assert_eq!(item.title, "Oat milk");

    ada.apply(Checklist::SET_DONE.with((milk.clone(), false)))
        .expect("ada unchecks");
    grace
        .apply(Checklist::SET_DONE.with((milk, true)))
        .expect("grace keeps it checked");
    ada.sync_with(&mut grace).expect("sync");
    assert_eq!(ada.state().expect("ada"), grace.state().expect("grace"));
}

/// Every action is a version attributed to whoever made it.
#[test]
fn history_is_attributed() {
    let mut ada = Replica::<Checklist>::new().as_author("ada");
    let mut grace = ada.fork().as_author("grace");
    ada.apply(Checklist::RENAME.with("Groceries".into()))
        .expect("rename");
    add(&mut grace, "Bread");
    ada.sync_with(&mut grace).expect("sync");
    let versions = ada.versions();
    let by = |author: &[u8], summary: &str| {
        versions.iter().any(|version| {
            version.actor == hex(author) && version.summary().as_deref() == Some(summary)
        })
    };
    assert!(
        by(b"ada", "Rename the checklist · Checklist"),
        "{versions:#?}"
    );
    assert!(
        by(b"grace", "Add the drafted item · Checklist"),
        "{versions:#?}"
    );
}

/// Undo takes back my last action, not the collaborator's that arrived
/// after it.
#[test]
fn undo_takes_back_only_my_own_actions() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(Checklist::RENAME.with("Groceries".into()))
        .expect("rename");
    let mut grace = Replica::<Checklist>::from_doc(app.peer_document()).as_author("grace");
    add(&mut grace, "Bread");
    app.merge_peer(grace.doc_mut()).expect("merge");
    app.harness()
        .expect(&SemanticExpectation::named("progress").name("0 of 1 done"))
        .expect("the collaborator's item shows up");

    assert!(app.undo());
    let state = app.state();
    assert_eq!(state.name, "", "my rename is undone");
    assert_eq!(state.items.len(), 1, "grace's item stays");
    assert!(app.redo());
    assert_eq!(app.state().name, "Groceries");
}

/// Removing every checked item is one action, so one undo brings them back.
#[test]
fn clearing_checked_items_is_one_undo_step() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    for title in ["Milk", "Bread", "Eggs"] {
        app.invoke(Checklist::SET_DRAFT.with(title.into()))
            .expect("draft");
        app.invoke(Checklist::ADD.invoke()).expect("add");
    }
    let ids = app.state().items.ids().cloned().collect::<Vec<_>>();
    for id in ids.iter().take(2) {
        app.harness()
            .perform_named(&format!("set-done-{id}"), &Action::ToggleCheck)
            .expect("the row checkbox is addressable");
    }
    assert!(
        app.invoke(Checklist::CLEAR_DONE.invoke()).is_err(),
        "a destructive action asks first"
    );
    app.invoke_confirmed(Checklist::CLEAR_DONE.invoke())
        .expect("clear");
    assert_eq!(app.state().items.len(), 1);
    assert!(app.undo());
    assert_eq!(app.state().progress(), (2, 3));
}

#[test]
fn a_viewer_follows_but_cannot_edit() {
    let mut app = TestApp::<App>::with_role(ui::Role::Viewer).expect("the app opens");
    assert!(app.invoke(Checklist::RENAME.with("Mine".into())).is_err());
    app.harness()
        .expect(&SemanticExpectation::named("name-input").enabled(false))
        .expect("the input disables itself");
}

#[test]
fn initial_native_app_contract_is_certified() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");
}
