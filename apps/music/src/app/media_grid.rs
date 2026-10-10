//! The cover grids every view shares (albums, artists, playlists, a Home
//! shelf) as the toolkit's [`MediaGrid`] of [`MediaCard`]s, embedded in the
//! page the app paints.
//!
//! The cards own their hover, press and focus, the Play action over a cover
//! and their accessible buttons. A grid is rebuilt when what its cards show
//! changes (a cover that arrived, a renamed playlist); what a card asks for
//! comes back as the app's own action.

use std::cell::RefCell;
use std::rc::Rc;

use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::widget::{Embedded, MediaAspect, MediaCard, MediaGrid, MediaSize, MediaSource};
use vello::kurbo::Rect;
use vello::peniko::ImageData;
use winit::event::WindowEvent;

use super::Action;

/// One card of a grid: what it shows and what it does.
pub(super) struct CardSpec {
    /// The cover-cache key; it names the card.
    pub art_id: String,
    pub title: String,
    pub meta: Vec<String>,
    pub image: Option<ImageData>,
    /// A person (an artist): a disc instead of a square cover.
    pub portrait: bool,
    /// What a press on the card does.
    pub action: Action,
    /// A Play action over the art while the card is under the pointer.
    pub play: Option<Action>,
    pub menu: Option<Action>,
}

/// What decides whether a grid is rebuilt: everything a card shows and does,
/// with its cover named by the picture's identity.
#[derive(Clone, Debug, PartialEq)]
struct CardKey {
    art_id: String,
    title: String,
    meta: Vec<String>,
    image: Option<u64>,
    portrait: bool,
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
            action: card.action.clone(),
            play: card.play.clone(),
            menu: card.menu.clone(),
        }
    }
}

type Asked = Rc<RefCell<Vec<Action>>>;

/// A media grid and the host that lays it out in the painted page.
pub(super) struct MediaGridHost {
    host: Embedded<MediaGrid>,
    key: Vec<CardKey>,
    asked: Asked,
    /// The grid shows this many loading cards instead of covers.
    loading: Option<usize>,
    /// Whether the grid was placed this frame.
    placed: bool,
}

impl MediaGridHost {
    pub(super) fn new() -> Self {
        Self {
            host: Embedded::new(MediaGrid::new(Vec::new()).with_size(MediaSize::Small)),
            key: Vec::new(),
            asked: Rc::default(),
            loading: None,
            placed: false,
        }
    }

    /// Show `count` loading cards: the shape of the covers on their way.
    pub(super) fn update_loading(&mut self, count: usize) {
        self.placed = false;
        if self.loading == Some(count) {
            return;
        }
        self.loading = Some(count);
        self.key.clear();
        self.host = Embedded::new(
            MediaGrid::loading(count, MediaAspect::Square).with_size(MediaSize::Small),
        );
    }

    /// Bring the grid up to date with this frame's cards.
    pub(super) fn update(&mut self, cards: Vec<CardSpec>) {
        self.placed = false;
        let key: Vec<CardKey> = cards.iter().map(CardKey::of).collect();
        if key == self.key && self.loading.is_none() {
            return;
        }
        self.loading = None;
        self.key = key;
        let built = cards
            .into_iter()
            .map(|card| self.build_card(card))
            .collect();
        self.host = Embedded::new(MediaGrid::new(built).with_size(MediaSize::Small));
    }

    fn build_card(&self, card: CardSpec) -> MediaCard {
        let source = card
            .image
            .map_or(MediaSource::Placeholder, MediaSource::Image);
        let activate = Rc::clone(&self.asked);
        let action = card.action;
        let mut built = MediaCard::new(format!("music.card.{}", card.art_id), source)
            .title(card.title)
            .on_activate(move || activate.borrow_mut().push(action.clone()));
        if card.portrait {
            built = built.aspect(MediaAspect::Circle);
        }
        for line in card.meta {
            built = built.meta(line);
        }
        if let Some(menu) = card.menu {
            let asked = Rc::clone(&self.asked);
            built = built.hover_action(
                Icon::MoreHorizontal,
                crate::i18n::t("music-spotify-playlist-actions"),
                move || {
                    asked.borrow_mut().push(menu.clone());
                },
            );
        } else if let Some(play) = card.play {
            let asked = Rc::clone(&self.asked);
            built = built.hover_action(Icon::Play, "Play", move || {
                asked.borrow_mut().push(play.clone());
            });
        }
        built
    }

    /// Lay the grid out from `top` across `[left, right]` and paint the
    /// cards inside `clip` (the page's viewport). Returns the y below it.
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
        let height = self
            .host
            .layout(Rect::new(left, top, right, top), ctx.theme, ctx.text);
        let bottom = top + height;
        if bottom > clip.y0 && top < clip.y1 {
            // Cards scrolled out of the page are not painted.
            let presence = PresenceCtx::default();
            let outer = presence.push_viewport(clip);
            self.host
                .draw(ctx.scene, ctx.transform, ctx.text, ctx.theme, &presence);
            presence.pop_viewport(outer);
            self.placed = true;
        }
        bottom
    }

    /// Route one window event to the grid; a grid out of view takes none.
    pub(super) fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if !self.placed {
            return;
        }
        self.host.handle_event(
            event,
            ctx.scale_factor,
            ctx.modifiers,
            &mut ctx.state,
            ctx.path,
            ctx.handled,
        );
    }

    /// What the cards asked for since the last call.
    pub(super) fn take_asked(&self) -> Vec<Action> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }

    /// Whether a card of the grid does `action` when pressed.
    #[cfg(test)]
    pub(super) fn offers(&self, action: &Action) -> bool {
        self.key
            .iter()
            .any(|card| &card.action == action || card.play.as_ref() == Some(action))
    }

    /// Where the card named by `art_id` stands.
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
