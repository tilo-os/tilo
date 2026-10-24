//! The native shell's strip navigation: the shared ribbon, rail and switcher
//! ([`tilo_desktop::strip_navigator`]) over the shell's chrome state. What
//! stays here is the shell's own: which display's content they stand in,
//! what covers them, a direct pan settling, and turning what they report
//! into chrome changes.

use std::time::Instant;

use tilo_desktop::strip_navigator::{StripFrame, StripNavigation, StripNavigator};
pub(crate) use tilo_desktop::strip_switcher::*;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, ModifiersState};

impl super::ShellOverlayHost {
    fn is_fullscreen(&self) -> bool {
        self.chrome
            .as_ref()
            .is_some_and(|chrome| chrome.borrow().is_fullscreen())
    }

    /// Whether the switcher is offered: a desktop with strips, outside the
    /// overview, Quick Settings and a fullscreen window.
    fn strip_switcher_offered(&self, ribbon: &ui::chrome::shell::StripRibbon) -> bool {
        let flags = self.overlay_flags();
        !flags.overview && !flags.quick_settings && ribbon.count > 0 && !self.is_fullscreen()
    }

    /// What the ribbon and rail stand in and report, read from the chrome
    /// and the shown strip's live row.
    pub(super) fn strip_frame(&self) -> StripFrame {
        let ribbon = self.strip_ribbon();
        let (rail, windows) = self.strip_rail_and_windows();
        let strip = self.active_strip_id();
        let strips = self.chrome.as_ref().map_or_else(Vec::new, |chrome| {
            let state = chrome.borrow();
            let tree = state.tree();
            tree.desktop
                .active_workspace()
                .map_or_else(Vec::new, |ws| ws.strips.iter().map(|s| s.id).collect())
        });
        let scroll_x = strip
            .and_then(|id| self.group_row_ptrs.get(&id).copied())
            // SAFETY: see field doc on `group_row_ptrs`.
            .map_or(0.0, |ptr| unsafe { &*ptr }.scroll_x());
        StripFrame {
            ribbon_offered: self.strip_switcher_offered(&ribbon),
            rail_offered: !self.overlay_flags().overview && !self.is_fullscreen(),
            ribbon,
            rail,
            strips,
            strip,
            windows,
            scroll_x,
        }
    }

    /// Feed the position the ribbon and rail are about to report to the
    /// navigator, which shows them while it changes. A direct pan's frames
    /// keep the chrome they began with.
    pub(super) fn prepare_strip_indicators(&mut self, now: Instant) {
        let pan_settling = || {
            self.active_strip_id()
                .and_then(|id| self.group_row_ptrs.get(&id).copied())
                // SAFETY: see field doc on `group_row_ptrs`.
                .is_some_and(|ptr| unsafe { &*ptr }.direct_pan_in_progress())
        };
        if self.strip_pan_active && pan_settling() {
            return;
        }
        let frame = self.strip_frame();
        if std::mem::take(&mut self.strip_pan_active) {
            self.strip_navigator.settle_after_pan(&frame);
        } else if self.strip_navigator.observe(&frame, now) {
            self.publish_strip_switcher_open();
        }
    }

    /// The ribbon and rail's opacity at `now`, for tests that navigate and
    /// then check what shows.
    pub fn strip_indicator_alpha_at(&mut self, now: Instant, reduce_motion: bool) -> f32 {
        self.prepare_strip_indicators(now);
        self.strip_navigator
            .indicator_alpha_at(&self.strip_frame(), now, reduce_motion)
    }

    /// The ribbon's pointer band: what the pointer comes to rest on to open
    /// the labels.
    #[cfg(test)]
    pub(super) fn strip_switcher_zone(&self) -> Rect {
        self.strip_frame().zone()
    }

    #[cfg(test)]
    pub(super) fn strip_ribbon_alpha(&self, now: Instant, reduce_motion: bool) -> f32 {
        self.strip_navigator
            .ribbon_alpha_at(&self.strip_frame(), now, reduce_motion)
    }

