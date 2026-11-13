//! The capture bar: its items, their places and their paint.

use super::receipt::ReceiptAction;
use super::{
    Button, ButtonFace, CaptureButtonFace, CaptureKind, CaptureMode, CaptureStudio, ControlSize,
    Icon, IconButton, OUTPUTS, PaintCtx, PipLayout, Point, RecordingCommand, Rect, ReplayCommand,
    Segment, SegmentedFace, SegmentedStyle, SourceMode, TARGETS, TextRenderer, TextRole, Theme,
    overlap,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BarItem {
    Output(usize),
    Target(usize),
    Shutter,
    Options,
    Time,
    Skipped,
    /// Save the kept last 30 seconds.
    Save,
}

/// The bar's measures in the theme it was last painted with. Render caches
/// only; before the first paint, the stock theme's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct BarMetrics {
    pad: f64,
    gap: f64,
    margin: f64,
    radius: f64,
    strip_h: f64,
    output_w: f64,
    target_w: f64,
    shutter: f64,
    icon_button: f64,
    time_w: f64,
    skipped_w: f64,
    save_w: f64,
}

impl Default for BarMetrics {
    fn default() -> Self {
        let spacing = crate::tokens::Spacing::BASE;
        let metrics = crate::tokens::Metrics::BASE;
        Self {
            pad: f64::from(spacing.s2),
            gap: f64::from(spacing.s4),
            margin: f64::from(spacing.s6),
            radius: f64::from(crate::tokens::Radii::default().panel),
            strip_h: 56.0,
            output_w: 96.0,
            target_w: 80.0,
            shutter: f64::from(metrics.min_pointer_target) * 2.0,
            icon_button: f64::from(metrics.min_pointer_target),
            time_w: 64.0,
            skipped_w: 52.0,
            save_w: 150.0,
        }
    }
}

pub(super) fn studio_text(key: &str) -> String {
    crate::i18n::widget_format(key, &[])
}

pub(super) fn skipped_sentence(skipped: u64) -> String {
    crate::i18n::widget_format("widget-recording-skipped", &[("count", skipped.into())])
}

pub(super) fn failed_sentence(reason: &str, kept: bool) -> String {
    let key = if kept {
        "widget-recording-incomplete"
    } else {
        "widget-recording-not-saved"
    };
    crate::i18n::widget_format(key, &[("reason", reason.into())])
}

pub(super) fn output_key(mode: CaptureMode) -> &'static str {
    match mode {
        CaptureMode::Screenshot => "widget-capture-mode-screenshot",
        CaptureMode::Recording => "widget-capture-mode-recording",
        CaptureMode::Pip => "widget-capture-mode-pip",
    }
}

pub(super) fn output_icon(mode: CaptureMode) -> Icon {
    match mode {
        CaptureMode::Screenshot => Icon::Camera,
        CaptureMode::Recording => Icon::CircleDot,
        CaptureMode::Pip => Icon::PictureInPicture2,
    }
}

pub(super) fn output_name(mode: CaptureMode) -> String {
    match mode {
        CaptureMode::Pip => studio_text("widget-capture-mode-pip-name"),
        mode => studio_text(output_key(mode)),
    }
}

pub(super) fn target_key(source: SourceMode) -> &'static str {
    match source {
        SourceMode::Region => "widget-capture-source-region",
        SourceMode::Window => "widget-capture-source-window",
        SourceMode::Display => "widget-capture-source-display",
        SourceMode::Strip => "widget-capture-source-strip",
    }
}

pub(super) fn target_icon(source: SourceMode) -> Icon {
    match source {
        SourceMode::Region => Icon::SquareDashed,
        SourceMode::Window => Icon::AppWindow,
        SourceMode::Display => Icon::Monitor,
        SourceMode::Strip => Icon::GalleryHorizontal,
    }
}

pub(super) fn output_segments() -> Vec<Segment> {
    OUTPUTS
        .iter()
        .map(|mode| Segment::new(studio_text(output_key(*mode))).with_icon(output_icon(*mode)))
        .collect()
}

