use std::collections::BTreeMap;

use super::*;
use tilo_ui_base::{component::BoundValue, component::Fired, component::Value};

fn message_actions(on: &str) -> tilo_actions::ResourceSpec {
    let mut published = tilo_actions::ResourceSpec::new(on, "test.message", "Message");
    for id in ["pin", "delete"] {
        published = published.with_action(
            tilo_actions::ActionSpec::operation(
                id,
                id,
                tilo_actions::OperationKind::Effect,
                "test.message.effect",
                &id,
            )
            .expect("a test action"),
        );
    }
    published
}

fn bound(on: &str, action: &str) -> Event {
    Event::Action(crate::Bind::bound(
        tilo_actions::ActionBinding::unit_for(&message_actions(on), action)
            .expect("the message publishes it"),
    ))
}

struct Node {
    kind: &'static str,
    key: String,
    props: BTreeMap<String, Value>,
    events: Vec<(&'static str, Option<(&'static str, &'static str)>)>,
    parts: BTreeMap<&'static str, Vec<Node>>,
    queue: tilo_ui_base::Intents<Fired>,
}

impl Node {
    fn new(kind: &'static str, key: &str, queue: &tilo_ui_base::Intents<Fired>) -> Self {
        Self {
            kind,
            key: key.to_owned(),
            props: BTreeMap::new(),
            events: Vec::new(),
            parts: BTreeMap::new(),
            queue: queue.clone(),
        }
    }

    fn prop(mut self, name: &str, value: Value) -> Self {
        self.props.insert(name.to_owned(), value);
        self
    }

    fn on(mut self, name: &'static str, runs: Option<(&'static str, &'static str)>) -> Self {
        self.events.push((name, runs));
        self
    }

    fn part(mut self, name: &'static str, nodes: Vec<Node>) -> Self {
        self.parts.insert(name, nodes);
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
            Some((_, Some((on, action)))) => bound(on, action),
            Some((_, None)) => Event::View(tilo_ui_base::component::Intent::new(
                &self.queue,
                &self.key,
                name,
            )),
            None => Event::None,
        }
    }

    fn binding(&mut self, _name: &str) -> Option<BoundValue> {
        None
    }

    fn children(&mut self, _part: &str) -> Vec<Element> {
        Vec::new()
    }

    fn part_kinds(&mut self, part: &str) -> Vec<Option<String>> {
        self.parts
            .get(part)
            .map(|nodes| nodes.iter().map(|n| Some(n.kind.to_owned())).collect())
            .unwrap_or_default()
    }

