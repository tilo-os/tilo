//! Windows shared live into a channel.
//!
//! A message that shares a window ([`crate::record::WindowShare`]) shows a
//! card: the app's mark, the window's title, the sharer's face and whether
//! they still have it open, over a miniature of the resource at the view's
//! own proportions. The miniature is not a picture the sharer sends: Chat
//! asks its desktop to draw the resource's own app there
//! ([`ui::window_share`]), from this device's copy of the record, so it
//! follows the sharer's work as the record syncs. Pressing the card opens
//! the resource as a window of the reader's own, with the access the sharer
//! gave. A share its sharer stopped turns into a neutral card that says so.
//!
//! The sharer shares from the attach menu's "Share a window" or from a
//! window's own menu ([`ui::window_share::Ask`]), choosing what the others
//! may do there (view only unless they say otherwise), and stops from the
//! message, its menu, or the window. While they share a window, their Chat
//! keeps the message saying whether the window is still open.

use std::cell::RefCell;
use std::collections::HashMap;

use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::tokens::TextRole;
use ui::widget::{
    AvatarFace, AvatarSize, BadgeFace, Button, ButtonFace, ButtonRole, CardFace, ChipStyle,
    ControlSize, MediaArt, MediaAspect, MediaBadge, MediaCardFace, MediaSize,
};
use ui::window_share::{Embed, EmbedState, OpenWindow, ResourceView};
use vello::kurbo::{Point, Rect};

use crate::app::{ChatApp, Hit};
use crate::messages::chat_part;
use crate::model::Message;
use crate::paint::{line_in, line_of};
use crate::parts::ChatPart;
use crate::record::ShareAccess;

/// The miniature's proportions: a window's, wider than tall.
pub(crate) const ASPECT: f64 = 16.0 / 10.0;

/// The widest a card grows: as wide as a picture in a message, so a
/// miniature stays a glance and not a second window in the conversation.
pub(crate) fn max_card_width(theme: &ui::theme::Theme) -> f64 {
    2.0 * MediaSize::Regular.extent(theme)
}

/// What Chat keeps of the windows shared into the room.
#[derive(Default)]
pub(crate) struct LiveShares {
    /// The "Share a window" panel, while it is open.
    pub(crate) panel: Option<Panel>,
    /// The miniatures this frame drew, asked of the desktop once it ends.
    embeds: RefCell<Vec<Embed>>,
    /// Whether each of this person's shares was last seen with its window
    /// open, by message: a change is written to the message.
    seen_open: HashMap<String, bool>,
    /// What the shares grant, and what the room lets this person do.
    pub(crate) grants: crate::share_grants::ShareGrants,
}

/// The "Share a window" panel: the window chosen and what the others may
/// do there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Panel {
    pub(crate) access: ShareAccess,
    /// The window chosen, by tab.
    pub(crate) chosen: Option<u64>,
}

/// The app and record `provider` names (`record:<app>:<record>`).
pub(crate) fn app_and_record(provider: &str) -> Option<(&str, &str)> {
    provider.strip_prefix("record:")?.split_once(':')
}

/// What the people a window is shared with may do, as a card says it.
pub(crate) fn access_label(access: ShareAccess) -> String {
    crate::i18n::t(match access {
        ShareAccess::View => "chat-share-view-only",
        ShareAccess::Edit => "chat-share-can-edit",
    })
}

/// The parts of a share's card.
struct CardParts {
    card: Rect,
    header: Rect,
    /// Where the miniature goes; none on a share that ended.
    view: Option<Rect>,
    /// The sharer's "Stop sharing", under the card.
    stop: Option<Rect>,
}

impl ChatApp {
    /// Whether `m` is a live share of this person's.
    pub(crate) fn is_my_live_share(&self, m: &Message) -> bool {
        m.share.as_ref().is_some_and(|share| !share.ended) && self.is_me(&m.author)
    }

    /// The height of `m`'s share card in a column `width` wide, the gap
    /// above it included; zero when `m` shares no window.
    pub(crate) fn share_card_height(
        &self,
        m: &Message,
        width: f64,
        tr: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
    ) -> f64 {
        if m.share.is_none() {
            return 0.0;
        }
        let parts = self.share_card_parts(m, Point::ZERO, width, tr, theme);
        let bottom = parts.stop.map_or(parts.card.y1, |stop| stop.y1);
        bottom + f64::from(theme.tokens.spacing.s2)
    }

