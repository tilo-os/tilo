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
use ui::widget::{Avatar, AvatarSize, Button, Column, Container, ControlSize, Row, Text, Widget};

use crate::model::{ActionKind, StoreItem, StoreSource};

/// A card's primary action button.
///
/// `key` is only for local widget identity and accessibility; it is never
/// written to the collaboration document.
pub struct CardAction<'a> {
    pub kind: ActionKind,
    /// A host-observation label, when the action's default would claim a state
    /// the host could not verify.
    pub label: Option<&'a str>,
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
    let avatar = Avatar::person(&item.name, &item.id).neutral().size(size);
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
) -> Box<dyn Widget> {
    let CardAction {
        kind: action,
        label,
        key: action_click_key,
        binding: action_binding,
        in_progress: operation_active,
    } = primary;
    let identity = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Tight)
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
        .with_rhythm(ui::tokens::Rhythm::Sibling)
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
        .with_rhythm(ui::tokens::Rhythm::Sibling)
        .push(Box::new(header))
        .push(Box::new(
            Text::new(item.summary()).with_role(TextRole::Body).dimmed(),
        ));

    let action_label = if operation_active {
        format!("{}…", action.progress_label())
    } else {
        label.unwrap_or_else(|| action.label()).to_owned()
    };
    let mut action_btn = Button::new(action_click_key, &action_label);
    if let Some(binding) = action_binding {
        action_btn = action_btn.on_action(binding);
    }
    let action_btn = action_btn
        .control_size(ControlSize::Small)
        .with_enabled(!operation_active && item.unavailable_action(action).is_none())
        .tooltip(if operation_active {
            format!("{} {} is in progress", action.progress_label(), item.name)
        } else {
            item.unavailable_action(action).map_or_else(
                || format!("{} {}", label.unwrap_or_else(|| action.label()), item.name),
                |(_, reason)| reason,
            )
        });

    let mut card = ui::widget::Card::new(Box::new(body))
        .with_footer(ui::widget::Actions::new(action_btn))
        .with_accessible_name(&item.name);
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
        StoreSource::FlatpakRemote { title, .. } => {
            return match item.latest_version() {
                "" => format!("Flatpak app from {title}"),
                version => format!("Flatpak app from {title}, version {version}"),
            };
        }
    };
    let version = visible_version(item);
    if version.is_empty() {
        kind.to_string()
    } else {
        format!("{kind}, version {version}")
    }
}

/// Native catalogue builds include the commit in `SemVer` metadata. Keep that
/// exact identity in package details and actions, rather than on every card.
pub fn visible_version(item: &StoreItem) -> &str {
    let version = item.latest_version();
    if matches!(item.source, StoreSource::Tilo { .. })
        && let Some((release, commit)) = version.split_once("+g")
        && commit.len() == 40
        && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return release;
    }
    version
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
    use super::{category_label, provenance, visible_version};

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
        let mut native = item;
        native.source = StoreSource::Tilo {
            registry_name: "Tilo".into(),
        };
        native.versions[0].version = "0.2.0-rc.3+g4b07574f432de8ca4fc7f108e07c0237b75b4b05".into();
        assert_eq!(provenance(&native), "Tilo app, version 0.2.0-rc.3");
        assert_eq!(
            native.latest_version(),
            "0.2.0-rc.3+g4b07574f432de8ca4fc7f108e07c0237b75b4b05"
        );
        native.versions[0].version = "0.2.0+gpu".into();
        assert_eq!(visible_version(&native), "0.2.0+gpu");
    }
}
