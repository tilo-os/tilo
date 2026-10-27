//! Keyboard, pointer and assistive-technology input on the player: key
//! bindings, clicks on the controls bar and menus, and the semantic controls
//! exposed to screen readers with the actions they accept.

use super::{
    KEY_SEEK_STEP, MenuHit, OpenMenu, PlayerSettingControl, PlayerSettingEvent, VideoPlayer,
    format_ms,
};
use crate::num::f64_of_u64;
use vello::kurbo::Rect;
use winit::event::ElementState;
use winit::keyboard::{Key, NamedKey};

impl VideoPlayer {
    /// Route one logical keyboard input through the focused player surface.
    ///
    /// The application shell can use this same narrow input surface instead
    /// of manufacturing platform-owned `winit::KeyEvent` values. Returning
    /// `true` means the player consumed the key.
    pub fn handle_key_input(&mut self, logical_key: &Key, state: ElementState) -> bool {
        if !self.focused || state != ElementState::Pressed {
            return false;
        }
        match logical_key {
            Key::Named(NamedKey::Space) if self.has_media() => {
                self.toggle_play();
                self.show_controls();
            }
            Key::Named(NamedKey::ArrowLeft) if self.has_media() => {
                let step = u64::try_from(KEY_SEEK_STEP.as_millis()).unwrap_or(u64::MAX);
                self.seek_to_ms(self.position_ms.saturating_sub(step));
                self.show_controls();
            }
            Key::Named(NamedKey::ArrowRight) if self.has_media() => {
                let step = u64::try_from(KEY_SEEK_STEP.as_millis()).unwrap_or(u64::MAX);
                self.seek_to_ms(self.position_ms.saturating_add(step));
                self.show_controls();
            }
            Key::Character(c) if c.eq_ignore_ascii_case("f") => self.toggle_fullscreen(),
            Key::Character(c) if c.eq_ignore_ascii_case("m") && self.has_media() => {
                self.toggle_mute();
                self.show_controls();
            }
            Key::Named(NamedKey::ArrowUp) if self.has_media() => {
                self.set_volume(self.volume + 0.05);
                self.show_controls();
            }
            Key::Named(NamedKey::ArrowDown) if self.has_media() => {
                self.set_volume(self.volume - 0.05);
                self.show_controls();
            }
            Key::Named(NamedKey::Home) if self.has_media() => {
                self.seek_to_ms(0);
                self.show_controls();
            }
            Key::Named(NamedKey::End) if self.has_media() => {
                self.seek_to_ms(self.duration_ms);
                self.show_controls();
            }
            Key::Named(NamedKey::Escape) if self.open_menu.is_some() => {
                self.open_menu = None;
                self.gear_expanded = None;
                self.dirty = true;
            }
            _ => return false,
        }
        true
    }