    fn share_card_parts(
        &self,
        m: &Message,
        origin: Point,
        width: f64,
        tr: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
    ) -> CardParts {
        let sp = &theme.tokens.spacing;
        let pad = f64::from(sp.s3);
        let w = width.clamp(0.0, max_card_width(theme));
        let top = origin.y + f64::from(sp.s2);
        let title = line_of(tr, &theme.text(TextRole::Body).strong());
        let caption = line_of(tr, &theme.text(TextRole::Caption));
        let header_h = pad + (title + caption).max(AvatarSize::Small.side(theme)) + pad;
        let ended = m.share.as_ref().is_none_or(|share| share.ended);
        let header = Rect::new(origin.x, top, origin.x + w, top + header_h);
        let view = (!ended).then(|| {
            let inner = w - 2.0 * f64::from(sp.s1);
            Rect::new(
                origin.x + f64::from(sp.s1),
                header.y1,
                origin.x + f64::from(sp.s1) + inner,
                header.y1 + inner / ASPECT,
            )
        });
        let card = Rect::new(
            origin.x,
            top,
            origin.x + w,
            view.map_or(header.y1, |view| view.y1 + f64::from(sp.s1)),
        );
        let stop = self.is_my_live_share(m).then(|| {
            let h = ButtonFace::height(ControlSize::Small, theme);
            let label = crate::i18n::t("chat-share-stop");
            let bw = ButtonFace::new(Rect::ZERO, &label)
                .control_size(ControlSize::Small)
                .intrinsic_width(tr, theme)
                .min(w);
            let y = card.y1 + f64::from(sp.s2);
            Rect::new(origin.x, y, origin.x + bw, y + h)
        });
        CardParts {
            card,
            header,
            view,
            stop,
        }
    }

    /// Paint `m`'s share card from `*y` in a column at `x`, `width` wide,
    /// inside the timeline `clip`; advances `*y` past it.
    pub(crate) fn draw_share_card(
        &self,
        mut paint: PaintCtx<'_>,
        m: &Message,
        (x, width, clip): (f64, f64, Rect),
        y: &mut f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let Some(share) = m.share.clone() else {
            return;
        };
        let theme = paint.theme;
        let parts = self.share_card_parts(m, Point::new(x, *y), width, paint.text, theme);
        let reference = m.reference.as_ref();
        let label = reference.map_or("", |reference| reference.label.as_str());
        let app = m
            .source_provider
            .as_deref()
            .and_then(app_and_record)
            .map(|(app, _)| app.to_owned())
            .unwrap_or_default();
        let hovered = !share.ended && self.hovered(parts.card);
        let mut face = CardFace::new();
        if !share.ended {
            face = face.pressable().interaction(ui::widget::Interaction {
                hovered,
                ..ui::widget::Interaction::default()
            });
        }
        face.paint(&mut paint, parts.card);
        self.draw_share_header(&mut paint, m, &share, (label, &app), parts.header);
        if let Some(view) = parts.view {
            self.draw_share_view(&mut paint, m, &app, view, clip);
        }
        if !share.ended {
            ChatApp::push_hit(hits, parts.card, Hit::OpenShare(m.id.clone()));
        }
        if let Some(stop) = parts.stop {
            let label = crate::i18n::t("chat-share-stop");
            Button::paint(
                paint.scene,
                paint.transform,
                paint.text,
                theme,
                &ButtonFace::new(stop, &label)
                    .control_size(ControlSize::Small)
                    .role(ButtonRole::Secondary)
                    .hovered(self.hovered(stop)),
            );
            ChatApp::push_hit(hits, stop, Hit::StopShare(m.id.clone()));
        }
        *y = parts.stop.map_or(parts.card.y1, |stop| stop.y1) + f64::from(theme.tokens.spacing.s2);
    }

