//! Per-widget accessibility coverage, proven on the **real export path**.

use std::collections::HashMap;

use accesskit::{Action as AkAction, Node as AkNode, NodeId as AkNodeId, Role as AkRole, Toggled};
use taffy::TaffyTree;

use crate::accesskit_bridge::build_tree_update;
use crate::theme::Theme;
use crate::widget::Widget;

fn access_nodes(root: &mut dyn Widget) -> Vec<(AkNodeId, AkNode)> {
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
    root.build_layout(&mut taffy, &theme, &mut node_map);
    // Read-only walk of the same widgets; the raw pointers in node_map alias
    // `root`'s subtree, which is owned by the caller and does not move.
    build_tree_update(&*root, &node_map, None, 1.0).nodes
}

/// A filter menu of kinds of text, as an editor offers one when a person types `/`.
fn kind_menu() -> super::FilterMenu<&'static str> {
    static SPEC: super::filter_menu::MenuSpec = super::filter_menu::MenuSpec {
        width: 248.0,
        max_rows: 8,
        placeholder: "slash-menu-filter",
        access_name: "slash-menu-name",
        widget_name: "KindMenu",
        test_id: "kind_menu",
    };
    super::FilterMenu::with_spec(
        &SPEC,
        ["Paragraph", "Heading 1", "Heading 2", "Quote"]
            .into_iter()
            .map(|label| super::filter_menu::MenuEntry::plain(label.to_owned(), &[], label))
            .collect(),
    )
}

fn of_role(nodes: &[(AkNodeId, AkNode)], role: AkRole) -> Vec<&AkNode> {
    nodes
        .iter()
        .filter(|(_, n)| n.role() == role)
        .map(|(_, n)| n)
        .collect()
}

fn by_label<'a>(nodes: &'a [(AkNodeId, AkNode)], role: AkRole, label: &str) -> &'a AkNode {
    nodes
        .iter()
        .find(|(_, n)| n.role() == role && n.label() == Some(label))
        .map_or_else(|| panic!("no {role:?} node labelled {label:?}"), |(_, n)| n)
}

tilo_test_env::scenario! {
    /// What the selection controls, gauges, filter menu and newer widgets say to assistive technology.
    /// Each phase was a test of its own and builds its own fixture.
    fn widgets_expose_their_semantics_to_the_accessibility_tree;

    phase selection_controls_expose_radio_list_tab_and_toggle_semantics;
    {
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
            assert_eq!(
                list.value(),
                Some("Yellow"),
                "value is the selected swatch's colour"
            );

            let items = of_role(&nodes, AkRole::ListItem);
            assert_eq!(items.len(), palette.len(), "one ListItem per swatch");
            let first = by_label(&nodes, AkRole::ListItem, "Red");
            assert_eq!(first.position_in_set(), Some(1));
            assert_eq!(first.size_of_set(), Some(4));
            // Selection is explicit tri-state on ListItems: the chosen one true, the
            // rest false (so an AT doesn't announce "selected" for the wrong swatch).
            let chosen = by_label(&nodes, AkRole::ListItem, "Yellow");
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

    phase gauges_and_filter_menu_expose_meters_and_menu_visibility;
    {
        fn ring_gauge_exposes_a_named_percentage_meter() {
            use super::gauge::ProgressRing;
            let mut g = ProgressRing::new(0.42).caption("CPU");
            let nodes = access_nodes(&mut g);
            let bar = by_label(&nodes, AkRole::ProgressIndicator, "CPU");
            assert_eq!(bar.value(), Some("42%"), "fraction renders as a percentage");
        }

        fn bar_gauge_prefers_the_caller_value_readout() {
            use super::gauge::BarGauge;
            let mut g = BarGauge::new(0.6).with_label("Memory", "3.2 GB");
            let nodes = access_nodes(&mut g);
            let bar = by_label(&nodes, AkRole::ProgressIndicator, "Memory");
            // A human-facing readout ("3.2 GB") wins over the raw percentage.
            assert_eq!(bar.value(), Some("3.2 GB"));

            let mut plain = BarGauge::new(0.25).with_label("Disk", "");
            let plain_nodes = access_nodes(&mut plain);
            let plain_bar = by_label(&plain_nodes, AkRole::ProgressIndicator, "Disk");
            assert_eq!(plain_bar.value(), Some("25%"));
        }

        fn filter_menu_open_exposes_menu_items() {
            let mut m = kind_menu();
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
                "every row is exposed"
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

        fn filter_menu_closed_is_a_hidden_menu() {
            let mut m = kind_menu(); // starts closed
            let nodes = access_nodes(&mut m);
            let menus = of_role(&nodes, AkRole::Menu);
            assert_eq!(menus.len(), 1);
            assert!(
                menus[0].is_hidden(),
                "a closed menu collapses out of the reading order"
            );
            assert_eq!(menus[0].is_expanded(), Some(false));
            assert!(of_role(&nodes, AkRole::MenuItem).is_empty());
        }

        ring_gauge_exposes_a_named_percentage_meter();
        bar_gauge_prefers_the_caller_value_readout();
        filter_menu_open_exposes_menu_items();
        filter_menu_closed_is_a_hidden_menu();
    }

}
