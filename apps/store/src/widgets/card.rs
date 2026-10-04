//! One catalogue card: an app as an object the person can open.
//!
//! Composition:
//!
//! ```text
//! ┌──────────────────────────────────────────────┐
//! │ [art]  Notes                                 │
//! │        Tilo app, version 0.1.0               │
//! │ Capture ideas and work together live.        │
//! │                                   [ Open ]   │
//! └──────────────────────────────────────────────┘
//! ```
//!
//! The whole card opens the app's details; its footer carries the one action
//! the catalogue offers for the app. Everything is stock `ui::widget`
//! composition, so the theme owns every colour, radius and size.

use ui::TextRole;
use ui::tilo_actions::ActionBinding;
use ui::widget::{
    Actions, Avatar, AvatarSize, Button, Card, Column, Container, ControlSize, Row, Text, Widget,
};

use crate::model::{ActionKind, StoreItem, StoreSource};

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

/// What pressing the card does: open the app's details. `key` names the card
/// for keyboard traversal and tests.
pub struct CardDetails<'a> {
    pub key: &'a str,
    pub binding: Option<ActionBinding<()>>,
}

/// The app's icon: its published artwork while it loads or when it is
/// missing, the toolkit's generated tile.
pub fn app_icon(item: &StoreItem, images: Option<&ui::ImageStore>, size: AvatarSize) -> Avatar {
    let avatar = Avatar::person(&item.name, &item.id).square().size(size);
    match (images, item.icon_url.as_deref()) {
        (Some(store), Some(url)) => avatar.with_image_url(store.clone(), url),
        _ => avatar,
    }
}

/// Build a single card widget.
pub fn store_card(
    item: &StoreItem,
    primary: CardAction<'_>,
    details: CardDetails<'_>,
    images: Option<&ui::ImageStore>,
    theme: &ui::Theme,
) -> Box<dyn Widget> {
    let CardAction {
        kind: action,
        key: action_click_key,
        binding: action_binding,
        in_progress: operation_active,
    } = primary;
    let spacing = &theme.tokens.spacing;
    let identity = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(spacing.s1)
        .push(Box::new(
            Text::new(&item.name).with_role(TextRole::Subheading),
        ))
        .push(Box::new(
            Text::new(&provenance(item))
                .with_role(TextRole::Caption)
                .dimmed(),
        ));
    let header = Row::new()
        .no_wrap()
        .with_gap(spacing.s3)
        .with_align_items(taffy::AlignItems::Center)
        .push(Box::new(app_icon(item, images, AvatarSize::Large)))
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(identity)),
        ));
    let body = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(spacing.s3)
        .push(Box::new(header))
        .push(Box::new(
            Text::new(&item.description)
                .with_role(TextRole::Body)
                .dimmed(),
        ));

    let action_label = if operation_active {
        format!("{}…", action.progress_label())
    } else {
        action.label().to_string()
    };
    let mut action_btn = Button::new(action_click_key, &action_label, None);
    if let Some(binding) = action_binding {
        action_btn = action_btn.on_action(binding);
    }
    let action_btn = action_btn
        .control_size(ControlSize::Small)
        .with_enabled(!operation_active)
        .tooltip(if operation_active {
            format!("{} {} is in progress", action.progress_label(), item.name)
        } else {
            format!("{} {}", action.label(), item.name)
        });

    let mut card = Card::new(Box::new(body)).with_footer(Actions::new(action_btn));
    if let Some(binding) = details.binding {
        card = card.on_action(details.key, binding);
    }
    Box::new(card)
}

/// The secondary line under a card's name: what kind of app it is and which
/// version the source lists.
pub fn provenance(item: &StoreItem) -> String {
    let kind = match &item.source {
        StoreSource::Tilo { .. } => "Tilo app",
        StoreSource::Flathub => "Flatpak app",
    };
    let version = item.latest_version();
    if version.is_empty() {
        kind.to_string()
    } else {
        format!("{kind}, version {version}")
    }
}

/// Turn registry category identifiers into human-facing labels without
/// changing the stable value used by filtering and search.
pub fn category_label(value: &str) -> String {
    match value {
        "AudioVideo" => "Audio & video".into(),
        "Development" => "Developer tools".into(),
        "Graphics" => "Graphics & design".into(),
        other => other.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{category_label, provenance};

    use crate::model::{InstalledState, StoreItem, StoreItemVersion, StoreSource, StoreTrust};

    /// Registry categories read as words and the secondary line names the source and version.
    #[test]
    fn card_categories_and_provenance_read_as_words() {
        assert_eq!(category_label("AudioVideo"), "Audio & video");
        assert_eq!(category_label("Development"), "Developer tools");
        assert_eq!(category_label("Games"), "Games");

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
        assert_eq!(provenance(&item), "Flatpak app, version 2.4");
    }
}
