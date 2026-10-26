//! The bar's "Controls your desktop" chip.
//!
//! A program running in an app (`tilo action` typed in a terminal) works in
//! that app's workspace without asking; once the person lets it reach the
//! rest of the desktop, every window of that app in that workspace says so
//! in its bar, for as long as the grant lasts. The chip opens what it allows
//! and lets the person stop it: a press is reported by
//! [`TabBar::take_desktop_control_press`] and the host opens the menu, since
//! only the host knows the grant.

use super::{Point, Rect, TOOLBAR_LEAD_GAP, TabBar, TextRenderer, Theme};
use crate::a11y::AccessNode;
use crate::icons::Icon;
use crate::widget::{ChipFace, ChipLead};
use std::collections::HashSet;
use vello::Scene;
use vello::kurbo::Affine;

/// The chip's accessibility action target.
pub(super) const DESKTOP_CONTROL_TARGET: &str = "tab-desktop-control";

/// The bar's part in the chip: which tabs show it, where it was painted,
/// and a press waiting for the host.
#[derive(Debug, Default)]
pub(super) struct DesktopControlChip {
    ids: HashSet<u64>,
    painted: Option<Rect>,
    pressed: Option<(u64, Rect)>,
}

impl TabBar {
    /// Mark `id` as a window whose app may control the desktop, or clear
    /// the mark. `true` when that changed what the bar shows.
    pub fn set_desktop_control(&mut self, id: u64, on: bool) -> bool {
        if on {
            self.desktop_control.ids.insert(id)
        } else {
            self.desktop_control.ids.remove(&id)
        }
    }

    pub fn controls_desktop(&self, id: u64) -> bool {
        self.desktop_control.ids.contains(&id)
    }

    /// A press on the chip, once: the tab it was on and where the chip
    /// stands, for the host's menu to hang from.
    pub fn take_desktop_control_press(&mut self) -> Option<(u64, Rect)> {
        self.desktop_control.pressed.take()
    }

    /// Where the "Controls your desktop" chip of the window in front stands,
    /// with its label: after the pills, before the window's controls. `None` when the window in front holds
    /// no grant, its app put slots in the row, a tab is being dragged, or
    /// the run is too short for it.
    pub fn desktop_control_chip(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<(Rect, String)> {
        if !self.desktop_control.ids.contains(&self.active_id)
            || self.active_slots().is_some()
            || self.pending_drag.is_some()
            || self.tab_rects.len() != self.tabs.len()
        {
            return None;
        }
        let label = crate::i18n!("tab-controls-desktop");
        let tab_row = self.tab_row();
        let x0 = self.status_chips_start() + TOOLBAR_LEAD_GAP;
        let limit = self.tab_row_limit(tab_row);
        let width =
            ChipFace::intrinsic_width(&label, ChipLead::Glyph(Icon::Monitor), true, tr, theme);
        let height = ChipFace::height(theme);
        (x0 + width <= limit).then(|| {
            (
                Rect::from_origin_size(
                    Point::new(x0, tab_row.center().y - height / 2.0),
                    (width, height),
                ),
                label,
            )
        })
    }

    /// Paint the chip where it stands, and remember where for the press.
    pub(super) fn draw_desktop_control_chip(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.desktop_control.painted = None;
        if let Some((rect, label)) = self.desktop_control_chip(tr, theme) {
            ChipFace::new(rect, &label)
                .lead(ChipLead::Glyph(Icon::Monitor))
                .menu()
                .hovered(rect.contains(self.last_cursor))
                .paint(scene, transform, tr, theme);
            self.desktop_control.painted = Some(rect);
        }
    }

    /// A press at `at` on the chip: report it. Whether it was on the chip.
    pub(super) fn press_desktop_control(&mut self, at: Point) -> bool {
        let Some(rect) = self
            .desktop_control
            .painted
            .filter(|rect| rect.contains(at))
        else {
            return false;
        };
        if !self.desktop_control.ids.contains(&self.active_id) {
            return false;
        }
        self.desktop_control.pressed = Some((self.active_id, rect));
        true
    }

    /// The chip activated without a pointer: report it from where it
    /// stands. Whether the bar shows it.
    pub fn activate_desktop_control(&mut self) -> bool {
        match self.desktop_control.painted {
            Some(rect) => self.press_desktop_control(rect.center()),
            None => false,
        }
    }

    /// The chip as an assistive technology reads it: a button that opens
    /// what the grant allows.
    pub(super) fn desktop_control_access(&self) -> Option<AccessNode> {
        use crate::a11y::{AccessState, Role};
        let rect = self.desktop_control.painted?;
        self.desktop_control.ids.contains(&self.active_id).then(|| {
            AccessNode::new(Role::Button)
                .with_name(crate::i18n!("tab-controls-desktop"))
                .with_action_target(DESKTOP_CONTROL_TARGET)
                .with_bounds(rect)
                .with_state(AccessState {
                    focusable: true,
                    expanded: Some(false),
                    ..AccessState::default()
                })
        })
    }
}
