//! The code scanner: the viewfinder with a scan guide, the decoded code's
//! outline, its result card beside or below the guide, and the recent scans.

use ui::TextRole;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::widget::{
    BadgeFace, Button, ButtonFace, ButtonRole, CardFace, ChipStyle, EmptyState, EmptyStateContent,
    ListRowFace, RowLeading, Tone, floating_surface,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::hud::{HudItem, HudMetrics, cluster_width, on_panel};
use super::{Action, CameraApp, SCAN_PAYLOAD, Storage, frame_image_cover, frame_image_fill};
use crate::frame;
use crate::model::{CodeKind, ScanEntry, ScannedCode, Screen, classify};
use crate::paint as p;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

/// Where the scanner's parts sit for one window size.
pub(super) struct ScannerLayout {
    /// The column beside the guide on a wide window.
    pub side: Option<Rect>,
    /// The square the code is held in.
    pub guide: Rect,
    /// The result card, when a code is decoded and not dismissed.
    pub card: Option<Rect>,
    /// The band above the guide that holds the hint.
    pub hint: Rect,
}

/// The icon for a kind of code.
pub(super) fn code_icon(kind: &CodeKind) -> Icon {
    match kind {
        CodeKind::Url => Icon::Globe,
        CodeKind::Wifi { .. } => Icon::Wifi,
        CodeKind::Contact { .. } | CodeKind::Phone => Icon::Users,
        CodeKind::Email => Icon::Mail,
        CodeKind::Geo => Icon::Pin,
        CodeKind::Text => Icon::FileText,
    }
}

/// The tag over a code detected in the viewfinder, e.g. "QR code detected".
pub(super) fn detected_label(symbology: &str) -> String {
    let name = if symbology.eq_ignore_ascii_case("QR Code") {
        "QR code"
    } else {
        symbology
    };
    format!("{name} detected")
}

/// What the result card shows for one code: the payload's main fact as
/// the title, what kind of code it is under it, and what can be done.
struct CardContent {
    title: String,
    detail: String,
    actions: Vec<&'static str>,
}

impl CardContent {
    fn of(code: &ScannedCode) -> Self {
        Self {
            title: code.kind.summary(&code.raw),
            detail: code.kind.description(),
            actions: code.kind.actions(),
        }
    }
}

impl CameraApp {
    pub(super) fn draw_scanner(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
    ) {
        let m = HudMetrics::of(theme);
        let live = self.feed_active();
        let trailing = [
            HudItem {
                label: "",
                icon: Some(Icon::Image),
                name: "Open Photos",
                on: None,
                action: Action::OpenPhotos,
            },
            HudItem {
                label: "",
                icon: Some(Icon::Settings),
                name: "Camera settings",
                on: None,
                action: Action::Nav(Screen::Settings),
            },
        ];
        let code = self.scanned.clone().filter(|_| !self.scan_dismissed);
        let mut layout = Self::scanner_layout(tr, theme, full, code.as_ref());
        let mut code = code;
        if live {
            self.update_scan_state(full, layout.guide);
            let decoded = self.scanned.clone().filter(|_| !self.scan_dismissed);
            if decoded.is_some() != code.is_some() {
                layout = Self::scanner_layout(tr, theme, full, decoded.as_ref());
                self.update_scan_state(full, layout.guide);
                code = self.scanned.clone().filter(|_| !self.scan_dismissed);
            }
        }

        if live {
            self.paint_scan_feed(scene, t, full);
            Self::draw_scan_guide(scene, t, theme, full, layout.guide);
            if code.is_some() {
                self.draw_detected_overlay(scene, t, tr, theme, full);
            }
            let hint = ui::i18n!("Point the camera at a QR code or barcode");
            let local = theme.on_ground(theme.tokens.colors.media.scrim);
            let mut ctx = PaintCtx::new(scene, t, tr, &local);
            let size = ctx.measure_role(&hint, TextRole::Label, None);
            if size.width <= layout.hint.width() {
                ctx.text_role(
                    &hint,
                    TextRole::Label,
                    theme.tokens.colors.media.ink,
                    Point::new(
                        layout.hint.center().x - size.width / 2.0,
                        layout.hint.y1 - size.height,
                    ),
                    None,
                );
            }
        } else {
            p::fill_rect(scene, t, full, theme.tokens.colors.media.letterbox);
            let area = Rect::new(
                full.x0,
                full.y0 + m.edge + m.bar(),
                layout.side.map_or(full.x1, |side| side.x0),
                match (layout.card, layout.side) {
                    (Some(card), None) => card.y0,
                    _ => full.y1 - m.edge - m.bar(),
                },
            );
            self.draw_scanner_offline(scene, t, tr, theme, area);
        }

        let width = cluster_width(&trailing, tr, theme);
        self.draw_cluster(
            scene,
            t,
            tr,
            theme,
            Point::new(full.x1 - m.edge - width, full.y0 + m.edge),
            &trailing,
        );
        if let (Some(code), Some(card)) = (code.as_ref(), layout.card) {
            self.draw_scan_card(scene, t, tr, theme, card, code);
        }
        if let Some(side) = layout.side {
            let top = layout
                .card
                .map_or(side.y0, |card| card.y1 + f64::from(theme.tokens.spacing.s4));
            self.draw_recent_scans(
                scene,
                t,
                tr,
                theme,
                Rect::new(side.x0, top, side.x1, side.y1),
            );
        }
        self.draw_mode_switcher(scene, t, tr, theme, full);
    }

    /// The guide, the hint and the result card for `full`, keeping the
    /// card clear of the guide: beside it on a wide window, under it
    /// otherwise.
    pub(super) fn scanner_layout(
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
        code: Option<&ScannedCode>,
    ) -> ScannerLayout {
        let m = HudMetrics::of(theme);
        let metrics = &theme.tokens.metrics;
        let spacing = &theme.tokens.spacing;
        let group = f64::from(spacing.s6);
        let top = full.y0 + m.edge + m.bar() + m.gap;
        let bottom = full.y1 - m.edge - m.bar() - group;
        // The side column holds text, so it and the width that fits it grow
        // with the text.
        let growth = f64::from(theme.text_growth());
        let column = f64::from(metrics.inspector_width) * growth;
        let wide = full.width() >= f64::from(metrics.compact_breakpoint) * growth + column;
        let side = wide.then(|| {
            let width = column;
            Rect::new(full.x1 - m.edge - width, top, full.x1 - m.edge, bottom)
        });
        let stage_x1 = side.map_or(full.x1, |side| side.x0 - m.edge);
        let card = code.map(|code| {
            let width = side.map_or_else(
                || (full.width() - 2.0 * m.edge).min(f64::from(metrics.panel_width_sheet)),
                |side| side.width(),
            );
            let height = Self::scan_card_height(tr, theme, width, code);
            if let Some(side) = side {
                Rect::new(side.x0, side.y0, side.x1, side.y0 + height)
            } else {
                let x0 = full.center().x - width / 2.0;
                Rect::new(x0, bottom - height, x0 + width, bottom)
            }
        });
        let hint_h = theme.text(TextRole::Label).line_height_px();
        let guide_top = top + f64::from(hint_h) + f64::from(spacing.s3);
        let guide_bottom = match (side, card) {
            (None, Some(card)) => card.y0 - group,
            _ => bottom,
        };
        let stage_w = stage_x1 - full.x0 - 2.0 * group;
        let side_len = (stage_w.min(full.height()) * 0.62)
            .min(guide_bottom - guide_top)
            .min(stage_w)
            .max(0.0);
        let centre = Point::new(
            f64::midpoint(full.x0, stage_x1),
            f64::midpoint(guide_top, guide_bottom),
        );
        let guide = Rect::from_center_size(centre, (side_len, side_len)).round();
        let hint = Rect::new(
            full.x0 + group,
            top,
            stage_x1 - group,
            guide.y0 - f64::from(spacing.s3),
        );
        ScannerLayout {
            side,
            guide,
            card,
            hint,
        }
    }

    /// Refresh the scan feed image and decode result for the window and the
    /// guide in it (only when either changes).
    fn update_scan_state(&mut self, full: Rect, guide: Rect) {
        let vw = full.width().max(2.0) as u32;
        let vh = full.height().max(2.0) as u32;
        match &self.storage {
            Storage::Sample => {
                let centre = guide.center() - full.origin();
                let key = (
                    vw,
                    vh,
                    centre.x.round() as i32,
                    centre.y.round() as i32,
                    guide.width().round() as u32,
                );
                if self.scan_view.as_ref().is_none_or(|(k, _)| *k != key) {
                    let (buf, scanned) = frame::scanner_frame_at(
                        4,
                        vw,
                        vh,
                        SCAN_PAYLOAD,
                        (centre.x as f32, centre.y as f32),
                        (guide.width() * 0.7) as f32,
                    );
                    self.scan_view = Some((key, frame_image_fill(buf)));
                    self.scanned = scanned;
                    self.scan_gen = (vw, vh);
                }
            }
            Storage::Disk(_) => {
                let cur_gen = self.frame_gen;
                if self.live_view.as_ref().is_none_or(|(g, _)| *g != cur_gen)
                    && let Some(fb) = self.latest_frame.clone()
                {
                    let (fw, fh) = (fb.w, fb.h);
                    self.scan_gen = (fw, fh);
                    let scanner_frame = fb.clone();
                    self.live_view = Some((cur_gen, frame_image_cover(fb)));
                    let Some(generation) = self.scan_generation.checked_add(1) else {
                        self.toast = Some("Code scan counter exhausted; restart Camera".into());
                        return;
                    };
                    let id = format!("live:{cur_gen}");
                    match self.worker.submit_scan(crate::worker::ScanJob {
                        generation,
                        id: id.clone(),
                        source: crate::worker::ScanSource::Frame(scanner_frame),
                        record_history: false,
                    }) {
                        Ok(()) => {
                            self.scan_generation = generation;
                            self.pending_scan = Some((id, generation));
                        }
                        // A frame arrives continuously. A single busy decoder
                        // is expected; keep the previous result until the next
                        // frame rather than spamming a toast.
                        Err(error) if error.contains("already running") => {}
                        Err(error) => self.toast = Some(error),
                    }
                }
            }
        }
    }

    fn paint_scan_feed(&mut self, scene: &mut Scene, t: Affine, full: Rect) {
        match &self.storage {
            Storage::Sample => {
                if let Some((_, img)) = self.scan_view.as_mut() {
                    img.paint_at(scene, t, full);
                }
            }
            Storage::Disk(_) => {
                if let Some((_, img)) = self.live_view.as_mut() {
                    img.paint_at(scene, t, full);
                }
            }
        }
    }

    /// Dim everything but the guide square with the toolkit's scrim and mark
    /// its corners in media ink.
    fn draw_scan_guide(scene: &mut Scene, t: Affine, theme: &Theme, full: Rect, guide: Rect) {
        let media = &theme.tokens.colors.media;
        for band in [
            Rect::new(full.x0, full.y0, full.x1, guide.y0),
            Rect::new(full.x0, guide.y1, full.x1, full.y1),
            Rect::new(full.x0, guide.y0, guide.x0, guide.y1),
            Rect::new(guide.x1, guide.y0, full.x1, guide.y1),
        ] {
            ui::overlay::scrim::modal(scene, t, band, 1.0, theme);
        }
        let spacing = &theme.tokens.spacing;
        let arm = f64::from(spacing.s6);
        let width = f64::from(spacing.s1);
        for (sx, sy) in [(-1.0_f64, -1.0_f64), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
            let corner = Point::new(
                if sx < 0.0 { guide.x0 } else { guide.x1 },
                if sy < 0.0 { guide.y0 } else { guide.y1 },
            );
            p::line(
                scene,
                t,
                corner,
                Point::new(corner.x - sx * arm, corner.y),
                media.ink,
                width,
            );
            p::line(
                scene,
                t,
                corner,
                Point::new(corner.x, corner.y - sy * arm),
                media.ink,
                width,
            );
        }
    }

    /// The decoded code's outline in the success colour and a tag naming it,
    /// mapped from decode space onto the picture.
    fn draw_detected_overlay(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
    ) {
        let Some(code) = self.scanned.as_ref() else {
            return;
        };
        let (gw, gh) = self.scan_gen;
        let sx = full.width() / f64::from(gw.max(1));
        let sy = full.height() / f64::from(gh.max(1));
        let points: Vec<Point> = code
            .corners
            .iter()
            .map(|(x, y)| Point::new(full.x0 + f64::from(*x) * sx, full.y0 + f64::from(*y) * sy))
            .collect();
        let success = theme.tokens.colors.semantic.success;
        p::outline(
            scene,
            t,
            &points,
            success,
            f64::from(theme.tokens.spacing.s1) / 2.0,
        );
        let label = detected_label(&code.symbology);
        let width = BadgeFace::intrinsic_width(&label, tr, theme);
        let height = BadgeFace::height(theme);
        let (cx, _) = code.center();
        let centre_x = full.x0 + f64::from(cx) * sx;
        let top = points.iter().map(|q| q.y).fold(f64::MAX, f64::min);
        let gap = f64::from(theme.tokens.spacing.s2);
        let rect = Rect::new(
            centre_x - width / 2.0,
            top - gap - height,
            centre_x + width / 2.0,
            top - gap,
        );
        BadgeFace::new(rect, &label)
            .style(ChipStyle::Tinted(Tone::Success))
            .paint(scene, t, tr, theme);
    }

    /// No camera: scan a code from a saved image instead.
    fn draw_scanner_offline(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let waiting = self.scan_image_picker.is_some();
        let content = EmptyStateContent {
            icon: Some(Icon::Image),
            message: Some("No camera is connected. Choose a saved image that shows a code."),
            action_label: (!waiting).then_some("Choose image"),
            ..EmptyStateContent::new("Scan a code from an image")
        };
        let _ = self.draw_media_empty_state(
            scene,
            t,
            tr,
            theme,
            area,
            &content,
            Action::ImportScanImage,
        );
    }

    /// The height of the result card for `code` at `width`.
    fn scan_card_height(
        tr: &mut TextRenderer,
        theme: &Theme,
        width: f64,
        code: &ScannedCode,
    ) -> f64 {
        let face = CardFace::new();
        let body = face.body_rect(tr, theme, Rect::new(0.0, 0.0, width, 1_000.0));
        let content = CardContent::of(code);
        let body_h = Self::scan_card_body(tr, theme, body.width(), &content).0;
        face.height_for_body(tr, theme, width, body_h)
    }

    /// The height of the card's header row: the code's two-line title block
    /// beside the dismiss button.
    fn scan_card_header(theme: &Theme) -> f64 {
        let lines = theme.text(TextRole::Subheading).line_height_px()
            + theme.text(TextRole::Caption).line_height_px();
        f64::from(lines).max(ButtonFace::height(ui::widget::ControlSize::Regular, theme))
    }

    /// The card body's height at `width`, and its action rects relative to
    /// the body's top-left corner.
    fn scan_card_body(
        tr: &mut TextRenderer,
        theme: &Theme,
        width: f64,
        content: &CardContent,
    ) -> (f64, Vec<Rect>) {
        let spacing = &theme.tokens.spacing;
        let control = ButtonFace::height(ui::widget::ControlSize::Regular, theme);
        let gap = f64::from(spacing.s2);
        let mut y = Self::scan_card_header(theme) + f64::from(spacing.s4);
        let mut x = 0.0;
        let mut rects = Vec::new();
        for (i, label) in content.actions.iter().enumerate() {
            let role = if i == 0 {
                ButtonRole::Primary
            } else {
                ButtonRole::Secondary
            };
            let w = ButtonFace::new(Rect::ZERO, label)
                .role(role)
                .intrinsic_width(tr, theme)
                .min(width);
            if x > 0.0 && x + w > width {
                x = 0.0;
                y += control + gap;
            }
            rects.push(Rect::new(x, y, x + w, y + control));
            x += w + gap;
        }
        (y + control, rects)
    }

    fn draw_scan_card(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        code: &ScannedCode,
    ) {
        let content = CardContent::of(code);
        let face = CardFace::new();
        let body = face.body_rect(tr, theme, card);
        let (_, actions) = Self::scan_card_body(tr, theme, body.width(), &content);
        let control = ButtonFace::height(ui::widget::ControlSize::Regular, theme);
        let spacing = &theme.tokens.spacing;
        let icon_size = f64::from(theme.tokens.metrics.icon_md);
        let colors = theme.tokens.colors.clone();
        {
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            face.paint(&mut ctx, card);
            let header = Rect::new(
                body.x0,
                body.y0,
                body.x1 - control,
                body.y0 + Self::scan_card_header(theme),
            );
            let icon = Rect::from_center_size(
                Point::new(header.x0 + icon_size / 2.0, header.center().y),
                (icon_size, icon_size),
            );
            code_icon(&code.kind).draw(ctx.scene, t, icon, colors.text_dim);
            let text_x = icon.x1 + f64::from(spacing.s3);
            let room = ui::num::f32_of_f64((header.x1 - text_x - f64::from(spacing.s2)).max(1.0));
            let title_style = theme
                .text(TextRole::Subheading)
                .with_kind(ui::TextKind::Identifier);
            let title = ctx
                .text
                .elide_to_width_style(&content.title, room, &title_style);
            let detail_style = theme.text(TextRole::Caption);
            let detail = ctx
                .text
                .elide_to_width_style(&content.detail, room, &detail_style);
            let block = f64::from(title_style.line_height_px() + detail_style.line_height_px());
            let top = header.center().y - block / 2.0;
            ctx.text(
                &title,
                &title_style,
                colors.text_primary,
                Point::new(text_x, top),
                None,
            );
            ctx.text(
                &detail,
                &detail_style,
                colors.text_dim,
                Point::new(text_x, top + f64::from(title_style.line_height_px())),
                None,
            );
        }
        let close = Rect::new(body.x1 - control, body.y0, body.x1, body.y0 + control);
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::icon_only(close, Icon::X, "Dismiss")
                .hovered(close.contains(self.last_mouse)),
        );
        self.tooltip_targets.push((close, "Dismiss".to_owned()));
        self.push_region(close, Action::DismissScan);
        for (i, (label, rect)) in content.actions.iter().zip(actions).enumerate() {
            let rect = rect + body.origin().to_vec2();
            let role = if i == 0 {
                ButtonRole::Primary
            } else {
                ButtonRole::Secondary
            };
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &ButtonFace::new(rect, label)
                    .role(role)
                    .hovered(rect.contains(self.last_mouse)),
            );
            self.push_region(rect, Action::Scan(i));
        }
    }

    /// The codes scanned most recently, newest first, on a floating panel in
    /// the side column.
    fn draw_recent_scans(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        column: Rect,
    ) {
        let spacing = &theme.tokens.spacing;
        let pad = f64::from(spacing.s2);
        let local = on_panel(theme);
        let entries = self.scan_history();
        let now = super::now_secs();
        let heading = ui::i18n!("Recent scans");
        let heading_h = f64::from(local.text(TextRole::Subheading).line_height_px());
        let header = heading_h + 2.0 * f64::from(spacing.s3);
        // Measure the rows that fit, then size the panel to them.
        let mut rows: Vec<(String, String, String, Icon, f64)> = Vec::new();
        let mut height = header;
        for entry in entries.iter().take(8) {
            let kind = classify(&entry.raw);
            let title = kind.summary(&entry.raw);
            let secondary = kind.title().to_owned();
            let meta = ui::format::relative(now - entry.ts);
            let row_h = ListRowFace::new(&title)
                .secondary(&secondary)
                .meta(&meta)
                .leading(RowLeading::Icon(code_icon(&kind)))
                .height(tr, &local);
            if height + row_h + pad > column.height() {
                break;
            }
            height += row_h;
            rows.push((title, secondary, meta, code_icon(&kind), row_h));
        }
        let empty = rows.is_empty();
        if empty {
            height = header + f64::from(theme.tokens.spacing.s16) * 3.0;
        }
        let panel = Rect::new(
            column.x0,
            column.y0,
            column.x1,
            column.y0 + (height + pad).min(column.height()),
        );
        if panel.height() <= header {
            return;
        }
        floating_surface::paint(scene, t, panel, theme);
        let mut ctx = PaintCtx::new(scene, t, tr, &local);
        let inset = f64::from(spacing.s4);
        ctx.text_role(
            &heading,
            TextRole::Subheading,
            local.text_primary,
            Point::new(panel.x0 + inset, panel.y0 + f64::from(spacing.s3)),
            None,
        );
        if empty {
            let area = Rect::new(panel.x0, panel.y0 + header, panel.x1, panel.y1 - pad);
            let content = EmptyStateContent {
                message: Some("Codes you scan appear here."),
                ..EmptyStateContent::new("No scans yet")
            };
            let _ = EmptyState::paint(&mut ctx, area, &content, Point::new(-1.0, -1.0));
            return;
        }
        let mut y = panel.y0 + header;
        let count = rows.len();
        for (i, (title, secondary, meta, icon, row_h)) in rows.iter().enumerate() {
            let face = ListRowFace::new(title)
                .secondary(secondary)
                .meta(meta)
                .leading(RowLeading::Icon(*icon));
            let face = if i + 1 < count {
                face.separator(false)
            } else {
                face
            };
            let rect = Rect::new(panel.x0 + pad, y, panel.x1 - pad, y + row_h);
            face.paint(&mut ctx, rect);
            y += row_h;
        }
    }

    /// The scan history to show. The hermetic sample shows a realistic
    /// history until a code is scanned.
    fn scan_history(&self) -> Vec<ScanEntry> {
        if !self.scans.is_empty() || !matches!(self.storage, Storage::Sample) {
            return self.scans.clone();
        }
        let now = super::now_secs();
        vec![
            ScanEntry {
                raw: "https://tilo.so/docs/getting-started".into(),
                ts: now - 4 * 60,
            },
            ScanEntry {
                raw: "BEGIN:VCARD\nVERSION:3.0\nFN:Maya Okafor\nTEL:+44 20 7946 0321\nEND:VCARD"
                    .into(),
                ts: now - 2 * 60 * 60,
            },
            ScanEntry {
                raw: "geo:51.5194,-0.1270".into(),
                ts: now - 26 * 60 * 60,
            },
            ScanEntry {
                raw: "WIFI:T:WPA2;S:Harbour Café Guest;P:flatwhite;H:false;;".into(),
                ts: now - 3 * 24 * 60 * 60,
            },
        ]
    }
}
