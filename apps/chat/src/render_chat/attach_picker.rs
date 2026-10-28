//! The workspace file picker behind the composer's attach button: the files
//! that can be shared, in the panel it opens above that button.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::chrome::nav_row_height;
use ui::format::size as format_bytes;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{ListRowFace, RowLeading};

use crate::app::{ChatApp, Hit};
use crate::paint::{line_in, line_of, push_clip};
use ui::chrome;

impl ChatApp {
    /// The workspace file picker, anchored above the composer's attach
    /// button. Lists the shareable files found in the shared workspace, so a
    /// share is always an actual file the person chose.
    #[allow(
        clippy::too_many_lines,
        reason = "one measure-then-paint pass keeps the panel height and its rows in step"
    )]
    pub(crate) fn draw_attach_picker(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        // Full-window dismiss target (lowest priority).
        ChatApp::push_hit(hits, self.rect, Hit::Dismiss);
        let sp = &theme.tokens.spacing;
        let local = theme.on_ground(chrome::panel_ground(theme));
        let pad = f64::from(sp.s2);
        let title_h = nav_row_height(theme);
        let caption_h = line_of(tr, &theme.text(TextRole::Caption));
        let status = self.attachment_status.as_deref().filter(|status| {
            !(self.attachment_picker_pending.is_some() && *status == "Loading attachments…")
        });
        let max_rows = self.attach_picker_max_rows();
        let rows: Vec<_> = self.attach_entries.iter().take(max_rows).collect();
        let row_h = ListRowFace::new("Ag")
            .secondary("Ag")
            .leading(RowLeading::Icon(Icon::Image))
            .height(tr, &local);
        // This device's files first, then a window to share live when the
        // desktop lists one; the workspace's files, where there is one,
        // under their own caption.
        let windows = !ui::window_share::windows().is_empty();
        let body_h = row_h
            + if windows { row_h } else { 0.0 }
            + if rows.is_empty() {
                0.0
            } else {
                caption_h + pad + rows.len() as f64 * row_h
            };
        let more = self.attach_entries.len().saturating_sub(max_rows);
        let panel_h = pad
            + title_h
            + if status.is_some() {
                caption_h + pad
            } else {
                0.0
            }
            + body_h
            + if more > 0 { caption_h + pad } else { 0.0 }
            + pad;
        let comp = self.composer_rect();
        let w = f64::from(theme.tokens.metrics.panel_width_list).min(self.rect.width() - 2.0 * pad);
        let x0 = comp.x0.min(self.rect.x1 - w - pad).max(self.rect.x0 + pad);
        let y1 = comp.y0 - pad;
        let y0 = (y1 - panel_h).max(self.rect.y0 + pad);
        let panel = Rect::new(x0, y0, x0 + w, y1);
        let radius = f64::from(theme.tokens.radii.panel);
        ui::surface::shadow(scene, tf, panel, radius, &theme.tokens.elevation.floating);
        ui::surface::panel(scene, tf, panel, radius, theme);
        push_clip(scene, tf, panel);

        let mut ctx = PaintCtx::new(scene, tf, tr, &local);
        let inner_x = panel.x0 + f64::from(sp.s4);
        let mut y = panel.y0 + pad;
        let title_band = Rect::new(panel.x0, y, panel.x1, y + title_h);
        line_in(
            &mut ctx,
            &crate::i18n::t("chat-attach"),
            &local.text(TextRole::Subheading),
            local.text_primary,
            inner_x,
            title_band,
            w - 2.0 * f64::from(sp.s4),
        );
        y = title_band.y1;
        if let Some(status) = status {
            let failed = status.starts_with("Not shared")
                || status.starts_with("Couldn't")
                || status.contains("cancelled");
            let ink = if failed {
                local.tokens.colors.semantic.danger
            } else {
                local.text_dim
            };
            let band = Rect::new(panel.x0, y, panel.x1, y + caption_h);
            line_in(
                &mut ctx,
                status,
                &local.text(TextRole::Caption),
                ink,
                inner_x,
                band,
                w - 2.0 * f64::from(sp.s4),
            );
            y = band.y1 + pad;
        }
        let device = Rect::new(panel.x0 + pad, y, panel.x1 - pad, y + row_h);
        ListRowFace::new(&crate::i18n::t("chat-attach-device"))
            .secondary(&crate::i18n::t("chat-attach-device-detail"))
            .leading(RowLeading::Icon(Icon::Upload))
            .interaction(ui::widget::Interaction {
                hovered: self.hovered(device),
                ..Default::default()
            })
            .paint(&mut ctx, device);
        ChatApp::push_hit(hits, device, Hit::AttachDevice);
        y = device.y1;
        if windows {
            let row = Rect::new(panel.x0 + pad, y, panel.x1 - pad, y + row_h);
            self.draw_share_window_row(&mut ctx, row, hits);
            y = row.y1;
        }
        if !rows.is_empty() {
            let band = Rect::new(panel.x0, y + pad, panel.x1, y + pad + caption_h);
            line_in(
                &mut ctx,
                &crate::i18n::t("chat-attach-workspace"),
                &local.text(TextRole::Caption),
                local.text_dim,
                inner_x,
                band,
                w - 2.0 * f64::from(sp.s4),
            );
            y = band.y1;
        }
        for e in rows {
            let row = Rect::new(panel.x0 + pad, y, panel.x1 - pad, y + row_h);
            let icon = if e.is_video {
                Icon::Film
            } else if crate::picker::is_image_name(&e.name) {
                Icon::Image
            } else {
                Icon::FileText
            };
            let size = format_bytes(e.size);
            ListRowFace::new(&e.name)
                .secondary(&size)
                .leading(RowLeading::Icon(icon))
                .interaction(ui::widget::Interaction {
                    hovered: self.hovered(row),
                    ..Default::default()
                })
                .paint(&mut ctx, row);
            if self.attachment_pending.is_none() {
                ChatApp::push_hit(hits, row, Hit::AttachPick(e.key()));
            }
            y += row_h;
        }
        if more > 0 {
            let band = Rect::new(panel.x0, y + pad, panel.x1, y + pad + caption_h);
            let label = crate::i18n::t_args("chat-attach-more", &[("count", more.into())]);
            line_in(
                &mut ctx,
                &label,
                &local.text(TextRole::Caption),
                local.text_dim,
                inner_x,
                band,
                w,
            );
        }
        scene.pop_layer();
    }

    /// At increased text sizes the picker becomes a short, readable sheet
    /// instead of shrinking its file names to preserve six rows.
    fn attach_picker_max_rows(&self) -> usize {
        if self.layout_text_scale > 1.5 {
            2
        } else if self.layout_text_scale > 1.15 {
            4
        } else {
            6
        }
    }
}
