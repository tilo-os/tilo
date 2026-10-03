//! Chrome over the viewfinder, and the pieces every screen shares: control
//! clusters on the floating material, the mode switcher, the shutter, the
//! tooltip and the toast.
//!
//! The viewfinder is media: what is drawn straight onto it (guides, the fade
//! under the controls) takes the theme's media colours, and every control
//! stands on a floating panel inked from the theme (`docs/ui-migration.md`,
//! "Scrims and media").

use ui::TextRole;
use ui::chrome::{SelectionState, paint_row_selection, panel_ground};
use ui::icons::Icon;
use ui::overlay::scrim::{FadeEdge, vertical_fade};
use ui::paint::PaintCtx;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::tooltip::draw_hover_tooltip;
use ui::widget::{
    BadgeFace, Button, ButtonFace, ButtonRole, ChipStyle, ControlSize, EmptyState,
    EmptyStateContent, SegmentedFace, SegmentedStyle, ToggleGroup, floating_surface,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::{Action, CameraApp, CameraIssue, Storage, frame_image_cover, frame_image_fill};
use crate::frame;
use crate::model::{CaptureMode, Screen, Timer};
use crate::paint as p;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

/// Geometry every floating control shares, read from the theme.
#[derive(Clone, Copy, Debug)]
pub(super) struct HudMetrics {
    /// Inset of floating chrome from the window edge.
    pub edge: f64,
    /// Inset of controls inside their panel.
    pub pad: f64,
    /// Height of one control.
    pub control: f64,
    /// Between two clusters.
    pub gap: f64,
}

impl HudMetrics {
    pub(super) fn of(theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        Self {
            edge: f64::from(spacing.s4),
            pad: f64::from(spacing.s1),
            control: ButtonFace::height(ControlSize::Regular, theme),
            gap: f64::from(spacing.s2),
        }
    }

    /// Height of a panel holding one row of controls.
    pub(super) fn bar(&self) -> f64 {
        self.control + 2.0 * self.pad
    }
}

/// One control in a floating cluster.
#[derive(Clone, Debug)]
pub(super) struct HudItem<'a> {
    /// Visible label; empty for an icon-only control.
    pub label: &'a str,
    pub icon: Option<Icon>,
    /// The accessible name and tooltip of an icon-only control.
    pub name: &'a str,
    /// `Some` for a control that is on or off.
    pub on: Option<bool>,
    pub action: Action,
}

impl HudItem<'_> {
    fn face(&self, rect: Rect) -> ButtonFace<'_> {
        let face = match (self.label.is_empty(), self.icon) {
            (true, Some(icon)) => ButtonFace::icon_only(rect, icon, self.name),
            (_, Some(icon)) => ButtonFace::new(rect, self.label).icon(icon),
            (_, None) => ButtonFace::new(rect, self.label),
        };
        face.role(ButtonRole::Plain)
    }

    fn width(&self, tr: &mut TextRenderer, theme: &Theme) -> f64 {
        let m = HudMetrics::of(theme);
        if self.label.is_empty() {
            m.control
        } else {
            self.face(Rect::ZERO)
                .intrinsic_width(tr, theme)
                .max(m.control)
        }
    }
}

/// The theme for what stands on a floating panel.
pub(super) fn on_panel(theme: &Theme) -> Theme {
    theme.on_ground(panel_ground(theme))
}

