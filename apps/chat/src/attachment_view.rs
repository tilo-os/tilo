//! The attachment a person opened: the picture at its own size, and what they
//! can do with it.
//!
//! Chat used to paint a thumbnail that answered nothing: a shared screenshot
//! could not be enlarged, a shared PDF could not be opened at all, and nothing
//! could be kept. Clicking a card now opens it, and the viewer offers the
//! ladder every conversation offers — the same words and the same order as
//! Messages (see `ui::widget::conversation::attachment`): the picture at full
//! size, a copy saved where the person says, and, once there is a copy, the
//! desktop's own file manager showing it.
//!
//! Chat owns the bytes: an attachment is a content hash in the app's content
//! store, never a path meaningful on one device. Saving is therefore a real
//! copy out, through the host's save dialog, and the bytes are written only to
//! the place the person chose: a path on a device that has them, or the file
//! a browser page's dialog keeps (or its download).

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::conversation::attachment::Availability;
use ui::widget::conversation::{
    AttachmentAction, AttachmentFace, AttachmentKind, AttachmentSummary, SavedCopy, full_size_rect,
};
use ui::widget::{Button, ButtonFace, ControlSize};

use crate::app::{ChatApp, Hit};
use crate::model::Id;
use tilo_reference::Attached;

/// The attachment the person has open over the conversation.
pub(crate) struct AttachmentView {
    /// The message it came from: the bytes stay where the message says.
    pub(crate) msg: Id,
    /// Which of the message's files it is.
    pub(crate) attachment: usize,
    /// The copy the person saved while the viewer was open.
    pub(crate) saved: Option<SavedCopy>,
}

impl AttachmentView {
    pub(crate) fn new(msg: Id, attachment: usize) -> Self {
        Self {
            msg,
            attachment,
            saved: None,
        }
    }
}

/// What kind of thing an attachment is.
pub(crate) fn kind_of(att: &Attached) -> AttachmentKind {
    match att {
        Attached::Image { .. } => AttachmentKind::Image,
        Attached::Video { .. } => AttachmentKind::Video,
        Attached::File { .. } | Attached::Resource { .. } | Attached::Text { .. } => {
            AttachmentKind::File
        }
    }
}

impl ChatApp {
    /// The file among the message `msg`'s attachments, when it has one.
    pub(crate) fn attachment_of(&self, msg: &str, attachment: usize) -> Option<Attached> {
        self.message(msg)
            .and_then(|m| m.attachments.get(attachment))
            .filter(|attached| attached.blob().is_some())
            .cloned()
    }

