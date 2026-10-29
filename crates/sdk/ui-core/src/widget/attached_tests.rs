use std::collections::BTreeMap;

use super::*;
use crate::component::{BoundValue, Fired, Intent, Value};

/// A component instance as a test writes it: its kind, props, the events it gives and the
/// components in its slots.
struct Node {
    kind: &'static str,
    key: String,
    props: BTreeMap<String, Value>,
    events: Vec<(&'static str, Option<tilo_actions::ActionAddress>)>,
    slots: BTreeMap<&'static str, Vec<Node>>,
    queue: crate::Intents<Fired>,
}

impl Node {
    fn new(kind: &'static str, key: &str, queue: &crate::Intents<Fired>) -> Self {
        Self {
            kind,
            key: key.to_owned(),
            props: BTreeMap::new(),
            events: Vec::new(),
            slots: BTreeMap::new(),
            queue: queue.clone(),
        }
    }

    fn prop(mut self, name: &str, value: Value) -> Self {
        self.props.insert(name.to_owned(), value);
        self
    }

    /// The event `name`, running the action `action` on `on` when given.
    fn on(mut self, name: &'static str, runs: Option<(&str, &str)>) -> Self {
        let address = runs.map(|(on, action)| tilo_actions::ActionAddress::new(on, action));
        self.events.push((name, address));
        self
    }

    fn slot(mut self, name: &'static str, nodes: Vec<Node>) -> Self {
        self.slots.insert(name, nodes);
        self
    }
}

impl Source for Node {
    fn key(&self) -> &str {
        &self.key
    }

    fn prop(&mut self, name: &str) -> Option<Value> {
        self.props.get(name).cloned()
    }

    fn event(&mut self, name: &str) -> Event {
        match self.events.iter().find(|(event, _)| *event == name) {
            Some((_, address)) => {
                Event::View(Intent::new(&self.queue, &self.key, name).invoking(address.clone()))
            }
            None => Event::None,
        }
    }

    fn binding(&mut self, _name: &str) -> Option<BoundValue> {
        None
    }

    fn children(&mut self, _slot: &str) -> Vec<Element> {
        Vec::new()
    }

    fn slot_kinds(&mut self, slot: &str) -> Vec<Option<String>> {
        self.slots
            .get(slot)
            .map(|nodes| nodes.iter().map(|n| Some(n.kind.to_owned())).collect())
            .unwrap_or_default()
    }

