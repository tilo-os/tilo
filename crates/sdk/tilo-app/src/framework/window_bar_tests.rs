// Test-only on its own, so the packaging scan of resource kinds, which reads
// each file alone, does not take its probe kind for the SDK's.
#![cfg(test)]

use std::rc::Rc;

use super::*;
use tilo_resource::{Effect as _, Model, Resource as _, resource};
use ui_core::window_toolbar::{ToolbarEvent, deliver};

#[derive(Model, Default, Debug, Clone)]
pub struct Tally {
    pub count: i64,
    /// Whether the counter is pinned in the window's bar.
    pub pinned: bool,
}

#[resource(kind = "test.window-bar-tally", label = "Tally")]
impl Tally {
    /// Add one
    #[action(icon = "plus")]
    pub fn add(&mut self) {
        self.count = self.count.saturating_add(1);
    }
}

/// What the tally's window shows: its views, its sidebar.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "test.window-bar-tally.view")]
pub enum TallyView {
    /// Show the list
    #[effect(view, idempotent)]
    List,
    /// Show the board
    #[effect(view, idempotent)]
    Board,
    /// Show the tally's sidebar
    #[effect(view)]
    Sidebar,
}

fn bar(dispatcher: &Dispatcher<Tally>) -> WindowBar<Tally> {
    WindowBar::new(dispatcher.clone())
}

fn press(id: &str) -> ToolbarEvent {
    ToolbarEvent::Action { id: id.into() }
}

/// The record after the dispatcher's queued invocations land on it.
fn landed(dispatcher: &Dispatcher<Tally>) -> Rc<Tally> {
    let mut doc = tilo_resource::Doc::new();
    Tally::initialize(&mut doc).unwrap();
    testing::resolve(dispatcher, &mut doc).unwrap();
    let read = tilo_resource::Reader::<Tally>::read(&doc).unwrap();
    Rc::clone(read.state())
}

/// The id of the invocation the dispatcher queued for an effect.
fn queued(dispatcher: &Dispatcher<Tally>) -> String {
    let (invocation, _) = dispatcher.core().take_attributed().expect("queued");
    invocation.address.action
}

#[test]
fn an_actions_press_submits_it_with_the_id_the_framework_made() {
    let dispatcher = testing::dispatcher::<Tally>("test.window-bar");
    let bar = bar(&dispatcher).action(Tally::ADD.with(()));
    let add = bar.slots().actions.first().cloned().expect("a slot");
    assert_eq!((add.name.as_str(), add.icon.as_str()), ("Add one", "plus"));
    assert!(add.id.starts_with("invoke:"), "the framework's id: {add:?}");
    assert!(add.enabled && !add.toggles);

    assert!(bar.press(&press(&add.id)));
    assert_eq!(landed(&dispatcher).count, 1, "the press submits the action");
}

#[test]
fn an_effects_press_reaches_the_effect_the_app_answers() {
    let dispatcher = testing::dispatcher::<Tally>("test.window-bar");
    let bar = bar(&dispatcher).effect(&TallyView::Sidebar);
    let entry = bar.slots().actions.first().cloned().expect("a slot");
    assert_eq!(entry.name, "Show the tally's sidebar");
    assert!(entry.enabled && !entry.toggles);

    assert!(bar.press(&press(&entry.id)));
    assert_eq!(queued(&dispatcher), TallyView::Sidebar.id());
}

#[test]
fn a_flag_toggles_the_records_own_value() {
    let dispatcher = testing::dispatcher::<Tally>("test.window-bar");
    let off = bar(&dispatcher).flag("pin", "Pin", &Tally::paths().pinned, false);
    let entry = off.slots().actions.first().cloned().expect("a slot");
    assert!(entry.toggles && !entry.on, "it shows the flag's value");
    assert!(entry.enabled, "the participant may edit the record");

    assert!(off.press(&press(&entry.id)));
    assert!(
        landed(&dispatcher).pinned,
        "the press sets the opposite of what it showed"
    );

    let on = bar(&dispatcher).flag("pin", "Pin", &Tally::paths().pinned, true);
    let entry = on.slots().actions.first().cloned().expect("a slot");
    assert!(entry.toggles && entry.on);
    assert!(on.press(&press(&entry.id)));
    assert!(!landed(&dispatcher).pinned, "and back");
}

#[test]
fn a_view_switcher_submits_the_choice_it_carries_and_names_the_chosen_one() {
    let dispatcher = testing::dispatcher::<Tally>("test.window-bar");
    let views = bar(&dispatcher).views(
        "View",
        1,
        [
            Choice::new("List", &TallyView::List),
            Choice::new("Board", &TallyView::Board),
        ],
    );
    let switcher = views.slots().views.expect("the views are a slot");
    assert_eq!(switcher.name, "View");
    assert_eq!(
        switcher
            .choices
            .iter()
            .map(|view| view.label.as_str())
            .collect::<Vec<_>>(),
        ["List", "Board"]
    );
    let shown = switcher.choices[1].clone();
    assert_eq!(switcher.chosen, shown.id, "the second choice is chosen");
    assert!(shown.id.starts_with("invoke:"), "the framework's id");

    assert!(views.press(&press(&shown.id)));
    assert_eq!(queued(&dispatcher), TallyView::Board.id());
    assert!(views.press(&press(&switcher.choices[0].id)));
    assert_eq!(queued(&dispatcher), TallyView::List.id());
}