    /// The card's head: the app's mark, the title, what the others may do,
    /// and the sharer's face with whether it is live; or, ended, that it
    /// is no longer shared.
    fn draw_share_header(
        &self,
        paint: &mut PaintCtx<'_>,
        m: &Message,
        share: &crate::record::WindowShare,
        (label, app): (&str, &str),
        header: Rect,
    ) {
        let theme = paint.theme;
        let sp = &theme.tokens.spacing;
        let mark = Self::draw_share_mark(paint, share, app, header);
        let text_x = mark.x1 + f64::from(sp.s3);
        let text_end = if share.ended {
            header.x1 - f64::from(sp.s3)
        } else {
            self.draw_share_presence(paint, m, share, header)
        };
        let title_style = theme.text(TextRole::Body).strong();
        let caption_style = theme.text(TextRole::Caption);
        let title_h = line_of(paint.text, &title_style);
        let caption_h = line_of(paint.text, &caption_style);
        let top = header.center().y - f64::midpoint(title_h, caption_h);
        let (heading, caption) = if share.ended {
            (crate::i18n::t("chat-share-ended"), label.to_owned())
        } else if let Some(Err(why)) = self.resource_navigation.status(&m.id) {
            // Why pressing it opened nothing, where the access stood.
            (label.to_owned(), why.clone())
        } else if share.access == ShareAccess::View
            && !self.is_me(&m.author)
            && self.can_already_edit(&m.id)
        {
            // The share limits no one: say what this reader may do.
            (label.to_owned(), crate::i18n::t("chat-share-already-edit"))
        } else {
            (label.to_owned(), access_label(share.access))
        };
        let ink = if share.ended {
            theme.text_dim
        } else {
            theme.text_primary
        };
        line_in(
            paint,
            &heading,
            &title_style,
            ink,
            text_x,
            Rect::new(text_x, top, text_end, top + title_h),
            text_end - text_x,
        );
        line_in(
            paint,
            &caption,
            &caption_style,
            theme.text_dim,
            text_x,
            Rect::new(text_x, top + title_h, text_end, top + title_h + caption_h),
            text_end - text_x,
        );
    }

    /// The app's mark at the head's start, or, ended, a closed eye.
    /// Returns where it stands.
    fn draw_share_mark(
        paint: &mut PaintCtx<'_>,
        share: &crate::record::WindowShare,
        app: &str,
        header: Rect,
    ) -> Rect {
        let theme = paint.theme;
        let icon = f64::from(theme.tokens.metrics.icon_md);
        let x0 = header.x0 + f64::from(theme.tokens.spacing.s3);
        let cy = header.center().y;
        let mark = Rect::new(x0, cy - icon / 2.0, x0 + icon, cy + icon / 2.0);
        let (glyph, ink) = if share.ended {
            (Icon::EyeOff, theme.text_dim)
        } else {
            (ui::icons::app_glyph(app, None), theme.text_primary)
        };
        glyph.draw(paint.scene, paint.transform, mark, ink);
        mark
    }

    /// The sharer's face at the head's end and, while they have the window
    /// open and are here, the live mark before it. Returns where the text
    /// before them ends.
    fn draw_share_presence(
        &self,
        paint: &mut PaintCtx<'_>,
        m: &Message,
        share: &crate::record::WindowShare,
        header: Rect,
    ) -> f64 {
        let theme = paint.theme;
        let sp = &theme.tokens.spacing;
        let gap = f64::from(sp.s2);
        let face = AvatarSize::Small.side(theme);
        let end = header.x1 - f64::from(sp.s3);
        let cy = header.center().y;
        let face_rect = Rect::new(end - face, cy - face / 2.0, end, cy + face / 2.0);
        let name = self.user_name(&m.author);
        AvatarFace::new(&name, &m.author)
            .presence(self.identity_colour(&m.author))
            .paint(paint, face_rect);
        if !(share.open && self.sharer_here(&m.author)) {
            return face_rect.x0 - gap;
        }
        let live = crate::i18n::t("chat-share-live");
        let live_w = BadgeFace::intrinsic_width(&live, paint.text, theme);
        let live_h = BadgeFace::height(theme);
        let x0 = face_rect.x0 - gap - live_w;
        BadgeFace::new(
            Rect::new(x0, cy - live_h / 2.0, x0 + live_w, cy + live_h / 2.0),
            &live,
        )
        .style(ChipStyle::Live)
        .paint(paint.scene, paint.transform, paint.text, theme);
        x0 - gap
    }

