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
    action_binding: Option<ui::tilo_actions::ActionBinding<()>>,
    detail_binding: Option<ui::tilo_actions::ActionBinding<()>>,
    compact: bool,
    large_text: bool,
    operation_active: bool,
) -> Box<dyn Widget> {
    // Identity is intentionally short. A title, origin/version and category
    // make a catalogue tile understandable at a glance; the description gets
    // its own line below rather than competing with the artwork and actions.
    let identity = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(3.0)
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

    let mut details = Button::new(&format!("{select_key}.button"), "Details", None);
    if let Some(binding) = detail_binding {
        details = details.on_action(binding);
    }
    let details = details.fit_content(40.0).tooltip(card_summary(item));
    let action_btn = action_btn.fit_content(40.0);

    let content: Box<dyn Widget> = if compact {
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(
                Row::new()
                    .no_wrap()
                    .push(icon_artwork(item, images, large_text))
                    .push(Box::new(Container::new().with_no_padding().with_width(8.0)))
                    .push(Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_flex_grow()
                            .with_child(Box::new(identity)),
                    )),
            ));
        if !large_text {
            // Intrinsic `Column`s deliberately use Taffy's block stack so a
            // scroll body can grow beyond its viewport. Block does not apply
            // flex `gap`, though, which previously made the description and
            // action row paint on top of the identity row on a phone-sized
            // Store window. These are real layout children rather than a
            // cosmetic margin, so the measured card height and painted rhythm
            // stay in agreement for every description length.
            column = column.push(Box::new(
                Container::new().with_no_padding().with_height(8.0),
            ));
            column = column.push(Box::new(
                Text::new(&item.description)
                    .with_scaled_size(14.0)
                    .with_color(theme.text_dim),
            ));
        }
        column = column.push(Box::new(
            Container::new()
                .with_no_padding()
                .with_height(if large_text { 6.0 } else { 8.0 }),
        ));
        column = column.push(Box::new(
            Row::new()
                .no_wrap()
                .push(Box::new(Spacer::new()))
                .push(Box::new(details))
                .push(Box::new(action_btn)),
        ));
        Box::new(column)
    } else {
        // A vertical desktop tile gives discovery copy and the action rail
        // stable, independent regions. The old horizontal row was readable
        // only in two very wide columns; at three useful desktop columns it
        // turned titles, artwork and buttons into one cramped line.
        let header = Row::new()
            .no_wrap()
            .push(Box::new(super::Artwork::new(
                images,
                item.icon_url.as_deref(),
                &item.name,
                58.0,
                58.0,
            )))
            .push(Box::new(
                Container::new().with_no_padding().with_width(10.0),
            ))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_child(Box::new(identity)),
            ));
        Box::new(
            Column::new()
                .with_no_padding()
                .with_fill_remaining_height()
                .push(Box::new(header))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(10.0),
                ))
                .push(Box::new(
                    Text::new(&item.description)
                        .with_scaled_size(13.0)
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
    // Source is metadata, not branding. A neutral OS surface keeps the browse
    // page calm and lets artwork plus the primary action establish hierarchy;
    // Tilo/Flatpak provenance remains explicit in the identity line.
    let card_fill = theme.bg_primary;
    let card = Container::new().with_fill(card_fill).with_child(content);
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

    #[test]
    fn registry_categories_are_presented_as_words() {
        assert_eq!(category_label("AudioVideo"), "Audio & video");
        assert_eq!(category_label("Development"), "Developer tools");
        assert_eq!(category_label("Games"), "Games");
    }

    #[test]
    fn card_tooltip_preserves_full_quiet_metadata_and_description() {
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