    fn components(
        &mut self,
        part: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<usize, String> {
        let Some(nodes) = self.parts.get_mut(part) else {
            return Ok(0);
        };
        for node in nodes.iter_mut() {
            read(node)?;
        }
        Ok(nodes.len())
    }
}

fn item(
    queue: &tilo_ui_base::Intents<Fired>,
    label: &str,
    runs: Option<(&'static str, &'static str)>,
) -> Node {
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

fn message(queue: &tilo_ui_base::Intents<Fired>) -> (Attachments, Vec<MenuPart>) {
    let menu = Node::new("ContextMenu", "menu", queue).part(
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
    let swipe = Node::new("Swipe", "swipe", queue).part(
        "trailing",
        vec![
            Node::new("SwipeAction", "reply", queue)
                .prop("label", Value::Text("Reply".into()))
                .on("swipe", None),
        ],
    );
    let mut row = Node::new("ListItem", "row", queue).part("attached", vec![menu, swipe]);
    let mut attached = Attachments::default();
    attached
        .fill(&mut row, "attached")
        .expect("what the view attached reads");
    let mut offered = Node::new("ListItem", "item", queue).part(
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

tilo_test_env::scenario! {
    /// What a view attaches to an item: its menu, its swipe and the commands the system adds.
    /// Each phase was a test of its own and builds its own fixture.
    fn attached_menus_place_what_a_view_and_the_system_offer;

    phase the_views_menu_comes_first_and_the_system_adds_only_what_it_did_not_place;
    {
        let queue = tilo_ui_base::Intents::default();

        // The view's order first; Pin is placed already, so only Delete is added
        // after a rule.
        let (attached, offered) = message(&queue);
        assert_eq!(
            labels(&item_menu(attached.menu, offered)),
            ["emoji 👍🎉", "—", "Reply", "Pin", "—", "Delete"],
            "the view's order first; Pin is placed already, so only Delete is added after a rule"
        );

        // Every offered action stays reachable when the view places none.
        let (_, offered) = message(&queue);
        assert_eq!(labels(&item_menu(Vec::new(), offered)), ["Pin", "Delete"], "no leading rule");

        // A view that places every action gets nothing added.
        let placed = vec![
            MenuPart::Item(MenuItemProps {
                label: "Pin".into(),
                choose: bound("m1", "pin"),
                ..MenuItemProps::default()
            }),
            MenuPart::Item(MenuItemProps {
                label: "Delete".into(),
                choose: bound("m1", "delete"),
                ..MenuItemProps::default()
            }),
        ];
        let (_, offered) = message(&queue);
        assert_eq!(labels(&item_menu(placed, offered)), ["Pin", "Delete"]);

        // The same action on another resource is not placed.
        let placed = vec![MenuPart::Item(MenuItemProps {
            label: "Pin".into(),
            choose: bound("m2", "pin"),
            ..MenuItemProps::default()
        })];
        let (_, offered) = message(&queue);
        assert_eq!(
            labels(&item_menu(placed, offered)),
            ["Pin", "—", "Pin", "Delete"]
        );

        // A command the view runs itself is not added again by its name.
        let (_, offered) = message(&queue);
        let own = |label: &str| {
            MenuPart::Item(MenuItemProps {
                label: label.into(),
                choose: Event::View(tilo_ui_base::component::Intent::new(&queue, label, "choose")),
                ..MenuItemProps::default()
            })
        };
        let menu = item_menu(vec![own("Pin")], offered);
        assert_eq!(labels(&menu), ["Pin", "—", "Delete"], "Pin stands once");
    }

    phase a_menus_entries_drop_rules_at_either_end_and_doubled;
    {
        let queue = tilo_ui_base::Intents::default();
        let parts = vec![
            MenuPart::Separator,
            MenuPart::Item(MenuItemProps {
                label: "Reply".into(),
                choose: Event::View(tilo_ui_base::component::Intent::new(&queue, "r", "choose")),
                ..MenuItemProps::default()
            }),
            MenuPart::Separator,
            MenuPart::Separator,
            MenuPart::Item(MenuItemProps {
                label: "Delete".into(),
                destructive: true,
                key: Some("Ctrl+Enter".into()),
                choose: Event::View(tilo_ui_base::component::Intent::new(&queue, "d", "choose")),
                ..MenuItemProps::default()
            }),
            MenuPart::Separator,
        ];
        let entries = menu_entries(parts);
        let shown: Vec<Option<&str>> = entries.iter().map(|entry| entry.item().label()).collect();
        assert_eq!(shown, [Some("Reply"), None, Some("Delete")]);
        assert_eq!(
            entries[2].item().shortcut(),
            Some(&tilo_ui_base::platform::Shortcut::from("Ctrl+Enter")),
            "the key the command declares shows at its row's end"
        );
    }

    phase what_a_view_attached_reads_its_swipe_and_its_menu;
    {
        let queue = tilo_ui_base::Intents::default();
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

    phase picking_from_the_menus_emoji_bar_reports_the_emoji;
    {
        let queue = tilo_ui_base::Intents::default();
        let (attached, _) = message(&queue);
        let entries = menu_entries(attached.menu);
        let mut menu = crate::widget::ContextMenu::new();
        let mut runs = Vec::new();
        for entry in entries {
            runs.push(entry.run_for_tests());
            menu.push(entry.item().clone());
        }
        menu.open_at([10.0, 10.0]);
        menu.focus_item(0);
        crate::testing::dispatch(
            &mut menu,
            &tilo_ui_base::synthetic_input::named_key(winit::keyboard::NamedKey::ArrowRight),
        );
        crate::testing::dispatch(
            &mut menu,
            &tilo_ui_base::synthetic_input::named_key(winit::keyboard::NamedKey::Enter),
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

    phase a_run_of_grouped_commands_folds_into_one_row_that_opens_them;
    {
        let queue = tilo_ui_base::Intents::default();
        let grouped = |label: &str| {
            MenuPart::Item(MenuItemProps {
                label: label.into(),
                group: Some("Due".into()),
                choose: Event::View(tilo_ui_base::component::Intent::new(&queue, label, "choose")),
                ..MenuItemProps::default()
            })
        };
        let entries = menu_entries(vec![grouped("Today"), grouped("Tomorrow")]);
        let shown: Vec<Option<&str>> = entries.iter().map(|entry| entry.item().label()).collect();
        assert_eq!(shown, [Some("Due")], "one run of a group is one row");
        let entries = menu_entries(vec![grouped("Today")]);
        assert_eq!(
            entries[0].item().label(),
            Some("Today"),
            "a group of one command stays the command"
        );
    }
}
