//! The bar's status cluster: the tray pill's slots and badges, the volume,
//! network and battery glyphs, the media indicator, the running applications'
//! icons and the presses they answer, and the status the bar caches on.

use super::*;

impl TopBarChrome {
    /// How many unread notifications the application with this verified id owns: what the bar shows
    /// on its own icon.
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

    /// The slots the cluster shows: each a reading the machine has, the bell always. A reading that
    /// never arrived contributes nothing rather than a plausible default.
    pub(super) fn tray_slots(&self) -> Vec<ui::widget::StatusSlot> {
        use ui::widget::StatusSlot;
        let mut slots = vec![StatusSlot::Bell {
            unread: self.bell.unread,
            quiet: self.bell.quiet,
        }];
        if let Some(_volume) = self.status.volume {
            slots.push(StatusSlot::Volume {
                silent: self.status.audio_is_silent(),
            });
        }
        if let Some(link) = self.status.link.as_ref() {
            slots.push(StatusSlot::Link {
                kind: link.kind,
                bars: link.bars(),
            });
        }
        if let Some(percent) = self.status.battery_percent() {
            slots.push(StatusSlot::Battery {
                percent,
                charging: self.status.battery_is_charging(),
            });
        }
        slots
    }

    /// The tray's status cluster as it shows now: its slots, the battery's reading and whether
    /// the pointer lights it.
    fn tray_face<'a>(
        &self,
        slots: &'a [ui::widget::StatusSlot],
    ) -> ui::widget::StatusClusterFace<'a> {
        use ui::widget::{StatusCluster, StatusClusterFace};
        // The battery's percentage is drawn only where the machine has a reading: an unknown
        // battery contributes nothing, not a zero.
        let status = match self.status.battery_percent() {
            Some(percent) => {
                StatusCluster::new().battery(percent, self.status.battery_is_charging())
            }
            None => StatusCluster::new(),
        };
        // One pill, two targets: the bell at its leading end and the rest of the cluster. Either
        // under the pointer lights the pill, as one control on the bar.
        let hovered = matches!(
            self.hovered_control().as_deref(),
            Some(Self::CONTROL_NOTIFICATIONS | Self::CONTROL_QUICK_SETTINGS)
        );
        StatusClusterFace::new(slots, status)
            .hovered(hovered)
            .at_height(BAR_CONTROL_PX)
    }

    /// Where the tray pill stands at the bar's end, and its two targets: the bell and the
    /// readings beside it. Returns its width.
    pub(super) fn place_tray_pill(
        &mut self,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
    ) -> f64 {
        use ui::widget::StatusClusterFace;
        let tokens = &theme.tokens;
        let pad_x = f64::from(tokens.spacing.s3);
        let rx = self.rect.x1 - pad_x;
        let slots = self.tray_slots();
        let face = self.tray_face(&slots);
        let tray_w = face.width(text_renderer, theme);
        self.tray_pill_rect =
            StatusClusterFace::place_at(rx - tray_w, center_y, tray_w, BAR_CONTROL_PX);
        let layout = face.layout(text_renderer, theme, self.tray_pill_rect);
        self.bell_badge_rect = layout.badge;
        self.tray_hit_rect = Rect::new(
            self.tray_pill_rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y1,
        );
        // The bell's slot is the cluster's first; a press there opens the notification centre, and
        // a press on the readings beside it opens quick settings. The bell's target takes the
        // pill's leading padding and stops halfway to the next slot, and the readings' target
        // starts where it ends, so the two meet without overlapping. A host with no quick settings
        // has one target: the whole pill.
        let bell = layout.slots.first().copied().unwrap_or(Rect::ZERO);
        self.bell_hit_rect = if self.bell.only {
            self.tray_hit_rect
        } else if bell.is_zero_area() {
            Rect::ZERO
        } else {
            let next = layout
                .slots
                .iter()
                .skip(1)
                .find(|slot| !slot.is_zero_area())
                .copied();
            let right = next.map_or_else(
                || bell.x1 + f64::from(tokens.spacing.s2),
                |next| f64::midpoint(bell.x1, next.x0),
            );
            // The unread count sits on the bell's shoulder and is the bell's too.
            let right = if layout.badge.is_zero_area() {
                right
            } else {
                right.max(layout.badge.x1 + f64::from(tokens.spacing.s1))
            };
            // The bell's target never takes the readings' slots.
            let right = next.map_or(right, |next| right.min(next.x0));
            Rect::new(
                self.tray_pill_rect.x0,
                self.tray_pill_rect.y0,
                right.min(self.tray_pill_rect.x1),
                self.tray_pill_rect.y1,
            )
        };
        // The readings' target begins where the bell's ends.
        if !self.bell_hit_rect.is_zero_area() && self.bell_hit_rect != self.tray_hit_rect {
            self.tray_hit_rect.x0 = self.bell_hit_rect.x1;
        }
        tray_w
    }

    /// Paint the tray pill where [`Self::place_tray_pill`] stood it, through the shared status
    /// cluster.
    pub(super) fn paint_tray_pill(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) {
        let slots = self.tray_slots();
        let _ = self.tray_face(&slots).paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            self.tray_pill_rect,
            theme.tokens.colors.surfaces.base,
        );
    }

    /// Where active microphone/camera use stands, leftward from `rx`: a bar chip
    /// carrying the microphone and camera glyphs and, while it is fresh or
    /// pointed at, the verified app's name. Clicking opens Quick Settings,
    /// whose privacy action can disconnect the active `PipeWire` clients.
    /// Returns the x cursor position the avatar stack should grow leftward
    /// from.
    pub(super) fn place_media_indicator(
        &mut self,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        rx: f64,
    ) -> f64 {
        use ui::icons::Icon;
        use ui::widget::BarChipFace;
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        self.placed.media = None;
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
        let glyphs: Vec<Icon> = [
            has_mic.then_some(Icon::Mic),
            has_camera.then_some(Icon::Camera),
        ]
        .into_iter()
        .flatten()
        .collect();
        let label = if self.media_expanded {
            let suffix = if labels.len() > 1 {
                format!(" +{}", labels.len().saturating_sub(1))
            } else {
                String::new()
            };
            format!("{}{suffix}", labels.first().copied().unwrap_or_default())
        } else {
            String::new()
        };
        let name = labels.first().copied().unwrap_or_default();
        let live = colors.semantic.success;
        let width = super::placed::MediaPlaced::chip_of(name, &glyphs, &label, None)
            .width(text_renderer, theme);
        self.media_indicator_rect =
            BarChipFace::place_at(rx - width, center_y, width, BAR_CONTROL_PX);
        let tone = (self.media_expanded && !labels.is_empty()).then_some(live);
        self.placed.media = Some(super::placed::MediaPlaced {
            name: name.to_owned(),
            glyphs,
            label,
            tone,
        });
        self.media_indicator_rect.x0 - f64::from(tokens.spacing.s2)
    }

    /// Paint the media indicator where [`Self::place_media_indicator`] stood it.
    pub(super) fn paint_media_indicator(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) {
        let Some(media) = self.placed.media.as_ref() else {
            return;
        };
        let _ = super::placed::MediaPlaced::chip_of(
            &media.name,
            &media.glyphs,
            &media.label,
            media.tone,
        )
        .paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            self.media_indicator_rect,
            theme.tokens.colors.surfaces.base,
        );
    }

    /// Pulled at the top of [`Widget::draw`], not pushed by `update` , so a just-dropped battery
    /// percent is drawn and counted as a change in the same pass. Also tracks the media-use
    /// signature so a newly-started capture announces itself as a named pill before settling into a
    /// dot.
    pub(super) fn refresh_status_and_media_state(&mut self) {
        self.status = self.live_status();
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

    /// Where running applications' status-notifier icons stand, leftward from `rx` beside the
    /// system tray pill. `Passive` items stay hidden, as the protocol asks. Returns where the next
    /// cluster may end.
    pub(super) fn place_app_tray(&mut self, theme: &Theme, center_y: f64, rx: f64) -> f64 {
        let tokens = &theme.tokens;
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
            self.placed.app_tray = items;
            return rx;
        }
        let slot = f64::from(tokens.spacing.s6);
        let gap = f64::from(tokens.spacing.s1);
        let mut right = rx;
        for item in items.iter().rev() {
            let hit = Rect::new(
                right - slot,
                center_y - BAR_CONTROL_PX / 2.0,
                right,
                center_y + BAR_CONTROL_PX / 2.0,
            );
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
        self.placed.app_tray = items;
        right - f64::from(tokens.spacing.s3) + gap
    }

    /// Paint the application tray's icons where [`Self::place_app_tray`] stood them: one that
    /// needs attention carries a warning dot.
    pub(super) fn paint_app_tray(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let icon_px = 18.0_f64;
        let items = std::mem::take(&mut self.placed.app_tray);
        // The slots were placed right to left.
        for (index, item) in items.iter().rev().enumerate() {
            let Some(hit) = self.app_tray.get(index).map(|slot| slot.rect) else {
                continue;
            };
            let icon_rect = Rect::from_center_size(hit.center(), (icon_px, icon_px));
            // A tray icon is a bar control: it lifts under the pointer and,
            // reached from the keyboard, shows the bar's focus ring through
            // `control_rect`. Its own picture replaces the glyph.
            let hovered = self
                .last_cursor
                .is_some_and(|point| hit.contains(point) && self.rect.contains(point));
            // The lift is the bar control's own, painted by its face: no surface of the tray icon's
            // own.
            ui::widget::BarControlFace::new(&item.title)
                .hovered(hovered)
                .paint_lift(scene, transform, hit, theme, tokens.colors.surfaces.base);
            self.draw_app_tray_icon(scene, transform, theme, text_renderer, item, icon_rect);
            // The application's own unread count: the bell's total says how many there are, this
            // says whose.
            let unread = item
                .owner_app
                .as_deref()
                .map_or(0, |app| self.app_badge(app));
            if unread > 0 {
                ui::widget::paint_unread_badge(
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
        }
        self.placed.app_tray = items;
    }

    /// One tray item's image, or its letter badge, in `icon_rect` .
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
                // The letter is one centred line, in the shared text-line painter: no bare run of
                // its own.
                let label = letter.to_string();
                let style = tokens.typography.style(TextRole::Caption);
                let line = ui::widget::TextLineFace::styled(&label, &style, TextRole::Caption)
                    .weight(TextWeight::Semibold);
                line.paint(
                    &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                    icon_rect,
                    colors.text_primary,
                );
            }
        }
    }

    /// A press on an application tray icon: the primary button activates the item (or asks for its
    /// menu when it has no primary action), the secondary button asks for its menu, the middle one
    /// runs its secondary action.
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

    /// The bar target naming the tray item addressed by `key` .
    pub(super) fn tray_target(key: &str) -> String {
        format!("{}{key}", Self::APP_TRAY_TARGET)
    }

    /// The tray key a bar target names, when it names one.
    pub(super) fn tray_target_key(target: &str) -> Option<&str> {
        target.strip_prefix(Self::APP_TRAY_TARGET)
    }
}
