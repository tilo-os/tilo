// Test-only on its own, so the packaging scan of resource kinds, which
// reads each file alone, does not take its probe kind for the SDK's.
#![cfg(test)]

use super::*;
use tilo_resource::{Model, Resource, resource};
use ui_core::window_toolbar::{ToolbarAction, ToolbarEvent};

#[derive(Model, Default, Debug, Clone)]
pub struct Counter {
    pub count: i64,
}

#[resource(kind = "test.bar-counter", label = "Counter")]
impl Counter {
    /// Add one
    #[action(icon = "plus")]
    pub fn add(&mut self) {
        self.count = self.count.saturating_add(1);
    }

    /// Clear the count
    #[action(destructive, icon = "trash-2")]
    pub fn clear(&mut self) {
        self.count = 0;
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "test.bar-counter.view")]
pub enum CounterView {
    /// Zoom in
    #[effect(view, icon = "zoom-in")]
    ZoomIn,
}

fn press(entry: &ToolbarAction) -> ToolbarEvent {
    ToolbarEvent::Action {
        id: entry.id.clone(),
    }
}

#[test]
fn an_entry_is_named_and_pictured_as_declared_and_its_press_submits_it() {
    let dispatcher = testing::dispatcher::<Counter>("test.bar");
    let bar = BarActions::new(dispatcher.clone());
    let add = bar.action(Counter::ADD.invoke());
    assert_eq!((add.name.as_str(), add.icon.as_str()), ("Add one", "plus"));
    assert!(add.enabled && !add.destructive);
    let clear = bar.action(Counter::CLEAR.invoke());
    assert!(clear.destructive, "a destructive verb's entry is too");
    assert_ne!(add.id, clear.id);

    assert!(bar.press(&press(&add)));
    assert!(bar.press(&press(&add)));
    let mut doc = tilo_resource::Doc::new();
    Counter::initialize(&mut doc).unwrap();
    let applied = testing::resolve(&dispatcher, &mut doc).unwrap();
    assert_eq!(applied.len(), 2, "each press is one step");
    let read = tilo_resource::Reader::<Counter>::read(&doc).unwrap();
    assert_eq!(read.state().count, 2);
}

#[test]
fn the_apps_own_slots_and_other_events_pass_by() {
    let bar = BarActions::new(testing::dispatcher::<Counter>("test.bar"));
    let _ = bar.action(Counter::ADD.invoke());
    assert!(!bar.press(&ToolbarEvent::Action {
        id: "reload".into()
    }));
    assert!(!bar.press(&ToolbarEvent::Back));
}

#[test]
fn an_effects_entry_reaches_the_effect() {
    let dispatcher = testing::dispatcher::<Counter>("test.bar");
    let bar = BarActions::new(dispatcher.clone());
    let zoom = bar.effect(&CounterView::ZoomIn);
    assert_eq!(
        (zoom.name.as_str(), zoom.icon.as_str()),
        ("Zoom in", "zoom-in")
    );
    assert!(zoom.enabled);
    assert!(bar.press(&press(&zoom)));
    let (invocation, _) = dispatcher.core().take_attributed().expect("queued");
    assert_eq!(invocation.address.action, CounterView::ZoomIn.id());
}

#[derive(Clone, Debug, PartialEq)]
enum Command {
    Panel,
    Open(u32),
}

#[test]
fn a_commands_entry_hands_its_command_back_and_keeps_its_id() {
    let bar = BarCommands::new();
    let panel = bar.entry("panel-left", "Panel", Command::Panel);
    let open = bar.entry("folder-open", "Open", Command::Open(1));
    assert_ne!(panel.id, open.id);
    assert_eq!(
        bar.entry("panel-left", "Panel", Command::Panel).id,
        panel.id
    );
    assert_eq!(bar.pressed(&press(&open)), Some(Command::Open(1)));
    assert_eq!(bar.pressed(&press(&panel)), Some(Command::Panel));
    assert_eq!(
        bar.pressed(&ToolbarEvent::Action {
            id: "reload".into()
        }),
        None
    );
    let other = BarCommands::new();
    assert_ne!(
        other.entry("panel-left", "Panel", Command::Panel).id,
        panel.id
    );
    assert_eq!(other.pressed(&press(&open)), None, "another bar's entry");
}