    /// The miniature: what this device's desktop draws of the resource's
    /// own app, or, while it cannot, a quiet stand-in that says why.
    fn draw_share_view(
        &self,
        paint: &mut PaintCtx<'_>,
        m: &Message,
        app: &str,
        view: Rect,
        clip: Rect,
    ) {
        let record = m.source_provider.as_deref().and_then(app_and_record);
        let state = ui::window_share::state(&m.id);
        let note = match (&state, ui::window_share::drawn()) {
            (Some(EmbedState::Showing), _) => None,
            (Some(EmbedState::Unavailable(why)), _) => Some(why.clone()),
            (_, true) => Some(crate::i18n::t("chat-share-loading")),
            (_, false) => Some(crate::i18n::t("chat-share-open-to-see")),
        };
        // The app's own mark on the media tile until its view is drawn over
        // it, with why it is not yet along the bottom.
        let badges = note.as_deref().map(|label| MediaBadge::Status {
            label,
            detail: None,
        });
        MediaCardFace::new(MediaArt::Kind(ui::icons::app_glyph(app, None)))
            .aspect(MediaAspect::Ratio(ASPECT))
            .badges(badges.as_slice())
            .paint(paint, view);
        let shown = view.intersect(clip);
        if let Some((app, record)) = record
            && shown.width() > 0.0
            && shown.height() > 0.0
        {
            self.live_shares.embeds.borrow_mut().push(Embed {
                key: m.id.clone(),
                view: ResourceView {
                    app: app.to_owned(),
                    record: record.to_owned(),
                    resource: m
                        .reference
                        .as_ref()
                        .map(|reference| reference.id.clone())
                        .unwrap_or_default(),
                },
                rect: view,
                clip,
            });
        }
    }

    /// Whether `author` is in the room now: a share is live only while its
    /// sharer is here to work on it.
    fn sharer_here(&self, author: &str) -> bool {
        self.user(author)
            .is_some_and(|user| user.status != crate::model::Presence::Offline)
    }

    /// Ask the desktop for the miniatures this frame drew, unless a
    /// popover covers the conversation they are in.
    pub(crate) fn declare_share_embeds(&self) {
        let mut embeds = std::mem::take(&mut *self.live_shares.embeds.borrow_mut());
        let covered = self.menu.is_some()
            || self.emoji_open
            || self.attach_open
            || self.live_shares.panel.is_some()
            || self.history_open.is_some()
            || self.channel_sheet.is_some()
            || (self.call.active && self.call.expanded);
        if covered {
            embeds.clear();
        }
        ui::window_share::declare(embeds);
    }

    /// Open the window `message` shares, as a window of this person's own.
    pub(crate) fn open_share(&mut self, message: &str) {
        if self
            .message(message)
            .and_then(|m| m.share.as_ref())
            .is_some_and(|share| !share.ended)
        {
            self.live_shares.grants.ask_again(message);
            self.open_shared_resource(message);
        }
    }

    /// Stop sharing what `message` shares.
    pub(crate) fn stop_share(&mut self, doc: &mut ui::widget::Edits<'_>, message: &str) {
        let Some(m) = self.message(message) else {
            return;
        };
        if !self.is_my_live_share(m) {
            return;
        }
        self.apply_part(
            doc,
            ChatPart::STOP_SHARING.with(chat_part::StopSharing {
                message: ui::tilo_resource::Id::from_raw(message),
            }),
        );
        self.live_shares.seen_open.remove(message);
        self.request_relayout();
    }