/// The width a cluster of `items` takes, panel padding included.
pub(super) fn cluster_width(items: &[HudItem<'_>], tr: &mut TextRenderer, theme: &Theme) -> f64 {
    let m = HudMetrics::of(theme);
    items.iter().map(|item| item.width(tr, theme)).sum::<f64>() + 2.0 * m.pad
}

impl CameraApp {
    /// Paint `items` as one floating cluster whose top-left corner is
    /// `origin`, register each control's region, and return the panel.
    pub(super) fn draw_cluster(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        origin: Point,
        items: &[HudItem<'_>],
    ) -> Rect {
        let m = HudMetrics::of(theme);
        let width = cluster_width(items, tr, theme);
        let panel = Rect::new(origin.x, origin.y, origin.x + width, origin.y + m.bar());
        floating_surface::paint(scene, t, panel, theme);
        let local = on_panel(theme);
        let ground = local.tokens.colors.surfaces.content;
        let mut x = panel.x0 + m.pad;
        for item in items {
            let w = item.width(tr, theme);
            let rect = Rect::new(x, panel.y0 + m.pad, x + w, panel.y0 + m.pad + m.control);
            let on = item.on == Some(true);
            if on {
                paint_row_selection(scene, t, rect, SelectionState::active(true), ground, &local);
            }
            let hovered = rect.contains(self.last_mouse) && !on;
            Button::paint(scene, t, tr, &local, &item.face(rect).hovered(hovered));
            if item.label.is_empty() {
                self.tooltip_targets.push((rect, item.name.to_owned()));
            }
            self.push_region(rect, item.action.clone());
            x += w;
        }
        panel
    }

    /// Photo, Video, Codes and Documents: the switcher docked at the bottom
    /// centre of every screen. Returns its panel.
    pub(super) fn draw_mode_switcher(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
    ) -> Rect {
        let m = HudMetrics::of(theme);
        let local = on_panel(theme);
        let entries = [
            ("Photo", Action::SetMode(CaptureMode::Photo)),
            ("Video", Action::SetMode(CaptureMode::Video)),
            ("Codes", Action::Nav(Screen::Scanner)),
            ("Documents", Action::Nav(Screen::Document)),
        ];
        let selected = match self.base_screen() {
            Screen::Camera | Screen::Settings => usize::from(self.mode == CaptureMode::Video),
            Screen::Scanner => 2,
            Screen::Document => 3,
        };
        let room = (full.width() - 2.0 * (m.edge + m.pad)).max(1.0);
        let mut labels: Vec<&str> = entries.iter().map(|(label, _)| *label).collect();
        let mut natural = SegmentedFace::intrinsic_width(&labels, ControlSize::Regular, tr, &local);
        if natural > room {
            // The short form of the one long label, before anything elides.
            if let Some(documents) = labels.last_mut() {
                *documents = "Docs";
            }
            natural = SegmentedFace::intrinsic_width(&labels, ControlSize::Regular, tr, &local);
        }
        let width = natural.min(room);
        let height = SegmentedFace::height(ControlSize::Regular, theme);
        let x0 = full.center().x - width / 2.0;
        let y1 = full.y1 - m.edge - m.pad;
        let segment = width / labels.len() as f64;
        let segments: Vec<Rect> = (0..labels.len())
            .map(|i| {
                let left = x0 + segment * i as f64;
                Rect::new(left, y1 - height, left + segment, y1)
            })
            .collect();
        let strip = Rect::new(x0, y1 - height, x0 + width, y1);
        let panel = strip.inflate(m.pad, m.pad);
        floating_surface::paint(scene, t, panel, theme);
        let hovered = segments
            .iter()
            .position(|seg| seg.contains(self.last_mouse));
        ToggleGroup::paint(
            scene,
            t,
            tr,
            &local,
            &SegmentedFace::new(&segments, &labels, selected)
                .style(SegmentedStyle::Quiet)
                .hovered(hovered),
        );
        for (rect, (_, action)) in segments.into_iter().zip(entries) {
            self.push_region(rect, action);
        }
        panel
    }

    /// The shutter: a ring in media ink around a disc (photo), a red dot
    /// (video) or a red square (recording). It is the one capture control
    /// the toolkit has no widget for, so its parts are painted here from the
    /// theme's media and status colours.
    fn draw_shutter(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        center: Point,
        live: bool,
    ) -> Rect {
        let colors = &theme.tokens.colors;
        let spacing = &theme.tokens.spacing;
        let radius = f64::from(theme.tokens.metrics.min_pointer_target);
        let ring = f64::from(spacing.s1);
        let recording = self.is_recording();
        let ink = if live {
            colors.media.ink
        } else {
            colors.media.ink_dim
        };
        let danger = colors.semantic.danger;
        p::circle_stroke(
            scene,
            t,
            center,
            radius - ring / 2.0,
            if recording { danger } else { ink },
            ring,
        );
        let core = radius - ring - f64::from(spacing.s1);
        match self.mode {
            CaptureMode::Video if recording => {
                let half = core / 2.0;
                p::fill_round(
                    scene,
                    t,
                    Rect::new(
                        center.x - half,
                        center.y - half,
                        center.x + half,
                        center.y + half,
                    ),
                    f64::from(theme.tokens.radii.detail),
                    danger,
                );
            }
            CaptureMode::Video => p::circle(scene, t, center, core, danger),
            CaptureMode::Photo => p::circle(scene, t, center, core, ink),
        }
        let hit = Rect::new(
            center.x - radius,
            center.y - radius,
            center.x + radius,
            center.y + radius,
        );
        self.push_region(hit, Action::Shutter);
        hit
    }

    pub(super) fn draw_camera(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
    ) {
        let m = HudMetrics::of(theme);
        let live = self.feed_active();
        if live {
            self.paint_viewfinder(scene, t, full);
            self.draw_viewfinder_guides(scene, t, theme, full);
            // One smooth fade under each control band keeps the controls'
            // edges and the guides legible over a bright scene.
            let top = m.edge + m.bar() + f64::from(theme.tokens.spacing.s6);
            vertical_fade(
                scene,
                t,
                Rect::new(full.x0, full.y0, full.x1, full.y0 + top),
                FadeEdge::Top,
                0.45,
                theme,
            );
            let bottom =
                3.0 * m.edge + m.bar() + 4.0 * f64::from(theme.tokens.metrics.min_pointer_target);
            vertical_fade(
                scene,
                t,
                Rect::new(full.x0, full.y1 - bottom, full.x1, full.y1),
                FadeEdge::Bottom,
                0.6,
                theme,
            );
        } else {
            p::fill_rect(scene, t, full, theme.tokens.colors.media.letterbox);
        }

        let top = self.draw_camera_top_controls(scene, t, tr, theme, full);
        let switcher = if self.is_recording() {
            None
        } else {
            Some(self.draw_mode_switcher(scene, t, tr, theme, full))
        };
        let radius = f64::from(theme.tokens.metrics.min_pointer_target);
        let shutter_bottom = switcher.map_or(full.y1 - m.edge, |panel| {
            panel.y0 - f64::from(theme.tokens.spacing.s4)
        });
        let center = Point::new(full.center().x, shutter_bottom - radius);
        if !live {
            let area = Rect::new(
                full.x0,
                top.y1,
                full.x1,
                center.y - radius - f64::from(theme.tokens.spacing.s4),
            );
            self.draw_no_feed(scene, t, tr, theme, area);
        }
        let shutter = self.draw_shutter(scene, t, theme, center, live);
        self.draw_recent_capture(scene, t, tr, theme, full, shutter);
        self.draw_lens_switch(scene, t, tr, theme, full, shutter);
        if self.is_recording() {
            self.draw_recording_badge(scene, t, tr, theme, full, top);
        } else if self.is_counting_down() {
            self.draw_countdown(scene, t, tr, theme, full, top);
        }
    }

    fn paint_viewfinder(&mut self, scene: &mut Scene, t: Affine, full: Rect) {
        let vw = full.width().max(2.0) as u32;
        let vh = full.height().max(2.0) as u32;
        match &self.storage {
            Storage::Sample => {
                let seed = Self::viewfinder_seed(self.facing);
                let stale = self
                    .cam_view
                    .as_ref()
                    .is_none_or(|(w, h, s, _)| (*w, *h, *s) != (vw, vh, seed));
                if stale {
                    self.cam_view = Some((
                        vw,
                        vh,
                        seed,
                        frame_image_fill(frame::viewfinder(seed, vw, vh)),
                    ));
                }
                if let Some((_, _, _, img)) = self.cam_view.as_mut() {
                    img.paint_at(scene, t, full);
                }
            }
            Storage::Disk(_) => {
                let generation = self.frame_gen;
                if self
                    .live_view
                    .as_ref()
                    .is_none_or(|(g, _)| *g != generation)
                    && let Some(fb) = self.latest_frame.clone()
                {
                    self.live_view = Some((generation, frame_image_cover(fb)));
                }
                if let Some((_, img)) = self.live_view.as_mut() {
                    img.paint_at(scene, t, full);
                }
            }
        }
    }

    /// The rule-of-thirds grid and the level guide, drawn on the picture in
    /// media ink.
    fn draw_viewfinder_guides(&self, scene: &mut Scene, t: Affine, theme: &Theme, vf: Rect) {
        let media = &theme.tokens.colors.media;
        let hairline = f64::from(theme.tokens.metrics.chrome_rule_px);
        if self.settings.grid {
            let guide = media.ink_dim;
            for k in 1..3 {
                let x = vf.x0 + vf.width() * f64::from(k) / 3.0;
                p::line(
                    scene,
                    t,
                    Point::new(x, vf.y0),
                    Point::new(x, vf.y1),
                    guide,
                    hairline,
                );
                let y = vf.y0 + vf.height() * f64::from(k) / 3.0;
                p::line(
                    scene,
                    t,
                    Point::new(vf.x0, y),
                    Point::new(vf.x1, y),
                    guide,
                    hairline,
                );
            }
        }
        if self.settings.level {
            let c = vf.center();
            let spacing = &theme.tokens.spacing;
            let reach = f64::from(spacing.s16);
            let gap = f64::from(spacing.s2);
            let tick = f64::from(spacing.s6);
            let width = f64::from(spacing.s0.max(2.0));
            p::line(
                scene,
                t,
                Point::new(c.x - reach, c.y),
                Point::new(c.x + reach, c.y),
                media.ink,
                width,
            );
            for side in [-1.0_f64, 1.0] {
                p::line(
                    scene,
                    t,
                    Point::new(c.x + side * (reach + gap), c.y),
                    Point::new(c.x + side * (reach + gap + tick), c.y),
                    media.ink_dim,
                    width,
                );
            }
        }
    }

    /// The quick capture settings at the top-leading corner and the Photos
    /// and settings buttons at the top-trailing corner. Returns the band the
    /// two clusters occupy.
    fn draw_camera_top_controls(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
    ) -> Rect {
        let m = HudMetrics::of(theme);
        let timer_label = match self.settings.timer {
            Timer::Off => "",
            Timer::S3 => "3 s",
            Timer::S10 => "10 s",
        };
        let next_timer = super::cycle(&Timer::ALL, self.settings.timer);
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
        let mut leading = vec![
            HudItem {
                label: self.settings.resolution.label(),
                icon: None,
                name: "Resolution",
                on: None,
                action: Action::CycleResolution,
            },
            HudItem {
                label: "HDR",
                icon: None,
                name: "HDR",
                on: Some(self.settings.hdr),
                action: Action::ToggleHdr,
            },
            HudItem {
                label: timer_label,
                icon: Some(Icon::Timer),
                name: "Self-timer",
                on: Some(self.settings.timer != Timer::Off),
                action: Action::SetTimer(next_timer),
            },
            HudItem {
                label: "",
                icon: Some(Icon::LayoutGrid),
                name: "Rule-of-thirds grid",
                on: Some(self.settings.grid),
                action: Action::ToggleGrid,
            },
        ];
        // Controls that do not fit beside the trailing cluster stay in the
        // settings sheet: first the grid, then HDR.
        let room = full.width() - 2.0 * m.edge - m.gap - cluster_width(&trailing, tr, theme);
        for drop in [Action::ToggleGrid, Action::ToggleHdr] {
            if cluster_width(&leading, tr, theme) <= room {
                break;
            }
            leading.retain(|item| {
                std::mem::discriminant(&item.action) != std::mem::discriminant(&drop)
            });
        }
        let origin = Point::new(full.x0 + m.edge, full.y0 + m.edge);
        let lead = self.draw_cluster(scene, t, tr, theme, origin, &leading);
        let trailing_w = cluster_width(&trailing, tr, theme);
        let trail = self.draw_cluster(
            scene,
            t,
            tr,
            theme,
            Point::new(full.x1 - m.edge - trailing_w, full.y0 + m.edge),
            &trailing,
        );
        lead.union(trail)
    }

    /// The latest capture, bottom-leading: its preview opens it in the app
    /// that owns it (Photos or Video Player).
    fn draw_recent_capture(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
        shutter: Rect,
    ) {
        let Some(capture) = self.captures.first().cloned() else {
            return;
        };
        let m = HudMetrics::of(theme);
        let side = 2.0 * m.control - 2.0 * m.pad;
        let height = side + 2.0 * m.pad;
        let (_, destination) = crate::actions::capture_destination(capture.is_video());
        let label = format!("Open in {destination}");
        let label_w = ButtonFace::new(Rect::ZERO, &label)
            .role(ButtonRole::Plain)
            .intrinsic_width(tr, theme);
        let limit = shutter.x0 - f64::from(theme.tokens.spacing.s4);
        let x0 = full.x0 + m.edge;
        let with_label = x0 + side + label_w + 2.0 * m.pad <= limit;
        let width = side + 2.0 * m.pad + if with_label { label_w } else { 0.0 };
        let panel = Rect::new(
            x0,
            shutter.center().y - height / 2.0,
            x0 + width,
            shutter.center().y + height / 2.0,
        );
        floating_surface::paint(scene, t, panel, theme);
        let preview = Rect::new(
            panel.x0 + m.pad,
            panel.y0 + m.pad,
            panel.x0 + m.pad + side,
            panel.y1 - m.pad,
        );
        self.ensure_thumb(&capture);
        let radius = f64::from(
            theme
                .tokens
                .radii
                .concentric(theme.tokens.radii.panel, theme.tokens.spacing.s1),
        );
        p::push_clip_round(scene, t, preview, radius);
        if let Some(img) = self.thumbs.get_mut(&capture.id) {
            img.paint_at(scene, t, preview);
        }
        p::pop(scene);
        if with_label {
            let local = on_panel(theme);
            let rect = Rect::new(
                preview.x1,
                panel.center().y - m.control / 2.0,
                preview.x1 + label_w,
                panel.center().y + m.control / 2.0,
            );
            let hovered = panel.contains(self.last_mouse);
            Button::paint(
                scene,
                t,
                tr,
                &local,
                &ButtonFace::new(rect, &label)
                    .role(ButtonRole::Plain)
                    .hovered(hovered),
            );
        } else {
            self.tooltip_targets.push((panel, label));
        }
        self.push_region(panel, Action::OpenRecentCapture);
    }

    /// Switch camera, bottom-trailing: names the camera in use.
    fn draw_lens_switch(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
        shutter: Rect,
    ) {
        let m = HudMetrics::of(theme);
        let labelled = [HudItem {
            label: self.facing.label(),
            icon: Some(Icon::Refresh),
            name: "Switch camera",
            on: None,
            action: Action::CycleFacing,
        }];
        let icon_only = [HudItem {
            label: "",
            ..labelled[0].clone()
        }];
        let limit = shutter.x1 + f64::from(theme.tokens.spacing.s4);
        let width = cluster_width(&labelled, tr, theme);
        let items: &[HudItem<'_>] = if full.x1 - m.edge - width >= limit {
            &labelled
        } else {
            &icon_only
        };
        let width = cluster_width(items, tr, theme);
        let origin = Point::new(full.x1 - m.edge - width, shutter.center().y - m.bar() / 2.0);
        self.draw_cluster(scene, t, tr, theme, origin, items);
    }

    /// "Recording" and the elapsed time, top centre, while a clip records.
    fn draw_recording_badge(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
        top: Rect,
    ) {
        let started = self.recording.as_ref().map_or(0, |s| s.started);
        let elapsed = (super::now_secs() - started).max(0);
        let label = format!("Recording {}", super::fmt_dur(elapsed * 1000));
        let width = BadgeFace::intrinsic_width(&label, tr, theme);
        let height = BadgeFace::height(theme);
        let rect = self.top_centre(full, top, width, height, theme);
        BadgeFace::new(rect, &label)
            .style(ChipStyle::Live)
            .paint(scene, t, tr, theme);
    }

    /// The running self-timer, top centre: how long is left, and how to stop it.
    fn draw_countdown(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
        top: Rect,
    ) {
        let Some(countdown) = self.countdown.as_ref() else {
            return;
        };
        let duration = countdown.due.saturating_duration_since(ui::time::now());
        let remaining = (duration.as_secs() + u64::from(duration.subsec_nanos() != 0)).max(1);
        let count = remaining.to_string();
        let hint = ui::i18n!("Tap the shutter to cancel");
        let local = on_panel(theme);
        let m = HudMetrics::of(theme);
        let gap = f64::from(theme.tokens.spacing.s2);
        let mut ctx = PaintCtx::new(scene, t, tr, &local);
        let number_style = local.text(TextRole::Title).numeric();
        let (nw, nh) = ctx.text.measure_style(&count, None, &number_style);
        let hint_size = ctx.measure_role(&hint, TextRole::Label, None);
        let pad_x = f64::from(theme.tokens.spacing.s3);
        let width = f64::from(nw) + gap + hint_size.width + 2.0 * pad_x;
        let height = f64::from(nh).max(m.bar());
        let rect = self.top_centre(full, top, width, height, theme);
        floating_surface::paint(ctx.scene, t, rect, theme);
        let number_top = rect.center().y - f64::from(nh) / 2.0;
        ctx.text(
            &count,
            &number_style,
            local.text_primary,
            Point::new(rect.x0 + pad_x, number_top),
            None,
        );
        ctx.text_role_in_band(
            &hint,
            TextRole::Label,
            local.text_dim,
            rect.x0 + pad_x + f64::from(nw) + gap,
            rect,
            None,
        );
    }

    /// A `width` × `height` rect centred at the top of the window, beside the
    /// corner clusters when it fits between them, under them otherwise.
    fn top_centre(&self, full: Rect, top: Rect, width: f64, height: f64, theme: &Theme) -> Rect {
        let m = HudMetrics::of(theme);
        let x0 = full.center().x - width / 2.0;
        let beside = Rect::new(
            x0,
            top.center().y - height / 2.0,
            x0 + width,
            top.center().y + height / 2.0,
        );
        let clear = self
            .regions
            .iter()
            .all(|(rect, _)| rect.intersect(beside.inflate(m.gap, 0.0)).area() <= 0.0);
        if clear {
            beside
        } else {
            let y0 = top.y1 + m.gap;
            Rect::new(x0, y0, x0 + width, y0 + height)
        }
    }

    /// No live picture: the letterbox with what happened and the one action
    /// that helps.
    fn draw_no_feed(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let (title, message) = self.no_feed_copy();
        let (label, action) = self.recovery_action();
        let content = EmptyStateContent {
            icon: Some(Icon::Camera),
            message: Some(message),
            action_label: Some(label),
            ..EmptyStateContent::new(title)
        };
        if let Some(button) =
            self.draw_media_empty_state(scene, t, tr, theme, area, &content, action.clone())
            && matches!(action, Action::OpenCameraChooser)
        {
            self.chooser_anchor = button;
        }
    }

    /// An empty or error state straight on the letterbox: its text in media
    /// ink, and its one action on the floating material where the empty
    /// state places it. Returns the action's rect.
    pub(super) fn draw_media_empty_state(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        content: &EmptyStateContent<'_>,
        action: Action,
    ) -> Option<Rect> {
        let ground = theme.on_ground(theme.tokens.colors.media.letterbox);
        // Where the stack puts its action, from the empty state itself.
        let mut probe = Scene::new();
        let slot = EmptyState::paint(
            &mut PaintCtx::new(&mut probe, t, tr, &ground),
            area,
            content,
            Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY),
        );
        let Some((label, slot)) = content.action_label.zip(slot) else {
            let _ = EmptyState::paint(
                &mut PaintCtx::new(scene, t, tr, &ground),
                area,
                content,
                self.last_mouse,
            );
            return None;
        };
        // The text without its action, placed exactly as with it.
        let text_area = Rect::new(
            area.x0,
            area.y0,
            area.x1,
            area.y1 - slot.height() - f64::from(theme.tokens.spacing.s4),
        );
        let text_only = EmptyStateContent {
            action_label: None,
            ..*content
        };
        let _ = EmptyState::paint(
            &mut PaintCtx::new(scene, t, tr, &ground),
            text_area,
            &text_only,
            self.last_mouse,
        );
        let item = [HudItem {
            label,
            icon: None,
            name: label,
            on: None,
            action,
        }];
        let m = HudMetrics::of(theme);
        let width = cluster_width(&item, tr, theme);
        let panel = self.draw_cluster(
            scene,
            t,
            tr,
            theme,
            Point::new(
                slot.center().x - width / 2.0,
                slot.center().y - m.bar() / 2.0,
            ),
            &item,
        );
        Some(panel)
    }

    /// The one recovery a no-feed state offers.
    pub(super) fn recovery_action(&self) -> (&'static str, Action) {
        if self.camera_issue == Some(CameraIssue::PermissionDenied) {
            ("Open privacy settings", Action::OpenCameraPrivacySettings)
        } else if self.camera_issue == Some(CameraIssue::StartFailed)
            && self.available_camera_devices().len() > 1
        {
            ("Choose camera", Action::OpenCameraChooser)
        } else {
            ("Refresh", Action::RetryCamera)
        }
    }

    /// The hover tooltip of an icon-only control, once the pointer has rested
    /// on it.
    pub(super) fn draw_tooltip(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let target = self
            .tooltip_targets
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(self.last_mouse))
            .cloned();
        self.tooltip.on_pointer(target, ui::time::now());
        if let Some((label, anchor)) = self.tooltip.visible() {
            draw_hover_tooltip(scene, t, tr, theme, anchor, label, self.rect);
        }
    }

    /// The transient result of the last action, above the bottom controls.
    pub(super) fn draw_toast(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let Some(msg) = self.toast.as_deref() else {
            return;
        };
        // Device and file failures never read as successes.
        let kind = if msg.starts_with("Save failed")
            || msg.starts_with("Couldn't")
            || msg.starts_with("Could not")
            || msg.starts_with("Text recognition")
            || msg.starts_with("Text could not")
        {
            ToastKind::Error
        } else if msg.starts_with("Connect a camera")
            || msg.starts_with("Nothing recorded")
            || msg.starts_with("No code found")
        {
            ToastKind::Warning
        } else {
            ToastKind::Success
        };
        paint_transient(
            scene,
            t,
            tr,
            theme,
            area,
            ToastAnchor::BottomCenter,
            &ToastCard::new(msg, kind),
        );
    }

    /// The recent-capture preview, decoded once per capture.
    pub(super) fn ensure_thumb(&mut self, cap: &crate::model::Capture) {
        if self.thumbs.contains_key(&cap.id) && !self.thumb_pending.contains_key(&cap.id) {
            return;
        }
        if self.thumb_pending.contains_key(&cap.id) || self.thumb_failed.contains(&cap.id) {
            return;
        }
        let (w, h) = super::THUMB;
        if matches!(self.storage, Storage::Sample) {
            if self.thumbs.len() >= super::MAX_THUMB_CACHE {
                self.thumbs.clear();
            }
            self.thumbs.insert(
                cap.id.clone(),
                frame_image_cover(frame::thumbnail(cap.scene, w, h)),
            );
            return;
        }
        let size = thumbnail::ThumbSize::new(w, h);
        // `media_id` names verified bytes in the content store, and is the
        // preview's content address; a local path is admitted only for the
        // recent capture projection.
        let media = cap
            .media_id
            .as_ref()
            .filter(|media_id| crate::media::is_media_id(media_id))
            .cloned();
        let address_key = media.as_deref().and_then(|id| {
            thumbnail::ThumbKey::of_address(id.strip_prefix(crate::media::MEDIA_PREFIX)?, size)
        });
        let request = match (&cap.path, media) {
            (Some(path), _) => {
                let key = address_key.or_else(|| {
                    let meta = crate::files::metadata(path)?;
                    let mtime_ms = meta
                        .modified
                        .duration_since(ui::time::UNIX_EPOCH)
                        .ok()?
                        .as_millis();
                    Some(thumbnail::ThumbKey::of_path_meta(
                        path,
                        u64::try_from(mtime_ms).unwrap_or(u64::MAX),
                        meta.len,
                        size,
                    ))
                });
                key.map(|key| {
                    thumbnail::ThumbRequest::file(
                        key,
                        thumbnail::category_for_path(path),
                        path.clone(),
                    )
                })
            }
            (None, Some(id)) => address_key.map(|key| {
                let content = self.content.clone();
                thumbnail::ThumbRequest::load(key, thumbnail::FileCategory::Image, move || {
                    crate::media::read(&content, &id)
                })
            }),
            (None, None) => None,
        };
        let Some(request) = request else {
            self.thumb_failed.insert(cap.id.clone());
            return;
        };
        let key = request.key();
        if let thumbnail::ThumbState::Badge(_) = self.thumb_service.request(request) {
            self.thumb_failed.insert(cap.id.clone());
            return;
        }
        self.thumb_pending.insert(cap.id.clone(), key);
        // A low-cost deterministic placeholder means the preview never
        // flashes blank while the shared pipeline is working.
        if self.thumbs.len() >= super::MAX_THUMB_CACHE {
            self.thumbs.clear();
        }
        self.thumbs.insert(
            cap.id.clone(),
            frame_image_cover(frame::thumbnail(cap.scene, w, h)),
        );
    }
}
