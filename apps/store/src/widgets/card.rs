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
use ui::widget::{Button, Column, Container, Row, Spacer, Text, Widget};

use crate::model::{ActionKind, StoreItem, StoreSource};

/// Logical px a future list virtualizer can use to reserve a card row.
///
/// The catalogue already bounds every page to [`crate::app::PAGE_SIZE`]; the
/// widget tree for that bounded page is deliberately composed up-front so
/// keyboard traversal and screen-reader order stay exact.
pub const CARD_HEIGHT: f32 = 96.0;

/// Build a single card widget.
///
/// * `select_key`         — stable widget identifier for the Details control.
/// * `action_click_key`   — stable widget identifier for the primary action.
///
/// Both controls receive typed resource-action bindings from the caller.
/// These identifiers are only for local widget identity and accessibility;
/// they are never written to the collaboration document.
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
    large_text: bool,
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
        .with_gap(4.0)
        .push(Box::new(
            Text::new(&item.name)
                .with_scaled_size(if compact { 18.0 } else { 20.0 })
                .with_weight(TextWeight::Semibold),
        ))
        .push(Box::new(
            Text::new(&format!("{} · {category}", badge_line(item)))
                .with_scaled_size(13.0)
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
    let mut action_btn = Button::new(action_click_key, &action_label, None);
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

    let mut details = Button::new(&format!("{select_key}.button"), "Details", None);
    if let Some(binding) = detail_binding {
        details = details.on_action(binding);
    }
    let details = details
        .fit_content(40.0)
        .tooltip(format!("Show details for {}", item.name));
    let action_btn = action_btn.fit_content(40.0);

    let content: Box<dyn Widget> = if compact {
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(if large_text { 6.0 } else { 10.0 })
            .push(Box::new(
                Row::new()
                    .no_wrap()
                    .push(icon_artwork(item, images, large_text))
                    .push(Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_flex_grow()
                            .with_child(Box::new(identity)),
                    )),
            ));
        if !large_text {
            column = column.push(Box::new(
                Text::new(&item.description)
                    .with_scaled_size(14.0)
                    .with_color(theme.text_dim),
            ));
        }
        column = column.push(Box::new(
            Row::new()
                .no_wrap()
                .push(Box::new(Spacer::new()))
                .push(Box::new(details))
                .push(Box::new(action_btn)),
        ));
        Box::new(column)
    } else {
        Box::new(
            Column::new()
                .with_no_padding()
                .with_gap(10.0)
                .push(Box::new(
                    Row::new()
                        .no_wrap()
                        .push(Box::new(super::Artwork::new(
                            images,
                            item.icon_url.as_deref(),
                            &item.name,
                            48.0,
                            48.0,
                        )))
                        .push(Box::new(
                            Container::new()
                                .with_no_padding()
                                .with_flex_grow()
                                .with_child(Box::new(identity)),
                        )),
                ))
                .push(Box::new(
                    Text::new(&item.description)
                        .with_scaled_size(14.0)
                        .with_color(theme.text_dim),
                ))
                .push(Box::new(Spacer::new()))
                .push(Box::new(
                    Row::new()
                        .no_wrap()
                        .push(Box::new(Spacer::new()))
                        .push(Box::new(details))
                        .push(Box::new(action_btn)),
                )),
        )
    };
    // A very low-tint surface gives source metadata, artwork and the primary
    // action a coherent visual field instead of the old generic list-row
    // treatment. The colour is derived from the active system theme rather
    // than a hard-coded brand wash, so custom and high-contrast themes retain
    // their contrast guarantees.
    let tint = match &item.source {
        StoreSource::Qos { .. } => theme.tokens.colors.semantic.success,
        StoreSource::Flathub => theme.tokens.colors.semantic.info,
    };
    let card_fill = theme.tokens.colors.tinted_chip_on(
        theme.bg_app,
        tint,
        0.08,
        ui::tokens::colors::NON_TEXT_MIN_CONTRAST,
    );
    let card = Container::new()
        .with_fill(card_fill.fill)
        .with_child(content);
    if compact {
        Box::new(card)
    } else {
        Box::new(card.filling())
    }
}

/// Bounded remote icon with the shared deterministic initial-art fallback.
/// [`super::Artwork`] requests HTTPS imagery through the Store's guarded
/// [`ui::ImageStore`]; an absent, rejected, or undecodable image keeps the
/// same card geometry and renders the fallback instead.
fn icon_artwork(
    item: &StoreItem,
    images: Option<ui::ImageStore>,
    large_text: bool,
) -> Box<dyn Widget> {
    Box::new(super::Artwork::new(
        images,
        item.icon_url.as_deref(),
        &item.name,
        // Keep the compact card's identity row at least as tall as its
        // artwork.  A 36px tile looked fine with a one-line title but let
        // taller fallback marks visually run into the following description
        // once metadata wrapped on narrow screens.
        if large_text { 44.0 } else { 52.0 },
        if large_text { 44.0 } else { 52.0 },
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
