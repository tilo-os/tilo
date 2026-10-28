//! The receipts: what became of a screenshot or a recording, at the top of
//! the screen, with what can be done with it next.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    reason = "receipt geometry is bounded logical-pixel arithmetic over a tiny fixed set of actions"
)]

use super::{
    CapturePreview, CaptureStudio, RECORDING_RECEIPT_DURATION, SCREENSHOT_RECEIPT_DURATION,
    ScreenshotKept, ScreenshotReceipt, leaving_opacity, studio_progress, studio_text, ui_count,
};
use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::Duration;
use crate::tokens::TextRole;
use crate::widget::{Button, ButtonFace, ButtonRole, ControlSize, IconButton};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Size as KurboSize};
use vello::peniko::{BlendMode, Color, Fill};

/// The screenshot receipt's buttons are small controls, an icon-only one
/// square, `s1` apart, inset `s1` in a bar that keeps `s2` off the screen
/// edge.
const RECEIPT_BUTTON_H: f64 = crate::tokens::Metrics::BASE.bar_control_height as f64;
const RECEIPT_BUTTON_W: f64 = RECEIPT_BUTTON_H;
const RECEIPT_BUTTON_GAP: f64 = crate::tokens::Spacing::BASE.s1 as f64;
const RECEIPT_INSET: f64 = crate::tokens::Spacing::BASE.s1 as f64;
/// The thumbnail's inset at rest.
const RECEIPT_THUMB_INSET: f64 = crate::tokens::Spacing::BASE.s2 as f64;
const RECEIPT_HEIGHT: f64 = RECEIPT_BUTTON_H + 2.0 * RECEIPT_INSET;
const RECEIPT_TEXT_GAP: f64 = crate::tokens::Spacing::BASE.s3 as f64;
const RECEIPT_EDGE_MARGIN: f64 = crate::tokens::Spacing::BASE.s2 as f64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReceiptAction {
    /// Copy the saved screenshot, offered when copying it did not succeed.
    Copy,
    /// Open it for markup.
    Edit,
    /// Open it in the image viewer.
    Open,
    /// Show it in Files.
    Reveal,
    /// Send it to the workspace or another device.
    Send,
    /// Play the saved recording.
    Play,
}

impl ReceiptAction {
    const ALL: [Self; 6] = [
        Self::Copy,
        Self::Edit,
        Self::Open,
        Self::Reveal,
        Self::Send,
        Self::Play,
    ];

    /// What the screenshot receipt offers for a screenshot `kept` so: a file
    /// can be edited, opened, shown and sent; one the clipboard refused,
    /// copied. A copy alone has nothing to act on.
    fn offered(kept: ScreenshotKept) -> Vec<Self> {
        if !kept.saved {
            return Vec::new();
        }
        let copy = (!kept.copied).then_some(Self::Copy);
        copy.into_iter()
            .chain([Self::Edit, Self::Open, Self::Reveal, Self::Send])
            .collect()
    }

    pub(super) fn label(self) -> String {
        studio_text(match self {
            Self::Copy => "widget-screenshot-copy",
            Self::Edit => "widget-screenshot-edit",
            Self::Open => "widget-screenshot-open",
            Self::Reveal => "widget-screenshot-reveal",
            Self::Send => "widget-screenshot-send",
            Self::Play => "widget-recording-play",
        })
    }

    fn icon(self) -> Icon {
        match self {
            Self::Copy => Icon::Copy,
            Self::Edit => Icon::Pencil,
            Self::Open => Icon::ExternalLink,
            Self::Reveal => Icon::FolderOpen,
            Self::Send => Icon::Send,
            Self::Play => Icon::Play,
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|a| *a == self).unwrap_or(0)
    }
}

/// Where everything in a receipt sits.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct ReceiptLayout {
    pub(super) pill: Rect,
    /// The screenshot's thumbnail; empty on a recording's receipt.
    pub(super) preview: Rect,
    pub(super) actions: Vec<(ReceiptAction, Rect)>,
    pub(super) dismiss: Rect,
    /// The actions show their labels (they fit), else only their icons.
    pub(super) labelled: bool,
}

/// A receipt's text as last measured. Render caches only.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ReceiptWidths {
    status: f64,
    actions: [f64; 6],
}

impl Default for ReceiptWidths {
    fn default() -> Self {
        Self {
            status: 110.0,
            actions: [72.0, 64.0, 68.0, 110.0, 72.0, 64.0],
        }
    }
}

