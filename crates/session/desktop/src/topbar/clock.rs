//! The bar's centre lane and the clock: the time it reads at the grain the
//! visible activity needs, the live activity over it, the width the pill takes
//! and when its frame is stale, and the floating clock overlay with the pause,
//! add and close controls a person drags it by.

use super::*;

impl TopBarChrome {
    /// The centre selector is visually anchored at the viewport centre, but
    /// its wide-state width must not consume the entire narrow bar. Keep the
    /// established 360 px treatment on laptop-sized and larger viewports and
    /// release the reclaimed lane to the workspace identity below that
    /// point. The lower bound still leaves enough room for the clock and its
    /// three position dots at the large-text setting.
    pub(super) fn center_band_width(&self, theme: &Theme) -> f64 {
        let text_headroom = (f64::from(theme.text_scale) - 1.0).max(0.0) * 16.0;
        if self.rect.width() < NARROW_MENU_BAR_PX {
            // A phone-wide bar keeps the time and gives the rest of the
            // lane to the workspace name and the people beside it.
            return 64.0 + text_headroom;
        }
        let min_width = (220.0 + text_headroom).min(260.0);
        (self.rect.width() - 580.0).clamp(min_width, 360.0)
    }

    pub(super) fn clock_viewport(&self) -> (f64, f64) {
        self.chrome
            .as_ref()
            .and_then(|chrome| chrome.snapshot().viewport_size)
            .unwrap_or_else(|| (self.rect.width(), f64::INFINITY))
    }

