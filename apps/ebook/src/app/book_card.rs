//! One book's card, as the shelf and search show it: its cover or
//! generated art, title, author and notes, its progress and whether it is
//! shared, and the actions over its cover while it is hovered.

use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::widget::{
    ButtonFace, ButtonRole, ChipStyle, ControlSize, Interaction, MediaArt, MediaAspect, MediaBadge,
    MediaCardFace,
};
use vello::kurbo::Rect;

use super::{Action, BooksApp};
use crate::model::Book;

/// The book-specific facts painted over and under a shelf cover.
pub(super) struct ShelfCard<'a> {
    pub(super) book: &'a Book,
    /// What opening the card does.
    pub(super) open: Action,
    /// Whether the book's text is on this device.
    pub(super) here: bool,
    pub(super) shared: bool,
    /// Since when its text has been on its way here, while it is.
    pub(super) arriving: Option<ui::time::Instant>,
    /// Where its text stands while it is neither here nor on its way, and
    /// what a screen reader hears besides.
    pub(super) status: Option<(&'static str, Option<&'static str>)>,
    pub(super) progress: f32,
    pub(super) notes: Option<String>,
    pub(super) focused: bool,
}

impl BooksApp {
    /// One book's media card: its cover (or generated art), title, author
    /// and note count, with its reading progress along the cover's foot.
    pub(super) fn book_face<'a>(
        card: &'a ShelfCard<'a>,
        art: MediaArt<'a>,
        badges: &'a [MediaBadge<'a>],
    ) -> MediaCardFace<'a> {
        let mut face = MediaCardFace::new(art)
            .aspect(MediaAspect::Portrait)
            .title(&card.book.title)
            .badges(badges);
        if !card.book.author.is_empty() {
            face = face.meta(&card.book.author);
        }
        if let Some(notes) = &card.notes {
            face = face.meta(notes);
        }
        face
    }

    pub(super) fn draw_book_card(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        card: &ShelfCard<'_>,
        cell: Rect,
        body: Rect,
    ) {
        let theme = ctx.theme;
        let hovered = self.is_hover(cell) && body.contains(self.cursor);
        let image = self.cover_image(card.book);
        let art = image
            .as_ref()
            .map_or(MediaArt::Placeholder, MediaArt::Image);
        // The card's state as the design system's badges: how far it is
        // read, and whether it is on the shared workspace. The book's
        // subject is not on the cover: it is a filter of the shelf and of
        // search.
        let mut badges = Vec::with_capacity(2);
        if let Some(started) = card.arriving {
            badges.push(MediaBadge::Loading(started));
        } else if let Some((label, detail)) = card.status {
            badges.push(MediaBadge::Status { label, detail });
        } else if card.progress > 0.0 {
            badges.push(MediaBadge::Progress(f64::from(card.progress)));
        }
        if card.shared {
            badges.push(MediaBadge::Tag("Shared", ChipStyle::Neutral));
        }
        let face = Self::book_face(card, art, &badges).interaction(Interaction {
            hovered,
            focused: card.focused,
            ..Interaction::default()
        });
        face.paint(ctx, cell);
        let cover = face.art_rect(cell);

        let inset = f64::from(theme.tokens.spacing.s2);

        // The whole card opens the book; clamp it to the scrolling body so a
        // card sliding under the header never takes its clicks.
        let hit = cell.intersect(body);
        if hit.height() > 0.0 {
            self.push_hit(hit, card.open.clone());
        }
        // Share and Remove over the cover's foot while the card is hovered;
        // pushed after the card so they win where they overlap it.
        let actions_h = ButtonFace::height(ControlSize::Small, theme) + inset;
        if hovered && cover.y1 <= body.y1 && cover.y1 - actions_h >= body.y0 {
            // A book whose text is elsewhere has nothing here to share.
            self.draw_book_card_hover_actions(ctx, cover, &card.book.id, card.shared || !card.here);
        }
    }

    /// Share (local books only) and Remove, at the cover's trailing foot.
    fn draw_book_card_hover_actions(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        cover: Rect,
        book_id: &str,
        shared: bool,
    ) {
        let theme = ctx.theme;
        let inset = f64::from(theme.tokens.spacing.s2);
        let height = ButtonFace::height(ControlSize::Small, theme);
        let y = cover.y1 - inset - height;
        let mut x1 = cover.x1 - inset;
        let remove = Rect::new(x1 - height, y, x1, y + height);
        self.button(
            ctx,
            ButtonFace::icon_only(remove, Icon::Trash2, "Remove from library")
                .role(ButtonRole::Secondary)
                .control_size(ControlSize::Small),
            Action::RemoveFromLibrary(book_id.to_string()),
        );
        x1 = remove.x0 - f64::from(theme.tokens.spacing.rhythm.related);
        if !shared {
            let width = ButtonFace::new(Rect::ZERO, "Share")
                .icon(Icon::Users)
                .control_size(ControlSize::Small)
                .intrinsic_width(ctx.text, theme);
            let share = Rect::new(x1 - width, y, x1, y + height);
            self.button(
                ctx,
                ButtonFace::new(share, "Share")
                    .icon(Icon::Users)
                    .control_size(ControlSize::Small),
                Action::ShareToWorkspace(book_id.to_string()),
            );
        }
    }
}