impl CaptureStudio {
    pub(super) fn screenshot_receipt_motion(receipt: &ScreenshotReceipt) -> f64 {
        if let Some(progress) = receipt.progress_override {
            return progress;
        }
        // A spatial handoff from selected pixels to shell chrome.
        studio_progress(
            crate::motion::Zoom::SPEC,
            crate::time::now().saturating_duration_since(receipt.shown_at),
        )
    }

    pub(super) fn screenshot_receipt_layout(&self) -> ReceiptLayout {
        let Some(receipt) = self.screenshot_receipt.as_ref() else {
            return ReceiptLayout::default();
        };
        self.receipt_layout_at(
            Self::screenshot_receipt_motion(receipt),
            receipt.origin,
            Some((receipt.width, receipt.height)),
            &ReceiptAction::offered(receipt.kept),
        )
    }

    pub(super) fn recording_receipt_layout(&self) -> ReceiptLayout {
        let center = self.status_rect().center();
        self.receipt_layout_at(
            1.0,
            center,
            None,
            &[ReceiptAction::Play, ReceiptAction::Reveal],
        )
    }

    /// A receipt at `p` of its way from what was captured (`0`) to its place
    /// at the top of the screen (`1`): its ground, the screenshot's
    /// thumbnail (`preview` is its pixel size), the actions and Dismiss.
    fn receipt_layout_at(
        &self,
        p: f64,
        origin: Point,
        preview: Option<(u32, u32)>,
        actions: &[ReceiptAction],
    ) -> ReceiptLayout {
        let bounds = self.status_rect();
        let room = (bounds.width() - 2.0 * RECEIPT_EDGE_MARGIN).max(0.0);
        let final_height = RECEIPT_HEIGHT;
        let thumb = preview.map_or(KurboSize::ZERO, |(w, h)| fit_preview(w, h, 46.0, 24.0));
        let lead = if preview.is_some() {
            RECEIPT_THUMB_INSET + thumb.width + RECEIPT_TEXT_GAP
        } else {
            RECEIPT_TEXT_GAP
        };
        let fixed =
            lead + self.receipt_widths.status + RECEIPT_TEXT_GAP + RECEIPT_BUTTON_W + RECEIPT_INSET;
        let labelled_width: f64 = actions
            .iter()
            .map(|action| self.receipt_widths.actions[action.index()] + RECEIPT_BUTTON_GAP)
            .sum();
        let icon_width = ui_count(actions.len()) * (RECEIPT_BUTTON_W + RECEIPT_BUTTON_GAP);
        // Labelled while they fit; on a narrow display, icons named for
        // assistive technology and their tooltip.
        let labelled = fixed + labelled_width <= room;
        let final_width = (fixed + if labelled { labelled_width } else { icon_width }).min(room);
        let (start_width, start_height) = match preview {
            // Begin as a recognizable thumbnail that gains room for its
            // status and actions on the way.
            Some((w, h)) => {
                let start = fit_preview(w, h, 144.0, 66.0);
                (start.width + 12.0, start.height + 12.0)
            }
            None => (final_width, final_height),
        };
        let width = start_width + (final_width - start_width) * p;
        let height = start_height + (final_height - start_height) * p;
        let edge_inset = final_width / 2.0 + RECEIPT_EDGE_MARGIN;
        let final_x = bounds
            .center()
            .x
            .min(bounds.x1 - edge_inset)
            .max(bounds.x0 + edge_inset);
        let final_y = bounds.y0 + RECEIPT_INSET + final_height / 2.0;
        let start = Point::new(
            origin.x.clamp(bounds.x0 + 32.0, bounds.x1 - 32.0),
            origin.y.clamp(bounds.y0 + 20.0, bounds.y1 - 20.0),
        );
        let center = Point::new(
            start.x + (final_x - start.x) * p,
            start.y + (final_y - start.y) * p,
        );
        let pill = Rect::from_center_size(center, (width, height));
        let button = |x1: f64, w: f64| {
            let cy = pill.center().y;
            Rect::new(
                x1 - w,
                cy - RECEIPT_BUTTON_H / 2.0,
                x1,
                cy + RECEIPT_BUTTON_H / 2.0,
            )
        };
        let dismiss = button(pill.x1 - RECEIPT_INSET, RECEIPT_BUTTON_W);
        let mut x1 = dismiss.x0 - RECEIPT_BUTTON_GAP;
        let mut placed = Vec::with_capacity(actions.len());
        for action in actions.iter().rev() {
            let w = if labelled {
                self.receipt_widths.actions[action.index()]
            } else {
                RECEIPT_BUTTON_W
            };
            let rect = button(x1, w);
            x1 = rect.x0 - RECEIPT_BUTTON_GAP;
            placed.push((*action, rect));
        }
        placed.reverse();
        let preview = preview.map_or(Rect::ZERO, |(w, h)| {
            let start = fit_preview(w, h, 144.0, 66.0);
            let end = fit_preview(w, h, 46.0, 24.0);
            let width = start.width + (end.width - start.width) * p;
            let height = start.height + (end.height - start.height) * p;
            let inset = 6.0 + 2.0 * p;
            Rect::from_center_size(
                Point::new(pill.x0 + inset + width / 2.0, pill.center().y),
                (width, height),
            )
        });
        ReceiptLayout {
            pill,
            preview,
            actions: placed,
            dismiss,
            labelled,
        }
    }

