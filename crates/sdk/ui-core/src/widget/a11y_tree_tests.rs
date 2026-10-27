//! Per-widget accessibility coverage, proven on the **real export path**.
//!
//! These tests build each newly-covered widget, lay it out exactly as the
//! `App` does, then run the production
//! [`crate::accesskit_bridge::build_tree_update`] over it and assert on the
//! emitted [`accesskit::Node`]s — the same tree that reaches AT-SPI2 / Orca.
//! Nothing here inspects the intermediate [`crate::a11y::AccessNode`] in
//! isolation; every assertion is against the bridge output (role, label,
//! value, toggled/selected/expanded/hidden state, supported actions), so a
//! regression in either the widget's `accessibility()` **or** the bridge's
//! mapping fails the test.

use std::collections::HashMap;

use accesskit::{Action as AkAction, Node as AkNode, NodeId as AkNodeId, Role as AkRole, Toggled};
use taffy::TaffyTree;

use crate::accesskit_bridge::build_tree_update;
use crate::theme::Theme;
use crate::widget::Widget;

/// Lay `root` out (populating the taffy `node_map` the bridge keys ids off)
/// and return the production AccessKit node list.
fn access_nodes(root: &mut dyn Widget) -> Vec<(AkNodeId, AkNode)> {
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
    root.build_layout(&mut taffy, &theme, &mut node_map);
    // Read-only walk of the same widgets; the raw pointers in node_map alias
    // `root`'s subtree, which is owned by the caller and does not move.
    build_tree_update(&*root, &node_map, None, 1.0).nodes
}

fn of_role(nodes: &[(AkNodeId, AkNode)], role: AkRole) -> Vec<&AkNode> {
    nodes
        .iter()
        .filter(|(_, n)| n.role() == role)
        .map(|(_, n)| n)
        .collect()
}

/// The single node of `role` whose label is `label`.
fn by_label<'a>(nodes: &'a [(AkNodeId, AkNode)], role: AkRole, label: &str) -> &'a AkNode {
    nodes
        .iter()
        .find(|(_, n)| n.role() == role && n.label() == Some(label))
        .map_or_else(|| panic!("no {role:?} node labelled {label:?}"), |(_, n)| n)
}