    /// Rebuild the switcher's model when the chrome changed, take the
    /// chrome's requests to open it, and let pointer timers settle. Returns
    /// whether the labels need a repaint.
    pub(super) fn refresh_strip_switcher_model(&mut self) -> bool {
        let now = ui::time::now();
        let mut dirty = false;
        if let Some(chrome) = self.chrome.clone() {
            let state = chrome.borrow();
            if self.strip_switcher_revision != Some(state.revision) {
                self.strip_switcher_revision = Some(state.revision);
                let local_user = state.active_local_user();
                let own = state.local_user_id.clone();
                let tree = state.tree();
                let model = SwitcherModel::of(&tree.desktop, &local_user, &own, |strip| {
                    tree.may_rename_strip(strip)
                });
                drop(tree);
                drop(state);
                dirty |= self.strip_navigator.is_open()
                    && self.strip_navigator.switcher().model() != &model;
                self.strip_navigator.set_model(model);
            }
        }
        let frame = self.strip_frame();
        let rename = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.borrow_mut().take_strip_rename_request());
        if rename {
            self.strip_navigator.rename_focused(&frame, now);
            dirty = true;
        }
        let show = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.borrow_mut().take_strip_switcher_request());
        if show && frame.ribbon_offered {
            self.strip_navigator.open_for_choosing(&frame, now);
            dirty = true;
        }
        if !frame.ribbon_offered && self.strip_navigator.is_open() {
            self.strip_navigator.close();
            dirty = true;
        }
        dirty |= self.strip_navigator.settle(&frame, now);
        if dirty {
            self.publish_strip_switcher_open();
        }
        dirty
    }

    /// Mirror the switcher's open state into the chrome, which repaints.
    pub(super) fn publish_strip_switcher_open(&self) {
        if let Some(chrome) = self.chrome.as_ref() {
            let open = self.strip_navigator.is_open();
            let mut state = chrome.borrow_mut();
            state.set_strip_switcher_open(open);
            state.bump();
        }
    }

    /// When the switcher's pointer timers next owe a frame.
    pub(super) fn strip_switcher_wake_deadline(&self, now: Instant) -> Option<Instant> {
        self.strip_navigator
            .labels_wake_deadline(&self.strip_frame(), now)
    }

    pub(super) fn toggle_strip_switcher_from_keyboard(&mut self) {
        self.strip_switcher_revision = None;
        self.refresh_strip_switcher_model();
        let frame = self.strip_frame();
        if !frame.ribbon_offered {
            return;
        }
        self.strip_navigator
            .toggle_from_keyboard(&frame, ui::time::now());
        self.publish_strip_switcher_open();
    }

    /// A key while the switcher takes the keyboard.
    pub(super) fn strip_switcher_key(&mut self, key: &Key, mods: ModifiersState) {
        let frame = self.strip_frame();
        if let Some(navigation) = self.strip_navigator.key(&frame, key, mods) {
            self.apply_strip_navigation(navigation);
        }
        self.publish_strip_switcher_open();
    }

    /// The pointer moved to `p` (layout pixels): the ribbon and rail
    /// thicken the segment under it, and the labels read its path.
    pub(super) fn strip_switcher_pointer_moved(&mut self, p: Point) {
        let frame = self.strip_frame();
        if self
            .strip_navigator
            .pointer_moved(&frame, p, ui::time::now())
        {
            self.publish_strip_switcher_open();
        }
    }

    pub(super) fn strip_switcher_pointer_left(&mut self) {
        if self.strip_navigator.pointer_left(ui::time::now()) {
            self.publish_strip_switcher_open();
        }
    }

    /// A press or release for the open labels; a secondary press (a
    /// long-press by touch) also on the ribbon, putting a strip's actions
    /// out. Returns whether it was the switcher's.
    pub(super) fn route_strip_switcher_pointer(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::MouseInput { state, button, .. } = event else {
            return false;
        };
        if !matches!(button, MouseButton::Left | MouseButton::Right) {
            return false;
        }
        if *state == ElementState::Released {
            return self.strip_navigator.release();
        }
        let Some(p) = self.last_cursor else {
            return false;
        };
        let frame = self.strip_frame();
        let now = ui::time::now();
        let answer = if *button == MouseButton::Right {
            self.strip_navigator.secondary_press(&frame, p, now)
        } else {
            self.strip_navigator.press_labels(&frame, p, now)
        };
        let Some(answer) = answer else {
            return false;
        };
        if let Some(navigation) = answer {
            self.apply_strip_navigation(navigation);
        }
        self.publish_strip_switcher_open();
        true
    }

    /// A press at `p` on a ribbon mark (which opens the labels, pinned) or
    /// a rail segment, once nothing over them took it. Returns whether it
    /// was theirs.
    pub(super) fn press_strip_ribbon_or_rail(&mut self, p: Point) -> bool {
        let frame = self.strip_frame();
        if self
            .strip_navigator
            .open_from_ribbon(&frame, p, ui::time::now())
        {
            self.publish_strip_switcher_open();
            return true;
        }
        let Some(navigation) = StripNavigator::rail_target(&frame, p) else {
            return false;
        };
        self.apply_strip_navigation(navigation);
        true
    }

    /// The workspace menu's New strip: made in the switcher, the one place
    /// strips are, with its name in place for typing.
    pub(super) fn new_strip_in_switcher(&mut self) {
        self.strip_switcher_revision = None;
        self.refresh_strip_switcher_model();
        let frame = self.strip_frame();
        if let Some(navigation) = self.strip_navigator.new_strip(&frame, ui::time::now()) {
            self.apply_strip_navigation(navigation);
        }
        self.publish_strip_switcher_open();
    }

    /// A wheel or two-finger scroll: over the ribbon or its labels it moves
    /// a strip up or down. Returns whether it was the switcher's.
    pub(super) fn route_strip_switcher_wheel(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::MouseWheel { delta, .. } = event else {
            return false;
        };
        let Some(p) = self.last_cursor else {
            return false;
        };
        let frame = self.strip_frame();
        let Some(answer) = self
            .strip_navigator
            .wheel(&frame, p, *delta, ui::time::now())
        else {
            return false;
        };
        if let Some(navigation) = answer {
            self.apply_strip_navigation(navigation);
            self.publish_strip_switcher_open();
        }
        true
    }

    /// Whether the pointer is on the open labels or the ribbon under them.
    pub(super) fn strip_switcher_captures_pointer_at(&self, p: Point) -> bool {
        self.strip_navigator.is_open()
            && self.strip_navigator.takes_wheel_at(&self.strip_frame(), p)
    }

    /// A semantic target of the switcher, activated as a press would.
    pub(super) fn activate_strip_switcher_target(&mut self, target: &str) -> Option<bool> {
        let frame = self.strip_frame();
        let answer = self.strip_navigator.activate_target(&frame, target)?;
        if let Some(navigation) = answer {
            self.apply_strip_navigation(navigation);
        }
        self.publish_strip_switcher_open();
        Some(true)
    }

    fn apply_strip_navigation(&mut self, navigation: StripNavigation) {
        let command = match navigation {
            StripNavigation::Window { strip, group } => {
                // The rail addresses windows off screen, so this is the one
                // pointer path that also pans the strip.
                self.focus_group_on_strip(strip, group);
                return;
            }
            StripNavigation::Strip(command) => command,
        };
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let created = matches!(command, Command::Create(_));
        let refused = {
            let mut state = chrome.borrow_mut();
            let refused = match command {
                Command::Focus(strip) => {
                    state.focus_strip(strip);
                    None
                }
                Command::Step(steps) => {
                    for _ in 0..steps.unsigned_abs() {
                        if !state.switch_strip(steps.signum()) {
                            break;
                        }
                    }
                    None
                }
                // Only where the desktop's policy lets this person add one.
                Command::Create(name) => {
                    if state.can_add_strip() {
                        state.create_strip(&name).err()
                    } else {
                        Some(tilo_desktop::i18n::t("strip-switcher-refused"))
                    }
                }
                Command::Rename(strip, name) => state.rename_strip(strip, &name).err(),
                // Offered only for an empty strip that is not the last, so
                // a refusal means it changed meanwhile.
                Command::Remove(strip) => (!state.remove_strip_if_empty(strip))
                    .then(|| tilo_desktop::i18n::t("strip-switcher-refused")),
            };
            if let Some(message) = &refused {
                state.push_warning_notification(
                    tilo_desktop::i18n::t("strip-switcher-refused"),
                    message,
                );
            }
            refused
        };
        if created && refused.is_none() {
            // The new strip is listed now, and its name goes in place.
            self.strip_switcher_revision = None;
            self.refresh_strip_switcher_model();
        }
    }

    /// Paint the labels, the ribbon and the rail, each a damage part of its
    /// own whether or not it shows: focus moving to another strip repaints
    /// the ribbon and the rail, not the stretch of desktop between them.
    /// Quick Settings replaces the ribbon and its labels for its whole
    /// open/close transition (`hidden_by_qs`); the rail remains.
    pub(super) fn draw_strip_navigation(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        (text, theme): (&mut TextRenderer, &Theme),
        hidden_by_qs: bool,
    ) {
        let desktop = transform.transform_rect_bbox(self.rect);
        let now = ui::time::now();
        let mut frame = self.strip_frame();
        if hidden_by_qs {
            frame.ribbon_offered = false;
        } else if self.strip_navigator.settle(&frame, now) {
            self.publish_strip_switcher_open();
        }
        let navigator = &mut self.strip_navigator;
        let damage = |bounds: Option<Rect>| bounds.map(|b| vec![transform.transform_rect_bbox(b)]);
        super::overlay_part(scene, "shell.strip-switcher", desktop, |scene| {
            let mut cx = DrawCx {
                scene,
                transform,
                text,
                theme,
            };
            damage(navigator.draw_labels(&frame, &mut cx, now))
        });
        super::overlay_part(scene, "shell.strip-ribbon", desktop, |scene| {
            let mut cx = DrawCx {
                scene,
                transform,
                text,
                theme,
            };
            damage(navigator.draw_ribbon(&frame, &mut cx, now))
        });
        super::overlay_part(scene, "shell.strip-rail", desktop, |scene| {
            let mut cx = DrawCx {
                scene,
                transform,
                text,
                theme,
            };
            damage(navigator.draw_rail(&frame, &mut cx, now))
        });
    }

    /// Visual extent of the labels, the ribbon and the rail; the ribbon and
    /// labels not while Quick Settings replaces them.
    pub(super) fn strip_navigation_bounds(&self, hidden_by_qs: bool) -> Option<Rect> {
        let frame = self.strip_frame();
        let ribbon = (!hidden_by_qs)
            .then(|| self.strip_navigator.ribbon_bounds(&frame))
            .flatten();
        let labels = (!hidden_by_qs && self.strip_navigator.is_open())
            .then(|| self.strip_navigator.labels_bounds());
        ribbon
            .into_iter()
            .chain(labels)
            .chain(self.strip_navigator.rail_bounds(&frame))
            .reduce(|current, rect| current.union(rect))
    }

    /// The switcher's named targets, for harnesses.
    pub(super) fn strip_switcher_test_rects(&self) -> Vec<(String, Rect)> {
        self.strip_navigator.test_rects(&self.strip_frame())
    }

    /// The open labels for assistive technology.
    pub(super) fn strip_switcher_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.strip_navigator.accessibility()
    }

    /// Test access to the switcher's state.
    #[cfg(test)]
    pub(crate) fn strip_switcher_for_tests(&mut self) -> &mut StripSwitcher {
        let frame = self.strip_frame();
        self.strip_navigator.switcher_for_tests(&frame)
    }
}
