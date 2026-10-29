//! The overview's keyboard and pointer: the rooms and the pictured windows
//! walked by keys, presses that go somewhere, a window carried to another
//! workspace or strip, a middle click that closes one, and a strip's name
//! typed in place.

use tilo_desktop_model::TabId;
use ui::num::{isize_of_usize, usize_of_isize};
use ui::text_selection::Selection;
use vello::kurbo::Point;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::layout::{Picture, StripControl};
use super::{Naming, OverviewPanel, OverviewPick};
use crate::overview::{OverviewPress, PressEnd, PressSource};
use crate::strip_switcher::{STRIP_NAME_MAX_CHARS, valid_strip_name};
use crate::text_entry::{self, Edit, Mods};

impl OverviewPanel {
    /// A key while the overview is up. Returns whether it was taken.
    pub(super) fn key(&mut self, key: &Key, mods: ModifiersState) -> bool {
        if self.naming.is_some() {
            self.naming_key(key, mods);
            return true;
        }
        if matches!(key, Key::Named(NamedKey::Escape)) {
            self.choose(OverviewPick::Dismiss);
            return true;
        }
        if self.strip_key(key, mods) {
            return true;
        }
        self.navigation_key(key, mods.shift_key())
    }

    /// The window selected by the keyboard is gone: the keyboard goes back
    /// to the room you are in, and the Enter that was meant for the window
    /// does nothing.
    pub(super) fn reconcile_window_selection(&mut self) {
        if let Some(tab) = self.window_selection
            && !self
                .row_windows()
                .iter()
                .any(|window| window.address.tab == tab)
        {
            self.window_selection = None;
            self.window_interrupted = true;
            self.selection = self.here_index();
            self.bump();
        }
    }