#[test]
fn selection_controls_expose_radio_list_tab_and_toggle_semantics() {
    fn color_swatch_picker_exposes_a_selectable_list() {
        use super::color_swatch::{ColorSwatch, ColorSwatchPicker};
        use vello::peniko::Color;
        let palette = [
            (0xE6, 0x4A, 0x4A),
            (0xE6, 0x8A, 0x3A),
            (0xE6, 0xC8, 0x4A),
            (0x6F, 0xC7, 0x4A),
        ];
        let mut p = ColorSwatchPicker::new(
            palette.map(|(r, g, b)| ColorSwatch::new(Color::from_rgb8(r, g, b))),
            4,
        )
        .with_selected(2);
        let nodes = access_nodes(&mut p);

        let lists = of_role(&nodes, AkRole::List);
        assert_eq!(lists.len(), 1, "one list container");
        let list = lists[0];
        // The chosen swatch's hex is the list value — derived from the real colour.
        assert_eq!(
            list.value(),
            Some("#E6C84A"),
            "value is selected swatch hex"
        );

        let items = of_role(&nodes, AkRole::ListItem);
        assert_eq!(items.len(), palette.len(), "one ListItem per swatch");
        // Item names are the real palette hex values (not fabricated labels).
        let first = by_label(&nodes, AkRole::ListItem, "#E64A4A");
        assert_eq!(first.position_in_set(), Some(1));
        assert_eq!(first.size_of_set(), Some(4));
        // Selection is explicit tri-state on ListItems: the chosen one true, the
        // rest false (so an AT doesn't announce "selected" for the wrong swatch).
        let chosen = by_label(&nodes, AkRole::ListItem, "#E6C84A");
        assert_eq!(
            chosen.is_selected(),
            Some(true),
            "selected swatch is marked selected"
        );
        assert_eq!(
            first.is_selected(),
            Some(false),
            "unselected swatch is explicitly not selected"
        );
        assert!(chosen.supports_action(AkAction::Click));
    }

    fn tab_bar_exposes_each_tab_with_selected_state() {
        use super::tabs::{TabBar, TabDescriptor};
        let mut bar = TabBar::new(
            "grp",
            vec![
                TabDescriptor::new(1, "Overview"),
                TabDescriptor::new(2, "Details"),
                TabDescriptor::new(3, "History"),
            ],
            2, // "Details" active
        );
        let nodes = access_nodes(&mut bar);

        let lists = of_role(&nodes, AkRole::TabList);
        assert_eq!(lists.len(), 1, "one tab list");
        assert_eq!(
            lists[0].value(),
            Some("Details"),
            "value is the active tab title"
        );

        let tabs = of_role(&nodes, AkRole::Tab);
        assert_eq!(tabs.len(), 3, "one Tab per descriptor");
        let details = by_label(&nodes, AkRole::Tab, "Details");
        let overview = by_label(&nodes, AkRole::Tab, "Overview");
        assert_eq!(details.is_selected(), Some(true), "active tab is selected");
        assert_eq!(
            overview.is_selected(),
            Some(false),
            "inactive tab is not selected"
        );
        assert_eq!(details.position_in_set(), Some(2));
        assert_eq!(details.size_of_set(), Some(3));
        assert!(
            details.supports_action(AkAction::Click),
            "tabs are activatable"
        );
        assert!(
            !details.supports_action(AkAction::ScrollIntoView),
            "an unpositioned drawn control does not advertise virtual-row scrolling"
        );
    }

    fn toggle_group_exposes_each_segment_with_checked_state() {
        use super::toggle_group::{Segment, ToggleGroup};
        let mut g = ToggleGroup::new(
            vec![
                Segment::new("Light"),
                Segment::new("Dark"),
                Segment::new("Auto"),
            ],
            1, // "Dark"
        );
        let nodes = access_nodes(&mut g);

        let group = of_role(&nodes, AkRole::Group);
        assert_eq!(group.len(), 1);
        assert_eq!(
            group[0].value(),
            Some("Dark"),
            "value is the selected segment"
        );

        // Segments map to Buttons carrying the pressed (toggled) state.
        let buttons = of_role(&nodes, AkRole::Button);
        assert_eq!(buttons.len(), 3, "one toggle button per segment");
        let dark = by_label(&nodes, AkRole::Button, "Dark");
        let light = by_label(&nodes, AkRole::Button, "Light");
        assert_eq!(
            dark.toggled(),
            Some(Toggled::True),
            "selected segment is pressed"
        );
        assert_eq!(light.toggled(), Some(Toggled::False));
        assert_eq!(dark.position_in_set(), Some(2));
        assert_eq!(dark.size_of_set(), Some(3));
        assert!(dark.supports_action(AkAction::Click));
    }

    color_swatch_picker_exposes_a_selectable_list();
    tab_bar_exposes_each_tab_with_selected_state();
    toggle_group_exposes_each_segment_with_checked_state();
}

// color_swatch::ColorSwatchPicker -> List + ListItem children

// gauge::ProgressRing / gauge::BarGauge -> ProgressBar with % value

