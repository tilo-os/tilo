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

use ui::widget::{Button, Column, Container, Row, Text, Widget};

use crate::model::{ActionKind, StoreItem, StoreSource};

/// Logical px the caller can use to reserve card row height in a
/// virtualised list (currently we render everything up-front —
/// virtualisation is a follow-up).
pub const CARD_HEIGHT: f32 = 84.0;

/// Build a single card widget.
///
/// * `select_key`         — CRDT key the whole card body writes on
///   click to open the detail sheet.
/// * `action_click_key`   — CRDT key the action button writes on
///   click. The app's `sync` reads it,
///   dispatches the action, and clears the
///   bit so it's a one-shot edge.
#[allow(
    clippy::too_many_arguments,
    reason = "card construction receives the item, paired typed actions, shared artwork service, theme, and responsive mode as one cohesive view boundary"
)]
pub fn store_card(
    item: &StoreItem,
    action: ActionKind,
    select_key: &str,
    action_click_key: &str,
    theme: &ui::Theme,
    images: Option<ui::ImageStore>,
    action_binding: Option<ui::qos_actions::ActionBinding<()>>,
    detail_binding: Option<ui::qos_actions::ActionBinding<()>>,
    compact: bool,
) -> Box<dyn Widget> {
    // Left column: name + description. Secondary text reads its dim tone
    // from the live theme so it stays legible in light and dark.
    // The text column grows to fill the space between the icon and the
    // action buttons (`fill_remaining_height` gives it `flex_grow: 1`,
    // `flex_basis: 0`), so the buttons sit on the right of the same row
    // rather than being pushed off it by a full-width text block.
    let text_col = Column::new()
        .with_no_padding()
        .with_fill_remaining_height()
        .push(Box::new(Text::new(&item.name).with_size(15.0)))
        .push(Box::new(
            Text::new(&badge_line(item))
                .with_size(11.0)
                .with_color(theme.text_dim),
        ))
        .push(Box::new(
            Text::new(&item.description)
                .with_size(12.0)
                .with_color(theme.text_dim),
        ));

    // The primary action button (Open / Update / Install / Remove-flatpak)
    // is emphasised; the optional secondary controls sit beside it as quiet
    // ghost buttons so the card has one clear primary action. They are direct
    // children of the row (fixed-width buttons) laid out as a horizontal
    // cluster on the right, not a vertical stack.
    let mut action_btn = Button::new(action_click_key, action.label(), Some(action_click_key));
    if let Some(binding) = action_binding {
        action_btn = action_btn.on_action(binding);
    }
    let action_btn = action_btn
        .primary()
        .tooltip(format!("{} {}", action.label(), item.name));

    let mut details = Button::new(&format!("{select_key}.button"), "Details", Some(select_key));
    if let Some(binding) = detail_binding {
        details = details.on_action(binding);
    }
    let content: Box<dyn Widget> = if compact {
        Box::new(
            Column::new()
                .with_intrinsic_height()
                .push(Box::new(
                    Row::new()
                        .no_wrap()
                        .push(icon_artwork(item, images))
                        .push(Box::new(text_col)),
                ))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(
                            details
                                .ghost()
                                .tooltip(format!("Show details for {}", item.name)),
                        ))
                        .push(Box::new(action_btn)),
                )),
        )
    } else {
        Box::new(
            Row::new()
                .push(icon_artwork(item, images))
                .push(Box::new(text_col))
                .push(Box::new(
                    details
                        .ghost()
                        .tooltip(format!("Show details for {}", item.name)),
                ))
                .push(Box::new(action_btn)),
        )
    };
    Box::new(Container::new().with_background().with_child(content))
}

/// Bounded remote icon with the shared deterministic initial-art fallback.
/// [`super::Artwork`] requests HTTPS imagery through the Store's guarded
/// [`ui::ImageStore`]; an absent, rejected, or undecodable image keeps the
/// same card geometry and renders the fallback instead.
fn icon_artwork(item: &StoreItem, images: Option<ui::ImageStore>) -> Box<dyn Widget> {
    Box::new(super::Artwork::new(
        images,
        item.icon_url.as_deref(),
        &item.name,
        48.0,
        48.0,
    ))
}

/// The right-hand badge line under the name: version + source label.
fn badge_line(item: &StoreItem) -> String {
    let source_label = match &item.source {
        StoreSource::Qos { registry_name } => format!("{} · {registry_name}", item.trust.label()),
        StoreSource::Flathub => item.trust.label().to_string(),
    };
    let v = item.latest_version();
    if v.is_empty() {
        source_label
    } else {
        format!("v{v} · {source_label}")
    }
}
