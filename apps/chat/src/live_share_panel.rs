//! The "Share a window" panel, above the composer: the windows this person
//! has open, what the others may do there, and Share.

use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{Button, ButtonFace, ButtonRole, ControlSize, ListRowFace, RowLeading};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use crate::app::{ChatApp, Hit};
use crate::live_share::access_label;
use crate::paint::{line_in, line_of, push_clip};
use crate::record::ShareAccess;

impl ChatApp {
    #[allow(
        clippy::too_many_lines,
        reason = "one measure-then-paint pass keeps the panel height and its rows in step"
    )]
    pub(crate) fn draw_share_panel(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let Some(panel) = self.live_shares.panel.clone() else {
            return;
        };
        ChatApp::push_hit(hits, self.rect, Hit::Dismiss);
        let windows = ui::window_share::windows();
        let sp = &theme.tokens.spacing;
        let local = theme.on_ground(ui::chrome::panel_ground(theme));
        let pad = f64::from(sp.s2);
        let inset = f64::from(sp.s4);
        let title_h = ui::chrome::nav_row_height(theme);
        let caption_h = line_of(tr, &local.text(TextRole::Caption));
        let control_h = ButtonFace::height(ControlSize::Regular, &local);
        let row_h = ListRowFace::new("Ag")
            .secondary("Ag")
            .leading(RowLeading::Icon(ui::icons::Icon::AppWindow))
            .height(tr, &local);
        let rows = windows.len().min(6);
        let list_h = if rows == 0 {
            caption_h * 2.0 + pad
        } else {
            rows as f64 * row_h
        };
        let panel_h =
            pad + title_h + control_h + pad + caption_h + list_h + pad + control_h + pad + pad;
        let comp = self.composer_rect();
        let w = f64::from(theme.tokens.metrics.panel_width_list).min(self.rect.width() - 2.0 * pad);
        let x0 = comp.x0.min(self.rect.x1 - w - pad).max(self.rect.x0 + pad);
        let y1 = comp.y0 - pad;
        let y0 = (y1 - panel_h).max(self.rect.y0 + pad);
        let rect = Rect::new(x0, y0, x0 + w, y1);
        let radius = f64::from(theme.tokens.radii.panel);
        ui::surface::shadow(scene, tf, rect, radius, &theme.tokens.elevation.floating);
        ui::surface::panel(scene, tf, rect, radius, theme);
        // Presses inside the panel never dismiss it.
        ChatApp::push_hit(hits, rect, Hit::FocusMain);
        push_clip(scene, tf, rect);
        let mut ctx = ui::paint::PaintCtx::new(scene, tf, tr, &local);
        let inner_x = rect.x0 + inset;
        let inner_w = w - 2.0 * inset;
        let mut y = rect.y0 + pad;
        let place = self
            .channel(&self.active_channel)
            .map(|channel| channel.kind.label(&channel.name))
            .unwrap_or_default();
        let title = crate::i18n::t_args("chat-share-window-to", &[("place", place.into())]);
        line_in(
            &mut ctx,
            &title,
            &local.text(TextRole::Subheading),
            local.text_primary,
            inner_x,
            Rect::new(rect.x0, y, rect.x1, y + title_h),
            inner_w,
        );
        y += title_h;
        // What the others may do: view only unless the sharer says so.
        let mut x = inner_x;
        for access in [ShareAccess::View, ShareAccess::Edit] {
            let label = access_label(access);
            let face = ButtonFace::new(Rect::ZERO, &label).toggled(panel.access == access);
            let bw = face.intrinsic_width(ctx.text, &local);
            let button = Rect::new(x, y, x + bw, y + control_h);
            Button::paint(
                ctx.scene,
                tf,
                ctx.text,
                &local,
                &ButtonFace::new(button, &label)
                    .toggled(panel.access == access)
                    .hovered(self.hovered(button)),
            );
            ChatApp::push_hit(hits, button, Hit::ShareAccess(access));
            x = button.x1 + pad;
        }
        y += control_h + pad;
        line_in(
            &mut ctx,
            &crate::i18n::t("chat-share-your-windows"),
            &local.text(TextRole::Caption),
            local.text_dim,
            inner_x,
            Rect::new(rect.x0, y, rect.x1, y + caption_h),
            inner_w,
        );
        y += caption_h;
        if windows.is_empty() {
            line_in(
                &mut ctx,
                &crate::i18n::t("chat-share-no-windows"),
                &local.text(TextRole::Caption),
                local.text_dim,
                inner_x,
                Rect::new(rect.x0, y + pad, rect.x1, y + pad + caption_h * 2.0),
                inner_w,
            );
            y += list_h;
        }
        for window in windows.iter().take(rows) {
            let row = Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + row_h);
            ListRowFace::new(&window.title)
                .secondary(&window.app_title)
                .leading(RowLeading::Icon(ui::icons::app_glyph(&window.app, None)))
                .selected(panel.chosen == Some(window.tab))
                .interaction(ui::widget::Interaction {
                    hovered: self.hovered(row),
                    ..Default::default()
                })
                .paint(&mut ctx, row);
            ChatApp::push_hit(hits, row, Hit::SharePick(window.tab));
            y += row_h;
        }
        y += pad;
        let chosen = panel
            .chosen
            .is_some_and(|tab| windows.iter().any(|window| window.tab == tab));
        let label = crate::i18n::t("chat-share-window-button");
        let bw = ButtonFace::new(Rect::ZERO, &label)
            .role(ButtonRole::Primary)
            .intrinsic_width(ctx.text, &local);
        let share = Rect::new(rect.x1 - inset - bw, y, rect.x1 - inset, y + control_h);
        Button::paint(
            ctx.scene,
            tf,
            ctx.text,
            &local,
            &ButtonFace::new(share, &label)
                .role(ButtonRole::Primary)
                .disabled(!chosen)
                .hovered(chosen && self.hovered(share)),
        );
        if chosen {
            ChatApp::push_hit(hits, share, Hit::ShareChosen);
        }
        if let Some(status) = self.attachment_status.as_deref() {
            line_in(
                &mut ctx,
                status,
                &local.text(TextRole::Caption),
                local.tokens.colors.semantic.danger,
                inner_x,
                Rect::new(rect.x0, y, share.x0 - pad, y + control_h),
                share.x0 - pad - inner_x,
            );
        }
        scene.pop_layer();
    }
}

impl ChatApp {
    /// The attach menu's "Share a window" row in `row`.
    pub(crate) fn draw_share_window_row(
        &self,
        ctx: &mut ui::paint::PaintCtx<'_>,
        row: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let title = crate::i18n::t("chat-share-window");
        let detail = crate::i18n::t("chat-share-window-detail");
        ListRowFace::new(&title)
            .secondary(&detail)
            .leading(RowLeading::Icon(ui::icons::Icon::AppWindow))
            .interaction(ui::widget::Interaction {
                hovered: self.hovered(row),
                ..Default::default()
            })
            .paint(ctx, row);
        ChatApp::push_hit(hits, row, Hit::ShareWindowPanel);
    }
}