    /// Routes a left-press at `self.last_cursor` through the settings menu,
    /// the setting icons + gear, then the standard transport controls, in
    /// that priority order.
    pub(super) fn press_at_cursor(&mut self) -> bool {
        let p = self.last_cursor;

        // An open menu owns the click: a row wins, a click off the menu (and
        // off the buttons that could re-open it) dismisses it.
        if self.open_menu.is_some() {
            if let Some(hit) = self
                .menu_hit_rects
                .iter()
                .find(|(_, r)| r.contains(p))
                .map(|(h, _)| *h)
            {
                self.activate_menu_hit(hit);
                return true;
            }
            let on_gear = self.gear_button_rect.width() > 0.0 && self.gear_button_rect.contains(p);
            let on_icon = self.setting_icon_rects.iter().any(|(_, r)| r.contains(p));
            if !on_gear && !on_icon {
                self.open_menu = None;
                self.gear_expanded = None;
                self.dirty = true;
                return true;
            }
        }

        if self.gear_button_rect.width() > 0.0 && self.gear_button_rect.contains(p) {
            self.open_menu = if self.open_menu == Some(OpenMenu::Gear) {
                None
            } else {
                Some(OpenMenu::Gear)
            };
            self.gear_expanded = None;
            self.dirty = true;
            return true;
        }

        if let Some((idx, _)) = self
            .setting_icon_rects
            .iter()
            .find(|(_, r)| r.contains(p))
            .copied()
        {
            self.activate_setting_icon(idx);
            return true;
        }

        if self.play_button_rect.contains(p) {
            if self.has_media() {
                self.toggle_play();
            }
            return true;
        }
        if self.fullscreen_button_rect.contains(p) {
            self.toggle_fullscreen();
            return true;
        }
        if self.volume_track_rect.width() > 0.0 && self.volume_track_rect.contains(p) {
            let frac = ((p.x - self.volume_track_rect.x0) / self.volume_track_rect.width())
                .clamp(0.0, 1.0);
            // `frac` is clamped to 0.0..=1.0; `set_volume` clamps again, so
            // the narrowing to `f32` cannot lose meaningful precision.
            #[allow(
                clippy::cast_possible_truncation,
                reason = "frac is clamped to 0.0..=1.0; f32 precision is sufficient for a volume fraction"
            )]
            self.set_volume(frac as f32);
            return true;
        }
        if self.mute_button_rect.width() > 0.0 && self.mute_button_rect.contains(p) {
            if self.has_media() {
                self.toggle_mute();
            }
            return true;
        }
        if self.scrub_track_rect.width() > 0.0 && self.scrub_track_rect.contains(p) {
            self.scrubbing = true;
            self.seek_from_cursor();
            return true;
        }

        // A plain click on the video surface toggles play/pause.
        if self.has_media() {
            self.toggle_play();
        }
        true
    }

    pub(super) fn activate_menu_hit(&mut self, hit: MenuHit) {
        match hit {
            MenuHit::Setting(idx) => {
                let Some(s) = self.settings.get(idx) else {
                    return;
                };
                match &s.control {
                    PlayerSettingControl::Toggle { .. } => self.toggle_setting(idx),
                    PlayerSettingControl::Button => {
                        let Some(id) = self.settings.get(idx).map(|s| s.id.clone()) else {
                            return;
                        };
                        self.setting_events
                            .push(PlayerSettingEvent::Activated { id });
                        self.open_menu = None;
                        self.dirty = true;
                    }
                    PlayerSettingControl::Choice { .. } => {
                        self.gear_expanded = if self.gear_expanded == Some(idx) {
                            None
                        } else {
                            Some(idx)
                        };
                        self.dirty = true;
                    }
                }
            }
            MenuHit::Option(idx, oi) => self.choose_option(idx, oi),
        }
    }

    pub(super) fn activate_setting_icon(&mut self, idx: usize) {
        let Some(s) = self.settings.get(idx) else {
            return;
        };
        match &s.control {
            PlayerSettingControl::Toggle { .. } => self.toggle_setting(idx),
            PlayerSettingControl::Button => {
                let Some(id) = self.settings.get(idx).map(|s| s.id.clone()) else {
                    return;
                };
                self.setting_events
                    .push(PlayerSettingEvent::Activated { id });
                self.dirty = true;
            }
            PlayerSettingControl::Choice { .. } => {
                self.open_menu = if self.open_menu == Some(OpenMenu::Choice(idx)) {
                    None
                } else {
                    Some(OpenMenu::Choice(idx))
                };
                self.dirty = true;
            }
        }
    }

    pub(super) fn toggle_setting(&mut self, idx: usize) {
        let Some(s) = self.settings.get_mut(idx) else {
            return;
        };
        let PlayerSettingControl::Toggle { on } = &mut s.control else {
            return;
        };
        *on = !*on;
        let new_on = *on;
        let Some(id) = self.settings.get(idx).map(|s| s.id.clone()) else {
            return;
        };
        self.setting_events
            .push(PlayerSettingEvent::Toggled { id, on: new_on });
        self.dirty = true;
    }

    pub(super) fn choose_option(&mut self, idx: usize, oi: usize) {
        let Some(s) = self.settings.get_mut(idx) else {
            return;
        };
        let PlayerSettingControl::Choice { options, selected } = &mut s.control else {
            return;
        };
        if oi >= options.len() {
            return;
        }
        *selected = oi;
        let Some(id) = self.settings.get(idx).map(|s| s.id.clone()) else {
            return;
        };
        self.setting_events
            .push(PlayerSettingEvent::Chose { id, index: oi });
        self.open_menu = None;
        self.gear_expanded = None;
        self.dirty = true;
    }

    pub(super) fn seek_from_cursor(&mut self) {
        if self.scrub_track_rect.width() <= 0.0 {
            return;
        }
        let frac = ((self.last_cursor.x - self.scrub_track_rect.x0)
            / self.scrub_track_rect.width())
        .clamp(0.0, 1.0);
        // `frac` is clamped to 0.0..=1.0, so the product stays within
        // `0..=duration_ms` and is always non-negative — the `u64` cast
        // cannot truncate or flip sign.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "frac is clamped to 0.0..=1.0, so frac * duration_ms stays within 0..=duration_ms"
        )]
        let target = (frac * f64_of_u64(self.duration_ms)) as u64;
        self.seek_to_ms(target);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the accessibility tree intentionally inventories the complete transport and settings surface in one order-matched function"
    )]
    pub(super) fn semantic_controls(&self) -> Vec<(String, crate::a11y::AccessNode, Rect)> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let mut controls = Vec::new();
        let focused = |target: &str| self.a11y_focus.as_deref() == Some(target);
        let focusable = |target: &str| AccessState {
            focusable: true,
            focused: focused(target),
            ..Default::default()
        };

        if self.has_media() {
            let play_target = "transport.play-pause".to_string();
            controls.push((
                play_target.clone(),
                AccessNode::new(Role::Button)
                    .with_name(if self.is_playing() { "Pause" } else { "Play" })
                    .with_description("Space")
                    .with_action_target(play_target.clone())
                    .with_state(AccessState {
                        disabled: !self.transport_enabled,
                        ..focusable(&play_target)
                    }),
                self.play_button_rect,
            ));

            let seek_target = "transport.seek".to_string();
            controls.push((
                seek_target.clone(),
                AccessNode::new(Role::Slider)
                    .with_name("Playback position")
                    .with_value(format!(
                        "{} of {}",
                        format_ms(self.position_ms),
                        format_ms(self.duration_ms)
                    ))
                    .with_description("Left and Right Arrow seek five seconds")
                    .with_action_target(seek_target.clone())
                    .with_state(AccessState {
                        disabled: !self.transport_enabled,
                        ..focusable(&seek_target)
                    }),
                self.scrub_track_rect,
            ));

            let mute_target = "audio.mute".to_string();
            controls.push((
                mute_target.clone(),
                AccessNode::new(Role::ToggleButton)
                    .with_name(if self.muted { "Unmute" } else { "Mute" })
                    .with_description("M")
                    .with_action_target(mute_target.clone())
                    .with_state(AccessState {
                        checked: Some(self.muted),
                        ..focusable(&mute_target)
                    }),
                self.mute_button_rect,
            ));

            let volume_target = "audio.volume".to_string();
            controls.push((
                volume_target.clone(),
                AccessNode::new(Role::Slider)
                    .with_name("Volume")
                    .with_value(format!("{}%", (self.effective_volume() * 100.0).round()))
                    .with_action_target(volume_target.clone())
                    .with_state(focusable(&volume_target)),
                self.volume_track_rect,
            ));
        }

        for (idx, rect) in &self.setting_icon_rects {
            let Some(setting) = self.settings.get(*idx) else {
                continue;
            };
            let target = format!("setting:{}", setting.id);
            let mut state = focusable(&target);
            let (role, value) = match &setting.control {
                PlayerSettingControl::Toggle { on } => {
                    state.checked = Some(*on);
                    (Role::ToggleButton, None)
                }
                PlayerSettingControl::Choice { options, selected } => {
                    state.expanded = Some(self.open_menu == Some(OpenMenu::Choice(*idx)));
                    (Role::ComboBox, options.get(*selected).cloned())
                }
                PlayerSettingControl::Button => (Role::Button, None),
            };
            controls.push((
                target.clone(),
                AccessNode::new(role)
                    .with_name(setting.name.clone())
                    .with_optional_value(value)
                    .with_action_target(target.clone())
                    .with_state(state),
                *rect,
            ));
        }

        if self.gear_button_rect.width() > 0.0 {
            let target = "settings.menu".to_string();
            controls.push((
                target.clone(),
                AccessNode::new(Role::Button)
                    .with_name("Playback settings")
                    .with_action_target(target.clone())
                    .with_state(AccessState {
                        expanded: Some(self.open_menu == Some(OpenMenu::Gear)),
                        ..focusable(&target)
                    }),
                self.gear_button_rect,
            ));
        }

        for (hit, rect) in &self.menu_hit_rects {
            match hit {
                MenuHit::Setting(idx) => {
                    let Some(setting) = self.settings.get(*idx) else {
                        continue;
                    };
                    let target = format!("menu-setting:{}", setting.id);
                    let mut state = focusable(&target);
                    let value = match &setting.control {
                        PlayerSettingControl::Toggle { on } => {
                            state.checked = Some(*on);
                            Some(if *on { "On".into() } else { "Off".into() })
                        }
                        PlayerSettingControl::Choice { options, selected } => {
                            state.expanded = Some(self.gear_expanded == Some(*idx));
                            options.get(*selected).cloned()
                        }
                        PlayerSettingControl::Button => None,
                    };
                    controls.push((
                        target.clone(),
                        AccessNode::new(Role::MenuItem)
                            .with_name(setting.name.clone())
                            .with_optional_value(value)
                            .with_action_target(target.clone())
                            .with_state(state),
                        *rect,
                    ));
                }
                MenuHit::Option(idx, option) => {
                    let Some(setting) = self.settings.get(*idx) else {
                        continue;
                    };
                    let PlayerSettingControl::Choice { options, selected } = &setting.control
                    else {
                        continue;
                    };
                    let Some(label) = options.get(*option) else {
                        continue;
                    };
                    let target = format!("setting-option:{}:{option}", setting.id);
                    controls.push((
                        target.clone(),
                        AccessNode::new(Role::MenuItem)
                            .with_name(label.clone())
                            .with_action_target(target.clone())
                            .with_state(AccessState {
                                selected: *selected == *option,
                                ..focusable(&target)
                            }),
                        *rect,
                    ));
                }
            }
        }

        let fullscreen_target = "presentation.fullscreen".to_string();
        controls.push((
            fullscreen_target.clone(),
            AccessNode::new(Role::Button)
                .with_name(if self.fullscreen {
                    "Exit fullscreen"
                } else {
                    "Enter fullscreen"
                })
                .with_description("F")
                .with_action_target(fullscreen_target.clone())
                .with_state(focusable(&fullscreen_target)),
            self.fullscreen_button_rect,
        ));
        controls
    }

    pub(super) fn setting_index(&self, id: &str) -> Option<usize> {
        self.settings.iter().position(|setting| setting.id == id)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "all semantic targets share one auditable dispatch table matching semantic_controls"
    )]
    pub(super) fn activate_semantic_target(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
    ) -> bool {
        use crate::a11y::Action;
        match target {
            "transport.play-pause"
                if self.has_media()
                    && self.transport_enabled
                    && matches!(action, Action::Activate | Action::ToggleCheck) =>
            {
                self.toggle_play();
                self.show_controls();
                true
            }
            "transport.seek" if self.has_media() && self.transport_enabled => match action {
                Action::Increment => {
                    self.seek_to_ms(self.position_ms.saturating_add(5_000));
                    true
                }
                Action::Decrement => {
                    self.seek_to_ms(self.position_ms.saturating_sub(5_000));
                    true
                }
                Action::SetValue(value) => value
                    .parse::<f64>()
                    .ok()
                    .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
                    .is_some_and(|seconds| {
                        let millis = crate::time::Duration::from_secs_f64(seconds).as_millis();
                        self.seek_to_ms(u64::try_from(millis).unwrap_or(u64::MAX));
                        true
                    }),
                _ => false,
            },
            "audio.mute"
                if self.has_media() && matches!(action, Action::Activate | Action::ToggleCheck) =>
            {
                self.toggle_mute();
                true
            }
            "audio.volume" if self.has_media() => match action {
                Action::Increment => {
                    self.set_volume(self.volume + 0.05);
                    true
                }
                Action::Decrement => {
                    self.set_volume(self.volume - 0.05);
                    true
                }
                Action::SetValue(value) => {
                    let normalized = value
                        .trim()
                        .strip_suffix('%')
                        .and_then(|raw| raw.parse::<f32>().ok().map(|percent| percent / 100.0));
                    value
                        .parse::<f32>()
                        .ok()
                        .or(normalized)
                        .is_some_and(|volume| {
                            self.set_volume(volume);
                            true
                        })
                }
                _ => false,
            },
            "presentation.fullscreen" if matches!(action, Action::Activate) => {
                self.toggle_fullscreen();
                true
            }
            "settings.menu" => match action {
                Action::Activate | Action::Expand => {
                    self.open_menu = Some(OpenMenu::Gear);
                    self.reveal_controls();
                    self.dirty = true;
                    true
                }
                Action::Collapse => {
                    self.open_menu = None;
                    self.gear_expanded = None;
                    self.dirty = true;
                    true
                }
                _ => false,
            },
            _ => {
                if let Some(id) = target.strip_prefix("setting:")
                    && let Some(index) = self.setting_index(id)
                    && matches!(
                        action,
                        Action::Activate | Action::ToggleCheck | Action::Expand
                    )
                {
                    self.activate_setting_icon(index);
                    return true;
                }
                if let Some(id) = target.strip_prefix("menu-setting:")
                    && let Some(index) = self.setting_index(id)
                    && matches!(
                        action,
                        Action::Activate | Action::ToggleCheck | Action::Expand
                    )
                {
                    self.activate_menu_hit(MenuHit::Setting(index));
                    return true;
                }
                if let Some(rest) = target.strip_prefix("setting-option:")
                    && let Some((id, option)) = rest.rsplit_once(':')
                    && let Some(index) = self.setting_index(id)
                    && let Ok(option) = option.parse::<usize>()
                    && matches!(action, Action::Activate | Action::ToggleCheck)
                {
                    self.choose_option(index, option);
                    return true;
                }
                false
            }
        }
    }
}