    /// Share `window` into the channel in front, letting the others
    /// `access` it.
    pub(crate) fn share_window(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        window: &OpenWindow,
        access: ShareAccess,
    ) -> Result<(), String> {
        if self.active_channel.is_empty() {
            return Err("Open a channel to share a window in".into());
        }
        // A window on one item of its record (a note of the notebook)
        // shares that item, under the item's own kind.
        let item_kind = window
            .item
            .as_deref()
            .and_then(ui::tilo_resource::items::item_kind);
        let reference = crate::record::SharedReference {
            kind: match item_kind {
                Some(kind) => kind,
                None if window.kind.is_empty() => "tilo.window".into(),
                None => window.kind.clone(),
            },
            id: window.view().resource,
            label: window.title.clone(),
            span: None,
        };
        self.try_apply_part(
            doc,
            ChatPart::SHARE_WINDOW.with(chat_part::ShareWindow {
                channel: ui::tilo_resource::Id::from_raw(self.active_channel.clone()),
                reference,
                source_provider: format!("record:{}:{}", window.app, window.record),
                access: Some(access),
                text: None,
                mentions: None,
            }),
        )?;
        self.live_shares.panel = None;
        self.timeline_at_end = true;
        self.scroll = 0.0;
        self.request_relayout();
        Ok(())
    }

    /// Open the "Share a window" panel, with `chosen` chosen.
    pub(crate) fn open_share_panel(&mut self, chosen: Option<u64>) {
        self.menu = None;
        self.emoji_open = false;
        self.emoji_target = None;
        self.history_open = None;
        self.attach_open = false;
        self.live_shares.panel = Some(Panel {
            access: ShareAccess::View,
            chosen,
        });
        self.request_relayout();
    }

    /// The panel's choices: what the others may do, and which window.
    pub(crate) fn choose_share_access(&mut self, access: ShareAccess) {
        if let Some(panel) = self.live_shares.panel.as_mut() {
            panel.access = access;
        }
    }

    pub(crate) fn choose_share_window(&mut self, tab: u64) {
        if let Some(panel) = self.live_shares.panel.as_mut() {
            panel.chosen = Some(tab);
        }
    }

    /// Share the window the panel has chosen.
    pub(crate) fn share_chosen(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some(panel) = self.live_shares.panel.clone() else {
            return;
        };
        let Some(window) = ui::window_share::windows()
            .into_iter()
            .find(|window| Some(window.tab) == panel.chosen)
        else {
            return;
        };
        if let Err(why) = self.share_window(doc, &window, panel.access) {
            self.attachment_status = Some(why);
        }
    }

    /// Keep this person's shares in step with their desktop: what it asked
    /// of Chat, which of the shared windows are still open, and which
    /// windows Chat shares, for the desktop to mark.
    pub(crate) fn follow_window_shares(&mut self, doc: &mut ui::widget::Edits<'_>) {
        self.follow_share_grants();
        while let Some(ask) = ui::window_share::take_ask() {
            match ask {
                ui::window_share::Ask::Share(window) => self.open_share_panel(Some(window.tab)),
                ui::window_share::Ask::Stop { message } => self.stop_share(doc, &message),
            }
        }
        let mine: Vec<(String, String, String, String, bool)> = self
            .search_messages
            .iter()
            .filter(|m| self.is_my_live_share(m))
            .filter_map(|m| {
                let (app, record) = app_and_record(m.source_provider.as_deref()?)?;
                let place = self
                    .channel(&m.channel)
                    .map(|channel| channel.kind.label(&channel.name))
                    .unwrap_or_default();
                Some((
                    m.id.clone(),
                    app.to_owned(),
                    record.to_owned(),
                    place,
                    m.share.as_ref().is_some_and(|share| share.open),
                ))
            })
            .collect();
        ui::window_share::publish_shared(
            mine.iter()
                .map(
                    |(message, app, record, place, _)| ui::window_share::SharedWindow {
                        message: message.clone(),
                        app: app.clone(),
                        record: record.clone(),
                        place: place.clone(),
                    },
                )
                .collect(),
        );
        // Only a desktop that lists its windows says whether one is open.
        if !ui::window_share::drawn() {
            return;
        }
        let windows = ui::window_share::windows();
        for (message, app, record, _, stored) in mine {
            let open = windows
                .iter()
                .any(|window| window.app == app && window.record == record);
            let seen = self.live_shares.seen_open.insert(message.clone(), open);
            if seen != Some(open) && stored != open {
                self.apply_part(
                    doc,
                    ChatPart::SHARE_OPEN.with(chat_part::ShareOpen {
                        message: ui::tilo_resource::Id::from_raw(message),
                        open,
                    }),
                );
            }
        }
    }
}
