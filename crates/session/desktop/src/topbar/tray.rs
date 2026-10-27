//! The bar's status cluster: the tray pill's slots and badges, the volume,
//! network and battery glyphs, the media indicator, the running applications'
//! icons and the presses they answer, and the status the bar caches on.

use super::*;

impl TopBarChrome {
    /// How many unread notifications the application with this verified id
    /// owns: what the bar shows on its own icon.
    pub(super) fn app_badge(&self, app_id: &str) -> u32 {
        self.chrome
            .as_ref()
            .map_or(0, |chrome| chrome.app_notification_badge(app_id))
    }

    pub(super) fn app_tray_revision(&self) -> u64 {
        self.chrome
            .as_ref()
            .map_or(0, |chrome| chrome.app_tray_revision())
    }

    /// The status reading quantised to what the tray paints.
    pub(super) fn status_key(&self) -> StatusKey {
        status_key_of(&self.status)
    }

    pub(super) fn live_status(&self) -> crate::chrome::SystemStatus {
        match self.chrome.as_ref() {
            Some(c) => c.snapshot().status,
            None => self.status.clone(),
        }
    }

    pub fn tray_pill_rect(&self) -> Rect {
        self.tray_pill_rect
    }

    /// A gauge, not a fixed glyph — the fill level is readable without
    /// reading the number.
    pub(super) fn draw_battery_gauge(
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        percent: u8,
        charging: bool,
        hue: Color,
        colors: &ui::tokens::colors::Colors,
    ) {
        // lucide's battery is a 16×12 body at x=2,y=6 in a 24 box, with
        // the nub from x=22. Keep those ratios so the gauge lines up
        // optically with the neighbouring stroked glyphs.
        let s = rect.width().min(rect.height()) / 24.0;
        let ox = rect.x0 + (rect.width() - 24.0 * s) / 2.0;
        let oy = rect.y0 + (rect.height() - 24.0 * s) / 2.0;
        let px = |x: f64| ox + x * s;
        let py = |y: f64| oy + y * s;
        let stroke_w = (ui::icons::LUCIDE_DEFAULT_STROKE_PX).max(1.0);

        let body = RoundedRect::new(px(2.0), py(6.0), px(18.0), py(18.0), 2.0 * s);
        scene.stroke(
            &vello::kurbo::Stroke::new(stroke_w),
            transform,
            colors.text_dim,
            None,
            &body,
        );
        // Terminal nub.
        scene.stroke(
            &vello::kurbo::Stroke::new(stroke_w).with_caps(vello::kurbo::Cap::Round),
            transform,
            colors.text_dim,
            None,
            &vello::kurbo::Line::new(
                vello::kurbo::Point::new(px(21.0), py(10.0)),
                vello::kurbo::Point::new(px(21.0), py(14.0)),
            ),
        );

        // Charge fill, inset one stroke inside the body so the outline
        // stays legible at every level.
        let inset = stroke_w + 0.5;
        let x0 = px(2.0) + inset;
        let x1 = px(18.0) - inset;
        let frac = (f64::from(percent) / 100.0).clamp(0.0, 1.0);
        if frac > 0.0 {
            // A hairline at 1% would disappear; give any nonzero charge
            // at least a visible sliver so "nearly empty" and "empty"
            // do not render identically.
            let w = ((x1 - x0) * frac).max(1.5);
            let fill = RoundedRect::new(
                x0,
                py(6.0) + inset,
                (x0 + w).min(x1),
                py(18.0) - inset,
                1.0 * s,
            );
            scene.fill(Fill::NonZero, transform, hue, None, &fill);
        }

        if charging {
            // Drawn in the surface colour with a faint outline (not an
            // accent) so the bolt reads on top of whatever fill level is
            // underneath it.
            let mut bolt = vello::kurbo::BezPath::new();
            bolt.move_to((px(11.5), py(7.5)));
            bolt.line_to((px(8.0), py(12.6)));
            bolt.line_to((px(11.0), py(12.6)));
            bolt.line_to((px(9.5), py(16.5)));
            bolt.line_to((px(13.5), py(11.0)));
            bolt.line_to((px(10.5), py(11.0)));
            bolt.close_path();
            scene.fill(Fill::NonZero, transform, colors.text_primary, None, &bolt);
            scene.stroke(
                &vello::kurbo::Stroke::new(0.75),
                transform,
                colors.surfaces.raised,
                None,
                &bolt,
            );
        }
    }

