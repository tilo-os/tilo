//! The cover grids every view shares, as the toolkit's [`MediaGrid`]: the cards own hover,
//! press and focus, and what a card asks for comes back as the app's action.

use std::rc::Rc;

use super::Action;
use super::page_part::{Asked, Placed, asking, binding, bound};
use crate::model::Library;
use ui::framework::Actions;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::widget::{MediaAspect, MediaCard, MediaGrid, MediaSize, MediaSource};
use vello::kurbo::Rect;
use vello::peniko::ImageData;

pub(super) struct CardSpec {
    /// The cover-cache key, which names the card.
    pub art_id: String,
    pub title: String,
    pub meta: Vec<String>,
    pub image: Option<ImageData>,
    pub portrait: bool,
    /// What the art shows when there is no picture: the object's own glyph.
    pub kind: Option<Icon>,
    pub action: Action,
    /// A Play action over the art while the card is under the pointer.
    pub play: Option<Action>,
    pub menu: Option<Action>,
}

#[derive(Clone, Debug, PartialEq)]
struct CardKey {
    art_id: String,
    title: String,
    meta: Vec<String>,
    image: Option<u64>,
    portrait: bool,
    kind: Option<Icon>,
    action: Action,
    play: Option<Action>,
    menu: Option<Action>,
}

impl CardKey {
    fn of(card: &CardSpec) -> Self {
        Self {
            art_id: card.art_id.clone(),
            title: card.title.clone(),
            meta: card.meta.clone(),
            image: card.image.as_ref().map(|image| image.data.id()),
            portrait: card.portrait,
            kind: card.kind,
            action: card.action.clone(),
            play: card.play.clone(),
            menu: card.menu.clone(),
        }
    }
}

pub(super) struct MediaGridHost {
    pub(super) host: Placed<MediaGrid>,
    key: Vec<CardKey>,
    asked: Asked,
    /// Cards on their way, asked at that time.
    loading: Option<(usize, ui::time::Instant)>,
}

impl MediaGridHost {
    pub(super) fn new() -> Self {
        Self {
            host: Placed::new(MediaGrid::new(Vec::new()).with_size(MediaSize::Small)),
            key: Vec::new(),
            asked: Rc::default(),
            loading: None,
        }
    }

    /// Show `count` cards on their way: the shape of the covers to come.
    pub(super) fn update_arriving(
        &mut self,
        count: usize,
        from: &ui::arriving::Source,
        since: ui::time::Instant,
    ) {
        self.host.unplace();
        if self.loading == Some((count, since)) {
            return;
        }
        self.loading = Some((count, since));
        self.key.clear();
        self.host.replace(
            MediaGrid::arriving(count, MediaAspect::Square, from, since)
                .with_size(MediaSize::Small),
        );
    }

    pub(super) fn update(&mut self, cards: Vec<CardSpec>, actions: Option<&Actions<Library>>) {
        self.host.unplace();
        let key: Vec<CardKey> = cards.iter().map(CardKey::of).collect();
        if key == self.key && self.loading.is_none() {
            return;
        }
        self.loading = None;
        self.key = key;
        let built = cards
            .into_iter()
            .map(|card| self.build_card(card, actions))
            .collect();
        self.host
            .replace(MediaGrid::new(built).with_size(MediaSize::Small));
    }

    /// A card opens what it stands for through the published effect.
    fn build_card(&self, card: CardSpec, actions: Option<&Actions<Library>>) -> MediaCard {
        // No picture: the object's glyph on the shared neutral tile; initials read as broken art.
        let source = match (card.image, card.kind) {
            (Some(image), _) => MediaSource::Image(image),
            (None, Some(icon)) => MediaSource::Kind(icon),
            (None, None) => MediaSource::Placeholder,
        };
        let built = MediaCard::new(format!("music.card.{}", card.art_id), source).title(card.title);
        let mut built = bound(actions, built, &card.action).unwrap_or_else(|built| {
            if card.action.only_shows() {
                built.on_view(asking(&self.asked, card.action))
            } else {
                built.on_effect(asking(&self.asked, card.action))
            }
        });
        if card.portrait {
            built = built.aspect(MediaAspect::Circle);
        }
        for line in card.meta {
            built = built.meta(line);
        }
        if let Some(menu) = card.menu {
            built = self.hover(
                built,
                Icon::MoreHorizontal,
                crate::i18n::t("music-spotify-playlist-actions"),
                menu,
                actions,
            );
        } else if let Some(play) = card.play {
            built = self.hover(built, Icon::Play, "Play", play, actions);
        }
        built
    }

    /// A card's hover action: the published effect where it has one.
    fn hover(
        &self,
        card: MediaCard,
        icon: Icon,
        label: impl Into<String>,
        action: Action,
        actions: Option<&Actions<Library>>,
    ) -> MediaCard {
        if let Some(binding) = binding(actions, &action) {
            return card.hover_action_binding(icon, label, binding);
        }
        if action.only_shows() {
            card.hover_action_view(icon, label, asking(&self.asked, action))
        } else {
            card.hover_action_effect(icon, label, asking(&self.asked, action))
        }
    }

    /// Lay the grid out from `top` and paint the cards inside `clip`.
    pub(super) fn place(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        (left, right): (f64, f64),
        top: f64,
        clip: Rect,
    ) -> f64 {
        if self.key.is_empty() && self.loading.is_none() {
            return top;
        }
        self.host.place(ctx, (left, right), top, clip)
    }

    pub(super) fn take_asked(&self) -> Vec<Action> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }

    #[cfg(test)]
    pub(super) fn offers(&self, action: &Action) -> bool {
        self.key
            .iter()
            .any(|card| &card.action == action || card.play.as_ref() == Some(action))
    }

    #[cfg(test)]
    pub(super) fn card_rect(&self, art_id: &str) -> Option<Rect> {
        use ui::widget::Widget as _;
        let id = format!("music.card.{art_id}");
        self.host
            .widget()
            .children()
            .into_iter()
            .find(|card| card.test_id() == Some(id.as_str()))
            .map(ui::widget::Widget::get_rect)
    }
}