pub(super) fn target_segments() -> Vec<Segment> {
    TARGETS
        .iter()
        .map(|source| {
            Segment::new(studio_text(target_key(*source))).with_icon(target_icon(*source))
        })
        .collect()
}

pub(super) fn delay_choice(seconds: u8) -> String {
    if seconds == 0 {
        studio_text("widget-capture-delay-none")
    } else {
        crate::i18n::widget_format(
            "widget-capture-delay-choice",
            &[("seconds", i64::from(seconds).into())],
        )
    }
}

pub(super) fn layout_key(layout: PipLayout) -> &'static str {
    match layout {
        PipLayout::Auto => "widget-capture-layout-auto",
        PipLayout::Grid => "widget-capture-layout-grid",
        PipLayout::Stack => "widget-capture-layout-stack",
    }
}

pub(super) fn ui_count(n: usize) -> f64 {
    crate::num::f64_of_usize(n)
}

impl CaptureStudio {
    pub(super) fn measure_bar(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let spacing = &theme.tokens.spacing;
        let outputs: Vec<String> = OUTPUTS
            .iter()
            .map(|mode| studio_text(output_key(*mode)))
            .collect();
        let targets: Vec<String> = TARGETS
            .iter()
            .map(|source| studio_text(target_key(*source)))
            .collect();
        let outputs: Vec<&str> = outputs.iter().map(String::as_str).collect();
        let targets: Vec<&str> = targets.iter().map(String::as_str).collect();
        let size = ControlSize::Regular;
        let label = theme.text(TextRole::Label);
        let time_w = f64::from(tr.measure_style("0:00:00", None, &label).0).ceil();
        let digits = f64::from(tr.measure_style("0000", None, &label).0).ceil();
        let skipped_w = f64::from(spacing.s4) + f64::from(spacing.s1) + digits;
        let save_label = studio_text("widget-replay-save");
        let save_w = ButtonFace::new(Rect::ZERO, &save_label)
            .icon(Icon::Download)
            .control_size(size)
            .intrinsic_width(tr, theme);
        self.metrics = BarMetrics {
            pad: f64::from(spacing.s2),
            gap: f64::from(spacing.s4),
            margin: f64::from(spacing.s6),
            radius: f64::from(theme.tokens.radii.panel),
            strip_h: SegmentedFace::stacked_height(size, theme),
            output_w: SegmentedFace::stacked_segment_width(&outputs, size, tr, theme),
            target_w: SegmentedFace::stacked_segment_width(&targets, size, tr, theme),
            shutter: CaptureButtonFace::diameter(theme),
            icon_button: size.height(theme),
            time_w,
            skipped_w,
            save_w,
        };
    }

    pub(super) fn bar_height(&self) -> f64 {
        let m = &self.metrics;
        m.strip_h.max(m.shutter) + 2.0 * m.pad
    }

    /// The setup bar: bottom centre, moved to the top when it would cover
    /// what is chosen and the top is clear of it.
    pub(super) fn setup_bar(&self) -> Rect {
        let m = &self.metrics;
        let w = 2.0 * m.pad + 2.0 * self.setup_side() + m.shutter;
        let w = w.min(self.rect.width());
        let h = self.bar_height();
        let cx = self.rect.center().x;
        let bottom = Rect::new(
            cx - w / 2.0,
            self.rect.y1 - m.margin - h,
            cx + w / 2.0,
            self.rect.y1 - m.margin,
        );
        let top = Rect::new(
            cx - w / 2.0,
            self.rect.y0 + m.margin,
            cx + w / 2.0,
            self.rect.y0 + m.margin + h,
        );
        match self.avoid_rect() {
            // Only to a place that is clear: over a window filling the
            // screen, both cover it, and the bottom is where it rests.
            Some(avoid) if overlap(bottom, avoid) > 0.0 && overlap(top, avoid) <= 0.0 => top,
            _ => bottom,
        }
    }