    /// The tray's status glyph slots: the bell, always (the unread count
    /// sits on it, and quiet mode silences it), then volume, network link
    /// and battery when known.
    pub(super) fn tray_slot_count(&self) -> usize {
        [
            true,
            self.status.volume.is_some(),
            self.status.link.is_some(),
            self.status.battery_percent().is_some(),
        ]
        .into_iter()
        .filter(|shown| *shown)
        .count()
    }

    /// Sizes the tray pill, paints its body + the critical-battery
    /// highlight, and returns the layout the icon helpers reuse.
    pub(super) fn draw_tray_pill_body(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
    ) -> TrayLayout {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let pad_x = f64::from(tokens.spacing.s3);
        let rx = self.rect.x1 - pad_x;

        // Sizes itself to what's actually knowable; an unarrived
        // reading contributes nothing rather than a plausible default.
        let tray_h = BAR_CONTROL_PX;
        // Square glyph slots the control's height, set close: the cluster is
        // one target, so its glyphs need no air of their own.
        let tray_slot = BAR_CONTROL_PX;
        let tray_pad = f64::from(tokens.spacing.s2);
        let tray_gap = 0.0;
        let icon_px = BAR_GLYPH_PX;
        let pct_style = tokens.typography.style(TextRole::Caption);
        let pct_label = self.status.battery_percent().map(|p| format!("{p}%"));
        let pct_strong = pct_style.clone().with_weight(TextWeight::Semibold);
        let pct_w = pct_label.as_ref().map_or(0.0, |s| {
            f64::from(text_renderer.measure_style(s, None, &pct_strong).0)
        });
        let slots = self.tray_slot_count();
        let mut tray_w = tray_pad * 2.0
            + small_count_to_f64(slots) * tray_slot
            + small_count_to_f64(slots.saturating_sub(1)) * tray_gap;
        if pct_w > 0.0 {
            tray_w += 3.0 + pct_w;
        }

        self.tray_pill_rect = Rect::new(
            rx - tray_w,
            center_y - tray_h / 2.0,
            rx,
            center_y + tray_h / 2.0,
        );
        self.tray_hit_rect = Rect::new(
            self.tray_pill_rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y1,
        );
        // Plain at rest, like every control on the bar: the status glyphs
        // stand on the bar itself.
        let tray =
            RoundedRect::from_rect(self.tray_pill_rect, f64::from(theme.tokens.radii.control));
        // The status cluster is one bar control: it lifts under the
        // pointer.
        paint_bar_selector(
            scene,
            transform,
            &tray,
            ui::chrome::SelectionState::new(
                false,
                self.hovered_control().as_deref() == Some(Self::CONTROL_QUICK_SETTINGS),
                false,
            ),
            theme,
        );
        // At 3% the user needs to notice the whole control, not just read
        // the glyph.
        if self.status.battery_is_critical() {
            scene.fill(
                Fill::NonZero,
                transform,
                colors.semantic.danger.with_alpha_factor(0.18),
                None,
                &tray,
            );
        }

        TrayLayout {
            tray_w,
            tray_slot,
            tray_pad,
            tray_gap,
            icon_px,
            pct_style,
            pct_label,
            center_y,
            dim: colors.text_dim,
            bright: colors.text_primary,
            warning: colors.semantic.warning,
            danger: colors.semantic.danger,
            success: colors.semantic.success,
        }
    }

