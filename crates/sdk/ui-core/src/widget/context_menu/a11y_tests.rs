use super::*;
use crate::widget::Widget;

#[test]
fn menu_accessibility_reports_state_rows_and_checks() {
    fn accessibility_reports_menu_role_and_collapsed_when_closed() {
        use crate::a11y::Role;
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("Open", || {}));
        let node = Widget::accessibility(&menu).expect("menu is not presentational");
        assert_eq!(node.role, Role::Menu);
        assert_eq!(
            node.state.expanded,
            Some(false),
            "closed menu is not expanded"
        );
        assert!(node.state.hidden, "closed menu is hidden from the tree");
    }

    fn accessibility_open_menu_is_expanded_and_visible() {
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("Open", || {}));
        menu.open_at([10.0, 20.0]);
        let node = Widget::accessibility(&menu).unwrap();
        assert_eq!(node.state.expanded, Some(true), "open menu is expanded");
        assert!(!node.state.hidden, "open menu is visible");
    }

    fn a_caption_is_read_among_the_rows_but_never_chosen() {
        let mut menu = ContextMenu::with_items(vec![
            MenuItem::selection("Lounge"),
            MenuItem::caption("Hosted by example.org"),
            MenuItem::separator(),
            MenuItem::selection("Overview"),
        ]);
        menu.open_at([10.0, 10.0]);
        assert!(
            item_shown(menu.items(), 2),
            "a caption above keeps the rule"
        );
        assert!(!menu.items()[1].is_enabled(), "keyboard passes over it");
        menu.click_item(1);
        assert_eq!(menu.take_selection(), None);
        assert!(menu.is_open(), "a click on the caption chooses nothing");
        let node = Widget::accessibility(&menu).expect("menu semantics");
        let caption = &node.children[1];
        assert_eq!(caption.role, crate::a11y::Role::StaticText);
        assert_eq!(caption.name.as_deref(), Some("Hosted by example.org"));
        assert!(
            menu.item_height(1) < menu.item_height(0),
            "a caption line is tighter than a row"
        );
    }

    fn accessibility_exposes_named_action_rows_and_skips_separators() {
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("Keep Offline", || {}));
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action("Sharing & Access…", || {}));
        menu.open_at([10.0, 20.0]);

        let node = Widget::accessibility(&menu).unwrap();
        assert_eq!(node.children.len(), 2);
        assert_eq!(node.children[0].role, crate::a11y::Role::MenuItem);
        assert_eq!(node.children[0].name.as_deref(), Some("Keep Offline"));
        assert_eq!(
            node.children[0].action_target.as_deref(),
            Some("context-menu:item:0")
        );
        assert_eq!(node.children[0].pos_in_set, Some(1));
        assert_eq!(node.children[0].set_size, Some(2));
        assert_eq!(node.children[1].name.as_deref(), Some("Sharing & Access…"));
        assert_eq!(
            node.children[1].action_target.as_deref(),
            Some("context-menu:item:2")
        );
        assert!(node.children.iter().all(|child| child.state.focusable));
    }

    fn accessibility_exposes_checked_state_without_polluting_the_label() {
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("List", || {}).with_checked(true));
        menu.push(MenuItem::action("Grid", || {}).with_checked(false));
        menu.open_at([10.0, 20.0]);

        let node = Widget::accessibility(&menu).unwrap();
        assert_eq!(node.children[0].name.as_deref(), Some("List"));
        assert_eq!(node.children[0].state.checked, Some(true));
        assert_eq!(node.children[1].name.as_deref(), Some("Grid"));
        assert_eq!(node.children[1].state.checked, Some(false));
    }

    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    accessibility_reports_menu_role_and_collapsed_when_closed();
    accessibility_open_menu_is_expanded_and_visible();
    a_caption_is_read_among_the_rows_but_never_chosen();
    accessibility_exposes_named_action_rows_and_skips_separators();
    accessibility_exposes_checked_state_without_polluting_the_label();
}