    /// Whether the tool's floating parts are at `p`: a screenshot's or a
    /// recording's receipt, or the bar while recording. They hang over the
    /// windows, so a press there, from the pointer or a finger, is the
    /// tool's and never the window's beneath.
    pub fn takes_pointer_at(&self, p: Point) -> bool {
        self.open
            && ((self.screenshot_receipt.is_some()
                && self.screenshot_receipt_layout().pill.contains(p))
                || (self.recording_finished_at.is_some()
                    && self.recording_receipt_layout().pill.contains(p))
                || (self.shows_status() && self.status_bar().contains(p)))
    }

    /// How far each half of the setup bar reaches from the shutter: the
    /// wider of the outputs, or the targets and the options, with the gap
    /// before the shutter. Both halves are this wide, so the shutter is the
    /// bar's centre whatever is chosen; each group packs toward the shutter
    /// at its own width, and the bar's outer padding takes the difference.
    pub(super) fn setup_side(&self) -> f64 {
        let m = &self.metrics;
        let outputs = OUTPUTS.len() as f64 * m.output_w;
        let targets = TARGETS.len() as f64 * m.target_w + m.gap + m.icon_button;
        outputs.max(targets) + m.gap
    }

    /// The bar while recording, counting down or keeping the last 30
    /// seconds: the shutter and a time (or Save), where the person moved it.
    /// The shutter keeps its place when the skipped-frames chip appears, so
    /// stopping a recording never means aiming again.
    pub(super) fn status_bar(&self) -> Rect {
        let m = &self.metrics;
        let content = if self.replay_active {
            m.save_w
        } else {
            m.time_w
        };
        let skipped = if self.shows_skipped() {
            m.skipped_w + m.gap
        } else {
            0.0
        };
        let w = 2.0 * m.pad + content + m.gap + skipped + m.shutter;
        let h = self.bar_height();
        let bounds = self.status_rect();
        let x0 = bounds.center().x - w + m.pad + m.shutter / 2.0 + self.status_offset.x;
        let y0 = bounds.y1 - m.margin - h + self.status_offset.y;
        let x0 = x0.clamp(bounds.x0, (bounds.x1 - w).max(bounds.x0));
        let y0 = y0.clamp(bounds.y0, (bounds.y1 - h).max(bounds.y0));
        Rect::new(x0, y0, x0 + w, y0 + h)
    }

    /// Whether the bar says how many frames the recording has skipped: only
    /// once it has skipped one, and never for the kept last 30 seconds.
    pub(super) fn shows_skipped(&self) -> bool {
        self.shows_status() && !self.replay_active && self.recording_skipped > 0
    }

    pub(super) fn recording_status(&self) -> String {
        if let Some(reason) = &self.recording_failure {
            return failed_sentence(reason, self.recording_kept);
        }
        if self.recording_skipped > 0 {
            skipped_sentence(self.recording_skipped)
        } else {
            studio_text("widget-recording-saved")
        }
    }

    /// What the receipt of the recording that ended offers: a whole file
    /// plays and shows in Files; one that was not finished only shows, and
    /// only while what was written is there.
    pub(super) fn recording_actions(&self) -> Vec<ReceiptAction> {
        if self.recording_failure.is_some() {
            return self
                .recording_kept
                .then_some(ReceiptAction::Reveal)
                .into_iter()
                .collect();
        }
        vec![ReceiptAction::Play, ReceiptAction::Reveal]
    }

    /// The bar shows its status form: a recording, its start, a countdown,
    /// or the kept last 30 seconds.
    pub(super) fn shows_status(&self) -> bool {
        self.open
            && (self.take.start_pending
                || self.take.active
                || self.replay_active
                || self.armed_capture.is_some())
    }

