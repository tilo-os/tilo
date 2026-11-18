//! The composer: the field the person types a message in, its attach and send
//! buttons, what it says when a draft cannot be sent, the read-only form a
//! viewer gets, and the typing line under it.
//!
//! The field is the toolkit's rich-text composer and offers what every text
//! field does: `:emoji:` through the picker and `@mentions`, so the composer
//! has no emoji button of its own.

use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{BlendMode, Fill};

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
        // With no conversation open a draft would have nowhere to go, so there is
        // no composer to type into.
        if self.direct_person().is_none() && !self.takes_messages() {
            self.composer_main.set_focus(false);
            return;
        }
        self.composer_top = Some(rect.y0);
        if !self.can_participate() && self.direct_person().is_none() {
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
        // While an agent is at work for this person and nothing is typed, the
        // send's place stops that work: what they type instead is sent, and
        // reaches the turn that is going.
        let stop = if interactive && !send_enabled {
            self.stop_offered(false)
        } else {
            None
        };
        let swap = self.stop_swap.follow(false, stop.is_some(), theme);
        let inner = self.draw_input_box(
            PaintCtx::new(scene, tf, tr, theme),
            rect,
            crate::app::stop::SendPlace { stop, swap },
            (send_enabled, self.direct_person().is_none(), interactive),
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
        } else if let Some(reason) = &self.not_stopped {
            Self::draw_not_stopped(PaintCtx::new(scene, tf, tr, theme), rect, reason);
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
        } else if let Some(key) = self
            .composing_key(crate::app::Focus::Main)
            .filter(|_| self.direct_person().is_none())
        {
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
        ChatApp::push_hit(hits, Rect::new(x, y, x + retry_w, y + h), Hit::RetrySend);
    }

    /// Under a box, while Chat refused a stop of an agent's work: that the work
    /// was not stopped, and why.
    pub(super) fn draw_not_stopped(paint: PaintCtx<'_>, field: Rect, reason: &str) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let style = theme.text(TextRole::Caption);
        let line = elide(
            tr,
            &crate::i18n::t_args(
                "chat-agent-not-stopped",
                &[("reason", reason.to_owned().into())],
            ),
            &style,
            field.width(),
        );
        tr.draw_style(
            scene,
            &line,
            theme.tokens.colors.semantic.danger,
            tf.pre_translate(vello::kurbo::Vec2::new(
                field.x0,
                field.y1 + f64::from(theme.tokens.spacing.s1),
            )),
            None,
            &style,
        );
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
    /// `place` says what stands where the box sends: its send, or, while an
    /// agent is at work for this person and nothing is typed, the stop of
    /// that work.
    pub(crate) fn draw_input_box(
        &self,
        paint: PaintCtx<'_>,
        rect: Rect,
        place: crate::app::stop::SendPlace,
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
        // The thread has its own box and its own send button: a send from the
        // conversation's box is found at that box, and nowhere else.
        let thread = matches!(focus_hit, Hit::FocusThread);
        let send_hit = if thread { Hit::ThreadSend } else { Hit::Send };
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
        let frame = tilo_rich_text::ComposerFrame::layout(rect, theme, &leading, &[send_w]);
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

        // Enter sends. The send icon is a quiet one in the corner, for a
        // pointer or a touch, lit once there is something to send. While an
        // agent is at work for this person and nothing is typed, the stop of
        // that work stands there instead: one takes the other's place by
        // fading, and a press acts on the one shown.
        let send = composer_slot(&frame.trailing, 0);
        let stop_label = crate::i18n::t("chat-agent-stop");
        let hovered = self.hovered(send);
        let face = |stops: bool| {
            if stops {
                ButtonFace::icon_only(send, Icon::Square, &stop_label)
                    .control_size(ControlSize::Regular)
                    .hovered(hovered)
            } else {
                ButtonFace::icon_only(send, Icon::Send, &send_label)
                    .control_size(ControlSize::Regular)
                    .hovered(hovered)
                    .disabled(!send_enabled)
            }
        };
        let stops = place.stop.is_some();
        let around = send.inflate(btn, btn);
        if let Some(opacity) = place.swap.leaving {
            paint_fading(
                PaintCtx::new(scene, tf, tr, theme),
                &face(!stops),
                around,
                opacity,
            );
        }
        paint_fading(
            PaintCtx::new(scene, tf, tr, theme),
            &face(stops),
            around,
            place.swap.shown,
        );
        let hit = match place.stop {
            Some(reply) => Hit::StopWork { reply, thread },
            None => send_hit,
        };
        ChatApp::push_hit(hits, send, hit);

        frame.editor
    }
}

/// Paint `face` at `opacity`: under a layer over `around` while it fades.
fn paint_fading(paint: PaintCtx<'_>, face: &ButtonFace<'_>, around: Rect, opacity: f32) {
    let PaintCtx {
        scene,
        transform: tf,
        text: tr,
        theme,
    } = paint;
    let fades = opacity < 1.0;
    if fades {
        scene.push_layer(Fill::NonZero, BlendMode::default(), opacity, tf, &around);
    }
    Button::paint(scene, tf, tr, theme, face);
    if fades {
        scene.pop_layer();
    }
}
