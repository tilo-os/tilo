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

use ui::text_render::TextWeight;
use ui::widget::{Actions, Button, Card, Column, Container, Row, Text, Widget};

use crate::model::{ActionKind, StoreItem, StoreSource};

/// Logical px the caller can use to reserve card row height in a
/// virtualised list (currently we render everything up-front —
/// virtualisation is a follow-up).
pub const CARD_HEIGHT: f32 = 96.0;

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
    clippy::too_many_lines,
    reason = "card construction receives the item, paired typed actions, shared artwork service, theme, and responsive mode as one cohesive view boundary; its compact and desktop compositions intentionally live together"
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
    operation_active: bool,
) -> Box<dyn Widget> {
    // Left column: name + description. Secondary text reads its dim tone
    // from the live theme so it stays legible in light and dark.
    // A flex-grow Container owns the width beside the fixed artwork. This is
    // important because a bare Column deliberately claims the full row width.
    let category = item
        .categories
        .first()
        .map_or_else(|| "General".to_owned(), |value| category_label(value));
    let identity = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(theme.tokens.spacing.s1)
        .push(Box::new(
            Text::new(&item.name)
                .with_scaled_size(18.0)
                .with_weight(TextWeight::Semibold),
        ))
        .push(Box::new(
            Text::new(&badge_line(item))
                .with_scaled_size(13.0)
                .with_color(theme.text_dim),
        ))
        .push(Box::new(
            Text::new(&category)
                .with_scaled_size(12.0)
                .with_color(theme.text_dim),
        ));

    // The primary action button (Open / Update / Install / Remove-flatpak)
    // is emphasised; the optional secondary controls sit beside it as quiet
    // ghost buttons so the card has one clear primary action. They are direct
    // children of the row (fixed-width buttons) laid out as a horizontal
    // cluster on the right, not a vertical stack.
    let action_label = if operation_active {
        format!("{}…", action.progress_label())
    } else {
        action.label().to_string()
    };
    let mut action_btn = Button::new(action_click_key, &action_label, Some(action_click_key));
    if let Some(binding) = action_binding {
        action_btn = action_btn.on_action(binding);
    }
    let action_btn = match action {
        ActionKind::Open
        | ActionKind::Update
        | ActionKind::FlatpakInstall
        | ActionKind::FlatpakUpdate
        | ActionKind::FlatpakOpen => action_btn.primary(),
        _ => action_btn,
    }
    .with_enabled(!operation_active)
    .tooltip(if operation_active {
        format!("{} {} is in progress", action.progress_label(), item.name)
    } else {
        format!("{} {}", action.label(), item.name)
    });

    let mut details = Button::new(&format!("{select_key}.button"), "Details", Some(select_key));
    if let Some(binding) = detail_binding {
        details = details.on_action(binding);
    }
    let details = details
        .fit_content(40.0)
        .tooltip(format!("Show details for {}", item.name));
    let action_btn = action_btn.fit_content(40.0);

    let artwork = if compact {
        icon_artwork(item, images)
    } else {
        Box::new(super::Artwork::new(
            images,
            item.icon_url.as_deref(),
            &item.name,
            52.0,
            52.0,
        ))
    };
    let header = Row::new().no_wrap().push(artwork).push(Box::new(
        Container::new()
            .with_no_padding()
            .with_flex_grow()
            .with_child(Box::new(identity)),
    ));
    Box::new(
        Card::composed(Box::new(
            Text::new(&item.description)
                .with_scaled_size(14.0)
                .with_color(theme.text_dim),
        ))
        .with_header(Box::new(header))
        .with_footer(Box::new(Actions::new(action_btn).secondary(details))),
    )
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
        36.0,
        36.0,
    ))
}

/// The right-hand badge line under the name: version + source label.
fn badge_line(item: &StoreItem) -> String {
    let v = item.latest_version();
    match &item.source {
        StoreSource::Qos { .. } => {
            if v.is_empty() {
                "qOS verified".into()
            } else {
                format!("qOS verified · v{v}")
            }
        }
        StoreSource::Flathub => {
            if v.is_empty() {
                "Flathub distribution".into()
            } else {
                format!("Flathub · v{v}")
            }
        }
    }
}

/// Turn registry category identifiers into human-facing labels without
/// changing the stable value used by filtering and search.
fn category_label(value: &str) -> String {
    match value {
        "AudioVideo" => "Audio & video".into(),
        "Development" => "Developer tools".into(),
        "Graphics" => "Graphics & design".into(),
        other => other.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::category_label;

    #[test]
    fn registry_categories_are_presented_as_words() {
        assert_eq!(category_label("AudioVideo"), "Audio & video");
        assert_eq!(category_label("Development"), "Developer tools");
        assert_eq!(category_label("Games"), "Games");
    }
}