    /// Where each thing on the bar sits now.
    pub(super) fn bar_items(&self) -> Vec<(BarItem, Rect)> {
        let m = self.metrics;
        if self.shows_status() {
            let bar = self.status_bar();
            let cy = bar.center().y;
            let mut x = bar.x0 + m.pad;
            let mut items = Vec::with_capacity(3);
            if self.replay_active {
                let h = m.icon_button;
                items.push((
                    BarItem::Save,
                    Rect::new(x, cy - h / 2.0, x + m.save_w, cy + h / 2.0),
                ));
                x += m.save_w;
            } else {
                items.push((
                    BarItem::Time,
                    Rect::new(x, bar.y0 + m.pad, x + m.time_w, bar.y1 - m.pad),
                ));
                x += m.time_w;
                if self.shows_skipped() {
                    x += m.gap;
                    items.push((
                        BarItem::Skipped,
                        Rect::new(
                            x,
                            cy - m.icon_button / 2.0,
                            x + m.skipped_w,
                            cy + m.icon_button / 2.0,
                        ),
                    ));
                    x += m.skipped_w;
                }
            }
            x += m.gap;
            items.push((
                BarItem::Shutter,
                Rect::from_center_size(Point::new(x + m.shutter / 2.0, cy), (m.shutter, m.shutter)),
            ));
            return items;
        }
        if !self.choosing() {
            return Vec::new();
        }
        let bar = self.setup_bar();
        let cy = bar.center().y;
        let strip =
            |x: f64, w: f64| Rect::new(x, cy - m.strip_h / 2.0, x + w, cy + m.strip_h / 2.0);
        let cx = bar.center().x;
        let mut items = Vec::with_capacity(9);
        let mut x = cx - m.shutter / 2.0 - m.gap - OUTPUTS.len() as f64 * m.output_w;
        for i in 0..OUTPUTS.len() {
            items.push((BarItem::Output(i), strip(x, m.output_w)));
            x += m.output_w;
        }
        items.push((
            BarItem::Shutter,
            Rect::from_center_size(Point::new(cx, cy), (m.shutter, m.shutter)),
        ));
        x = cx + m.shutter / 2.0 + m.gap;
        for i in 0..TARGETS.len() {
            items.push((BarItem::Target(i), strip(x, m.target_w)));
            x += m.target_w;
        }
        x += m.gap;
        items.push((
            BarItem::Options,
            Rect::new(
                x,
                cy - m.icon_button / 2.0,
                x + m.icon_button,
                cy + m.icon_button / 2.0,
            ),
        ));
        items
    }

    pub(super) fn bar_item(&self, item: BarItem) -> Rect {
        self.bar_items()
            .into_iter()
            .find(|(it, _)| *it == item)
            .map_or(Rect::ZERO, |(_, rect)| rect)
    }

    pub(super) fn shutter_kind(&self) -> CaptureKind {
        if self.take.active || self.take.start_pending || self.replay_active {
            return CaptureKind::Recording;
        }
        match self.mode {
            CaptureMode::Screenshot => CaptureKind::Photo,
            CaptureMode::Recording => CaptureKind::Video,
            CaptureMode::Pip => CaptureKind::Pip,
        }
    }

    pub(super) fn shutter_name(&self) -> String {
        if self.replay_active {
            return studio_text("widget-replay-stop");
        }
        if self.take.active || self.take.start_pending {
            return studio_text("widget-capture-stop");
        }
        studio_text(match self.mode {
            CaptureMode::Screenshot => "widget-capture-take",
            CaptureMode::Recording => "widget-capture-record",
            CaptureMode::Pip => "widget-capture-create-pip",
        })
    }

    pub(super) fn shutter_ready(&self) -> bool {
        if self.replay_active {
            return true;
        }
        if self.take.start_pending || self.armed_capture.is_some() {
            return false;
        }
        if self.take.active {
            return !self.take.stop_pending;
        }
        self.has_capture_source()
    }

