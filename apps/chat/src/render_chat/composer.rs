//! The composer: the field the person types a message in, its attach and
//! emoji buttons, what it says when a draft cannot be sent, the read-only form
//! a viewer gets, and the typing line under it.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::Widget;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{Button, ButtonFace, ControlSize};

use super::{composer_slot, paint_field, typing_line};
use crate::app::{ChatApp, Hit};
use crate::paint::{elide, line_in, line_of, width_of};

impl ChatApp {
    pub(crate) fn draw_composer(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let rect = self.composer_rect();
        self.composer_top = None;
        // With no channel open a draft would have nowhere to go, so there is
        // no composer to type into.
        if self.direct_person.is_none() && self.channel(&self.active_channel).is_none() {
            self.composer_main.set_focus(false);
            return;
        }
        self.composer_top = Some(rect.y0);
        if !self.can_participate() && self.direct_person.is_none() {
            self.composer_main.set_focus(false);
            Self::draw_read_only_composer(scene, tf, tr, theme, rect);
            return;
        }
        let interactive = self.main_composer_can_edit();
        self.composer_main.set_read_only(!interactive);
        // While an inline message edit is in progress the caret lives in the
        // editor, not the composer.
        let focused = interactive
            && self.is_focused()
            && matches!(self.focus, crate::app::Focus::Main)
            && self.editing.is_none();
        let draft = self.composer_main.value();
        let send_enabled = interactive && !draft.trim().is_empty();
        let inner = self.draw_input_box(
            PaintCtx::new(scene, tf, tr, theme),
            rect,
            &draft,
            (send_enabled, self.direct_person.is_none(), interactive),
            Hit::FocusMain,
            hits,
        );
        self.composer_main
            .set_mention_paint(self.mention_paint(theme));
        self.composer_main.set_mention_people(self.mention_people());
        self.composer_main.set_popover_bounds(self.rect);
        self.composer_main.layout(inner, theme, tr);
        self.composer_main.set_focus(focused);
        self.composer_main.draw(scene, tf, tr, theme, presence);
        let key = ChatApp::draft_presence_key(&self.active_channel);
        // How a file being shared is doing, while the picker is closed: an
        // upload under way, or why it was not shared.
        let upload = self.attachment_status.as_deref().filter(|status| {
            !self.attach_open
                && (status.starts_with("Uploading")
                    || status.starts_with("Not shared")
                    || status.starts_with("Couldn't")
                    || status.starts_with("The file chooser"))
        });
        if let Some(reason) = &self.not_sent {
            Self::draw_not_sent(PaintCtx::new(scene, tf, tr, theme), rect, reason, hits);
        } else if self.deleted_here() {
            Self::draw_deleted(PaintCtx::new(scene, tf, tr, theme), rect, hits);
        } else if let Some(status) = upload {
            let style = theme.text(TextRole::Caption);
            let ink = if status.starts_with("Uploading") {
                theme.text_dim
            } else {
                theme.tokens.colors.semantic.danger
            };
            let shown = elide(tr, status, &style, rect.width());
            tr.draw_style(
                scene,
                &shown,
                ink,
                tf.pre_translate(vello::kurbo::Vec2::new(
                    rect.x0,
                    rect.y1 + f64::from(theme.tokens.spacing.s1),
                )),
                None,
                &style,
            );
        } else if self.direct_person.is_none() {
            Self::draw_draft_presence(&self.composer_main, &key, scene, tf, tr, theme, presence);
        }
    }

    /// Under the composer, while a message was refused: that it was not
    /// sent, why, and a Retry that sends the kept text again.
    fn draw_not_sent(paint: PaintCtx<'_>, field: Rect, reason: &str, hits: &mut Vec<(Rect, Hit)>) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let style = theme.text(TextRole::Caption);
        let y = field.y1 + f64::from(theme.tokens.spacing.s1);
        let retry = &crate::i18n::t("chat-retry");
        let strong = style.clone().strong();
        let retry_w = width_of(tr, retry, &strong);
        let line = elide(
            tr,
            &crate::i18n::t_args("chat-not-sent", &[("reason", reason.to_owned().into())]),
            &style,
            (field.width() - retry_w - f64::from(theme.tokens.spacing.s3)).max(0.0),
        );
        let line_w = width_of(tr, &line, &style);
        let danger = theme.tokens.colors.semantic.danger;
        tr.draw_style(
            scene,
            &line,
            danger,
            tf.pre_translate(vello::kurbo::Vec2::new(field.x0, y)),
            None,
            &style,
        );
        let x = field.x0 + line_w + f64::from(theme.tokens.spacing.s3);
        let h = line_of(tr, &style);
        tr.draw_style(
            scene,
            retry,
            theme.text_primary,
            tf.pre_translate(vello::kurbo::Vec2::new(x, y)),
            None,
            &strong,
        );
        ChatApp::push_hit(hits, Rect::new(x, y, x + retry_w, y + h), Hit::Send);
    }

    /// Whether the message the person just deleted was in the channel shown.
    fn deleted_here(&self) -> bool {
        self.deleted
            .borrow()
            .as_ref()
            .is_some_and(|(_, channel)| *channel == self.active_channel)
    }