#[test]
fn gauges_and_slash_menu_expose_meters_and_menu_visibility() {
    fn ring_gauge_exposes_a_named_percentage_meter() {
        use super::gauge::ProgressRing;
        let mut g = ProgressRing::new(0.42).caption("CPU");
        let nodes = access_nodes(&mut g);
        let bar = by_label(&nodes, AkRole::ProgressIndicator, "CPU");
        // The dim caption names it; the fraction becomes a spoken percentage.
        assert_eq!(bar.value(), Some("42%"), "fraction renders as a percentage");
    }

    fn bar_gauge_prefers_the_caller_value_readout() {
        use super::gauge::BarGauge;
        let mut g = BarGauge::new(0.6).with_label("Memory", "3.2 GB");
        let nodes = access_nodes(&mut g);
        let bar = by_label(&nodes, AkRole::ProgressIndicator, "Memory");
        // A human-facing readout ("3.2 GB") wins over the raw percentage.
        assert_eq!(bar.value(), Some("3.2 GB"));

        // With no readout, the fraction falls back to a percentage.
        let mut plain = BarGauge::new(0.25).with_label("Disk", "");
        let plain_nodes = access_nodes(&mut plain);
        let plain_bar = by_label(&plain_nodes, AkRole::ProgressIndicator, "Disk");
        assert_eq!(plain_bar.value(), Some("25%"));
    }

    fn slash_menu_open_exposes_menu_items() {
        use super::slash_menu::SlashMenu;
        let mut m = SlashMenu::new();
        m.set_visible(true);
        let nodes = access_nodes(&mut m);

        let menus = of_role(&nodes, AkRole::Menu);
        assert_eq!(menus.len(), 1, "one menu container");
        assert_eq!(menus[0].is_expanded(), Some(true), "open menu is expanded");
        assert!(!menus[0].is_hidden(), "open menu is not hidden");

        // Every canonical row is an activatable MenuItem; the cursor row is
        // selected. `Paragraph` is the first row and the default highlight.
        let items = of_role(&nodes, AkRole::MenuItem);
        assert_eq!(
            items.len(),
            m.visible_len(),
            "every block-type row is exposed"
        );
        let para = by_label(&nodes, AkRole::MenuItem, "Paragraph");
        assert_eq!(para.position_in_set(), Some(1));
        assert_eq!(para.size_of_set(), Some(items.len()));
        assert_eq!(
            para.is_selected(),
            Some(true),
            "the highlighted row is selected"
        );
        assert!(
            para.supports_action(AkAction::Click),
            "menu items are activatable"
        );
        let h2 = by_label(&nodes, AkRole::MenuItem, "Heading 2");
        assert_eq!(
            h2.is_selected(),
            Some(false),
            "non-highlighted rows are not selected"
        );
    }

    fn slash_menu_closed_is_a_hidden_menu() {
        use super::slash_menu::SlashMenu;
        let mut m = SlashMenu::new(); // starts closed
        let nodes = access_nodes(&mut m);
        let menus = of_role(&nodes, AkRole::Menu);
        assert_eq!(menus.len(), 1);
        assert!(
            menus[0].is_hidden(),
            "a closed menu collapses out of the reading order"
        );
        assert_eq!(menus[0].is_expanded(), Some(false));
        // No item rows are exposed while closed.
        assert!(of_role(&nodes, AkRole::MenuItem).is_empty());
    }

    ring_gauge_exposes_a_named_percentage_meter();
    bar_gauge_prefers_the_caller_value_readout();
    slash_menu_open_exposes_menu_items();
    slash_menu_closed_is_a_hidden_menu();
}

// slash_menu::SlashMenu -> Menu + MenuItem children (hidden when closed)

// tabs::TabBar -> TabList + Tab children with selected state

// toggle_group::ToggleGroup -> Group + ToggleButton children with checked

// Composed UI: leaf widgets under one presentational container all reach
// the window root through the real collapse/reparent walk.

