//! The bar's own door to another application, and a tab's chips, in the desktop's language: a
//! German window names them in German, as it names every other window control.

use super::tests::lay_out_at_width_with;
use super::*;

/// A German desktop names the bar's door, its tab's close chip and what a tab is doing in German,
/// and speaks them so: the bar's own words come from the catalogue like every other window
/// control's.
#[test]
fn a_german_bar_names_its_door_and_its_tabs_controls_in_german() {
    use crate::a11y::Role;
    use crate::i18n::I18nManager;
    const EN: &str = include_str!("../../locales/en-US.ftl");
    const DE: &str = include_str!("../../locales/de-DE.ftl");
    I18nManager::install("en-US", "de-DE", &[("en-US", EN), ("de-DE", DE)]).unwrap();
    let _settled = crate::motion::testing::settled();
    let theme = crate::theme::Theme::dark().with_reduce_motion(true);
    let mut b = TabBar::new("g1", vec![TabDescriptor::new(1, "Chat")], 1);
    b.set_window_controls(true);
    b.set_mark(1, Some(TabMark::Muted));
    lay_out_at_width_with(&mut b, 900.0, &theme);

    let door = b
        .bar_layout()
        .controls
        .into_iter()
        .find(|control| control.slot == "new-tab")
        .expect("a bar with room shows its door");
    assert_eq!(door.name, "Neuer Tab");
    let at = Point::new(door.rect[0] + 16.0, door.rect[1] + 16.0);
    assert_eq!(
        b.tip_at(at).map(|(_, tip)| tip).as_deref(),
        Some("Neuer Tab"),
        "the tip under the pointer speaks German too"
    );

    let node = Widget::accessibility(&b).expect("TabBar always reports a node");
    let door_node = node
        .children
        .iter()
        .find(|child| child.action_target.as_deref() == Some(NEW_TAB_TARGET))
        .expect("the door is a node of the bar");
    assert_eq!(
        door_node.name.as_deref(),
        Some("Neuer Tab"),
        "a German reader hears the door in German"
    );
    let tab = node
        .children
        .iter()
        .find(|child| child.role == Role::Tab)
        .expect("the bar shows its tab");
    assert_eq!(
        tab.description.as_deref(),
        Some("Stummgeschaltet"),
        "what the tab is doing is German too"
    );
    let chip = |target: &str| {
        tab.children
            .iter()
            .find(|child| child.action_target.as_deref() == Some(target))
            .unwrap_or_else(|| panic!("the tab has no {target} chip"))
            .name
            .clone()
    };
    assert_eq!(chip("tab-close:1").as_deref(), Some("Chat schließen"));
    assert_eq!(chip("tab-mute:1").as_deref(), Some("Chat stummschalten"));

    I18nManager::init("en-US");
}