#[test]
fn a_view_switcher_can_switch_on_a_verb_of_the_record() {
    let dispatcher = testing::dispatcher::<Tally>("test.window-bar");
    let views = bar(&dispatcher).views("View", 0, [Choice::new("Count", Tally::ADD.with(()))]);
    let switcher = views.slots().views.expect("the views are a slot");
    assert_eq!(switcher.choices.len(), 1);
    assert!(views.press(&press(&switcher.choices[0].id)));
    assert_eq!(landed(&dispatcher).count, 1);
}

#[test]
fn a_bar_declared_anew_from_the_same_window_is_the_same_bar() {
    let dispatcher = testing::dispatcher::<Tally>("test.window-bar");
    let declare = |dispatcher: &Dispatcher<Tally>| {
        bar(dispatcher)
            .views(
                "View",
                0,
                [
                    Choice::new("List", &TallyView::List),
                    Choice::new("Board", &TallyView::Board),
                ],
            )
            .effect(&TallyView::Sidebar)
            .flag("pin", "Pin", &Tally::paths().pinned, true)
            .action(Tally::ADD.with(()))
            .slots()
    };
    assert_eq!(declare(&dispatcher), declare(&dispatcher));
}

#[test]
fn a_toggle_shows_the_state_the_effect_it_submits_sets() {
    let dispatcher = testing::dispatcher::<Tally>("test.window-bar");
    let bar = bar(&dispatcher)
        .effect(&TallyView::Sidebar)
        .named("panel-left", "Sidebar")
        .on(true);
    let entry = bar.slots().actions.first().cloned().expect("a slot");
    assert!(entry.toggles && entry.on, "it shows the state it sets");
    assert_eq!(
        (entry.name.as_str(), entry.icon.as_str()),
        ("Sidebar", "panel-left")
    );
    assert!(bar.press(&press(&entry.id)));
    assert_eq!(queued(&dispatcher), TallyView::Sidebar.id());
}

#[test]
fn the_entry_declared_last_takes_its_place_and_its_enabled_state() {
    let dispatcher = testing::dispatcher::<Tally>("test.window-bar");
    let bar = bar(&dispatcher)
        .effect(&TallyView::List)
        .named("upload", "Import")
        .in_menu()
        .effect(&TallyView::Board)
        .named("check", "Apply")
        .enabled(false);
    let toolbar = bar.slots();
    let import = toolbar.actions.first().expect("a slot");
    let apply = toolbar.actions.get(1).expect("a slot");
    assert!(import.menu && import.enabled, "the menu holds the import");
    assert!(
        !apply.menu && !apply.enabled,
        "the row holds Apply, disabled"
    );
}

#[test]
fn an_entry_is_named_as_the_window_says_it_and_a_window_can_have_no_place() {
    let dispatcher = testing::dispatcher::<Tally>("test.window-bar");
    let bar = bar(&dispatcher)
        .place(None)
        .action(Tally::ADD.with(()))
        .named("minus", "Remove one")
        .enabled(false)
        .enabled(true);
    let toolbar = bar.slots();
    assert!(toolbar.location.is_none(), "the window shows no location");
    let entry = toolbar.actions.first().expect("a slot");
    assert_eq!(
        (entry.icon.as_str(), entry.name.as_str()),
        ("minus", "Remove one")
    );
    assert!(
        !entry.enabled,
        "the window narrows what is possible; it never widens it"
    );
}

#[test]
fn the_location_and_the_leading_actions_are_slots_like_any_other() {
    let dispatcher = testing::dispatcher::<Tally>("test.window-bar");
    let bar = bar(&dispatcher)
        .label("Graph 2D")
        .leading(Tally::ADD.with(()))
        .action(Tally::ADD.with(()));
    let toolbar = bar.slots();
    assert_eq!(
        toolbar.location.as_ref().map(|place| place.label.as_str()),
        Some("Graph 2D")
    );
    assert_eq!(toolbar.leading.len(), 1, "leading, not the row");
    assert_eq!(toolbar.actions.len(), 1, "the row's own actions");
}

#[test]
fn the_sidebars_toggle_is_the_apps_when_the_app_shows_the_sidebar() {
    let dispatcher = testing::dispatcher::<Tally>("test.window-bar");
    let bar = bar(&dispatcher).sidebar(true, &TallyView::Sidebar);
    assert_eq!(bar.slots().sidebar, Some(true));
    assert!(bar.press(&ToolbarEvent::ToggleSidebar));
    assert_eq!(queued(&dispatcher), TallyView::Sidebar.id());
    assert!(!bar.press(&ToolbarEvent::Back), "no navigation to run");
}

#[test]
fn the_bars_events_are_taken_and_dispatched_and_others_pass_by() {
    let dispatcher = testing::dispatcher::<Tally>("test.window-bar");
    let actions = bar(&dispatcher).action(Tally::ADD.with(()));
    let add = actions.slots().actions.first().cloned().expect("a slot");
    deliver(press(&add.id));
    assert!(actions.take_events());
    assert_eq!(
        landed(&dispatcher).count,
        1,
        "the record's action was submitted"
    );
    assert!(!actions.take_events(), "nothing left to take");

    let effects = bar(&dispatcher).effect(&TallyView::Sidebar);
    let sidebar = effects.slots().actions.first().cloned().expect("a slot");
    deliver(press(&sidebar.id));
    assert!(effects.take_events());
    assert_eq!(queued(&dispatcher), TallyView::Sidebar.id());

    assert!(!effects.press(&press("someone-elses")));
    assert!(!effects.press(&ToolbarEvent::Back));
}
