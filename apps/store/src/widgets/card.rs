//! One store card — the row that renders per item in the main list.
//!
//! Composition:
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────────┐
//! │ [art]  Name · provenance                                             │
//! │        Clear one-or-two-line reason to care                          │
//! │                                      [Details] [Primary action]      │
//! └──────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! Everything is built from stock `ui::widget::*` primitives — the card is
//! just a `Container` + a `Row` — so it inherits the OS theme without any
//! bespoke rendering. The action button label + click path are wired by the
//! caller: this fn is pure composition.

use ui::text_render::TextWeight;
use ui::tilo_actions::ActionBinding;
use ui::widget::{Actions, Button, Card, Column, Container, Row, Text, Widget};

use crate::model::{ActionKind, StoreItem, StoreSource};

/// Logical px a future list virtualizer can use to reserve a card row.
///
/// The catalogue already bounds every page to `crate::app::PAGE_SIZE`; the
/// widget tree for that bounded page is deliberately composed up-front so
/// keyboard traversal and screen-reader order stay exact.
pub const CARD_HEIGHT: f32 = 96.0;

/// A card's primary action button.
///
/// `key` is only for local widget identity and accessibility; it is never
/// written to the collaboration document.
pub struct CardAction<'a> {
    pub kind: ActionKind,
    pub key: &'a str,
    pub binding: Option<ActionBinding<()>>,
    /// The action is running for this item, so the button shows progress and
    /// is disabled.
    pub in_progress: bool,
}

/// A card's Details control: its button is keyed `{key}.button`.
pub struct CardDetails<'a> {
    pub key: &'a str,
    pub binding: Option<ActionBinding<()>>,
}

/// How a card arranges itself for the catalogue's responsive mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardLayout {
    Wide,
    /// Narrow windows and large text; `large_text` shrinks the artwork.
    Stacked {
        large_text: bool,
    },
}

/// Build a single card widget.
#[allow(
    clippy::too_many_lines,
    reason = "its compact and desktop compositions intentionally live together"
)]
pub fn store_card(
    item: &StoreItem,
    primary: CardAction<'_>,
    details: CardDetails<'_>,
    theme: &ui::Theme,
    images: Option<ui::ImageStore>,
    layout: CardLayout,
) -> Box<dyn Widget> {
    let CardAction {
        kind: action,
        key: action_click_key,
        binding: action_binding,
        in_progress: operation_active,
    } = primary;
    let compact = matches!(layout, CardLayout::Stacked { .. });
    // Identity is intentionally short. A title, origin/version and category
    // make a catalogue tile understandable at a glance; the description gets
    // its own line below rather than competing with the artwork and actions.
    let identity = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(theme.tokens.spacing.s1)
        .push(Box::new(
            Text::new(&item.name)
                .with_scaled_size(if compact { 18.0 } else { 20.0 })
                .with_weight(TextWeight::Semibold),
        ))
        .push(Box::new(
            Text::new(&desktop_metadata(item))
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

    let mut details_btn = Button::new(&format!("{}.button", details.key), "Details", None);
    if let Some(binding) = details.binding {
        details_btn = details_btn.on_action(binding);
    }
    let details = details_btn.fit_content(40.0).tooltip(card_summary(item));
    let action_btn = action_btn.fit_content(40.0);

    let artwork = match layout {
        CardLayout::Stacked { large_text } => icon_artwork(item, images, large_text),
        CardLayout::Wide => Box::new(super::Artwork::new(
            images,
            item.icon_url.as_deref(),
            &item.name,
            52.0,
            52.0,
        )),
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
        StoreSource::Tilo { .. } => {
            if v.is_empty() {
                "Tilo verified".into()
            } else {
                format!("Tilo verified · v{v}")
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

fn desktop_metadata(item: &StoreItem) -> String {
    let category = item.categories.first().map_or_else(
        || "General".to_string(),
        |category| category_label(category),
    );
    format!("{} · {category}", badge_line(item))
}

/// Full card copy for the Details hover/focus affordance. The visual card
/// deliberately keeps metadata quiet and bounded; assistive and pointer users
/// can still inspect the unabridged source, category, version and description
/// without relying on clipped paint.
fn card_summary(item: &StoreItem) -> String {
    let categories = if item.categories.is_empty() {
        "General".to_owned()
    } else {
        item.categories
            .iter()
            .map(|value| category_label(value))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "Show details for {}. {}. Categories: {categories}. {}",
        item.name,
        badge_line(item),
        item.description
    )
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
    use super::{card_summary, category_label};

    use crate::model::{InstalledState, StoreItem, StoreItemVersion, StoreSource, StoreTrust};

    /// Registry categories read as words and the tooltip keeps full metadata.
    #[test]
    fn card_categories_and_tooltip_preserve_metadata() {
        // Case `registry_categories_are_presented_as_words`.
        {
            assert_eq!(category_label("AudioVideo"), "Audio & video");
            assert_eq!(category_label("Development"), "Developer tools");
            assert_eq!(category_label("Games"), "Games");
        }

        // Case `card_tooltip_preserves_full_quiet_metadata_and_description`.
        {
            let item = StoreItem {
                id: "org.example.Editor".into(),
                name: "Editor".into(),
                description: "Write without distractions.".into(),
                tagline: String::new(),
                supported_languages: Vec::new(),
                categories: vec!["Development".into(), "Productivity".into()],
                icon_url: None,
                screenshot_urls: Vec::new(),
                homepage_url: None,
                source_url: None,
                install_ref_path: None,
                source: StoreSource::Flathub,
                trust: StoreTrust::Flathub,
                versions: vec![StoreItemVersion {
                    version: "2.4".into(),
                    released_at_unix: 0,
                    download_url: None,
                    root_hash_hex: None,
                    size_bytes: 0,
                }],
                publisher_pubkey_hex: None,
                installed: InstalledState::Not,
            };
            let summary = card_summary(&item);
            assert!(summary.contains("Flathub · v2.4"));
            assert!(summary.contains("Developer tools, Productivity"));
            assert!(summary.contains("Write without distractions."));
        }
    }
}
