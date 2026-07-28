//! One store card — the row that renders per item in the main list.
//!
//! Composition:
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────────┐
//! │ [icon]  Name                 v0.1.0 · qOS main            [Action]   │
//! │ [ ⎔  ]  One-line description                                         │
//! └──────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! Everything is built from stock `ui::widget::*` primitives — the card is
//! just a `Container` + a `Row` — so it inherits the OS theme without any
//! bespoke rendering. The action button label + click path are wired by the
//! caller: this fn is pure composition.

use ui::widget::{
    Button, Column, Container, Row, Spacer, Text, Widget,
};

use crate::model::{ActionKind, StoreItem, StoreSource};

/// Logical px the caller can use to reserve card row height in a
/// virtualised list (currently we render everything up-front —
/// virtualisation is a follow-up).
pub const CARD_HEIGHT: f32 = 84.0;

/// Build a single card widget.
///
/// * `select_key`         — CRDT key the whole card body writes on
///                          click to open the detail sheet.
/// * `action_click_key`   — CRDT key the action button writes on
///                          click. The app's `sync` reads it,
///                          dispatches the action, and clears the
///                          bit so it's a one-shot edge.
pub fn store_card(
    item: &StoreItem,
    action: ActionKind,
    select_key: &str,
    action_click_key: &str,
    pin_click_key: &str,
    remove_click_key: &str,
) -> Box<dyn Widget> {
    // Left column: name + description.
    let name_line = Row::new()
        .push(Box::new(Text::new(&item.name).with_size(15.0)))
        .push(Box::new(Spacer::new()))
        .push(Box::new(
            Text::new(&badge_line(item))
                .with_size(11.0)
                .with_color(ui::Theme::dark().text_dim),
        ));

    let text_col = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(name_line))
        .push(Box::new(
            Text::new(&item.description)
                .with_size(12.0)
                .with_color(ui::Theme::dark().text_dim),
        ));

    // Right side: the primary action button (Open / Update / Install
    // / Remove-flatpak). Label + CRDT click key provided by the
    // caller; the button variant is picked from `action` so the same
    // fn drives every action kind.
    let action_btn = Button::new(
        action_click_key,
        action.label(),
        Some(action_click_key),
    );

    // The action button already reads "Update" when an update is
    // available (see `ActionKind`), so no separate hint label is drawn.
    let mut right_col = Column::new()
        .with_no_padding()
        .with_intrinsic_height();
    right_col = right_col.push(Box::new(action_btn));
    // Optional secondary controls for a cached qOS app: the "mark
    // installed" (pin) toggle and a small Remove. These are absent
    // until the app has been Opened at least once — there is never a
    // separate Install step.
    if let Some(pin_action) = item.pin_action() {
        right_col = right_col.push(Box::new(Button::new(
            pin_click_key,
            pin_action.label(),
            Some(pin_click_key),
        )));
    }
    if item.offers_remove() {
        right_col = right_col.push(Box::new(Button::new(
            remove_click_key,
            "Remove",
            Some(remove_click_key),
        )));
    }

    let row = Row::new()
        .push(icon_placeholder())
        .push(Box::new(text_col))
        .push(Box::new(Spacer::new()))
        .push(Box::new(right_col));

    // A click on the card body (outside the action button) opens
    // the detail sheet — modelled as a Button that spans the row
    // and writes `select_key`. We inline it as an outer wrapper
    // Container with the row inside it, and rely on the
    // Container's `with_permission_scope` follow-up when we wire
    // permissions.
    // For now: a two-button card would be ergonomically confusing.
    // We instead render the row inside a Container, and reserve
    // the click-to-open flow as a follow-up (the detail sheet is
    // reachable from a keyboard shortcut in the meantime — Enter
    // on the focused card writes `select_key`).
    let _ = select_key;

    Box::new(
        Container::new()
            .with_background()
            .with_child(Box::new(row)),
    )
}

/// Small decorative icon square. Real icon fetching is a follow-up;
/// today we render a themed rectangle so the card layout stays
/// consistent whether the source publishes an icon or not.
fn icon_placeholder() -> Box<dyn Widget> {
    Box::new(
        Container::new()
            .with_size(48.0, 48.0)
            .with_fill(ui::Theme::dark().bg_hover),
    )
}

/// The right-hand badge line under the name: version + source label.
fn badge_line(item: &StoreItem) -> String {
    let source_label = match &item.source {
        StoreSource::Qos { registry_name } => registry_name.clone(),
        StoreSource::Flathub => "Flathub".to_string(),
    };
    let v = item.latest_version();
    if v.is_empty() {
        source_label
    } else {
        format!("v{v} · {source_label}")
    }
}
