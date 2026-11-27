//! Every widget that hides itself answers the accessibility question its node answers: an
//! unrevealed tooltip, a closed menu, dialog, palette and options menu, and the ones that never
//! hide.
use tilo_ui_base::widget::Widget;

use tilo_ui_app::widget::popover::Popover;
use tilo_ui_app::widget::{Column, ContextMenu, ModalDialog, Tooltip};

fn agreed(widget: &dyn Widget) -> bool {
    let hidden = widget.accessibility_hidden();
    assert_eq!(
        hidden,
        widget.accessibility().is_some_and(|node| node.state.hidden),
        "{}",
        widget.name()
    );
    hidden
}

#[test]
fn widgets_that_hide_answer_accessibility_hidden_as_their_node() {
    assert!(agreed(&Tooltip::new("Tip")), "an unrevealed tooltip");

    let mut menu = ContextMenu::new();
    assert!(agreed(&menu), "a closed menu");
    menu.open_at([0.0, 0.0]);
    assert!(!agreed(&menu), "an open menu");

    let mut dialog = ModalDialog::new("Title", Column::new());
    assert!(agreed(&dialog), "a closed dialog");
    dialog.set_open(true);
    assert!(!agreed(&dialog), "an open dialog");

    let mut palette = tilo_ui_app::ActionPalette::rows(Vec::new());
    assert!(agreed(&palette), "a closed palette");
    palette.open();
    assert!(!agreed(&palette), "an open palette");

    let mut options = Popover::new()
        .with_label("Interval")
        .with_items(["1 s", "2 s"]);
    assert!(agreed(&options), "a closed options menu");
    options.set_open(true);
    assert!(!agreed(&options), "an open options menu");

    let mut panel = Popover::with_content(Column::new()).with_label("Share");
    assert!(agreed(&panel), "a closed labelled panel");
    panel.set_open(true);
    assert!(!agreed(&panel), "an open labelled panel");
    assert!(
        !agreed(&Popover::with_content(Column::new())),
        "an unlabelled panel has no node of its own"
    );

    assert!(!agreed(&Column::new()), "a container is never hidden");
}
