//! The feedback and display components: their parts read from their slots, and what a person
//! does to them is reported as their events.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion"
)]

use std::collections::BTreeMap;

use super::*;
use crate::headless::FrameHarness;
use crate::theme::Theme;
use crate::widget::{
    Badge, BadgeProps, Breadcrumb, BreadcrumbProps, DisclosureRow, DisclosureRowProps, StatGroup,
    StatGroupProps, Toolbar, ToolbarProps, Widget,
};

/// An instance as a test gives it: values by prop, the events it handles queued on `queue`,
/// and the components in its slots.
struct Node {
    kind: &'static str,
    key: String,
    props: BTreeMap<String, Value>,
    events: Vec<&'static str>,
    slots: Vec<(&'static str, Node)>,
    queue: crate::Intents<Fired>,
}

impl Node {
    fn new(kind: &'static str, key: &str, queue: &crate::Intents<Fired>) -> Self {
        Self {
            kind,
            key: key.to_owned(),
            props: BTreeMap::new(),
            events: Vec::new(),
            slots: Vec::new(),
            queue: queue.clone(),
        }
    }

    fn prop(mut self, name: &str, value: Value) -> Self {
        self.props.insert(name.to_owned(), value);
        self
    }

    fn on(mut self, event: &'static str) -> Self {
        self.events.push(event);
        self
    }