    /// Measure a receipt's status and action labels as they are painted.
    fn measure_receipt(&mut self, status: &str, tr: &mut TextRenderer, theme: &Theme) {
        let status_style = theme
            .text(TextRole::Caption)
            .with_weight(crate::text_render::TextWeight::Medium);
        // Rounded up, with a pixel to spare, so the measured text is never
        // elided by the arithmetic that places it.
        self.receipt_widths.status =
            f64::from(tr.measure_style(status, None, &status_style).0).ceil() + 1.0;
        for action in ReceiptAction::ALL {
            let label = action.label();
            let face = ButtonFace::new(Rect::ZERO, &label)
                .icon(action.icon())
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small);
            self.receipt_widths.actions[action.index()] =
                (face.intrinsic_width(tr, theme).ceil() + 2.0).max(RECEIPT_BUTTON_W);
        }
    }

    /// Paint a receipt: its floating ground, the thumbnail, the status, the
    /// actions and Dismiss, at `alpha`.
    fn paint_receipt(
        &self,
        ctx: &mut PaintCtx<'_>,
        layout: &ReceiptLayout,
        status: &str,
        preview: Option<(&CapturePreview, bool)>,
        (p, alpha): (f64, f32),
        held_action: Option<ReceiptAction>,
    ) {
        let PaintCtx {
            scene,
            transform,
            text: tr,
            theme,
        } = ctx.reborrow();
        let pill = layout.pill;
        let radius = 6.0 + (crate::widget::floating_surface::single_row_radius(theme) - 6.0) * p;
        let fading = alpha < 1.0;
        if fading {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                alpha,
                transform,
                &pill.inflate(48.0, 48.0),
            );
        }
        crate::widget::floating_surface::paint_rounded(scene, transform, pill, radius, theme);
        let colors = &theme.tokens.colors;
        if let Some((pixels, saved)) = preview {
            let rect = layout.preview;
            let hovered = saved && rect.contains(self.cursor);
            let corner = f64::from(theme.tokens.radii.item);
            scene.push_clip_layer(
                Fill::NonZero,
                transform,
                &RoundedRect::from_rect(rect, corner),
            );
            let columns = usize::from(pixels.columns.max(1));
            let rows = usize::from(pixels.rows.max(1));
            let cell_width = rect.width() / columns as f64;
            let cell_height = rect.height() / rows as f64;
            for (index, rgba) in pixels.pixels.iter().enumerate() {
                let (column, row) = (index % columns, index / columns);
                if row >= rows {
                    break;
                }
                let cell = Rect::new(
                    rect.x0 + column as f64 * cell_width,
                    rect.y0 + row as f64 * cell_height,
                    rect.x0 + (column + 1) as f64 * cell_width + 0.25,
                    rect.y0 + (row + 1) as f64 * cell_height + 0.25,
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]),
                    None,
                    &cell,
                );
            }
            scene.pop_layer();
            crate::surface::ContentFrame::Thumbnail { selected: hovered }
                .draw(scene, transform, rect, theme);
        }
        let content_alpha = crate::motion::unit_opacity(crate::motion::stage(p, 0.32, 1.0));
        if content_alpha > 0.0 {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                content_alpha,
                transform,
                &pill.inflate(24.0, 24.0),
            );
            let status_style = theme
                .text(TextRole::Caption)
                .with_weight(crate::text_render::TextWeight::Medium);
            let status_x = if preview.is_some() {
                layout.preview.x1 + RECEIPT_TEXT_GAP
            } else {
                pill.x0 + RECEIPT_TEXT_GAP
            };
            let actions_x0 = layout
                .actions
                .first()
                .map_or(layout.dismiss.x0, |(_, rect)| rect.x0);
            let room = (actions_x0 - RECEIPT_TEXT_GAP - status_x).max(0.0);
            let shown = tr.elide_to_width_style(status, room as f32, &status_style);
            let line = f64::from(tr.measure_style("Ag", None, &status_style).1);
            tr.draw_style(
                scene,
                &shown,
                colors.text_primary,
                transform.pre_translate((status_x, pill.center().y - line / 2.0).into()),
                None,
                &status_style,
            );
            for (action, rect) in &layout.actions {
                let label = action.label();
                let hovered = rect.contains(self.cursor) || held_action == Some(*action);
                let face = if layout.labelled {
                    ButtonFace::new(*rect, &label)
                } else {
                    ButtonFace::icon_only(*rect, action.icon(), &label)
                }
                .icon(action.icon())
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small)
                .hovered(hovered);
                Button::paint(scene, transform, tr, theme, &face);
            }
            let dismiss_name = studio_text("widget-capture-dismiss");
            let face = IconButton::face(layout.dismiss, Icon::X, &dismiss_name)
                .control_size(ControlSize::Small)
                .hovered(layout.dismiss.contains(self.cursor));
            Button::paint(scene, transform, tr, theme, &face);
            scene.pop_layer();
        }
        let tip = if layout.labelled {
            None
        } else {
            layout
                .actions
                .iter()
                .find(|(_, rect)| rect.contains(self.cursor))
                .map(|(action, rect)| (*rect, action.label()))
        };
        let tip = tip.or_else(|| {
            layout
                .dismiss
                .contains(self.cursor)
                .then(|| (layout.dismiss, studio_text("widget-capture-dismiss")))
        });
        if let Some((anchor, label)) = tip {
            crate::widget::tooltip::draw_hover_tooltip(
                scene,
                transform,
                tr,
                theme,
                anchor,
                &label,
                self.status_rect(),
            );
        }
        if fading {
            scene.pop_layer();
        }
    }

    pub(super) fn draw_screenshot_receipt(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(receipt) = self.screenshot_receipt.clone() else {
            return;
        };
        let status = studio_text(receipt.kept.status_key());
        self.measure_receipt(&status, tr, theme);
        // Choosing a Send target holds the receipt fully shown.
        let choosing = self.send_menu.is_some();
        let elapsed = if choosing {
            Duration::ZERO
        } else {
            crate::time::now().saturating_duration_since(receipt.shown_at)
        };
        if elapsed >= SCREENSHOT_RECEIPT_DURATION && !self.screenshot_pin_pending {
            self.screenshot_receipt = None;
            self.open = self.shows_status();
            return;
        }
        let p = if theme.reduce_motion {
            1.0
        } else {
            Self::screenshot_receipt_motion(&receipt)
        };
        let alpha = if theme.reduce_motion || choosing {
            1.0
        } else {
            leaving_opacity(receipt.shown_at, SCREENSHOT_RECEIPT_DURATION)
        };
        let layout = self.screenshot_receipt_layout();
        self.paint_receipt(
            &mut PaintCtx::new(scene, transform, tr, theme),
            &layout,
            &status,
            Some((&receipt.preview, receipt.kept.saved)),
            (p, alpha),
            choosing.then_some(ReceiptAction::Send),
        );
    }

    pub(super) fn draw_recording_receipt(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(finished_at) = self.recording_finished_at else {
            return;
        };
        if crate::time::now().saturating_duration_since(finished_at) >= RECORDING_RECEIPT_DURATION {
            self.recording_finished_at = None;
            self.open = self.shows_status();
            return;
        }
        let status = studio_text("widget-recording-saved");
        self.measure_receipt(&status, tr, theme);
        let alpha = if theme.reduce_motion {
            1.0
        } else {
            leaving_opacity(finished_at, RECORDING_RECEIPT_DURATION)
        };
        let layout = self.recording_receipt_layout();
        self.paint_receipt(
            &mut PaintCtx::new(scene, transform, tr, theme),
            &layout,
            &status,
            None,
            (1.0, alpha),
            None,
        );
    }
}

fn fit_preview(width: u32, height: u32, max_width: f64, max_height: f64) -> KurboSize {
    let aspect = f64::from(width.max(1)) / f64::from(height.max(1));
    if max_width / max_height > aspect {
        KurboSize::new(max_height * aspect, max_height)
    } else {
        KurboSize::new(max_width, max_width / aspect)
    }
}