    fn components(
        &mut self,
        slot: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<usize, String> {
        let Some(nodes) = self.slots.get_mut(slot) else {
            return Ok(0);
        };
        for node in nodes.iter_mut() {
            read(node)?;
        }
        Ok(nodes.len())
    }
}

fn item(queue: &crate::Intents<Fired>, label: &str, runs: Option<(&str, &str)>) -> Node {
    Node::new("MenuItem", label, queue)
        .prop("label", Value::Text(label.to_owned()))
        .on("choose", runs)
}

fn labels(parts: &[MenuPart]) -> Vec<String> {
    parts
        .iter()
        .map(|part| match part {
            MenuPart::Item(item) => item.label.clone(),
            MenuPart::Separator => "—".to_owned(),
            MenuPart::Emoji(bar) => format!("emoji {}", bar.emoji.join("")),
        })
        .collect()
}

/// A message's row as a view writes it: its own menu (an emoji bar, Reply, Pin), and the
/// actions the system offers on the message (Pin, Delete).
fn message(queue: &crate::Intents<Fired>) -> (Attachments, Vec<MenuPart>) {
    let menu = Node::new("ContextMenu", "menu", queue).slot(
        "items",
        vec![
            Node::new("EmojiBar", "bar", queue)
                .prop(
                    "emoji",
                    Value::List(vec![Value::Text("👍".into()), Value::Text("🎉".into())]),
                )
                .prop("more", Value::Bool(true))
                .on("pick", None),
            Node::new("MenuSeparator", "sep", queue),
            item(queue, "Reply", None),
            item(queue, "Pin", Some(("m1", "pin"))),
        ],
    );
    let swipe = Node::new("Swipe", "swipe", queue).slot(
        "trailing",
        vec![
            Node::new("SwipeAction", "reply", queue)
                .prop("label", Value::Text("Reply".into()))
                .on("swipe", None),
        ],
    );
    let mut row = Node::new("ListItem", "row", queue).slot("attached", vec![menu, swipe]);
    let mut attached = Attachments::default();
    attached
        .fill(&mut row, "attached")
        .expect("what the view attached reads");
    let mut offered = Node::new("ListItem", "item", queue).slot(
        "menu",
        vec![
            item(queue, "Pin", Some(("m1", "pin"))),
            item(queue, "Delete", Some(("m1", "delete"))),
        ],
    );
    let mut menu: Vec<MenuPart> = Vec::new();
    menu.fill(&mut offered, "menu")
        .expect("the offered menu reads");
    (attached, menu)
}

#[test]
fn a_views_menu_comes_first_and_the_system_adds_what_it_did_not_place() {
    let queue = crate::Intents::default();
    let (attached, offered) = message(&queue);
    let menu = item_menu(attached.menu, offered);
    assert_eq!(
        labels(&menu),
        ["emoji 👍🎉", "—", "Reply", "Pin", "—", "Delete"],
        "the view's order first; Pin is placed already, so only Delete is added after a rule"
    );
}

#[test]
fn every_offered_action_stays_reachable_when_the_view_places_none() {
    let queue = crate::Intents::default();
    let (_, offered) = message(&queue);
    let menu = item_menu(Vec::new(), offered);
    assert_eq!(labels(&menu), ["Pin", "Delete"], "no leading rule");
}

#[test]
fn a_view_that_places_every_action_gets_nothing_added() {
    let queue = crate::Intents::default();
    let placed = vec![
        MenuPart::Item(MenuItemProps {
            label: "Pin".into(),
            choose: Event::View(
                Intent::new(&queue, "p", "choose")
                    .invoking(Some(tilo_actions::ActionAddress::new("m1", "pin"))),
            ),
            ..MenuItemProps::default()
        }),
        MenuPart::Item(MenuItemProps {
            label: "Delete".into(),
            choose: Event::View(
                Intent::new(&queue, "d", "choose")
                    .invoking(Some(tilo_actions::ActionAddress::new("m1", "delete"))),
            ),
            ..MenuItemProps::default()
        }),
    ];
    let (_, offered) = message(&queue);
    assert_eq!(labels(&item_menu(placed, offered)), ["Pin", "Delete"]);
}

#[test]
fn the_same_action_on_another_resource_is_not_placed() {
    let queue = crate::Intents::default();
    let placed = vec![MenuPart::Item(MenuItemProps {
        label: "Pin".into(),
        choose: Event::View(
            Intent::new(&queue, "p", "choose")
                .invoking(Some(tilo_actions::ActionAddress::new("m2", "pin"))),
        ),
        ..MenuItemProps::default()
    })];
    let (_, offered) = message(&queue);
    assert_eq!(
        labels(&item_menu(placed, offered)),
        ["Pin", "—", "Pin", "Delete"]
    );
}

#[test]
fn a_menus_entries_drop_rules_at_either_end_and_doubled() {
    let queue = crate::Intents::default();
    let parts = vec![
        MenuPart::Separator,
        MenuPart::Item(MenuItemProps {
            label: "Reply".into(),
            choose: Event::View(Intent::new(&queue, "r", "choose")),
            ..MenuItemProps::default()
        }),
        MenuPart::Separator,
        MenuPart::Separator,
        MenuPart::Item(MenuItemProps {
            label: "Delete".into(),
            destructive: true,
            choose: Event::View(Intent::new(&queue, "d", "choose")),
            ..MenuItemProps::default()
        }),
        MenuPart::Separator,
    ];
    let entries = menu_entries(parts);
    let shown: Vec<Option<&str>> = entries.iter().map(|entry| entry.item().label()).collect();
    assert_eq!(shown, [Some("Reply"), None, Some("Delete")]);
}

#[test]
fn what_a_view_attached_reads_its_swipe_and_its_menu() {
    let queue = crate::Intents::default();
    let (attached, _) = message(&queue);
    assert_eq!(
        attached
            .swipe
            .trailing
            .as_ref()
            .map(|swipe| swipe.label.as_str()),
        Some("Reply")
    );
    assert!(attached.swipe.leading.is_none());
    assert_eq!(attached.menu.len(), 4);
    assert!(attached.toolbar.is_empty());
}

#[test]
fn picking_from_the_menus_emoji_bar_reports_the_emoji() {
    let queue = crate::Intents::default();
    let (attached, _) = message(&queue);
    let entries = menu_entries(attached.menu);
    let mut menu = crate::widget::ContextMenu::new();
    let mut runs = Vec::new();
    for entry in entries {
        runs.push(entry.run_for_tests());
        menu.push(entry.item().clone());
    }
    menu.open_at([10.0, 10.0]);
    // Arm the bar's second cell from the keyboard and pick it.
    menu.focus_item(0);
    crate::testing::dispatch(
        &mut menu,
        &crate::synthetic_input::named_key(winit::keyboard::NamedKey::ArrowRight),
    );
    crate::testing::dispatch(
        &mut menu,
        &crate::synthetic_input::named_key(winit::keyboard::NamedKey::Enter),
    );
    assert_eq!(menu.take_selection(), Some(0), "the bar's row was chosen");
    if let Some(run) = &runs[0] {
        run();
    }
    let fired = queue.take();
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].event, "pick");
    assert_eq!(fired[0].value, Some(Value::Text("🎉".into())));
}