    fn holding(mut self, slot: &'static str, child: Node) -> Self {
        self.slots.push((slot, child));
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
        if self.events.contains(&name) {
            Event::View(Intent::new(&self.queue, &self.key, name))
        } else {
            Event::None
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
            .iter()
            .filter(|(name, _)| *name == slot)
            .map(|(_, child)| Some(child.kind.to_owned()))
            .collect()
    }

    fn components(
        &mut self,
        slot: &str,
        read: &mut dyn FnMut(&mut dyn Source) -> Result<(), String>,
    ) -> Result<usize, String> {
        let mut count = 0;
        for (name, child) in &mut self.slots {
            if *name == slot {
                read(child)?;
                count += 1;
            }
        }
        Ok(count)
    }
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

fn harness<W: Widget + 'static>(root: W) -> FrameHarness {
    FrameHarness::for_root(crate::State::new(), root, (900, 600), Theme::light())
}

fn fired(key: &str, event: &str, value: Option<Value>) -> Fired {
    Fired {
        key: key.to_owned(),
        event: event.to_owned(),
        value,
    }
}

#[test]
fn a_stat_group_reads_its_tiles_and_a_pressed_tile_reports_itself() {
    let queue = crate::Intents::default();
    let tile = |key: &str, label: &str, value: &str| {
        Node::new("StatTile", key, &queue)
            .prop("label", text(label))
            .prop("value", text(value))
            .on("activate")
    };
    let mut group = Node::new("StatGroup", "week", &queue)
        .holding("tiles", tile("editors", "Editors", "18"))
        .holding("tiles", tile("comments", "Comments", "42"));
    let props = StatGroupProps::from_source(&mut group).unwrap();
    assert_eq!(props.tiles.len(), 2);
    assert_eq!(props.tiles[1].key, "comments");
    let mut host = harness(StatGroup::from_props("week", props));
    let rect = host.root::<StatGroup>().children()[1].get_rect();
    host.click_at(rect.center().x, rect.center().y);
    assert_eq!(queue.take(), [fired("comments", "activate", None)]);
}

#[test]
fn a_stat_group_holds_only_stat_tiles() {
    let queue = crate::Intents::default();
    let mut group = Node::new("StatGroup", "week", &queue).holding(
        "tiles",
        Node::new("Badge", "new", &queue).prop("count", Value::Int(3)),
    );
    // The host reads a slot's children by the component the slot accepts.
    assert_eq!(
        <Vec<crate::widget::StatTileProps> as SlotType>::accepts(),
        Accepts::Component("StatTile".into())
    );
    assert!(StatGroupProps::from_source(&mut group).is_err());
}

#[test]
fn a_pressed_toolbar_command_reports_itself_and_its_parts_keep_their_order() {
    let queue = crate::Intents::default();
    let mut bar = Node::new("Toolbar", "format", &queue)
        .holding(
            "items",
            Node::new("ToolbarButton", "bold", &queue)
                .prop("label", text("Bold"))
                .prop("toggled", Value::Bool(false))
                .on("press"),
        )
        .holding("items", Node::new("ToolbarSeparator", "rule", &queue))
        .holding(
            "items",
            Node::new("ToolbarButton", "share", &queue)
                .prop("label", text("Share"))
                .on("press"),
        );
    let props = ToolbarProps::from_source(&mut bar).unwrap();
    let mut host = harness(Toolbar::from_props("format", props));
    assert_eq!(host.root::<Toolbar>().separator_count(), 1);
    let share = host.root::<Toolbar>().item_rect(2).expect("share is shown");
    host.click_at(share.center().x, share.center().y);
    let bold = host.root::<Toolbar>().item_rect(0).expect("bold is shown");
    host.click_at(bold.center().x, bold.center().y);
    assert_eq!(
        queue.take(),
        [fired("share", "press", None), fired("bold", "press", None)]
    );
}

#[test]
fn a_toolbar_refuses_a_component_it_does_not_hold() {
    let queue = crate::Intents::default();
    let mut bar = Node::new("Toolbar", "format", &queue).holding(
        "items",
        Node::new("MenuItem", "x", &queue).prop("label", text("Rename")),
    );
    let error = ToolbarProps::from_source(&mut bar).err().expect("refused");
    assert!(error.to_string().contains("ui.ToolbarButton"), "{error}");
}

#[test]
fn a_pressed_place_of_a_path_reports_itself() {
    let queue = crate::Intents::default();
    let segment = |key: &str, label: &str| {
        Node::new("BreadcrumbSegment", key, &queue)
            .prop("label", text(label))
            .on("activate")
    };
    let mut trail = Node::new("Breadcrumb", "path", &queue)
        .holding("segments", segment("workspace", "Workspace"))
        .holding("segments", segment("projects", "Projects"))
        .holding("segments", segment("notes", "Release notes"));
    let props = BreadcrumbProps::from_source(&mut trail).unwrap();
    let mut host = harness(Breadcrumb::from_props("path", props));
    let (_, projects) = host.root::<Breadcrumb>().segment_rects()[1];
    host.click_at(projects.center().x, projects.center().y);
    assert_eq!(queue.take(), [fired("projects", "activate", None)]);
}

#[test]
fn a_disclosure_row_reports_its_toggle_and_opens_where_its_view_says_without_reporting() {
    let queue = crate::Intents::default();
    let row = |expanded: Option<bool>| {
        let node = Node::new("DisclosureRow", "summary", &queue)
            .prop("title", text("Rewrote the summary"))
            .on("toggle");
        let mut node = match expanded {
            Some(open) => node.prop("expanded", Value::Bool(open)),
            None => node,
        };
        DisclosureRowProps::from_source(&mut node).unwrap()
    };
    let mut host = harness(DisclosureRow::from_props("summary", row(None)));
    let rect = host.root::<DisclosureRow>().get_rect();
    host.click_at(rect.center().x, rect.y0 + 8.0);
    assert!(host.root::<DisclosureRow>().is_open());
    assert_eq!(
        queue.take(),
        [fired("summary", "toggle", Some(Value::Bool(true)))]
    );

    let mut widget = DisclosureRow::from_props("summary", row(Some(false)));
    assert!(!widget.is_open());
    assert!(widget.set_props(row(Some(true))));
    assert!(widget.is_open());
    assert!(
        queue.take().is_empty(),
        "the view's own change is not reported back"
    );
}

#[test]
fn a_badge_takes_a_new_count_in_place() {
    let props = |count: i64| BadgeProps {
        count: Some(count),
        ..BadgeProps::default()
    };
    let mut badge = Badge::from_props("unread", props(3));
    assert_eq!(badge.display_text().as_deref(), Some("3"));
    assert!(badge.set_props(props(120)));
    assert_eq!(badge.display_text().as_deref(), Some("99+"));
}
