//! An app as the Store's screens show it: its real icon, its name, its tag
//! line and its source, as a card on Home and in a category, and as a row in
//! Updates, On this device, Bots and search. Facts are separate text runs,
//! never joined into one string; every control is the shared widget.

use ui::TextRole;
use ui::tilo_actions::ActionBinding;
use ui::widget::media_card::{MediaAspect, MediaSource};
use ui::widget::{
    Actions, Avatar, AvatarSize, Button, ButtonRole, Column, Container, ControlSize, ListRow,
    ProgressBar, Row, Text, Widget,
};

use crate::model::{StoreItem, StoreSource};

type Element = Box<dyn Widget>;

/// The glyph that stands for a Tilo app everywhere on the desktop: Tilo apps
/// ship no raster icon, and the shell shows this one in search, tabs and the
/// switcher (`ui::icons::app_glyph`).
#[must_use]
pub fn tilo_glyph(item: &StoreItem) -> ui::icons::Icon {
    if item.bot.is_some() {
        return ui::icons::Icon::Bot;
    }
    ui::icons::app_glyph(&item.id, item.categories.first().map(String::as_str))
}

/// Whether the app's icon is artwork its source publishes (a Flathub icon)
/// rather than the desktop's glyph for it.
fn has_artwork(item: &StoreItem) -> bool {
    !matches!(item.source, StoreSource::Tilo { .. }) && item.icon_url.is_some()
}

/// The app's real icon at `size`: a Flathub app's published artwork (its
/// initials on the neutral tile until it loads), a Tilo app's glyph.
#[must_use]
pub fn icon(item: &StoreItem, images: Option<&ui::ImageStore>, size: AvatarSize) -> Avatar {
    let avatar = Avatar::person(&item.name, &item.id).neutral().size(size);
    match (images, item.icon_url.as_deref()) {
        (Some(store), Some(url)) if has_artwork(item) => avatar.with_image_url(store.clone(), url),
        _ if matches!(item.source, StoreSource::Tilo { .. }) => avatar.with_glyph(tilo_glyph(item)),
        _ => avatar,
    }
}

/// The app's icon as a row's leading art.
fn row_art(item: &StoreItem, images: Option<&ui::ImageStore>) -> MediaSource {
    match (images, item.icon_url.as_deref()) {
        (Some(store), Some(url)) if has_artwork(item) => MediaSource::Url {
            store: store.clone(),
            url: url.to_owned(),
        },
        _ => MediaSource::Kind(tilo_glyph(item)),
    }
}

/// The one thing a person does with an app from a list or its page.
pub struct Primary {
    /// Names the control for keyboard traversal, tests and agents; never
    /// written to the document.
    pub key: String,
    /// What pressing it does, in the person's words ("Open", "Install").
    pub label: String,
    /// Says what it does with the app's name, or why it cannot run now.
    pub tooltip: String,
    pub binding: Option<ActionBinding<()>>,
    /// Whether it can run now.
    pub enabled: bool,
    /// While it runs: what is happening, shown inline in its place.
    pub progress: Option<String>,
}

impl Primary {
    /// The control: the button, or while it runs, the progress in its place.
    #[must_use]
    pub fn control(self, size: ControlSize, role: ButtonRole) -> Element {
        if let Some(progress) = self.progress {
            return ProgressBar::indeterminate()
                .with_label(progress)
                .into_widget();
        }
        Box::new(self.button(size, role))
    }

    /// The button alone, for a footer that takes a [`Button`].
    #[must_use]
    pub fn button(self, size: ControlSize, role: ButtonRole) -> Button {
        let mut button = Button::new(&self.key, &self.label)
            .role(role)
            .control_size(size)
            .with_enabled(self.enabled)
            .tooltip(self.tooltip);
        if let Some(binding) = self.binding {
            button = button.on_action(binding);
        }
        button
    }
}