#[test]
fn new_widgets_are_reachable_and_announced_from_the_window_root() {
    fn composed_ui_reaches_every_new_widget_through_the_window_root() {
        use super::Column;
        use super::color_swatch::{ColorSwatch, ColorSwatchPicker};
        use super::gauge::{BarGauge, ProgressRing};
        use super::slash_menu::SlashMenu;
        use super::toggle_group::{Segment, ToggleGroup};

        let mut slash = SlashMenu::new();
        slash.set_visible(true);
        let swatches = [ColorSwatch::named(
            vello::peniko::Color::from_rgb8(0x4A, 0x8F, 0xE6),
            "Blue",
        )];
        let mut col = Column::new()
            .push(Box::new(ToggleGroup::new(
                vec![
                    Segment::new("Allow"),
                    Segment::new("Inherit"),
                    Segment::new("Deny"),
                ],
                2,
            )))
            .push(Box::new(
                ColorSwatchPicker::new(swatches, 1).with_selected(0),
            ))
            .push(Box::new(ProgressRing::new(0.9).caption("GPU")))
            .push(Box::new(BarGauge::new(0.5).with_label("Battery", "50%")))
            .push(Box::new(slash));
        let nodes = access_nodes(&mut col);

        // The presentational Column collapses; its interactive descendants reparent
        // onto the synthetic Window root, which must own all five.
        let (_, window) = nodes
            .iter()
            .find(|(id, _)| *id == crate::accesskit_bridge::ROOT_ID)
            .expect("window root present");
        assert_eq!(
            window.children().len(),
            5,
            "all five widgets hang off the window root"
        );
        // No GenericContainer leaked from the collapsed Column.
        assert!(of_role(&nodes, AkRole::GenericContainer).is_empty());

        assert_eq!(of_role(&nodes, AkRole::Group).len(), 1, "segmented group");
        assert_eq!(of_role(&nodes, AkRole::List).len(), 1, "swatch list");
        assert_eq!(
            of_role(&nodes, AkRole::ProgressIndicator).len(),
            2,
            "two gauges"
        );
        assert_eq!(of_role(&nodes, AkRole::Menu).len(), 1, "slash menu");
        assert!(nodes.iter().any(|(_, n)| n.label() == Some("GPU")));
        assert!(nodes.iter().any(|(_, n)| n.label() == Some("Battery")));
    }

    fn screen_reader_announces_focus_and_value_change_for_new_widgets() {
        use super::gauge::ProgressRing;
        use super::toggle_group::{Segment, ToggleGroup};
        use crate::screen_reader::{ScreenReader, describe};

        // Focus narration composes name + role + value + state straight off the
        // widget's own accessibility() node.
        let segmented = ToggleGroup::new(
            vec![
                Segment::new("Allow"),
                Segment::new("Inherit"),
                Segment::new("Deny"),
            ],
            0,
        );
        let group = Widget::accessibility(&segmented).unwrap();
        // The group is unnamed and announces its active choice ("Allow") as value.
        assert_eq!(describe(&group), "group, Allow");
        // Its first segment reads as an on toggle positioned in the set.
        assert_eq!(
            describe(&group.children[0]),
            "Allow, toggle button, on, 1 of 3"
        );
        assert_eq!(
            describe(&group.children[1]),
            "Inherit, toggle button, off, 2 of 3"
        );

        let gauge = ProgressRing::new(0.42).caption("CPU");
        let gnode = Widget::accessibility(&gauge).unwrap();
        assert_eq!(describe(&gnode), "CPU, progress bar, 42%");

        // Drive the real reader: focus announcement, then a value change.
        let rec = Rec::default();
        let mut reader = ScreenReader::with_speaker(Box::new(rec.clone()));
        reader.announce_focus(&gnode);
        reader.announce_value_change("90%");
        assert_eq!(
            rec.0.lock().unwrap().spoken(),
            &["CPU, progress bar, 42%".to_string(), "90%".to_string()]
        );
    }

    composed_ui_reaches_every_new_widget_through_the_window_root();
    screen_reader_announces_focus_and_value_change_for_new_widgets();
}

// Screen-reader announcement path: focus + value-change narration on the
// same AccessNodes the bridge consumes.

use std::sync::{Arc, Mutex};
use tts::{NullSpeaker, Speaker, Utterance};

/// A speaker that records what the reader pushes through it, for readback.
/// `Arc<Mutex<_>>` rather than `Rc<RefCell<_>>` so `Rec` implements `Send`, as
/// required by the [`Speaker`] trait, without an `unsafe impl`.
#[derive(Clone, Default)]
struct Rec(Arc<Mutex<NullSpeaker>>);
impl Speaker for Rec {
    fn speak_utterance(&mut self, u: Utterance) {
        self.0.lock().unwrap().speak_utterance(u);
    }
    fn stop(&mut self) {
        self.0.lock().unwrap().stop();
    }
    fn set_rate(&mut self, r: f32) {
        self.0.lock().unwrap().set_rate(r);
    }
    fn set_voice(&mut self, v: &str) {
        self.0.lock().unwrap().set_voice(v);
    }
    fn is_speaking(&self) -> bool {
        self.0.lock().unwrap().is_speaking()
    }
    fn backend_name(&self) -> &'static str {
        "rec"
    }
}
