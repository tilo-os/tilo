use generated_app::{App, Items};
use ui::a11y::Action;
use ui::headless::SemanticExpectation;
use ui::prelude::{Id, Replica, TestApp};

fn titles(items: &Items) -> Vec<&str> {
    items
        .items
        .values()
        .map(|item| item.title.as_str())
        .collect()
}

fn first_id(items: &Items) -> Id {
    items.items.ids().next().cloned().expect("an item")
}

/// The resource on its own: an item needs a title, and every item action
/// refuses an item that does not exist.
#[test]
fn items_are_added_edited_and_removed_by_actions() {
    let mut items = Replica::<Items>::new();
    assert!(items.apply(Items::ADD.invoke()).is_err(), "no empty items");
    items
        .apply(Items::SET_DRAFT.with("Buy milk"))
        .expect("draft");
    items.apply(Items::ADD.invoke()).expect("add");
    let state = items.state().expect("items");
    assert_eq!(titles(&state), ["Buy milk"]);
    assert!(state.draft.is_empty(), "adding clears the draft");

    let id = first_id(&state);
    items
        .apply(Items::RENAME.with((id.clone(), "Buy oat milk".into())))
        .expect("rename");
    items
        .apply(Items::SET_DONE.with((id.clone(), true)))
        .expect("check off");
    let state = items.state().expect("items");
    assert_eq!(titles(&state), ["Buy oat milk"]);
    assert!(state.items.values().all(|item| item.done));

    items.apply(Items::REMOVE.with(id.clone())).expect("remove");
    assert!(items.apply(Items::REMOVE.with(id)).is_err());
    assert!(items.state().expect("items").items.is_empty());
}

/// The app starts empty, the draft field and Add button create an item, its
/// row edits it, and removing it is one undo step.
#[test]
fn the_list_starts_empty_and_rows_edit_their_item() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let inspection = app.harness().inspection_json().expect("inspection");
    assert!(inspection.contains("Nothing here yet"), "{inspection}");

    app.harness()
        .perform_named("draft-input", &Action::SetValue("Buy milk".into()))
        .expect("the draft field is addressable");
    app.harness()
        .click_named("add")
        .expect("the add button is addressable");
    let id = first_id(&app.state());
    assert_eq!(titles(&app.state()), ["Buy milk"]);

    app.harness()
        .perform_named(&format!("set-done-{id}"), &Action::ToggleCheck)
        .expect("the row's checkbox is addressable");
    app.harness()
        .expect(&SemanticExpectation::named(format!("set-done-{id}")).checked(true))
        .expect("the checkbox follows the item");
    assert!(app.state().items.values().all(|item| item.done));

    app.harness()
        .click_named(&format!("remove-{id}"))
        .expect("the row's remove button is addressable");
    assert!(app.state().items.is_empty());
    assert!(app.undo(), "removing is one undo step");
    assert_eq!(titles(&app.state()), ["Buy milk"]);
}

/// Items are stored: they survive a relaunch with no save step.
#[test]
fn items_survive_a_relaunch() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(Items::SET_DRAFT.with("Call Ada"))
        .expect("draft");
    app.invoke(Items::ADD.invoke()).expect("add");
    let app = app.reopen().expect("the app reopens");
    assert_eq!(titles(&app.state()), ["Call Ada"]);
}