    pub(super) fn bar_tip(&self) -> Option<(Rect, String)> {
        if self.options_menu.is_some() || self.send_menu.is_some() {
            return None;
        }
        let (item, rect) = self
            .bar_items()
            .into_iter()
            .find(|(_, rect)| rect.contains(self.cursor))?;
        // No shortcut helpers in this tool. The outputs and targets carry
        // their labels, so only the unlabelled shutter and options are named,
        // and an output whose label is a short form (PiP) spells it out.
        let tip = match item {
            BarItem::Shutter => self.shutter_name(),
            BarItem::Options => studio_text("widget-capture-options"),
            BarItem::Output(i) => {
                let name = output_name(OUTPUTS[i]);
                if name == studio_text(output_key(OUTPUTS[i])) {
                    return None;
                }
                name
            }
            BarItem::Target(_) | BarItem::Time | BarItem::Save => return None,
            BarItem::Skipped => skipped_sentence(self.recording_skipped),
        };
        Some((rect, tip))
    }

    pub(super) fn press_status(&mut self, p: Point) {
        let items = self.bar_items();
        match items
            .iter()
            .find(|(_, rect)| rect.contains(p))
            .map(|(i, _)| *i)
        {
            Some(BarItem::Shutter) if self.replay_active => {
                self.queue_replay_command(ReplayCommand::Stop);
            }
            Some(BarItem::Shutter) if self.take.active && !self.take.stop_pending => {
                self.queue_recording_command(RecordingCommand::Stop);
            }
            Some(BarItem::Save) => {
                let _ = self.save_recent_replay();
            }
            Some(BarItem::Shutter) => {}
            _ => self.status_drag = Some((p, self.status_offset)),
        }
    }