    /// What the ladder needs to know about the open attachment, in the
    /// toolkit's vocabulary.
    pub(crate) fn attachment_summary<'a>(&self, att: &'a Attached) -> AttachmentSummary<'a> {
        let mut summary = AttachmentSummary::ready(kind_of(att), att.label(), att.size());
        if !self.embed_allowed(att.blob().unwrap_or_default()) {
            summary = summary.in_state(Availability::NotShared);
        }
        if let Some(view) = &self.attachment_view
            && let Some(saved) = &view.saved
        {
            summary = summary.saved_as(saved.clone());
        }
        summary
    }

    /// Ask the host where to put a copy of `att`, then write it there.
    pub(crate) fn begin_attachment_save(&self, msg: &str, attachment: usize) {
        let Some(att) = self.attachment_of(msg, attachment) else {
            return;
        };
        let Some(jobs) = &self.jobs else {
            return;
        };
        let pick = ui::portal::PickRequest::save(
            &crate::i18n::t("chat-attachment-save-title"),
            &crate::i18n::t("chat-attachment-save-accept"),
            att.label(),
        )
        .thread_name("chat-attachment-save");
        let blob = att.blob().unwrap_or_default().to_owned();
        let msg = msg.to_owned();
        jobs.start::<crate::ChatWindow>("attachment-save", move |cx| {
            Ok(cx.choose_destination(pick, move |window, chosen, cx| {
                let Some(destination) = chosen else {
                    return Ok(ui::framework::Effected::done());
                };
                // The bytes were verified when they arrived; a copy is written
                // only where the person chose, and never invented when the
                // store has none (a demo attachment is a picture with no file
                // behind it).
                let Some(bytes) = window.state().content.read(&blob) else {
                    window.state().attachment_status =
                        Some(crate::i18n::t("chat-attachment-not-on-device"));
                    return Ok(ui::framework::Effected::done());
                };
                let place = destination.clone();
                Ok(
                    cx.write_destination(&place, bytes, move |window, written, _| {
                        window.state().saved_attachment(&msg, &destination, written);
                        Ok(ui::framework::Effected::done())
                    }),
                )
            }))
        });
    }

    /// Say how writing the copy of an attachment of the message `msg` to
    /// `destination` went.
    fn saved_attachment(
        &mut self,
        msg: &str,
        destination: &ui::portal::SaveDestination,
        written: Result<(), String>,
    ) {
        if let Err(reason) = written {
            self.attachment_status = Some(format!(
                "{}: {reason}",
                crate::i18n::t("chat-attachment-save-failed")
            ));
            return;
        }
        // Only a copy with a path on this device can be shown in its folder;
        // one the browser keeps is in its downloads.
        if let Some(path) = destination.location()
            && let Some(view) = self.attachment_view.as_mut()
            && view.msg == msg
        {
            view.saved = Some(SavedCopy {
                name: crate::i18n::t("chat-attachment-saved"),
                target: path.to_string_lossy().into_owned(),
            });
        }
        self.attachment_status = Some(format!(
            "{}: {}",
            crate::i18n::t("chat-attachment-saved"),
            destination.name()
        ));
    }

    /// Show the saved copy's folder in the desktop's file manager.
    pub(crate) fn reveal_saved_copy(&self) {
        let Some(view) = &self.attachment_view else {
            return;
        };
        let Some(saved) = &view.saved else {
            return;
        };
        ui::portal::LaunchRequest::reveal(saved.target.clone()).send();
    }

    /// Paint the open attachment over the whole conversation.
    #[allow(
        clippy::too_many_lines,
        reason = "one paint pass for one overlay: scrim, picture, toolbar and its controls in the order they are layered"
    )]
    pub(crate) fn draw_attachment_viewer(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let Some(view) = &self.attachment_view else {
            return;
        };
        let msg = view.msg.clone();
        let index = view.attachment;
        ui::overlay::scrim::modal(scene, tf, self.rect, 1.0, theme);
        // Anywhere on the scrim closes: the picture is the only thing that
        // keeps a click.
        ChatApp::push_hit(hits, self.rect, Hit::Dismiss);
        let Some(att) = self.attachment_of(&msg, index) else {
            // The message went while the viewer was open.
            self.attachment_view = None;
            return;
        };
        let summary = self.attachment_summary(&att);
        let spacing = &theme.tokens.spacing;
        let inset = f64::from(spacing.s6);
        let bar_h = ButtonFace::height(ControlSize::Regular, theme);
        let stage = Rect::new(
            self.rect.x0 + inset,
            self.rect.y0 + inset,
            self.rect.x1 - inset,
            self.rect.y1 - inset,
        );
        let picture = Rect::new(
            stage.x0,
            stage.y0,
            stage.x1,
            (stage.y1 - bar_h - inset).max(stage.y0),
        );
        let mut paint = PaintCtx::new(scene, tf, tr, theme);
        // The picture itself, when there is one and it is readable here.
        let readable = Self::summary_allows(&summary);
        if readable && summary.kind.previews() {
            if let Some(image) = self.attachment_picture(&att) {
                let target = full_size_rect(picture, image.width, image.height);
                paint.fill_round(
                    target,
                    f64::from(theme.tokens.radii.tile),
                    theme.tokens.colors.surfaces.raised,
                );
                let scale = target.width() / f64::from(image.width.max(1));
                let place = tf * Affine::translate((target.x0, target.y0));
                let place = place * Affine::scale_non_uniform(scale, scale);
                paint.scene.draw_image(&image, place);
                ChatApp::push_hit(hits, target, Hit::Dismiss);
            } else {
                let caption = crate::i18n::t("chat-attachment-loading");
                paint.text_role_in_band(
                    &caption,
                    TextRole::Body,
                    theme.text_dim,
                    picture.x0,
                    picture,
                    Some(ui::num::f32_of_f64(picture.width())),
                );
            }
        }
        // The line under the picture: what it is, its size, and where a copy
        // went — or why nothing can be done with it.
        let face = AttachmentFace::new(&summary);
        let meta_w = (picture.width() - 2.0 * f64::from(spacing.s3)).max(1.0);
        let meta = face.meta();
        let style = theme.text(TextRole::Caption);
        let meta_h = f64::from(
            paint
                .text
                .measure_style(&meta, Some(ui::num::f32_of_f64(meta_w)), &style)
                .1,
        );
        let meta_band = Rect::new(
            picture.x0 + f64::from(spacing.s3),
            picture.y1 + f64::from(spacing.s2),
            picture.x1 - f64::from(spacing.s3),
            picture.y1 + f64::from(spacing.s2) + meta_h,
        );
        let reason = summary.unavailable_reason();
        let meta_ink = if reason.is_some() {
            theme.tokens.colors.semantic.danger
        } else {
            theme.text_dim
        };
        paint.text(
            &meta,
            &style,
            meta_ink,
            Point::new(meta_band.x0, meta_band.y0),
            Some(ui::num::f32_of_f64(meta_w)),
        );
        // The ladder, right to left: Close, then what can be done.
        let gap = f64::from(spacing.s2);
        let mut x = stage.x1;
        let mut button = |hits: &mut Vec<(Rect, Hit)>,
                          paint: &mut PaintCtx<'_>,
                          label: &str,
                          icon: Icon,
                          hit: Hit| {
            let face = ButtonFace::new(Rect::ZERO, label).icon(icon);
            let w = face.intrinsic_width(paint.text, paint.theme);
            let rect = Rect::new(x - w, stage.y1 - bar_h, x, stage.y1);
            let hovered = self.hovered(rect);
            Button::paint(
                paint.scene,
                paint.transform,
                paint.text,
                paint.theme,
                &ButtonFace::new(rect, label).icon(icon).hovered(hovered),
            );
            ChatApp::push_hit(hits, rect, hit);
            x = rect.x0 - gap;
        };
        button(
            hits,
            &mut paint,
            &crate::i18n::t("chat-attachment-close"),
            Icon::X,
            Hit::CloseAttachment,
        );
        if view.saved.is_some() {
            let label = AttachmentAction::OpenSaved.label();
            button(
                hits,
                &mut paint,
                &label,
                AttachmentAction::OpenSaved.icon(),
                Hit::Attachment {
                    msg: msg.clone(),
                    attachment: index,
                    action: AttachmentAction::OpenSaved,
                },
            );
        }
        if readable {
            let label = AttachmentAction::Save.label();
            button(
                hits,
                &mut paint,
                &label,
                AttachmentAction::Save.icon(),
                Hit::Attachment {
                    msg,
                    attachment: index,
                    action: AttachmentAction::Save,
                },
            );
        }
        // The name, at the leading edge of the toolbar.
        let title_style = theme.text(TextRole::Subheading);
        paint.text(
            att.label(),
            &title_style,
            theme.text_primary,
            Point::new(
                stage.x0 + f64::from(spacing.s3),
                stage.y1 - bar_h + (bar_h - f64::from(title_style.line_height_px())) / 2.0,
            ),
            Some(ui::num::f32_of_f64((x - stage.x0).max(1.0))),
        );
    }

    /// The decoded picture of an image, or a clip's poster frame: what the
    /// viewer shows at full size.
    pub(crate) fn attachment_picture(&self, att: &Attached) -> Option<vello::peniko::ImageData> {
        match att {
            Attached::Image { blob, .. } => self.images.get(&crate::images::blob_key(blob)),
            Attached::Video { blob, .. } => self.video_poster(blob),
            Attached::File { .. } | Attached::Resource { .. } | Attached::Text { .. } => None,
        }
    }

    /// Whether the summary says the bytes can be read here.
    fn summary_allows(summary: &AttachmentSummary<'_>) -> bool {
        summary.availability == Availability::Ready
    }
}