    /// Under the composer, after the person deleted a message of their
    /// own: that it was deleted, and an Undo that brings it back.
    fn draw_deleted(paint: PaintCtx<'_>, field: Rect, hits: &mut Vec<(Rect, Hit)>) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let style = theme.text(TextRole::Caption);
        let y = field.y1 + f64::from(theme.tokens.spacing.s1);
        let line = &crate::i18n::t("chat-message-deleted");
        let undo = &crate::i18n::t("chat-undo");
        let strong = style.clone().strong();
        let line_w = width_of(tr, line, &style);
        let undo_w = width_of(tr, undo, &strong);
        tr.draw_style(
            scene,
            line,
            theme.text_dim,
            tf.pre_translate(vello::kurbo::Vec2::new(field.x0, y)),
            None,
            &style,
        );
        let x = field.x0 + line_w + f64::from(theme.tokens.spacing.s3);
        let h = line_of(tr, &style);
        tr.draw_style(
            scene,
            undo,
            theme.text_primary,
            tf.pre_translate(vello::kurbo::Vec2::new(x, y)),
            None,
            &strong,
        );
        ChatApp::push_hit(hits, Rect::new(x, y, x + undo_w, y + h), Hit::UndoDelete);
    }

    /// Who else is writing in this composer's channel or thread, under the
    /// composer: "Ada is typing…". What they write stays theirs.
    pub(super) fn draw_draft_presence(
        composer: &crate::composer::DraftComposer,
        key: &ui::presence::PresenceKey,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some(line) = typing_line(presence, key) else {
            return;
        };
        let field = composer.rect();
        let style = theme.text(TextRole::Caption);
        let shown = elide(tr, &line, &style, field.width());
        tr.draw_style(
            scene,
            &shown,
            theme.text_dim,
            tf.pre_translate(vello::kurbo::Vec2::new(
                field.x0,
                field.y1 + f64::from(theme.tokens.spacing.s1),
            )),
            None,
            &style,
        );
    }

    /// A viewer must never be handed an editable-looking field that silently
    /// drops a send. This keeps the permission boundary visible at the exact
    /// point where someone would otherwise try to act.
    pub(super) fn draw_read_only_composer(
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) {
        let mut ctx = PaintCtx::new(scene, tf, tr, theme);
        let pad = f64::from(theme.tokens.spacing.s4);
        line_in(
            &mut ctx,
            &crate::i18n::t("chat-conversation-read-only"),
            &theme.text(TextRole::Body),
            theme.text_dim,
            rect.x0 + pad,
            rect,
            rect.width() - 2.0 * pad,
        );
    }

    /// The composer field and its buttons — attach, emoji and send — with
    /// their hit regions. Returns the band the composer's editor is laid out
    /// into; the editor owns the text, caret, selection and placeholder.
    ///
    /// The band is sized from the measured `draft` (one line when empty) and
    /// centred on the field, so a short draft lines up with the buttons; a
    /// long draft fills the field, which itself grows with the text.
    pub(crate) fn draw_input_box(
        &self,
        paint: PaintCtx<'_>,
        rect: Rect,
        draft: &str,
        (send_enabled, attach, interactive): (bool, bool, bool),
        focus_hit: Hit,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> Rect {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let btn = ButtonFace::height(ControlSize::Regular, theme);
        paint_field(scene, tf, theme, rect);
        if interactive {
            ChatApp::push_hit(hits, rect, focus_hit);
        }

        if !interactive {
            return tilo_rich_text::ComposerFrame::layout(rect, theme, &[], &[]).editor;
        }

        let send_label = crate::i18n::t("chat-send");
        let send_probe = ButtonFace::icon_only(Rect::ZERO, Icon::Send, &send_label)
            .control_size(ControlSize::Regular);
        let send_w = send_probe.intrinsic_width(tr, theme);
        let leading = if attach { vec![btn] } else { Vec::new() };
        let emoji_label = crate::i18n::t("chat-emoji-insert");
        let emoji_w =
            ButtonFace::icon_only(Rect::ZERO, Icon::Smile, &emoji_label).intrinsic_width(tr, theme);
        let frame =
            tilo_rich_text::ComposerFrame::layout(rect, theme, &leading, &[send_w, emoji_w]);
        if attach {
            let plus = composer_slot(&frame.leading, 0);
            Button::paint(
                scene,
                tf,
                tr,
                theme,
                &ButtonFace::icon_only(plus, Icon::Paperclip, &crate::i18n::t("chat-attach"))
                    .hovered(self.hovered(plus)),
            );
            ChatApp::push_hit(hits, plus, Hit::Attach);
        }

        // Enter sends. The send icon is a quiet one beside the emoji, for a
        // pointer or a touch, lit once there is something to send.
        let send = composer_slot(&frame.trailing, 0);
        let send_face = ButtonFace::icon_only(send, Icon::Send, &send_label)
            .control_size(ControlSize::Regular)
            .hovered(self.hovered(send));
        Button::paint(
            scene,
            tf,
            tr,
            theme,
            &if send_enabled {
                send_face
            } else {
                send_face.disabled(true)
            },
        );
        ChatApp::push_hit(hits, send, Hit::Send);

        let emoji = composer_slot(&frame.trailing, 1);
        Button::paint(
            scene,
            tf,
            tr,
            theme,
            &ButtonFace::icon_only(emoji, Icon::Smile, &emoji_label).hovered(self.hovered(emoji)),
        );
        ChatApp::push_hit(hits, emoji, Hit::ComposerEmoji(emoji));

        let _ = draft;
        frame.editor
    }
}