    /// Paint the bar: the setup form while choosing, the status form while
    /// recording, counting down or keeping the last 30 seconds.
    pub(super) fn paint_bar(&self, ctx: &mut PaintCtx<'_>) {
        let PaintCtx {
            scene,
            transform,
            text: tr,
            theme,
        } = ctx.reborrow();
        let status = self.shows_status();
        let bar = if status {
            self.status_bar()
        } else {
            self.setup_bar()
        };
        let on =
            crate::widget::paint_see_through(scene, transform, bar, self.metrics.radius, theme);
        let items = self.bar_items();
        let hovered = |item: BarItem| {
            items
                .iter()
                .any(|(it, rect)| *it == item && rect.contains(self.cursor))
        };
        let rects = |pick: fn(BarItem) -> bool| -> Vec<Rect> {
            items
                .iter()
                .filter(|(item, _)| pick(*item))
                .map(|(_, rect)| *rect)
                .collect()
        };
        if !status {
            let outputs = rects(|item| matches!(item, BarItem::Output(_)));
            let output_items = output_segments();
            let chosen = OUTPUTS.iter().position(|m| *m == self.mode).unwrap_or(0);
            crate::widget::ToggleGroup::paint(
                scene,
                transform,
                tr,
                &on,
                &SegmentedFace::of_segments(&outputs, &output_items, chosen)
                    .stacked(true)
                    .style(SegmentedStyle::Quiet)
                    .hovered(outputs.iter().position(|r| r.contains(self.cursor))),
            );
            let targets = rects(|item| matches!(item, BarItem::Target(_)));
            let target_items = target_segments();
            let chosen = TARGETS.iter().position(|s| *s == self.source).unwrap_or(0);
            crate::widget::ToggleGroup::paint(
                scene,
                transform,
                tr,
                &on,
                &SegmentedFace::of_segments(&targets, &target_items, chosen)
                    .stacked(true)
                    .style(SegmentedStyle::Quiet)
                    .hovered(targets.iter().position(|r| r.contains(self.cursor))),
            );
            // Two separate choices with the shutter between them, set off
            // by a rule on each side: what comes out, then what it is of.
            let ground = crate::chrome::panel_ground(theme);
            let inset = f64::from(theme.tokens.spacing.s2);
            for (left, right) in [
                (BarItem::Output(OUTPUTS.len() - 1), BarItem::Shutter),
                (BarItem::Shutter, BarItem::Target(0)),
            ] {
                let x = (self.bar_item(left).x1 + self.bar_item(right).x0) / 2.0;
                crate::surface::vertical_separator(
                    scene, transform, x, bar.y0, bar.y1, inset, ground,
                );
            }
            let options = self.bar_item(BarItem::Options);
            let name = studio_text("widget-capture-options");
            let enabled = !self.option_sections().is_empty();
            let face = IconButton::face(options, Icon::MoreHorizontal, &name)
                .control_size(ControlSize::Regular)
                .toggled(self.options_menu.is_some())
                .hovered(enabled && hovered(BarItem::Options))
                .disabled(!enabled);
            Button::paint(scene, transform, tr, &on, &face);
        }
        for (item, rect) in &items {
            match item {
                BarItem::Time => {
                    let text = match self.armed_capture.as_ref() {
                        Some(armed) => {
                            let left = armed.duration.saturating_sub(
                                crate::time::now().saturating_duration_since(armed.started_at),
                            );
                            left.as_secs_f64().ceil().max(1.0).to_string()
                        }
                        None => crate::format::timecode(self.recording_elapsed().as_secs()),
                    };
                    let style = on.text(TextRole::Label);
                    let (w, h) = tr.measure_style(&text, None, &style);
                    tr.draw_style(
                        scene,
                        &text,
                        on.tokens.colors.text_primary,
                        transform.pre_translate(
                            (
                                rect.center().x - f64::from(w) / 2.0,
                                rect.center().y - f64::from(h) / 2.0,
                            )
                                .into(),
                        ),
                        None,
                        &style,
                    );
                }
                BarItem::Save => {
                    let label = studio_text("widget-replay-save");
                    let ready = self.replay_buffered_seconds > 0;
                    let face = ButtonFace::new(*rect, &label)
                        .icon(Icon::Download)
                        .control_size(ControlSize::Regular)
                        .hovered(ready && rect.contains(self.cursor))
                        .disabled(!ready);
                    Button::paint(scene, transform, tr, &on, &face);
                }
                BarItem::Skipped => {
                    // The warning glyph and the count, both in the warning
                    // ink the bar's other reads do not use, so a recording
                    // that lost frames says so without a word.
                    let ink = on.tokens.colors.semantic.warning;
                    let glyph = f64::from(on.tokens.spacing.s4);
                    let text = crate::format::count(self.recording_skipped);
                    let style = on.text(TextRole::Label);
                    let (w, h) = tr.measure_style(&text, None, &style);
                    let w = f64::from(w);
                    let h = f64::from(h);
                    let left =
                        rect.center().x - (glyph + f64::from(on.tokens.spacing.s1) + w) / 2.0;
                    Icon::TriangleAlert.draw(
                        scene,
                        transform,
                        Rect::from_center_size(
                            Point::new(left + glyph / 2.0, rect.center().y),
                            (glyph, glyph),
                        ),
                        ink,
                    );
                    tr.draw_style(
                        scene,
                        &text,
                        ink,
                        transform.pre_translate(
                            (
                                left + glyph + f64::from(on.tokens.spacing.s1),
                                rect.center().y - h / 2.0,
                            )
                                .into(),
                        ),
                        None,
                        &style,
                    );
                }
                BarItem::Shutter => {
                    CaptureButtonFace::new(self.shutter_kind())
                        .on_chrome(true)
                        .live(self.shutter_ready())
                        .hovered(self.shutter_ready() && rect.contains(self.cursor))
                        .paint(&mut PaintCtx::new(scene, transform, tr, &on), rect.center());
                }
                BarItem::Output(_) | BarItem::Target(_) | BarItem::Options => {}
            }
        }
        if let Some((anchor, tip)) = self.bar_tip() {
            crate::widget::tooltip::draw_hover_tooltip(
                scene,
                transform,
                tr,
                theme,
                anchor,
                &tip,
                self.status_rect(),
            );
        }
    }
}
