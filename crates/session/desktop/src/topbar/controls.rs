//! What the bar's controls are and where they are: the target names assistive
//! technology and a pointer address, the rect each stands in, what a press
//! activates, and the focus ring that says the keyboard is here.

use super::*;

impl TopBarChrome {
    /// The bar control under the pointer, from the hit rects the last paint
    /// laid down: plain at rest, it lifts while the pointer is over it.
    pub(super) fn hovered_control(&self) -> Option<String> {
        let point = self.last_cursor?;
        if !self.rect.contains(point) {
            return None;
        }
        self.control_targets()
            .into_iter()
            .find(|target| self.control_rect(target).contains(point))
    }

    pub(super) fn control_targets(&self) -> Vec<String> {
        let mut controls: Vec<String> = Vec::new();
        if self.tilo_mark.is_some() {
            controls.push(Self::CONTROL_TILO_MARK.to_string());
        }
        controls.push(Self::CONTROL_WORKSPACE_SETTINGS.to_string());
        if self.shows_strip_name() {
            controls.push(Self::CONTROL_STRIP_NAME.to_string());
        }
        if self.is_compact() {
            return controls;
        }
        controls.push(Self::CONTROL_QUICK_SETTINGS.to_string());
        if self.collaborators_visible() {
            controls.push(Self::CONTROL_COLLABORATORS.to_string());
        }
        if !self.invite_rect.is_zero_area() {
            controls.push(Self::CONTROL_INVITE.to_string());
        }
        if !self.ask.pill.is_zero_area() {
            controls.push(Self::CONTROL_ASK_LATER.to_string());
            controls.push(Self::CONTROL_ASK_ANSWER.to_string());
        }
        if self.own_chip().is_some() {
            controls.push(Self::CONTROL_PROFILE.to_string());
        }
        if !self.media_indicator_rect.is_zero_area() {
            controls.push(Self::CONTROL_MEDIA.to_string());
        }
        // Each running application's tray icon is a control of the bar like
        // any other: Tab reaches it, a reader names it, and it lifts under
        // the pointer.
        controls.extend(
            self.app_tray
                .iter()
                .map(|slot| Self::tray_target(&slot.key)),
        );
        if self.continuity_label().is_some() {
            controls.push(Self::CONTROL_CONTINUITY.to_string());
            controls.push(Self::CONTROL_CONTINUITY_DISMISS.to_string());
        }
        controls
    }

    pub(super) fn control_rect(&self, target: &str) -> Rect {
        match target {
            Self::CONTROL_TILO_MARK => self.tilo_mark_rect,
            Self::CONTROL_WORKSPACE_SETTINGS => self.ws_pill_rect,
            Self::CONTROL_STRIP_NAME => self.ws_strip_rect,
            Self::CONTROL_QUICK_SETTINGS => self.tray_hit_rect,
            Self::CONTROL_PROFILE => self.own_avatar_rect,
            Self::CONTROL_INVITE => self.invite_rect,
            // The people button opens everyone here; the faces beside it
            // each open one person, so they are not this control.
            Self::CONTROL_COLLABORATORS => {
                let people = if self.people_button_rect.is_zero_area() {
                    self.avatar_stack_rect
                } else {
                    self.people_button_rect
                };
                if people.is_zero_area() {
                    self.sharing_indicator_rect
                } else if self.sharing_indicator_rect.is_zero_area() {
                    people
                } else {
                    people.union(self.sharing_indicator_rect)
                }
            }
            Self::CONTROL_MEDIA => self.media_hit_rect(),
            Self::CONTROL_CONTINUITY => self.continuity_rect,
            Self::CONTROL_CONTINUITY_DISMISS => self.continuity_dismiss_rect,
            Self::CONTROL_ASK_ANSWER => self.ask.answer,
            Self::CONTROL_ASK_LATER => self.ask.later,
            target => Self::tray_target_key(target)
                .and_then(|key| {
                    self.app_tray
                        .iter()
                        .find(|slot| slot.key == key)
                        .map(|slot| slot.rect)
                })
                .unwrap_or(Rect::ZERO),
        }
    }

    /// Pointer/semantic target of the media indicator. The painted pill is
    /// a quiet 22 px status mark, which is deliberately smaller than a
    /// control; the target it answers to is still the bar's full control
    /// height (HIG §9.6: a small glyph gets a 32 px hit rect). Zero while
    /// nothing is capturing, so the control disappears with the mark.
    pub(super) fn media_hit_rect(&self) -> Rect {
        if self.media_indicator_rect.is_zero_area() {
            return Rect::ZERO;
        }
        let cy = self.media_indicator_rect.center().y;
        Rect::new(
            self.media_indicator_rect.x0,
            cy - BAR_CONTROL_PX / 2.0,
            self.media_indicator_rect.x1,
            cy + BAR_CONTROL_PX / 2.0,
        )
    }