    /// Arrows keep the ring of rooms; Tab adds the rows that make or join
    /// one, Home and End reach its ends; the pictured windows take Left,
    /// Right, Up and Down once the keyboard is among them.
    fn navigation_key(&mut self, key: &Key, shift: bool) -> bool {
        if !matches!(
            key,
            Key::Named(
                NamedKey::ArrowDown
                    | NamedKey::ArrowUp
                    | NamedKey::ArrowLeft
                    | NamedKey::ArrowRight
                    | NamedKey::Tab
                    | NamedKey::Home
                    | NamedKey::End
                    | NamedKey::Enter
                    | NamedKey::Space
            )
        ) {
            return false;
        }
        self.reconcile_window_selection();
        if std::mem::take(&mut self.window_interrupted)
            && matches!(key, Key::Named(NamedKey::Enter | NamedKey::Space))
        {
            return true;
        }
        if self.window_key(key) {
            return true;
        }
        if matches!(key, Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight)) {
            return false;
        }
        self.window_selection = None;
        self.window_interrupted = false;
        let count = self.desk.cards.len();
        if count == 0 {
            return false;
        }
        match key {
            Key::Named(NamedKey::Enter | NamedKey::Space) => self.activate_selection(count),
            Key::Named(NamedKey::Home) => self.selection = self.here_index(),
            Key::Named(NamedKey::End) => self.selection = Some(self.last_row(count)),
            _ => self.step_selection(key, shift, count),
        }
        self.scroll_selection_into_view();
        self.bump();
        true
    }

    /// The last row the keyboard reaches: Join, New workspace, or the last
    /// room.
    fn last_row(&self, count: usize) -> usize {
        count
            .saturating_add(usize::from(self.can_add()))
            .saturating_add(usize::from(self.desk.can_join))
            .saturating_sub(1)
    }

    fn step_selection(&mut self, key: &Key, shift: bool, count: usize) {
        let tab = matches!(key, Key::Named(NamedKey::Tab));
        let back = matches!(key, Key::Named(NamedKey::ArrowUp)) || (tab && shift);
        let focus_count = if tab {
            self.last_row(count).saturating_add(1)
        } else {
            count
        };
        let mut current = self.selection.or_else(|| self.here_index()).unwrap_or(0);
        // Making and joining are outside the arrows' ring of rooms.
        if !tab && current >= count {
            current = if back { 0 } else { count.saturating_sub(1) };
        }
        let next = isize_of_usize(current)
            .wrapping_add(if back { -1 } else { 1 })
            .rem_euclid(isize_of_usize(focus_count.max(1)));
        let next = usize_of_isize(next);
        // Join follows New workspace in the ring only where both are offered.
        self.selection = Some(if next == count && !self.can_add() && self.desk.can_join {
            count.saturating_add(1)
        } else {
            next
        });
    }

    fn activate_selection(&mut self, count: usize) {
        let target = self.selection.or_else(|| self.here_index());
        if target == Some(count) && self.can_add() {
            self.choose(OverviewPick::NewWorkspace);
            return;
        }
        if target == Some(count.saturating_add(1)) || (target == Some(count) && !self.can_add()) {
            if self.desk.can_join {
                self.choose(OverviewPick::JoinWorkspace);
            }
            return;
        }
        if let Some(card) = target.and_then(|index| self.desk.cards.get(index)) {
            let id = card.id;
            self.choose(OverviewPick::Workspace(id));
        }
    }

    /// Left and Right walk the pictured windows in reading order, a ring;
    /// Up and Down go to the nearest window of the strip above or below;
    /// Enter on a selected window goes there.
    fn window_key(&mut self, key: &Key) -> bool {
        let picture = self.picture();
        let windows: Vec<(usize, usize, TabId)> = picture
            .windows()
            .map(|(si, wi, window)| (si, wi, window.address.tab))
            .collect();
        let count = windows.len();
        if count == 0 {
            self.window_selection = None;
            self.window_interrupted = false;
            return false;
        }
        let current = self
            .window_selection
            .and_then(|tab| windows.iter().position(|(_, _, t)| *t == tab));
        match key {
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                let Some(&(_, _, tab)) = current.and_then(|index| windows.get(index)) else {
                    return false;
                };
                self.choose(OverviewPick::Window(tab));
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                let next = current.map_or(0, |index| {
                    index.saturating_add(1).checked_rem(count).unwrap_or(0)
                });
                self.select_window_at(&windows, next);
                true
            }
            Key::Named(NamedKey::ArrowLeft) => {
                let last = count.saturating_sub(1);
                let next = current.map_or(last, |index| index.checked_sub(1).unwrap_or(last));
                self.select_window_at(&windows, next);
                true
            }
            Key::Named(key @ (NamedKey::ArrowUp | NamedKey::ArrowDown)) => {
                let Some(index) = current else {
                    return false;
                };
                match window_in_next_strip(&picture, &windows, index, *key == NamedKey::ArrowUp) {
                    Some(next) => {
                        self.select_window_at(&windows, next);
                        true
                    }
                    None => false,
                }
            }
            _ => false,
        }
    }

    fn select_window_at(&mut self, windows: &[(usize, usize, TabId)], index: usize) {
        if let Some(&(_, _, tab)) = windows.get(index) {
            self.select_window(tab);
        }
    }

    /// Put the keyboard on pictured window `tab`; the picture is this
    /// workspace's, so the room you are in is selected with it.
    pub fn select_window(&mut self, tab: TabId) {
        self.window_selection = Some(tab);
        self.window_interrupted = false;
        self.selection = self.here_index();
        self.scroll_selection_into_view();
        self.bump();
    }

    /// F2 names the strip the keyboard is in; Alt with Up or Down moves it.
    fn strip_key(&mut self, key: &Key, mods: ModifiersState) -> bool {
        let picture = self.picture();
        let Some(si) = self.keyboard_strip(&picture) else {
            return false;
        };
        let control = match key {
            Key::Named(NamedKey::F2) => StripControl::Rename,
            Key::Named(NamedKey::ArrowUp) if mods.alt_key() => StripControl::MoveUp,
            Key::Named(NamedKey::ArrowDown) if mods.alt_key() => StripControl::MoveDown,
            _ => return false,
        };
        self.apply_strip_control(&picture, si, control);
        self.bump();
        true
    }

    pub(super) fn begin_naming(&mut self, picture: &Picture, si: usize) {
        let Some(strip) = picture.strips.get(si).filter(|strip| strip.renamable) else {
            return;
        };
        let text = strip.name.clone();
        let end = text.len();
        self.naming = Some(Naming {
            strip: strip.id,
            text,
            selection: Selection {
                anchor: 0,
                cursor: end,
            },
            input: None,
        });
        self.bump();
    }

    /// Do what strip `si`'s `control` says.
    pub(super) fn apply_strip_control(
        &mut self,
        picture: &Picture,
        si: usize,
        control: StripControl,
    ) {
        let Some(strip) = picture.strips.get(si).filter(|strip| strip.renamable) else {
            return;
        };
        let before = match control {
            StripControl::Rename => {
                self.begin_naming(picture, si);
                return;
            }
            StripControl::MoveUp => match si.checked_sub(1) {
                Some(above) => picture.strips.get(above).map(|s| s.id),
                None => return,
            },
            StripControl::MoveDown => {
                if si.saturating_add(1) >= picture.strips.len() {
                    return;
                }
                picture.strips.get(si.saturating_add(2)).map(|s| s.id)
            }
        };
        let strip = strip.id;
        self.choose(OverviewPick::MoveStrip { strip, before });
    }

    /// A key while a strip's name is typed: Enter keeps it, Escape keeps the
    /// old one.
    fn naming_key(&mut self, key: &Key, mods: ModifiersState) {
        let Some(naming) = self.naming.as_mut() else {
            return;
        };
        match text_entry::apply(
            key,
            Mods::from_state(mods),
            &mut naming.text,
            &mut naming.selection,
        ) {
            Edit::Submit => self.commit_naming(),
            Edit::Cancel => self.naming = None,
            Edit::Changed => cap_strip_name(naming),
            Edit::Ignored | Edit::ConsumedQuiet => {}
        }
        self.bump();
    }

    /// Text an input method committed into a strip's name field.
    pub(super) fn naming_commit_text(&mut self, text: &str) -> bool {
        let Some(naming) = self.naming.as_mut() else {
            return false;
        };
        text_entry::commit(text, &mut naming.text, &mut naming.selection);
        cap_strip_name(naming);
        self.bump();
        true
    }

    /// Give the strip the typed name, when it is a name and a new one.
    fn commit_naming(&mut self) {
        let Some(naming) = self.naming.take() else {
            return;
        };
        let Some(name) = valid_strip_name(&naming.text).map(str::to_owned) else {
            return;
        };
        let unchanged = self
            .here_card()
            .and_then(|card| card.strips.iter().find(|strip| strip.id == naming.strip))
            .is_some_and(|strip| strip.name == name);
        if !unchanged {
            self.choose(OverviewPick::RenameStrip {
                strip: naming.strip,
                name,
            });
        }
    }

    /// Pointer and touch input while a strip's name is typed: on the field
    /// it is the field's; a press anywhere else keeps the name and is taken,
    /// so it does not also act on what is under it.
    fn naming_takes(&mut self, event: &WindowEvent, scale_factor: f64) -> bool {
        if self.naming.is_none() {
            return false;
        }
        if let WindowEvent::CursorMoved { .. } = event {
            self.naming_field_pointer(event, scale_factor);
            return false;
        }
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            ..
        } = event
        else {
            self.naming_field_pointer(event, scale_factor);
            return matches!(event, WindowEvent::MouseInput { .. });
        };
        let picture = self.picture();
        let on_field = self
            .naming
            .as_ref()
            .and_then(|naming| picture.strips.iter().position(|s| s.id == naming.strip))
            .and_then(|si| self.naming_field(&picture, si))
            .zip(self.cursor)
            .is_some_and(|(field, p)| field.contains(p));
        if on_field {
            self.naming_field_pointer(event, scale_factor);
        } else {
            self.commit_naming();
        }
        self.bump();
        true
    }

    /// Hand a pointer event to the name field and copy its selection back.
    fn naming_field_pointer(&mut self, event: &WindowEvent, scale_factor: f64) -> bool {
        let Some(naming) = self.naming.as_mut() else {
            return false;
        };
        let Some(field) = naming.input.as_mut() else {
            return false;
        };
        let took =
            field.handle_chrome_pointer(event, scale_factor, winit::event::Modifiers::default());
        if took {
            let (anchor, cursor) = field.selection_anchor_focus_or_caret();
            naming.selection = Selection { anchor, cursor };
            self.bump();
        }
        took
    }

    /// Every pointer and touch event while the overview is up. Returns
    /// whether it was taken.
    pub(super) fn pointer(&mut self, event: &WindowEvent, scale_factor: f64) -> bool {
        if self.naming_takes(event, scale_factor) {
            return true;
        }
        if self.press_event(event, scale_factor) {
            return true;
        }
        match event {
            // Pans the list; the header stays pinned.
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match *delta {
                    MouseScrollDelta::LineDelta(_, y) => -f64::from(y) * 30.0,
                    MouseScrollDelta::PixelDelta(p) => -p.y,
                };
                if dy.abs() <= f64::EPSILON {
                    return false;
                }
                self.scroll_y = (self.scroll_y + dy).clamp(0.0, self.max_scroll_y());
                self.bump();
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(p) = self.cursor {
                    self.click_at(p);
                }
                true
            }
            WindowEvent::MouseInput { .. } => true,
            _ => false,
        }
    }

    /// A click or tap away from the pictured windows: a strip's name or
    /// controls, a row that makes or joins a workspace, a site workspace's
    /// Leave, a room to go to; anything else leaves the overview.
    pub(super) fn click_at(&mut self, p: Point) {
        if self.strip_click(p) {
            return;
        }
        if self.add_hit_rect().is_some_and(|add| add.contains(p)) {
            self.choose(OverviewPick::NewWorkspace);
            return;
        }
        if self.join_hit_rect().is_some_and(|join| join.contains(p)) {
            self.choose(OverviewPick::JoinWorkspace);
            return;
        }
        let leaving = (0..self.desk.cards.len())
            .find(|&idx| self.leave_rect(idx).is_some_and(|leave| leave.contains(p)));
        if let Some(card) = leaving.and_then(|idx| self.desk.cards.get(idx)) {
            let id = card.id;
            self.choose(OverviewPick::LeaveWorkspace(id));
            return;
        }
        match self.hit_room(p).and_then(|idx| self.desk.cards.get(idx)) {
            Some(card) => {
                let id = card.id;
                self.choose(OverviewPick::Workspace(id));
            }
            // A press on nothing leaves the overview where it was.
            None => self.choose(OverviewPick::Dismiss),
        }
    }

    /// A click or tap on the picture's strip names and controls. Returns
    /// whether it landed on one.
    fn strip_click(&mut self, p: Point) -> bool {
        let picture = self.picture();
        let Some(si) = picture
            .places
            .iter()
            .position(|place| place.label.contains(p))
        else {
            return false;
        };
        let control = self
            .strip_controls(&picture, si)
            .into_iter()
            .find(|(_, rect)| rect.contains(p))
            .map(|(control, _)| control);
        match control {
            Some(control) => self.apply_strip_control(&picture, si, control),
            // The name itself: a strip this machine names is renamed in
            // place.
            None => self.begin_naming(&picture, si),
        }
        self.bump();
        true
    }

    /// Presses that can become drags: the primary button on a pictured
    /// window, and a finger anywhere. A middle click on a window closes it.
    fn press_event(&mut self, event: &WindowEvent, scale_factor: f64) -> bool {
        let taken = match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Middle,
                ..
            } => match self.cursor.and_then(|p| self.window_at(p)) {
                Some(tab) => {
                    self.window_selection = None;
                    self.window_interrupted = false;
                    self.choose(OverviewPick::CloseWindow(tab));
                    true
                }
                None => false,
            },
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => match self
                .cursor
                .and_then(|p| self.window_at(p).map(|tab| (p, tab)))
            {
                Some((p, tab)) => {
                    self.press = Some(OverviewPress::new(PressSource::Mouse, Some(tab), p));
                    true
                }
                None => false,
            },
            WindowEvent::CursorMoved { .. } => match (self.press.as_mut(), self.cursor) {
                (Some(press), Some(p)) if press.source == PressSource::Mouse => {
                    press.moved(p);
                    true
                }
                _ => false,
            },
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => match self
                .press
                .filter(|press| press.source == PressSource::Mouse)
            {
                Some(press) => {
                    self.press = None;
                    let at = self.cursor.unwrap_or(press.at);
                    self.finish_press(press.release(at));
                    true
                }
                None => false,
            },
            WindowEvent::Touch(touch) => {
                let p = Point::new(
                    touch.location.x / scale_factor,
                    touch.location.y / scale_factor,
                );
                let source = PressSource::Touch(touch.id);
                let mine = self.press.is_some_and(|press| press.source == source);
                match touch.phase {
                    TouchPhase::Started => {
                        // A second finger makes it a gesture, not a tap or a
                        // drag; the host's gestures take it.
                        let held = self.press.is_some();
                        if !held {
                            self.press = Some(OverviewPress::new(source, self.window_at(p), p));
                        } else if !mine {
                            self.press = None;
                        }
                    }
                    TouchPhase::Moved => {
                        if let Some(press) = self.press.as_mut().filter(|_| mine) {
                            press.moved(p);
                        }
                    }
                    TouchPhase::Ended => {
                        if let Some(press) = self.press.filter(|_| mine) {
                            self.press = None;
                            self.finish_press(press.release(p));
                        }
                    }
                    TouchPhase::Cancelled => {
                        if mine {
                            self.press = None;
                        }
                    }
                }
                // The overview is modal: no finger reaches what is beneath.
                true
            }
            _ => false,
        };
        if taken {
            self.bump();
        }
        taken
    }

    /// Act on a press that was let go.
    fn finish_press(&mut self, end: PressEnd) {
        match end {
            PressEnd::Tap { tab: Some(tab), at } => {
                if self.window_at(at) == Some(tab) {
                    self.choose(OverviewPick::Window(tab));
                }
            }
            PressEnd::Tap { tab: None, at } => self.click_at(at),
            PressEnd::Drop { tab, at } => self.drop_window(tab, at),
            PressEnd::Nothing => {}
        }
    }

    /// A pictured window let go at `at`: onto another workspace it moves
    /// there, onto another strip of this one its window moves to that
    /// strip.
    fn drop_window(&mut self, tab: TabId, at: Point) {
        if let Some(card) = self
            .hit_room(at)
            .filter(|room| Some(*room) != self.here_index())
            .and_then(|room| self.desk.cards.get(room))
        {
            let to = card.id;
            self.window_selection = None;
            self.window_interrupted = false;
            self.choose(OverviewPick::MoveWindow { tab, to });
            return;
        }
        let picture = self.picture();
        let from = picture
            .find(tab)
            .and_then(|(si, _, _)| picture.strips.get(si))
            .map(|s| s.id);
        if let Some(strip) = self
            .strip_at(&picture, at)
            .filter(|strip| Some(*strip) != from)
        {
            self.choose(OverviewPick::MoveWindowToStrip { tab, strip });
        }
    }

    /// Start typing a new name for strip `strip` in place, as F2 or a press
    /// on its name does; whether it can be named here.
    pub fn rename_in_place(&mut self, strip: tilo_desktop_model::StripId) -> bool {
        let picture = self.picture();
        let Some(si) = picture.strips.iter().position(|s| s.id == strip) else {
            return false;
        };
        self.begin_naming(&picture, si);
        self.naming.is_some()
    }

    /// Carry pictured window `tab` to `to`, as a drag that has not been let
    /// go yet; whether it is pictured.
    pub fn carry(&mut self, tab: TabId, to: Point) -> bool {
        let picture = self.picture();
        let Some((si, wi, _)) = picture.find(tab) else {
            return false;
        };
        let Some(from) = picture.tile(si, wi) else {
            return false;
        };
        let mut press = OverviewPress::new(PressSource::Mouse, Some(tab), from.center());
        press.moved(to);
        self.press = Some(press);
        self.cursor = Some(to);
        self.bump();
        true
    }

    /// Put the keyboard on room or row `index` (past the rooms: New
    /// workspace, then Join).
    pub(super) fn focus_row(&mut self, index: usize) {
        self.selection = Some(index);
        self.window_selection = None;
        self.window_interrupted = false;
        self.scroll_selection_into_view();
        self.bump();
    }
}