    /// Draws the bell, quiet with nothing unread, with a compact unread
    /// count on its shoulder, and silenced while fresh banners are held (Do
    /// Not Disturb): the state lives in the status cluster, not in a card
    /// over the person's work. Returns the x cursor position for the next
    /// slot.
    pub(super) fn draw_tray_notification_badge(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        layout: &TrayLayout,
        sx: f64,
    ) -> f64 {
        // A bell that is only the way to the centre rests as quietly as
        // the other status glyphs; something unread or held lights it.
        let ink = if self.bell.unread == 0 && !self.bell.quiet {
            layout.dim
        } else {
            layout.bright
        };
        let glyph = layout.slot_rect(sx);
        ui::icons::draw_icon(
            scene,
            transform,
            glyph,
            if self.bell.quiet {
                ui::icons::Icon::BellOff
            } else {
                ui::icons::Icon::Bell
            },
            ink,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        let next = sx + layout.tray_slot + layout.tray_gap;
        self.bell_badge_rect = Rect::ZERO;
        if self.bell.unread == 0 {
            return next;
        }
        let label = Self::notification_badge_label(self.bell.unread);
        // The count is set at the chrome's numeric-readout size, so it
        // follows the person's text scale like every other bar label.
        let label_style = &layout.pct_style;
        let (label_w, label_h) = text_renderer.measure_style(&label, None, label_style);
        // The count sits on the bell's top-right shoulder inside the bell's
        // own slot, a wider count growing leftward over the glyph, so it
        // takes no room of its own: the cluster keeps its width whatever the
        // count, and the faces to its left never move under a pointer
        // already on its way to one.
        // It stays inside the bar with a hair of air, so a tall count (a
        // large text size) is nudged down rather than cut off at the bar's,
        // and so the screen's, top edge.
        let (badge_w, badge_h) = Self::notification_badge_size(label_w, label_h, theme);
        let inset = f64::from(theme.tokens.spacing.s1) / 2.0;
        let right = glyph.x1 + badge_h / 2.0 + 1.0;
        let top = (glyph.y0 - badge_h / 2.0 + 2.0)
            .min(self.rect.y1 - inset - badge_h)
            .max(self.rect.y0 + inset);
        let badge = Rect::new(right - badge_w, top, right, top + badge_h);
        self.bell_badge_rect = badge;
        // A count badge is a capsule (Selector radius) in the danger hue;
        // its ink is whichever text tone stays legible on that fill, so a
        // light or high-contrast palette is not handed a fixed white.
        scene.fill(
            Fill::NonZero,
            transform,
            layout.danger,
            None,
            &RoundedRect::from_rect(
                badge,
                ui::chrome::ChromeRole::Selector.radius(theme, badge.height()),
            ),
        );
        let ink = theme
            .tokens
            .colors
            .ink_on(layout.danger, layout.bright, 4.5);
        text_renderer.draw_style(
            scene,
            &label,
            ink,
            at(
                transform,
                (
                    badge.x0 + (badge.width() - f64::from(label_w)) / 2.0,
                    badge.y0 + (badge.height() - f64::from(label_h)) / 2.0,
                ),
            ),
            None,
            label_style,
        );
        next
    }

    /// `9+` once the count needs a second digit: the badge sits on the
    /// bell's shoulder, and a wider one would hide the bell it counts for.
    /// The exact number is in the bell's name and the notification centre.
    pub(super) fn notification_badge_label(count: u32) -> String {
        if count > 9 {
            "9+".to_string()
        } else {
            count.to_string()
        }
    }

    /// Badge size for a measured count label: `s1` of padding above and
    /// below the line, `s1` either side, never shorter than `s4` nor
    /// narrower than it is tall (so a single digit is a round dot rather
    /// than a sliver). Whole pixels, so the capsule's edges stay crisp.
    pub(super) fn notification_badge_size(label_w: f32, label_h: f32, theme: &Theme) -> (f64, f64) {
        let s1 = f64::from(theme.tokens.spacing.s1);
        let s4 = f64::from(theme.tokens.spacing.s4);
        let h = (f64::from(label_h) + s1).max(s4).round();
        let w = (f64::from(label_w) + 2.0 * s1).max(h).round();
        (w, h)
    }

    /// Draws the volume glyph if the status has a reading. Returns the
    /// x cursor position for the next slot (unchanged if not drawn).
    pub(super) fn draw_tray_volume_icon(
        scene: &mut Scene,
        transform: Affine,
        status: &crate::chrome::SystemStatus,
        layout: &TrayLayout,
        sx: f64,
    ) -> f64 {
        if status.volume.is_none() {
            return sx;
        }
        let silent = status.audio_is_silent();
        ui::icons::draw_icon(
            scene,
            transform,
            layout.slot_rect(sx),
            if silent {
                ui::icons::Icon::VolumeX
            } else {
                ui::icons::Icon::Volume2
            },
            // Silence is chosen and may want undoing, so it reads
            // full strength; a normal level stays dim.
            if silent { layout.bright } else { layout.dim },
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        sx + layout.tray_slot + layout.tray_gap
    }

    /// Draws the network glyph if the status has a reading. Returns the
    /// x cursor position for the next slot (unchanged if not drawn).
    pub(super) fn draw_tray_network_icon(
        scene: &mut Scene,
        transform: Affine,
        status: &crate::chrome::SystemStatus,
        layout: &TrayLayout,
        sx: f64,
    ) -> f64 {
        let Some(link) = status.link.clone() else {
            return sx;
        };
        let r = layout.slot_rect(sx);
        match link.kind {
            crate::chrome::LinkKind::Wired => {
                ui::icons::draw_icon(
                    scene,
                    transform,
                    r,
                    ui::icons::Icon::EthernetPort,
                    layout.dim,
                    ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
            crate::chrome::LinkKind::Offline => {
                // Warning hue answers "why is nothing loading" at
                // a glance.
                ui::icons::draw_icon(
                    scene,
                    transform,
                    r,
                    ui::icons::Icon::WifiOff,
                    layout.warning,
                    ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
            crate::chrome::LinkKind::Wifi => {
                // `icon_subpaths(Wifi)` yields [dot, outer, middle,
                // inner], so a weak signal lights fewer arcs — the
                // same glyph, read as a meter.
                let bars = crate::chrome::link_bars(&link);
                let dim = layout.dim;
                let faint = dim.with_alpha_factor(0.25);
                ui::icons::draw_icon_layered(
                    scene,
                    transform,
                    r,
                    ui::icons::Icon::Wifi,
                    ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                    |i| {
                        // 0 = base dot (always lit), 1 = outer
                        // (needs 4 bars), 2 = middle (3), 3 = inner (2).
                        let needed = match i {
                            0 => 1,
                            1 => 4,
                            2 => 3,
                            _ => 2,
                        };
                        if bars >= needed { dim } else { faint }
                    },
                );
            }
        }
        sx + layout.tray_slot + layout.tray_gap
    }

    /// Draws the battery gauge + percent label if the status has a
    /// reading.
    pub(super) fn draw_tray_battery_icon(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        status: &crate::chrome::SystemStatus,
        layout: &TrayLayout,
        sx: f64,
        colors: &ui::tokens::colors::Colors,
    ) {
        let Some(pct) = status.battery_percent() else {
            return;
        };
        let charging = status.battery_is_charging();
        let hue = if status.battery_is_critical() {
            layout.danger
        } else if status.battery_is_low() {
            layout.warning
        } else if charging {
            layout.success
        } else {
            layout.dim
        };
        Self::draw_battery_gauge(
            scene,
            transform,
            layout.slot_rect(sx),
            pct,
            charging,
            hue,
            colors,
        );
        let label_x = sx + layout.tray_slot;
        if let Some(label) = layout.pct_label.as_ref() {
            // The gauge is the at-a-glance read; the number is
            // what you check when the gauge worries you.
            let (_, lh) = text_renderer.measure_style(
                label,
                None,
                &layout.pct_style.clone().with_weight(TextWeight::Semibold),
            );
            text_renderer.draw_style(
                scene,
                label,
                if status.battery_is_low() {
                    hue
                } else {
                    layout.bright
                },
                at(
                    transform,
                    (label_x + 3.0, layout.center_y - f64::from(lh) / 2.0),
                ),
                None,
                &layout.pct_style.clone().with_weight(TextWeight::Semibold),
            );
        }
    }

    /// Settings/volume/network/battery glyphs in the already-sized and
    /// already-painted tray pill from [`Self::draw_tray_pill_body`].
    pub(super) fn draw_tray_icons(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        layout: &TrayLayout,
    ) {
        let colors = &theme.tokens.colors;
        let mut sx = self.tray_pill_rect.x0 + layout.tray_pad;
        sx = self.draw_tray_notification_badge(scene, transform, theme, text_renderer, layout, sx);
        sx = Self::draw_tray_volume_icon(scene, transform, &self.status, layout, sx);
        sx = Self::draw_tray_network_icon(scene, transform, &self.status, layout, sx);
        Self::draw_tray_battery_icon(
            scene,
            transform,
            text_renderer,
            &self.status,
            layout,
            sx,
            colors,
        );
    }

    /// Active microphone/camera use, drawn leftward from `rx`. New use is
    /// announced as a named pill for four seconds, then settles into a
    /// quiet dot; hovering that exact dot expands the same verified app
    /// label again. Clicking opens Quick Settings, whose privacy action
    /// can disconnect the active `PipeWire` clients. Returns the x cursor
    /// position the avatar stack should grow leftward from.
    pub(super) fn draw_media_indicator(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        rx: f64,
    ) -> f64 {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        if self.status.media.is_empty() {
            self.media_indicator_rect = Rect::ZERO;
            return rx;
        }
        let mut labels: Vec<&str> = self.status.media.iter().map(|u| u.label.as_str()).collect();
        labels.compact_sort();
        labels.dedup();
        let has_mic = self
            .status
            .media
            .iter()
            .any(|u| u.kind == crate::chrome::MediaKind::Microphone);
        let has_camera = self
            .status
            .media
            .iter()
            .any(|u| u.kind == crate::chrome::MediaKind::Camera);
        let media_color = colors.semantic.success;
        let indicator_h = BAR_CONTROL_PX;
        if self.media_expanded {
            let suffix = if labels.len() > 1 {
                format!(" +{}", labels.len().saturating_sub(1))
            } else {
                String::new()
            };
            let label = format!("{}{suffix}", labels.first().copied().unwrap_or_default());
            let style = tokens.typography.style(TextRole::Caption);
            let (label_w, label_h) = text_renderer.measure_style(
                &label,
                None,
                &style.clone().with_weight(TextWeight::Semibold),
            );
            let icon_count = usize::from(has_mic).saturating_add(usize::from(has_camera));
            let width = 16.0 + small_count_to_f64(icon_count) * 15.0 + f64::from(label_w);
            self.media_indicator_rect = Rect::new(
                rx - width,
                center_y - indicator_h / 2.0,
                rx,
                center_y + indicator_h / 2.0,
            );
            let rr = RoundedRect::from_rect(
                self.media_indicator_rect,
                ui::chrome::ChromeRole::Selector.radius(theme, indicator_h),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                media_color.with_alpha_factor(0.18),
                None,
                &rr,
            );
            let mut ix = self.media_indicator_rect.x0 + 7.0;
            for icon in [
                has_mic.then_some(ui::icons::Icon::Mic),
                has_camera.then_some(ui::icons::Icon::Camera),
            ]
            .into_iter()
            .flatten()
            {
                ui::icons::draw_icon(
                    scene,
                    transform,
                    Rect::new(ix, center_y - 6.0, ix + 12.0, center_y + 6.0),
                    icon,
                    media_color,
                    ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
                ix += 15.0;
            }
            text_renderer.draw_style(
                scene,
                &label,
                colors.text_primary,
                at(transform, (ix + 2.0, center_y - f64::from(label_h) / 2.0)),
                None,
                &style.with_weight(TextWeight::Semibold),
            );
        } else {
            self.media_indicator_rect = Self::draw_media_glyphs(
                scene,
                transform,
                media_color,
                (has_mic, has_camera),
                (rx, center_y),
            );
        }
        self.media_indicator_rect.x0 - f64::from(tokens.spacing.s3)
    }

    /// The settled media indicator ending at `rx`: what is in use as its
    /// glyph, a microphone or a camera, since a bare dot never said what is
    /// live. Returns where it was drawn.
    pub(super) fn draw_media_glyphs(
        scene: &mut Scene,
        transform: Affine,
        color: Color,
        (mic, camera): (bool, bool),
        (rx, center_y): (f64, f64),
    ) -> Rect {
        let icons: Vec<ui::icons::Icon> = [
            mic.then_some(ui::icons::Icon::Mic),
            camera.then_some(ui::icons::Icon::Camera),
        ]
        .into_iter()
        .flatten()
        .collect();
        let width = (small_count_to_f64(icons.len()) * 15.0 + 5.0).max(BAR_CONTROL_PX);
        let rect = Rect::new(
            rx - width,
            center_y - BAR_CONTROL_PX / 2.0,
            rx,
            center_y + BAR_CONTROL_PX / 2.0,
        );
        let mut ix = rect.center().x - (small_count_to_f64(icons.len()) * 15.0 - 3.0) / 2.0;
        for icon in icons {
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(ix, center_y - 6.0, ix + 12.0, center_y + 6.0),
                icon,
                color,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            ix += 15.0;
        }
        rect
    }

    /// Pulled at the top of [`Widget::draw`], not pushed by `update`, so a
    /// just-dropped battery percent is drawn and counted as a change in the
    /// same pass. Also tracks the media-use signature so a newly-started
    /// capture announces itself as a named pill before settling into a dot.
    pub(super) fn refresh_status_and_media_state(&mut self) {
        self.status = self.live_status();
        if let Some(chrome) = &self.chrome {
            let snapshot = chrome.snapshot();
            if snapshot.clock_activity.is_none() && self.clock_overlay_rect != Rect::ZERO {
                self.clock_drag_offset = None;
                self.clock_touch_id = None;
                self.clock_control_focus = None;
                self.cache_fp = CacheFingerprint::default();
            }
        }
        self.bell.unread = self
            .chrome
            .as_ref()
            .map_or(0, |chrome| chrome.snapshot().notification_badge);
        if let Some(chrome) = self.chrome.as_ref() {
            let snapshot = chrome.snapshot();
            self.bell.quiet = snapshot.notifications_quiet;
            self.bell.only = snapshot.notifications_only;
        }
        if let Some(chrome) = self.chrome.as_ref() {
            let glance = chrome.snapshot().activity_glance;
            if glance.pulse != self.activity_pulse {
                self.activity_pulse = glance.pulse;
                self.activity_started =
                    (glance.unread > 0 && glance.headline.is_some()).then(ui::time::now);
            }
            if glance.unread == 0 {
                self.activity_started = None;
            }
            self.activity_headline = glance.headline;
            self.activity_unread = glance.unread;
        }
        let signature: Vec<(u32, u8, String)> = self
            .status
            .media
            .iter()
            .map(|use_| {
                (
                    use_.client_id,
                    match use_.kind {
                        crate::chrome::MediaKind::Microphone => 0,
                        crate::chrome::MediaKind::Camera => 1,
                    },
                    use_.label.clone(),
                )
            })
            .collect();
        if signature != self.media_signature {
            self.media_signature = signature;
            self.media_started = (!self.media_signature.is_empty()).then(ui::time::now);
        }
        let media_hovered = self
            .last_cursor
            .is_some_and(|p| self.media_hit_rect().contains(p));
        self.media_expanded = !self.media_signature.is_empty()
            && (media_hovered
                || self.media_started.is_some_and(|at| {
                    ui::time::now().saturating_duration_since(at) < Duration::from_secs(4)
                }));
    }

    /// Running applications' status-notifier icons, drawn leftward from
    /// `rx` beside the system tray pill. `Passive` items stay hidden, as the
    /// protocol asks; one that needs attention carries a warning dot.
    /// Returns where the next cluster may end.
    pub(super) fn draw_app_tray(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        rx: f64,
    ) -> f64 {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let items: Vec<AppTrayItem> = self
            .chrome
            .as_ref()
            .map(|chrome| chrome.app_tray())
            .unwrap_or_default()
            .into_iter()
            .filter(|item| item.status != AppTrayStatus::Passive)
            .collect();
        self.app_tray.clear();
        self.app_tray_images
            .retain(|key, _| items.iter().any(|item| &item.key == key));
        if items.is_empty() {
            return rx;
        }
        let slot = f64::from(tokens.spacing.s6);
        let gap = f64::from(tokens.spacing.s1);
        let icon_px = 18.0_f64;
        let mut right = rx;
        for item in items.iter().rev() {
            let hit = Rect::new(
                right - slot,
                center_y - BAR_CONTROL_PX / 2.0,
                right,
                center_y + BAR_CONTROL_PX / 2.0,
            );
            let icon_rect = Rect::from_center_size(hit.center(), (icon_px, icon_px));
            // A tray icon is a bar control: it lifts under the pointer and,
            // reached from the keyboard, shows the bar's focus ring through
            // `control_rect`. Its own picture replaces the glyph.
            let hovered = self
                .last_cursor
                .is_some_and(|point| hit.contains(point) && self.rect.contains(point));
            paint_bar_selector(
                scene,
                transform,
                &RoundedRect::from_rect(hit, f64::from(tokens.radii.control)),
                ui::chrome::SelectionState::new(false, hovered, false),
                theme,
            );
            self.draw_app_tray_icon(scene, transform, theme, text_renderer, item, icon_rect);
            // The application's own unread count: the bell's total says how
            // many there are, this says whose.
            let unread = item
                .owner_app
                .as_deref()
                .map_or(0, |app| self.app_badge(app));
            if unread > 0 {
                paint_unread_badge(
                    scene,
                    transform,
                    theme,
                    text_renderer,
                    icon_rect,
                    unread.min(99),
                );
            }
            if item.status == AppTrayStatus::NeedsAttention {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.semantic.warning,
                    None,
                    &vello::kurbo::Circle::new((icon_rect.x1, icon_rect.y0 + 2.0), 3.0),
                );
            }
            self.app_tray.push(AppTraySlot {
                key: item.key.clone(),
                id: item.id.clone(),
                owner_app: item.owner_app.clone(),
                title: item.title.clone(),
                item_is_menu: item.item_is_menu,
                rect: hit,
            });
            right = hit.x0 - gap;
        }
        right - f64::from(tokens.spacing.s3) + gap
    }

    /// One tray item's image, or its letter badge, in `icon_rect`.
    pub(super) fn draw_app_tray_icon(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        item: &AppTrayItem,
        icon_rect: Rect,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let icon_px = icon_rect.width();
        match &item.icon {
            AppTrayIcon::Pixels {
                width,
                height,
                rgba,
            } => {
                let identity = std::sync::Arc::as_ptr(rgba) as usize;
                let image = match self.app_tray_images.get(&item.key) {
                    Some((cached, image)) if *cached == identity => image.clone(),
                    _ => {
                        let image = ui::raster::rgba8(
                            vello::peniko::Blob::new(rgba.clone()),
                            *width,
                            *height,
                        );
                        self.app_tray_images
                            .insert(item.key.clone(), (identity, image.clone()));
                        image
                    }
                };
                let sx = icon_px / f64::from((*width).max(1));
                let sy = icon_px / f64::from((*height).max(1));
                scene.fill(
                    Fill::NonZero,
                    transform
                        .pre_translate((icon_rect.x0, icon_rect.y0).into())
                        .pre_scale_non_uniform(sx, sy),
                    &vello::peniko::ImageBrush::new(image),
                    None,
                    &Rect::new(0.0, 0.0, f64::from(*width), f64::from(*height)),
                );
            }
            AppTrayIcon::Letter(letter) => {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.surfaces.raised,
                    None,
                    &vello::kurbo::Circle::new(icon_rect.center(), icon_px / 2.0),
                );
                let label = letter.to_string();
                let style = tokens.typography.style(TextRole::Caption);
                let (w, h) = text_renderer.measure_style(
                    &label,
                    None,
                    &style.clone().with_weight(TextWeight::Semibold),
                );
                text_renderer.draw_style(
                    scene,
                    &label,
                    colors.text_primary,
                    at(
                        transform,
                        (
                            icon_rect.center().x - f64::from(w) / 2.0,
                            icon_rect.center().y - f64::from(h) / 2.0,
                        ),
                    ),
                    None,
                    &style.with_weight(TextWeight::Semibold),
                );
            }
        }
    }

    /// A press on an application tray icon: the primary button activates
    /// the item (or asks for its menu when it has no primary action), the
    /// secondary button asks for its menu, the middle one runs its
    /// secondary action.
    pub(super) fn handle_app_tray_press(&self, event: &WindowEvent, handled: &mut bool) -> bool {
        let WindowEvent::MouseInput {
            state: winit::event::ElementState::Pressed,
            button,
            ..
        } = event
        else {
            return false;
        };
        let Some(point) = self.last_cursor else {
            return false;
        };
        let Some(slot) = self.app_tray.iter().find(|slot| slot.rect.contains(point)) else {
            return false;
        };
        let (x, y) = pointer_coordinates(point);
        let request = match button {
            winit::event::MouseButton::Left if slot.item_is_menu => {
                AppTrayRequest::ContextMenu { x, y }
            }
            winit::event::MouseButton::Left => AppTrayRequest::Activate { x, y },
            winit::event::MouseButton::Right => AppTrayRequest::ContextMenu { x, y },
            winit::event::MouseButton::Middle => AppTrayRequest::SecondaryActivate { x, y },
            _ => return false,
        };
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.intent(ChromeIntent::AppTray {
                key: slot.key.clone(),
                request,
            });
        }
        *handled = true;
        true
    }

    /// The bar target naming the tray item addressed by `key`.
    pub(super) fn tray_target(key: &str) -> String {
        format!("{}{key}", Self::APP_TRAY_TARGET)
    }

    /// The tray key a bar target names, when it names one.
    pub(super) fn tray_target_key(target: &str) -> Option<&str> {
        target.strip_prefix(Self::APP_TRAY_TARGET)
    }
}

/// An unread count on an application's icon, in the same badge a sidebar's
/// unread count uses: filled in the person's colour, clamped at 99.
pub(super) fn paint_unread_badge(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    text: &mut TextRenderer,
    icon: Rect,
    count: u32,
) {
    let label = count.to_string();
    let height = ui::widget::BadgeFace::height(theme);
    let width = ui::widget::BadgeFace::intrinsic_width(&label, text, theme);
    let centre = Point::new(icon.x1 + 2.0, icon.y0 + 2.0);
    ui::widget::BadgeFace::new(Rect::from_center_size(centre, (width, height)), &label)
        .style(ui::widget::ChipStyle::Live)
        .paint(scene, transform, text, theme);
}

/// Per-frame sizing + palette snapshot for the tray pill, computed once
/// in [`TopBarChrome::draw_tray_pill_body`] and reused by the icon
/// helpers so [`Widget::draw`] stays under the line budget.
pub(super) struct TrayLayout {
    pub(super) tray_w: f64,
    pub(super) tray_slot: f64,
    pub(super) tray_pad: f64,
    pub(super) tray_gap: f64,
    pub(super) icon_px: f64,
    pub(super) pct_style: TextStyle,
    pub(super) pct_label: Option<String>,
    pub(super) center_y: f64,
    pub(super) dim: Color,
    pub(super) bright: Color,
    pub(super) warning: Color,
    pub(super) danger: Color,
    pub(super) success: Color,
}

impl TrayLayout {
    /// The hit/draw rect for the `n`th 18px tray slot starting at `x`.
    fn slot_rect(&self, x: f64) -> Rect {
        Rect::new(
            x + (self.tray_slot - self.icon_px) / 2.0,
            self.center_y - self.icon_px / 2.0,
            x + f64::midpoint(self.tray_slot, self.icon_px),
            self.center_y + self.icon_px / 2.0,
        )
    }
}