    /// The status cluster's bell, read out: whether fresh banners are
    /// silenced and how many are unread. `None` when no bell is painted.
    pub(super) fn bell_name(&self) -> Option<String> {
        if self.bell.unread == 0 && !self.bell.quiet {
            return None;
        }
        Some(t_args(
            if self.bell.quiet {
                "topbar-bell-silenced"
            } else {
                "topbar-bell"
            },
            &[("unread", self.bell.unread.into())],
        ))
    }

    pub(super) fn control_name(&self, target: &str) -> String {
        if let Some(key) = Self::tray_target_key(target) {
            // The application's own name for its icon, never its bus
            // address: an item is one app's window or state, said in the
            // words that app chose.
            return self
                .app_tray
                .iter()
                .find(|slot| slot.key == key)
                .map(|slot| slot.title.clone())
                .unwrap_or_else(|| t("topbar-app-tray-item"));
        }
        match target {
            Self::CONTROL_TILO_MARK => t("topbar-tilo-menu"),
            Self::CONTROL_WORKSPACE_SETTINGS => t_args(
                "topbar-workspace-menu",
                &[("workspace", self.workspace_name.as_str().into())],
            ),
            Self::CONTROL_PROFILE => self.profile_control_name(),
            Self::CONTROL_STRIP_NAME => t_args(
                "topbar-strip-name",
                &[("strip", self.strip_name().unwrap_or_default().into())],
            ),
            Self::CONTROL_QUICK_SETTINGS if self.bell.only => t("topbar-open-notifications"),
            Self::CONTROL_QUICK_SETTINGS => t("topbar-open-quick-settings"),
            Self::CONTROL_INVITE => t("topbar-invite"),
            Self::CONTROL_COLLABORATORS => {
                let named = self.sharing_viewers();
                let watching = self.watching();
                let name = if named.is_empty() {
                    t("topbar-show-collaborators")
                } else {
                    let viewers = named
                        .iter()
                        .map(|(peer, name, is_agent, _)| {
                            let kind = match is_agent {
                                Some(true) => "agent",
                                Some(false) => "person",
                                None => "session",
                            };
                            let (view_only, controlled) = self.sharing_targets_for_peer(*peer);
                            let mut access = Vec::new();
                            if !controlled.is_empty() {
                                access.push(t_args(
                                    "topbar-viewer-can-control",
                                    &[("targets", controlled.join(", ").into())],
                                ));
                            }
                            if !view_only.is_empty() {
                                access.push(t_args(
                                    "topbar-viewer-view-only",
                                    &[("targets", view_only.join(", ").into())],
                                ));
                            }
                            t_args(
                                "topbar-viewer-entry",
                                &[
                                    ("name", name.as_str().into()),
                                    ("kind", kind.into()),
                                    ("access", access.join("; ").into()),
                                ],
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("; ");
                    t_args(
                        "topbar-collaborators-visible",
                        &[("viewers", viewers.into())],
                    )
                };
                if watching == 0 {
                    name
                } else {
                    format!(
                        "{name}; {}",
                        t_args("topbar-watching-description", &[("count", watching.into())])
                    )
                }
            }
            Self::CONTROL_MEDIA => t("topbar-capture-privacy"),
            Self::CONTROL_CONTINUITY => self
                .continuity_label()
                .unwrap_or_else(|| t("topbar-control")),
            Self::CONTROL_CONTINUITY_DISMISS => t("continuity-dismiss"),
            Self::CONTROL_ASK_ANSWER => self.chrome_avatar_ask().map_or_else(
                || t("topbar-control"),
                |ask| format!("{}: {}", ask.question, ask.answer),
            ),
            Self::CONTROL_ASK_LATER => t("notification-not-now"),
            _ => t("topbar-control"),
        }
    }

    /// The click half of [`Widget::handle_event`]'s `MouseInput`
    /// branch, split out to keep that function under the line budget.
    pub(super) fn handle_left_click(&mut self, handled: &mut bool) {
        if self.activity_is_visible()
            && self
                .last_cursor
                .is_some_and(|p| self.center_rect.contains(p))
            && let Some(chrome) = self.chrome.as_ref()
        {
            let focus = chrome.snapshot().activity_glance.focus;
            chrome.intent(ChromeIntent::ShowActivity(focus));
            self.activity_started = None;
            *handled = true;
            return;
        }
        if let Some(p) = self.last_cursor {
            let target = if self.continuity_dismiss_rect.contains(p) {
                Some(Self::CONTROL_CONTINUITY_DISMISS)
            } else if self.continuity_rect.contains(p) {
                Some(Self::CONTROL_CONTINUITY)
            } else {
                None
            };
            if let Some(target) = target {
                *handled = self.activate_continuity_control(target);
                return;
            }
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // MouseInput has no position; rely on the last CursorMoved.
        let Some(p) = self.last_cursor else {
            return;
        };
        let intent = if self.tilo_mark.is_some() && self.tilo_mark_rect.contains(p) {
            ChromeIntent::ToggleTiloMenu
        } else if self.ws_strip_rect.contains(p) {
            ChromeIntent::ShowStrips
        } else if self.ws_pill_rect.contains(p) {
            ChromeIntent::ToggleWorkspaceMenu
        } else if self.own_avatar_rect.contains(p) {
            self.avatar_intent()
        } else if self.tray_hit_rect.contains(p) {
            ChromeIntent::ToggleQuickSettings
        } else if self.media_hit_rect().contains(p) {
            ChromeIntent::ShowQuickSettings
        } else if let Some(intent) = self
            .arrival_intent_at(p)
            .or_else(|| self.people_intent_at(p))
        {
            intent
        } else {
            return;
        };
        *handled = chrome.intent(intent);
    }

    /// Keyboard focus is shown; focus a click gave the bar is not.
    pub(super) fn focus_ring_visible(&self) -> bool {
        self.focused && self.focus_origin == FocusOrigin::Keyboard
    }

    pub(super) fn draw_focus_indicator(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if !self.focus_ring_visible() {
            return;
        }
        let Some(target) = self.focused_control.as_deref() else {
            return;
        };
        let rect = self.control_rect(target);
        if rect.is_zero_area() {
            return;
        }
        let ring = RoundedRect::from_rect(
            rect.inset(-2.0),
            ui::chrome::ChromeRole::Selector.radius(theme, rect.height() + 4.0),
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(2.0),
            transform,
            theme.border_focus,
            None,
            &ring,
        );
    }

    pub(super) fn activate_control(&self, target: &str) -> bool {
        if let Some(done) = self.activate_ask_control(target) {
            return done;
        }
        if matches!(
            target,
            Self::CONTROL_CONTINUITY | Self::CONTROL_CONTINUITY_DISMISS
        ) {
            return self.activate_continuity_control(target);
        }
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        if let Some(key) = Self::tray_target_key(target) {
            // A key or a reader's activation does what a left click does:
            // the item's primary action, or its own menu when it has none.
            let rect = self.control_rect(target);
            let (x, y) = pointer_coordinates(rect.center());
            let item_is_menu = self
                .app_tray
                .iter()
                .find(|slot| slot.key == key)
                .is_some_and(|slot| slot.item_is_menu);
            return chrome.intent(ChromeIntent::AppTray {
                key: key.to_string(),
                request: if item_is_menu {
                    AppTrayRequest::ContextMenu { x, y }
                } else {
                    AppTrayRequest::Activate { x, y }
                },
            });
        }
        let intent = match target {
            Self::CONTROL_TILO_MARK if self.tilo_mark.is_some() => ChromeIntent::ToggleTiloMenu,
            Self::CONTROL_WORKSPACE_SETTINGS => ChromeIntent::ToggleWorkspaceMenu,
            Self::CONTROL_STRIP_NAME => ChromeIntent::ShowStrips,
            Self::CONTROL_PROFILE => self.avatar_intent(),
            Self::CONTROL_QUICK_SETTINGS | Self::CONTROL_MEDIA => ChromeIntent::ToggleQuickSettings,
            Self::CONTROL_COLLABORATORS => ChromeIntent::ToggleCollaborators,
            Self::CONTROL_INVITE => ChromeIntent::Invite,
            _ => return false,
        };
        chrome.intent(intent)
    }

    /// Accepting focuses the offered tab here; dismissing hides the offer
    /// until that device's focus moves again. Neither happens on its own.
    pub(super) fn activate_continuity_control(&self, target: &str) -> bool {
        let Some(chrome) = self.chrome.as_ref() else {
            return false;
        };
        chrome.intent(if target == Self::CONTROL_CONTINUITY {
            ChromeIntent::AcceptContinuity
        } else {
            ChromeIntent::DismissContinuity
        })
    }

    pub(super) fn handle_keyboard(&mut self, event: &WindowEvent, handled: &mut bool) {
        use winit::keyboard::{Key, NamedKey};
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return;
        };
        if !self.focused || key.state != winit::event::ElementState::Pressed {
            return;
        }
        let controls = self.control_targets();
        if controls.is_empty() {
            return;
        }
        let current = self
            .focused_control
            .as_deref()
            .and_then(|target| controls.iter().position(|candidate| *candidate == target))
            .unwrap_or(0);
        let focus = match &key.logical_key {
            Key::Named(NamedKey::ArrowLeft) => Some(
                current
                    .checked_sub(1)
                    .unwrap_or_else(|| controls.len().saturating_sub(1)),
            ),
            Key::Named(NamedKey::ArrowRight) => Some(
                current
                    .checked_add(1)
                    .filter(|&index| index < controls.len())
                    .unwrap_or(0),
            ),
            Key::Named(NamedKey::Home) => Some(0),
            Key::Named(NamedKey::End) => Some(controls.len().saturating_sub(1)),
            _ => None,
        };
        if let Some(target) = focus.and_then(|index| controls.get(index)) {
            self.focused_control = Some(target.clone());
            *handled = true;
            return;
        }
        if matches!(
            &key.logical_key,
            Key::Named(NamedKey::Enter | NamedKey::Space)
        ) && let Some(target) = controls.get(current)
        {
            *handled = self.activate_control(target);
        }
    }

    /// Pointer motion: where the pointer is, for clicks, and a floating
    /// clock being dragged. A wheel over the bar is
    /// nobody's: the bar has nothing to scroll and nothing to cycle.
    pub(super) fn handle_pointer_motion(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        handled: &mut bool,
    ) -> bool {
        let WindowEvent::CursorMoved { position, .. } = event else {
            return false;
        };
        let point = Point::new(position.x / scale_factor, position.y / scale_factor);
        self.last_cursor = Some(point);
        if self.clock_touch_id.is_none() && self.move_clock_drag_to(point) {
            *handled = true;
        }
        true
    }

    pub(super) fn handle_left_release(&mut self, event: &WindowEvent, handled: &mut bool) -> bool {
        if !matches!(
            event,
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) {
            return false;
        }
        if self.clock_touch_id.is_none() && self.clock_drag_offset.take().is_some() {
            *handled = true;
        }
        true
    }

    pub(super) fn handle_left_press(&mut self, event: &WindowEvent, handled: &mut bool) {
        if !matches!(
            event,
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) || self.clock_touch_id.is_some()
        {
            return;
        }
        let Some(point) = self.last_cursor else {
            self.handle_left_click(handled);
            return;
        };
        if self.activate_clock_control_at(point) || self.begin_clock_drag_at(point) {
            *handled = true;
            return;
        }
        self.handle_left_click(handled);
    }
}

/// How the bar's focus last arrived, which decides whether its ring shows
/// and whether it holds an auto-hiding bar shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FocusOrigin {
    /// Tab reached the bar, or a key went to it while it had focus.
    Keyboard,
    Pointer,
    /// The shell placed its focus here with no input (the first focusable
    /// control while a window types): nobody is reading the bar.
    Placed,
}

/// The bell in the bar's status cluster.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct TrayBell {
    /// Unread notifications, counted beside the bell.
    pub(super) unread: u32,
    /// Fresh banners are held (Do Not Disturb, presenting, a focus
    /// schedule): the bell is silenced.
    pub(super) quiet: bool,
    /// The host has no quick settings, so the cluster is the bell itself:
    /// always shown, and what the cluster is named.
    pub(super) only: bool,
}

/// Paint a bar control's lift at `shape` in `state`: the design system's
/// bar control ([`ui::widget::BarControlFace::fill`]), plain at rest, half a
/// lift under the pointer, a whole lift while pressed or open. The bar has
/// no surface of its own: its controls stand on the desktop ground and lift
/// off that.
pub(super) fn paint_bar_selector(
    scene: &mut Scene,
    transform: Affine,
    shape: &RoundedRect,
    state: ui::chrome::SelectionState,
    theme: &Theme,
) {
    let face = ui::widget::BarControlFace::new("")
        .open(state.active)
        .pressed(state.pressed)
        .hovered(state.hover);
    if let Some(fill) = face.fill(theme, theme.tokens.colors.surfaces.base) {
        scene.fill(Fill::NonZero, transform, fill, None, shape);
    }
}

/// Whether `key` moves keyboard focus through the bar: the keys whose use
/// shows the focus ring (design-system: focus-visible follows keyboard
/// navigation, never a pointer press or a key that moves nothing).
pub(super) fn navigates(key: &winit::keyboard::Key) -> bool {
    use winit::keyboard::{Key, NamedKey};
    matches!(
        key,
        Key::Named(
            NamedKey::Tab
                | NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::ArrowUp
                | NamedKey::ArrowDown
                | NamedKey::Home
                | NamedKey::End
        )
    )
}