fn cap_strip_name(naming: &mut Naming) {
    if naming.text.chars().count() > STRIP_NAME_MAX_CHARS {
        naming.text = naming.text.chars().take(STRIP_NAME_MAX_CHARS).collect();
        naming.selection = Selection::collapsed(naming.text.len());
    }
}

/// The window of the nearest strip above (`up`) or below window `index`'s
/// that has windows, whose centre is closest to its centre.
fn window_in_next_strip(
    picture: &Picture,
    windows: &[(usize, usize, TabId)],
    index: usize,
    up: bool,
) -> Option<usize> {
    let &(si, wi, _) = windows.get(index)?;
    let x = picture.tile(si, wi)?.center().x;
    let strips: Vec<usize> = if up {
        (0..si).rev().collect()
    } else {
        (si.saturating_add(1)..picture.strips.len()).collect()
    };
    let target = strips.into_iter().find(|s| {
        picture
            .strips
            .get(*s)
            .is_some_and(|strip| !strip.windows.is_empty())
    })?;
    windows
        .iter()
        .enumerate()
        .filter(|(_, (s, _, _))| *s == target)
        .min_by(|(_, (s1, w1, _)), (_, (s2, w2, _))| {
            let d = |s: usize, w: usize| {
                picture
                    .tile(s, w)
                    .map_or(f64::INFINITY, |tile| (tile.center().x - x).abs())
            };
            d(*s1, *w1).total_cmp(&d(*s2, *w2))
        })
        .map(|(i, _)| i)
}