    pub(super) fn publish_clock_activity_command(&self, timer: &str, stopwatch: &str) {
        let activity = self
            .chrome
            .as_ref()
            .and_then(|chrome| chrome.snapshot().clock_activity);
        let is_stopwatch = activity.as_ref().is_some_and(|activity| activity.stopwatch);
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.intent(ChromeIntent::ClockCommand {
                command: if is_stopwatch { stopwatch } else { timer }.to_string(),
                activity_id: activity.and_then(|activity| activity.activity_id),
            });
        }
    }

    /// Route a point through the exact control rectangles painted for the
    /// floating activity. Shared by mouse and touch so the two modalities can
    /// never drift into different timer/stopwatch behavior.
    pub(super) fn activate_clock_control_at(&mut self, point: Point) -> bool {
        let Some(control) = self.clock_control_at(point) else {
            return false;
        };
        self.activate_clock_control(control);
        true
    }

    pub(super) fn clock_control_at(&self, point: Point) -> Option<u8> {
        if self.clock_pause_rect.contains(point) {
            Some(0)
        } else if self.clock_add_rect.contains(point) {
            Some(1)
        } else if self.clock_close_rect.contains(point) {
            Some(2)
        } else {
            None
        }
    }

    pub(super) fn activate_clock_control(&mut self, control: u8) {
        self.clock_control_focus = Some(control);
        match control {
            0 => self.publish_clock_activity_command("toggle", "toggle-stopwatch"),
            1 => self.publish_clock_activity_command("add-minute", "lap-stopwatch"),
            2 => {
                if let Some(chrome) = self.chrome.as_ref() {
                    chrome.intent(ChromeIntent::HideClock);
                }
                self.publish_clock_activity_command("close", "close-stopwatch");
            }
            _ => return,
        }
        self.cache_fp = CacheFingerprint::default();
    }

    pub(super) fn begin_clock_drag_at(&mut self, point: Point) -> bool {
        let header = clock_overlay_drag_region(self.clock_overlay_rect);
        if !header.contains(point) {
            return false;
        }
        let origin = self.clock_overlay_rect.origin();
        self.clock_drag_offset = Some(vello::kurbo::Vec2::new(
            point.x - origin.x,
            point.y - origin.y,
        ));
        true
    }

    pub(super) fn move_clock_drag_to(&mut self, point: Point) -> bool {
        let Some(offset) = self.clock_drag_offset else {
            return false;
        };
        let width = self.clock_overlay_rect.width();
        self.clock_overlay_pos = Some(clamp_clock_overlay(
            Point::new(point.x - offset.x, point.y - offset.y),
            (width, self.clock_overlay_rect.height()),
            self.clock_viewport(),
            self.rect.y1,
        ));
        self.cache_fp = CacheFingerprint::default();
        true
    }

    pub(super) fn live_clock(&self) -> ClockSnapshot {
        self.chrome
            .as_ref()
            .map_or_else(|| self.clock.clone(), |c| c.snapshot().clock)
    }

    /// The host's clock reading, already formatted — and pinned by the host
    /// when a screenshot harness needs every frame to hold still.
    pub(super) fn current_clock_string(&self) -> String {
        self.live_clock().clock
    }

    /// Bold, so it reads as the chrome's focal glance value.
    pub fn clock_text_weight(&self) -> TextWeight {
        TextWeight::Bold
    }

    /// The centre band. The clock is plain text on the bar, with no pill
    /// (a pill around five characters read as a second bar inside the bar);
    /// a live activity over it keeps a quiet selector pill. Returns the
    /// centre line the time or the activity draws on.
    pub(super) fn draw_center_band(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
    ) -> f64 {
        let lane_w = self.center_band_width(theme);
        let target_w = self.center_pill_width(theme, text_renderer, lane_w);
        let now = ui::time::now();
        if (self.center_lane.width() - lane_w).abs() > 0.5 {
            // A new lane (the bar's first paint, a resized display): the
            // selector takes its width as it is.
            self.center_width.snap(target_w);
        } else if (self.center_width.target() - target_w).abs() > 0.25 {
            self.center_width.follow(
                target_w,
                theme.motion(),
                ui::tokens::MotionSource::Pointer,
                now,
            );
        }
        let band_w = self.center_width.get(now).clamp(0.0, lane_w);
        // The same height as every other bar control, so the band's top
        // and bottom edges line up with the pills either side of it.
        let band_h = BAR_CONTROL_PX;
        let band_cx = f64::midpoint(self.rect.x0, self.rect.x1);
        let band = Rect::new(
            band_cx - band_w / 2.0,
            center_y - band_h / 2.0,
            band_cx + band_w / 2.0,
            center_y + band_h / 2.0,
        );
        self.center_rect = band;
        self.center_lane = Rect::new(
            band_cx - lane_w / 2.0,
            band.y0,
            band_cx + lane_w / 2.0,
            band.y1,
        );
        // The quiet selector pill comes and goes with the activity.
        let pill = self.center_activity_opacity(now);
        if pill > 0.0 {
            let band_rr = RoundedRect::from_rect(
                band,
                ui::chrome::ChromeRole::Selector.radius(theme, band.height()),
            );
            if pill < 1.0 {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    pill,
                    transform,
                    &band.inflate(2.0, 2.0),
                );
            }
            paint_bar_selector(
                scene,
                transform,
                &band_rr,
                ui::chrome::SelectionState::active(true),
                theme,
            );
            if pill < 1.0 {
                scene.pop_layer();
            }
        }
        let (content_x0, content_x1) = Self::center_content_lane(band);
        f64::midpoint(content_x0, content_x1)
    }

    /// The clock with nothing live over it: drawn as bare text.
    #[cfg(test)]
    pub(super) fn center_is_plain_text(&self) -> bool {
        !self.activity_is_visible()
    }

    /// How strongly the live activity (and its pill) shows at `now`, as it
    /// cross-fades with the time.
    pub(super) fn center_activity_opacity(&self, now: Instant) -> f32 {
        if *self.center_fade.shown() {
            return self.center_fade.incoming(now);
        }
        self.center_fade
            .outgoing(now)
            .filter(|(activity, _)| **activity)
            .map_or(0.0, |(_, opacity)| opacity)
    }

    /// The lane the active centre widget draws into: the same inset in from
    /// either end of the band. One function so
    /// [`Self::draw_center_band`] and [`Self::activity_content_width`]
    /// cannot disagree about how much room the activity headline has.
    pub(super) fn center_content_lane(band: Rect) -> (f64, f64) {
        // Symmetric, so the content's centre is the band's centre.
        (band.x0 + CENTER_INSET_PX, band.x1 - CENTER_INSET_PX)
    }

    /// The date, said beside the time to assistive technology.
    pub(super) fn date_description(&self) -> String {
        let clock = self.live_clock();
        format!("{} {}", clock.weekday, clock.date)
            .trim()
            .to_owned()
    }

    /// What the clock widget reads: the time, or the running Clocks
    /// timer or stopwatch with its label.
    pub(super) fn clock_widget_text(&self) -> String {
        self.chrome
            .as_ref()
            .and_then(|chrome| chrome.snapshot().clock_activity)
            .map_or_else(
                || self.current_clock_string(),
                |activity| {
                    let reading = ui::format::timecode(activity.remaining_secs);
                    if activity.label.is_empty() {
                        reading
                    } else {
                        format!("{}  {reading}", activity.label)
                    }
                },
            )
    }

    /// The width of the centre widget inside its `lane_w` lane. A live
    /// activity fills the lane; the clock is a five-character reading, so its target hugs it: the time and the
    /// content inset either side. The time is measured with every digit set
    /// as the widest one, so the target does not change width minute to
    /// minute.
    pub(super) fn center_pill_width(
        &self,
        theme: &Theme,
        text: &mut TextRenderer,
        lane_w: f64,
    ) -> f64 {
        if self.activity_is_visible() {
            return lane_w;
        }
        let style = theme
            .tokens
            .typography
            .style(TextRole::Body)
            .with_weight(self.clock_text_weight());
        let widest_digit = ('0'..='9')
            .map(|digit| {
                (
                    text.measure_style(&digit.to_string(), None, &style).0,
                    digit,
                )
            })
            .fold((0.0_f32, '0'), |widest, next| {
                if next.0 > widest.0 { next } else { widest }
            })
            .1;
        let reading: String = self
            .clock_widget_text()
            .chars()
            .map(|c| if c.is_ascii_digit() { widest_digit } else { c })
            .collect();
        let (width, _) = text.measure_style(&reading, None, &style);
        (f64::from(width) + 2.0 * CENTER_INSET_PX).min(lane_w)
    }

    pub(super) fn draw_clock_widget(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        content_cx: f64,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let s = self.clock_widget_text();
        let style = tokens.typography.style(TextRole::Body);
        // Measured at the draw weight — Bold advances wider
        // than Regular, so centering would drift otherwise.
        let weight = self.clock_text_weight();
        let (tw, th) = text_renderer.measure_style(&s, None, &style.clone().with_weight(weight));
        let tx = content_cx - f64::from(tw) / 2.0;
        let ty = center_y - f64::from(th) / 2.0;
        text_renderer.draw_style(
            scene,
            &s,
            colors.text_primary,
            at(transform, (tx, ty)),
            None,
            &style.with_weight(weight),
        );
    }

    /// Sizes the center band, then draws the time, or a live activity over
    /// it. Split out of [`Widget::draw`] to stay under clippy's line budget.
    pub(super) fn draw_center_widget(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
    ) {
        let content_cx = self.draw_center_band(scene, transform, theme, text_renderer, center_y);
        // The time and a live activity cross-fade in place.
        let now = ui::time::now();
        let mut layers = Vec::with_capacity(2);
        if let Some((activity, opacity)) = self.center_fade.outgoing(now) {
            layers.push((*activity, opacity));
        }
        layers.push((*self.center_fade.shown(), self.center_fade.incoming(now)));
        for (activity, opacity) in layers {
            let layered = opacity < 1.0;
            if layered {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    opacity,
                    transform,
                    &self.center_lane,
                );
            }
            if activity {
                self.draw_activity_widget(
                    scene,
                    transform,
                    theme,
                    text_renderer,
                    center_y,
                    content_cx,
                );
            } else {
                self.draw_clock_widget(
                    scene,
                    transform,
                    theme,
                    text_renderer,
                    center_y,
                    content_cx,
                );
            }
            if layered {
                scene.pop_layer();
            }
        }
    }

    /// The centre's cross-fade as the cache sees it: its progress in
    /// thousandths while it runs, so each of its frames draws anew.
    pub(super) fn center_fade_key(&self) -> Option<u16> {
        let now = ui::time::now();
        self.center_fade.is_moving(now).then(|| {
            let permille = (self.center_fade.incoming(now) * 1000.0)
                .round()
                .clamp(0.0, 1000.0);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to 0..=1000 just above"
            )]
            let permille = permille as u16;
            permille
        })
    }

    /// The centre selector's width as the cache sees it while it morphs:
    /// in quarter pixels, so each of its frames draws anew.
    pub(super) fn center_width_key(&self) -> Option<u32> {
        let now = ui::time::now();
        self.center_width.is_moving(now).then(|| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a bar's width in quarter pixels, clamped non-negative"
            )]
            let quarters = (self.center_width.get(now) * 4.0).round().max(0.0) as u32;
            quarters
        })
    }

    /// Whether the centre is moving: the time and an activity cross-fading,
    /// or the selector morphing between their widths.
    pub(super) fn center_is_moving(&self) -> bool {
        let now = ui::motion::frame_now();
        self.center_fade.is_moving(now) || self.center_width.is_moving(now)
    }

    /// When the centre's motion settles, if it is moving.
    #[cfg(test)]
    pub(super) fn center_settles_at(&self) -> Option<Instant> {
        self.center_fade
            .settles_at()
            .into_iter()
            .chain(self.center_width.settles_at())
            .max()
    }

    /// Whether the centre on screen is not the centre as it is now: the
    /// cached frame was painted part-way through a cross-fade that has
    /// since settled, or the activity came or went since. Without this the
    /// last frame painted while the fade moved stays up, the time and the
    /// activity both at part opacity, because nothing else asks to paint
    /// once the motion has settled.
    pub(super) fn center_frame_stale(&self) -> bool {
        // A bar that paints nothing has no centre on screen to go stale.
        if self.presence() == TopBarPresence::Hidden
            || self.rect.width() <= 0.0
            || self.rect.height() <= 0.0
        {
            return false;
        }
        self.center_fade_key() != self.cache_fp.center_fade
            || self.center_width_key() != self.cache_fp.center_width
            || self.activity_is_visible() != self.cache_fp.activity_visible
    }

    pub(super) fn activity_is_visible(&self) -> bool {
        self.activity_headline.is_some()
            && self.activity_started.is_some_and(|started| {
                ui::time::now().saturating_duration_since(started) < Duration::from_secs(6)
            })
    }

    pub(super) fn draw_activity_widget(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        center_x: f64,
    ) {
        let Some(_headline) = self.activity_headline.as_deref() else {
            return;
        };
        let style = theme.text(TextRole::Caption);
        // Keep activity in one line. `measure(..., Some(width))` wraps at
        // word boundaries, which made a large-text notification grow to two
        // lines inside the fixed-height selector and clip into the bar edge.
        // The dots reserve the first three pitches of the band; the trailing
        // inset reserves the right breathing room. Elision preserves that
        // geometry at every text scale and viewport width.
        let max_width = self.activity_content_width(theme);
        let label = self.activity_display_label(text_renderer, &style, max_width);
        let (w, h) = text_renderer.measure_style(
            &label,
            None,
            &style.clone().with_weight(TextWeight::Medium),
        );
        text_renderer.draw_style(
            scene,
            &label,
            theme.tokens.colors.text_primary,
            at(
                transform,
                (center_x - f64::from(w) / 2.0, center_y - f64::from(h) / 2.0),
            ),
            None,
            &style.clone().with_weight(TextWeight::Medium),
        );
    }

    /// Width of the centre lane the activity headline may occupy — the
    /// band minus its insets, exactly as
    /// [`Self::draw_center_band`] lays it out.
    pub(super) fn activity_content_width(&self, theme: &Theme) -> f32 {
        let (x0, x1) = Self::center_content_lane(self.center_rect);
        ui::num::f32_of_f64((x1 - x0).max(f64::from(theme.tokens.spacing.s12)))
    }

    pub(super) fn activity_display_label(
        &self,
        text_renderer: &mut TextRenderer,
        style: &TextStyle,
        max_width: f32,
    ) -> String {
        let Some(headline) = self.activity_headline.as_deref() else {
            return String::new();
        };
        // The headline alone: the unread count is the badge's, never joined
        // into the sentence, and clicking the pill opens the activity.
        let medium = style.clone().with_weight(TextWeight::Medium);
        let (full_width, _) = text_renderer.measure_style(headline, None, &medium);
        if full_width <= max_width {
            return headline.to_string();
        }
        // Aggregate activity sometimes lists its examples after a colon
        // (the workspaces just recorded). Keep the whole event clause rather
        // than cutting part-way through the examples.
        if let Some((summary, _examples)) = headline.split_once(':') {
            let (compact_width, _) = text_renderer.measure_style(summary, None, &medium);
            if compact_width <= max_width {
                return summary.to_string();
            }
        }
        text_renderer.elide_to_width_style(headline, max_width, &medium)
    }

    pub(super) fn draw_clock_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        activity: &ClockActivity,
    ) {
        let (width, height) = (clock_overlay_width(self.rect.x1), CLOCK_OVERLAY_HEIGHT);
        let fallback = Point::new((self.rect.x1 - width - 16.0).max(8.0), self.rect.y1 + 16.0);
        let clamped = clamp_clock_overlay(
            self.clock_overlay_pos.unwrap_or(fallback),
            (width, height),
            self.clock_viewport(),
            self.rect.y1,
        );
        let panel = Rect::from_origin_size(clamped, (width, height));
        self.clock_overlay_pos = Some(clamped);
        self.clock_overlay_rect = panel;
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(panel, 18.0),
        );
        let grip = Line::new(
            Point::new(panel.center().x - 14.0, panel.y0 + 9.0),
            Point::new(panel.center().x + 14.0, panel.y0 + 9.0),
        );
        scene.stroke(
            &Stroke::new(3.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &grip,
        );
        text_renderer.draw_style(
            scene,
            &activity.label,
            theme.tokens.colors.text_dim,
            at(transform, (panel.x0 + 18.0, panel.y0 + 21.0)),
            None,
            &theme.text(TextRole::Label),
        );
        let elapsed = ui::format::timecode(activity.remaining_secs);
        text_renderer.draw_style(
            scene,
            &elapsed,
            theme.tokens.colors.text_primary,
            at(transform, (panel.x0 + 18.0, panel.y0 + 43.0)),
            None,
            &theme.text(TextRole::Heading).with_weight(TextWeight::Bold),
        );
        [
            self.clock_pause_rect,
            self.clock_add_rect,
            self.clock_close_rect,
        ] = clock_overlay_controls(panel);
        let [pause_label, add_label, close_label] = clock_overlay_labels(activity);
        ui::widget::Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ui::widget::ButtonFace::new(self.clock_pause_rect, &pause_label)
                .role(ButtonRole::Primary)
                .icon(if activity.paused {
                    ui::icons::Icon::Play
                } else {
                    ui::icons::Icon::Pause
                })
                .focused(self.clock_control_focus == Some(0)),
        );
        ui::widget::Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ui::widget::ButtonFace::new(self.clock_add_rect, &add_label)
                .focused(self.clock_control_focus == Some(1)),
        );
        ui::widget::Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ui::widget::ButtonFace::new(self.clock_close_rect, "")
                .role(ButtonRole::Plain)
                .icon(ui::icons::Icon::X)
                .accessible_name(&close_label)
                .focused(self.clock_control_focus == Some(2)),
        );
    }

    pub(super) fn handle_clock_keyboard(
        &mut self,
        logical_key: &winit::keyboard::Key,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
        match logical_key {
            Key::Named(NamedKey::Escape) => {
                if let Some(chrome) = self.chrome.as_ref() {
                    chrome.intent(ChromeIntent::HideClock);
                }
                self.publish_clock_activity_command("close", "close-stopwatch");
            }
            Key::Named(NamedKey::Tab) => {
                self.clock_control_focus = Some(if modifiers.state().shift_key() {
                    match self.clock_control_focus {
                        None | Some(0 | 3..) => 2,
                        Some(1) => 0,
                        Some(2) => 1,
                    }
                } else {
                    match self.clock_control_focus {
                        None | Some(2..) => 0,
                        Some(0) => 1,
                        Some(1) => 2,
                    }
                });
                self.cache_fp = CacheFingerprint::default();
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                let Some(control) = self.clock_control_focus else {
                    return false;
                };
                self.activate_clock_control(control);
            }
            Key::Named(
                key @ (NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::ArrowUp
                | NamedKey::ArrowDown),
            ) if modifiers.state().alt_key() => {
                let step = if modifiers.state().shift_key() {
                    32.0
                } else {
                    8.0
                };
                let mut position = self.clock_overlay_pos.unwrap_or_else(|| {
                    Point::new(
                        (self.rect.x1 - clock_overlay_width(self.rect.x1) - 16.0).max(8.0),
                        self.rect.y1 + 16.0,
                    )
                });
                match key {
                    NamedKey::ArrowLeft => position.x -= step,
                    NamedKey::ArrowRight => position.x += step,
                    NamedKey::ArrowUp => position.y -= step,
                    NamedKey::ArrowDown => position.y += step,
                    _ => {}
                }
                self.clock_overlay_pos = Some(clamp_clock_overlay(
                    position,
                    (clock_overlay_width(self.rect.x1), CLOCK_OVERLAY_HEIGHT),
                    self.clock_viewport(),
                    self.rect.y1,
                ));
                self.cache_fp = CacheFingerprint::default();
            }
            _ => return false,
        }
        true
    }

    pub(super) fn handle_clock_touch(
        &mut self,
        touch: &winit::event::Touch,
        scale_factor: f64,
    ) -> bool {
        let point = Point::new(
            touch.location.x / scale_factor,
            touch.location.y / scale_factor,
        );
        match touch.phase {
            winit::event::TouchPhase::Started => {
                if self.clock_touch_id.is_none()
                    && self.clock_touch_control.is_none()
                    && let Some(control) = self.clock_control_at(point)
                {
                    self.clock_touch_control = Some((touch.id, control));
                    self.clock_control_focus = Some(control);
                    self.cache_fp = CacheFingerprint::default();
                    return true;
                }
                if self.clock_touch_id.is_none()
                    && self.clock_touch_control.is_none()
                    && self.begin_clock_drag_at(point)
                {
                    self.clock_touch_id = Some(touch.id);
                    return true;
                }
            }
            winit::event::TouchPhase::Moved
                if self
                    .clock_touch_control
                    .is_some_and(|(id, _)| id == touch.id) =>
            {
                // Keep ownership so lifting elsewhere cancels the press;
                // never turn a control gesture into an accidental drag.
                return true;
            }
            winit::event::TouchPhase::Moved if self.clock_touch_id == Some(touch.id) => {
                return self.move_clock_drag_to(point);
            }
            winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled
                if self
                    .clock_touch_control
                    .is_some_and(|(id, _)| id == touch.id) =>
            {
                let pending = self.clock_touch_control.take();
                if let Some(control) = released_clock_touch_control(
                    pending,
                    touch.id,
                    touch.phase == winit::event::TouchPhase::Ended,
                    self.clock_control_at(point),
                ) {
                    self.activate_clock_control(control);
                }
                return true;
            }
            winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled
                if self.clock_touch_id == Some(touch.id) =>
            {
                self.clock_touch_id = None;
                self.clock_drag_offset = None;
                return true;
            }
            _ => {}
        }
        false
    }

    pub(super) fn clock_accessibility_node(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let chrome = self.chrome.as_ref()?;
        let snapshot = chrome.snapshot();
        if !snapshot.overlays.clock {
            return None;
        }
        let activity = snapshot.clock_activity.as_ref()?;
        let stopwatch = activity.stopwatch;
        let paused = activity.paused;
        let reading = ui::format::timecode(activity.remaining_secs);
        Some(
            AccessNode::new(Role::Group)
                .with_name(t(if stopwatch {
                    "topbar-clock-floating-stopwatch"
                } else {
                    "topbar-clock-floating-timer"
                }))
                .with_description(t("topbar-clock-floating-description"))
                .with_bounds(self.clock_overlay_rect)
                .with_children(vec![
                    AccessNode::new(Role::StaticText)
                        .with_name(if activity.label.is_empty() {
                            t(if stopwatch {
                                "topbar-clock-stopwatch"
                            } else {
                                "topbar-clock-timer"
                            })
                        } else {
                            activity.label.clone()
                        })
                        .with_value(&reading)
                        .with_description(t(if paused {
                            "topbar-clock-paused"
                        } else {
                            "topbar-clock-running"
                        })),
                    AccessNode::new(Role::Button)
                        .with_name(t(if paused {
                            "topbar-clock-resume"
                        } else {
                            "topbar-clock-pause"
                        }))
                        .with_action_target("clock.overlay.toggle")
                        .with_bounds(self.clock_pause_rect)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.clock_control_focus == Some(0)),
                        ),
                    AccessNode::new(Role::Button)
                        .with_name(t(if stopwatch {
                            "topbar-clock-record-lap"
                        } else {
                            "topbar-clock-add-one-minute"
                        }))
                        .with_action_target("clock.overlay.add-minute")
                        .with_bounds(self.clock_add_rect)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.clock_control_focus == Some(1)),
                        ),
                    AccessNode::new(Role::Button)
                        .with_name(t(if stopwatch {
                            "topbar-clock-close-stopwatch"
                        } else {
                            "topbar-clock-close-timer"
                        }))
                        .with_action_target("clock.overlay.close")
                        .with_bounds(self.clock_close_rect)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.clock_control_focus == Some(2)),
                        ),
                ]),
        )
    }

    pub(super) fn handle_clock_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) -> bool {
        if matches!(event, WindowEvent::Focused(false)) {
            self.set_focus(false);
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
            && self
                .chrome
                .as_ref()
                .is_some_and(|chrome| chrome.snapshot().overlays.clock)
        {
            *handled = self.handle_clock_keyboard(&key.logical_key, modifiers);
            return true;
        }
        if let WindowEvent::Touch(touch) = event
            && self
                .chrome
                .as_ref()
                .is_some_and(|chrome| chrome.snapshot().overlays.clock)
        {
            *handled = self.handle_clock_touch(touch, scale_factor);
            return true;
        }
        false
    }
}