/// An app as a card: its icon, name and source above its tag line, and its
/// one action. The whole card opens the app's page.
#[must_use]
pub fn card(
    item: &StoreItem,
    primary: Primary,
    open_page: Option<ActionBinding<()>>,
    images: Option<&ui::ImageStore>,
) -> Element {
    let identity = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Tight)
        .push(Box::new(
            Text::new(&item.name).with_role(TextRole::Subheading),
        ))
        .push(Box::new(
            Text::new(&crate::catalogue::source_label(item))
                .with_role(TextRole::Caption)
                .dimmed(),
        ));
    let header = Row::new()
        .no_wrap()
        .with_rhythm(ui::tokens::Rhythm::Sibling)
        .with_align_items(taffy::AlignItems::Center)
        .push(Box::new(icon(item, images, AvatarSize::Large)))
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(identity)),
        ));
    let mut body = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Sibling)
        .push(Box::new(header));
    let tagline = item.summary();
    if !tagline.is_empty() {
        body = body.push(Box::new(
            Text::new(tagline).with_role(TextRole::Body).dimmed(),
        ));
    }
    let mut card = ui::widget::Card::new(Box::new(body)).with_accessible_name(&item.name);
    card = if primary.progress.is_some() {
        // The footer takes a button; while the action runs its progress
        // stands in the card's body instead, where the button was.
        let progress = primary.progress.clone().unwrap_or_default();
        card.with_footer(Actions::new(
            Button::new(&primary.key, &progress)
                .control_size(ControlSize::Small)
                .with_enabled(false),
        ))
    } else {
        card.with_footer(Actions::new(
            primary.button(ControlSize::Small, ButtonRole::Secondary),
        ))
    };
    if let Some(binding) = open_page {
        card = card.on_action(format!("store.card.{}", item.id), binding);
    }
    Box::new(card)
}

/// An app as a row: its icon, name, tag line and source, and an optional
/// control at its end. Activating the row opens the app's page.
#[must_use]
pub fn row(
    item: &StoreItem,
    trailing: Option<Element>,
    meta: Option<String>,
    open_page: Option<ActionBinding<()>>,
    images: Option<&ui::ImageStore>,
) -> Element {
    let mut row = ListRow::new(format!("store.row.{}", item.id), &item.name);
    // The same mark as the app's card and page: its artwork, else the
    // desktop's glyph for it on the neutral square, never its initials.
    row = match row_art(item, images) {
        MediaSource::Kind(glyph) => row.avatar(&item.name, &item.id).avatar_glyph(glyph),
        art => row.media(art, MediaAspect::Square),
    };
    let mut row = row.meta(meta.unwrap_or_else(|| crate::catalogue::source_label(item)));
    let tagline = item.summary();
    if !tagline.is_empty() {
        row = row.secondary(tagline);
    }
    if let Some(control) = trailing {
        row = row.trailing(control);
    }
    if let Some(binding) = open_page {
        row = row.disclosure().on_action(binding);
    }
    Box::new(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    // USER REQUIREMENT (2026-10-19): "app icons, tag line, directly opening tapps without explicit installation"
    #[test]
    fn a_tilo_app_shows_the_desktops_glyph_and_a_flathub_app_its_artwork() {
        let mut notes = crate::fixture::fixture_catalog().remove(0);
        notes.id = "so.tilo.notes".into();
        notes.source = StoreSource::Tilo {
            registry_name: "Tilo main".into(),
        };
        notes.icon_url = Some("https://apps.tilo.so/notes.svg".into());
        assert!(
            !has_artwork(&notes),
            "a Tilo app's registry poster is not its icon; the desktop's glyph is"
        );
        assert_eq!(tilo_glyph(&notes), ui::icons::Icon::FileText);
        let mut vlc = notes.clone();
        vlc.id = "org.videolan.VLC".into();
        vlc.source = StoreSource::Flathub;
        vlc.icon_url = Some("https://dl.flathub.org/media/vlc.png".into());
        assert!(has_artwork(&vlc));
        assert!(matches!(
            row_art(&vlc, Some(&crate::fixture::artwork_store())),
            MediaSource::Url { .. }
        ));
        assert!(matches!(row_art(&notes, None), MediaSource::Kind(_)));
    }
}