pub(super) const CLOCK_OVERLAY_HEIGHT: f64 = 150.0;

/// The floating clock's pause, add and close control labels.
pub(super) fn clock_overlay_labels(activity: &ClockActivity) -> [String; 3] {
    let pause = if activity.paused {
        "topbar-clock-resume"
    } else {
        "topbar-clock-pause"
    };
    let (add, close) = if activity.stopwatch {
        ("topbar-clock-lap", "topbar-clock-close-stopwatch")
    } else {
        ("topbar-clock-add-minute", "topbar-clock-close-timer")
    };
    [t(pause), t(add), t(close)]
}

pub(super) fn clamp_clock_overlay(
    position: Point,
    overlay: (f64, f64),
    viewport: (f64, f64),
    top: f64,
) -> Point {
    Point::new(
        position
            .x
            .clamp(8.0, (viewport.0 - overlay.0 - 8.0).max(8.0)),
        position
            .y
            .clamp(top + 8.0, (viewport.1 - overlay.1 - 8.0).max(top + 8.0)),
    )
}

pub(super) fn clock_overlay_width(viewport_width: f64) -> f64 {
    (viewport_width - 16.0).clamp(204.0, 252.0)
}

pub(super) fn clock_overlay_controls(panel: Rect) -> [Rect; 3] {
    let left = panel.x0 + 12.0;
    let right = panel.x1 - 12.0;
    let gap = 6.0;
    let width = (right - left - gap) / 2.0;
    // The floating surface is commonly used directly on a touch screen.
    // Keep every action at the platform's 44px minimum rather than inheriting
    // the compact 36px desktop-button height.
    let y0 = panel.y1 - 56.0;
    let y1 = panel.y1 - 12.0;
    [
        Rect::new(left, y0, left + width, y1),
        Rect::new(left + width + gap, y0, right, y1),
        Rect::new(
            panel.x1 - 52.0,
            panel.y0 + 8.0,
            panel.x1 - 8.0,
            panel.y0 + 52.0,
        ),
    ]
}

pub(super) fn clock_overlay_drag_region(panel: Rect) -> Rect {
    Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + 66.0)
}

pub(super) fn released_clock_touch_control(
    pending: Option<(u64, u8)>,
    touch_id: u64,
    ended: bool,
    control_under_finger: Option<u8>,
) -> Option<u8> {
    pending.and_then(|(owner, control)| {
        (ended && owner == touch_id && control_under_finger == Some(control)).then_some(control)
    })
}

/// Seconds the centre band's time reading stays the same for: the clock
/// paints `HH:MM`, a live activity counts seconds.
pub(super) fn time_bucket_secs(activity_visible: bool) -> u64 {
    if activity_visible { 1 } else { 60 }
}

/// How long from `since_epoch` (wall time) until the next whole multiple of
/// `bucket_secs` seconds, when a reading of that grain changes.
pub(super) fn until_next_time_bucket(since_epoch: Duration, bucket_secs: u64) -> Duration {
    let bucket = Duration::from_secs(bucket_secs.max(1));
    let into = Duration::from_nanos(
        since_epoch
            .as_nanos()
            .checked_rem(bucket.as_nanos())
            .and_then(|into| u64::try_from(into).ok())
            .unwrap_or_default(),
    );
    bucket.saturating_sub(into)
}
