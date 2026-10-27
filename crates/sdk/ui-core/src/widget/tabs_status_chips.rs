//! The bar's status chips: "Controls your desktop", "Dev build · by …" and
//! "Shared with …".
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

/// The bar's "Dev build · by …" mark: the windows running a developer's own
/// build of their app, which the room they are in runs, by who installed
/// it there. Only a label: what the build is, the room says.
#[derive(Debug, Default)]
pub(super) struct DevBuildChip {
    installers: std::collections::HashMap<u64, String>,
    painted: Option<Rect>,
}

/// Who can see a window its person shared with a workspace where each
/// person lays out their own windows: the workspace, by name, and the people
/// in it now, as the window's bar says while the window is in front.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SharedWith {
    /// The workspace's name.
    pub place: String,
    /// The people in the workspace now who see the window, by presence id,
    /// with their names: their faces stand after the chip.
    pub people: Vec<(crate::permissions::UserId, String)>,
}

impl SharedWith {
    fn label(&self) -> String {
        crate::i18n!("tab-shared-with", "place" => self.place.as_str())
    }
}

/// The bar's "Shared with …" mark: the windows shared with their workspace,
/// and where the mark was last painted.
#[derive(Debug, Default)]
pub(super) struct SharedChip {
    marks: std::collections::HashMap<u64, SharedWith>,
    /// Where the mark was last painted.
    painted: Option<Rect>,
}

/// How the faces of who can see a shared window stand: a window bar's.
const SHARED_FACES: crate::presence::FaceStack = crate::presence::FaceStack::WINDOW;

impl TabBar {
    /// Mark `id` as a window shared with its workspace, seen by whom
    /// `shared` names, or clear the mark. `true` when that changed what the
    /// bar shows.
    pub fn set_shared(&mut self, id: u64, shared: Option<SharedWith>) -> bool {
        match shared {
            Some(shared) => self.shared.marks.insert(id, shared.clone()) != Some(shared),
            None => self.shared.marks.remove(&id).is_some(),
        }
    }

    /// Who can see the window `id`, while it is shared.
    pub fn shared_with(&self, id: u64) -> Option<&SharedWith> {
        self.shared.marks.get(&id)
    }

    /// Where the "Shared with …" mark of the window in front stands, with
    /// its label: the chip, then the faces of the people who see the window.
    /// After the bar's other status chips. `None` when the window in front
    /// is not shared, its app put slots in the row, a tab is being dragged,
    /// or the run is too short for it.
    pub fn shared_chip(&self, tr: &mut TextRenderer, theme: &Theme) -> Option<(Rect, String)> {
        let shared = self.shared.marks.get(&self.active_id)?;
        if self.active_slots().is_some()
            || self.pending_drag.is_some()
            || self.tab_rects.len() != self.tabs.len()
        {
            return None;
        }
        let label = shared.label();
        let tab_row = self.tab_row();
        let x0 = match (
            self.dev_build_chip(tr, theme),
            self.desktop_control_chip(tr, theme),
        ) {
            (Some((before, _)), _) | (None, Some((before, _))) => before.x1 + TOOLBAR_LEAD_GAP,
            (None, None) => self.status_chips_start() + TOOLBAR_LEAD_GAP,
        };
        let limit = self.tab_row_limit(tab_row);
        let faces = if shared.people.is_empty() {
            0.0
        } else {
            TOOLBAR_LEAD_GAP / 2.0 + SHARED_FACES.width(shared.people.len())
        };
        let width = ChipFace::intrinsic_width(&label, ChipLead::None, false, tr, theme);
        let height = ChipFace::height(theme).max(SHARED_FACES.width(1).min(tab_row.height()));
        (x0 + width + faces <= limit).then(|| {
            (
                Rect::from_origin_size(
                    Point::new(x0, tab_row.center().y - height / 2.0),
                    (width + faces, height),
                ),
                label,
            )
        })
    }

    /// Paint the mark where it stands: the chip naming the workspace, and
    /// the faces of the people in it, each as they look everywhere.
    pub(super) fn draw_shared_chip(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        self.shared.painted = None;
        let Some((rect, label)) = self.shared_chip(tr, theme) else {
            return;
        };
        let Some(shared) = self.shared.marks.get(&self.active_id) else {
            return;
        };
        let chip_width = ChipFace::intrinsic_width(&label, ChipLead::None, false, tr, theme);
        let chip_height = ChipFace::height(theme);
        let chip = Rect::from_origin_size(
            Point::new(rect.x0, rect.center().y - chip_height / 2.0),
            (chip_width, chip_height),
        );
        ChipFace::new(chip, &label).paint(scene, transform, tr, theme);
        if !shared.people.is_empty() {
            let (shown, more) = SHARED_FACES.split(shared.people.len());
            let faces: Vec<crate::presence::Face<'_>> = shared
                .people
                .iter()
                .take(shown)
                .map(|(user, name)| {
                    let (red, green, blue) = presence.color_for(user);
                    crate::presence::Face {
                        is_agent: presence.is_agent(user),
                        dimmed: presence.availability_of(user).dims(),
                        ..crate::presence::Face::new(
                            name,
                            &user.0,
                            vello::peniko::Color::from_rgb8(red, green, blue),
                        )
                    }
                })
                .collect();
            SHARED_FACES.paint(
                &mut crate::paint::PaintCtx::new(scene, transform, tr, theme),
                (rect.x1, rect.center().y),
                (&faces, more),
                self.ground(theme),
            );
        }
        self.shared.painted = Some(rect);
    }

    /// The mark as an assistive technology reads it: what it says, and who
    /// sees the window.
    pub(super) fn shared_access(&self) -> Option<AccessNode> {
        let rect = self.shared.painted?;
        let shared = self.shared.marks.get(&self.active_id)?;
        let node = AccessNode::new(crate::a11y::Role::Label)
            .with_name(shared.label())
            .with_bounds(rect);
        Some(if shared.people.is_empty() {
            node
        } else {
            let names: Vec<&str> = shared
                .people
                .iter()
                .map(|(_, name)| name.as_str())
                .collect();
            node.with_description(names.join(", "))
        })
    }

    /// Mark `id` as running a developer's build installed by `installer`,
    /// or clear the mark. `true` when that changed what the bar shows.
    pub fn set_dev_build(&mut self, id: u64, installer: Option<String>) -> bool {
        match installer {
            Some(name) => self.dev_build.installers.insert(id, name.clone()) != Some(name),
            None => self.dev_build.installers.remove(&id).is_some(),
        }
    }

    /// Where the "Dev build · by …" mark of the window in front stands,
    /// with its label: after the "Controls your desktop" chip when that
    /// shows. `None` when the window in front runs no developer's build, its
    /// app put slots in the row, a tab is being dragged, or the run is too
    /// short for it.
    pub fn dev_build_chip(&self, tr: &mut TextRenderer, theme: &Theme) -> Option<(Rect, String)> {
        let installer = self.dev_build.installers.get(&self.active_id)?;
        if self.active_slots().is_some()
            || self.pending_drag.is_some()
            || self.tab_rects.len() != self.tabs.len()
        {
            return None;
        }
        let label = crate::i18n!("tab-dev-build", "installer" => installer.as_str());
        let tab_row = self.tab_row();
        let x0 = match self.desktop_control_chip(tr, theme) {
            Some((control, _)) => control.x1 + TOOLBAR_LEAD_GAP,
            None => self.status_chips_start() + TOOLBAR_LEAD_GAP,
        };
        let limit = self.tab_row_limit(tab_row);
        let width =
            ChipFace::intrinsic_width(&label, ChipLead::Glyph(Icon::Code), false, tr, theme);
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

    /// Paint the mark where it stands.
    pub(super) fn draw_dev_build_chip(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.dev_build.painted = None;
        if let Some((rect, label)) = self.dev_build_chip(tr, theme) {
            ChipFace::new(rect, &label)
                .lead(ChipLead::Glyph(Icon::Code))
                .paint(scene, transform, tr, theme);
            self.dev_build.painted = Some(rect);
        }
    }

    /// The mark as an assistive technology reads it.
    pub(super) fn dev_build_access(&self) -> Option<AccessNode> {
        let rect = self.dev_build.painted?;
        let installer = self.dev_build.installers.get(&self.active_id)?;
        Some(
            AccessNode::new(crate::a11y::Role::Label)
                .with_name(crate::i18n!("tab-dev-build", "installer" => installer.as_str()))
                .with_bounds(rect),
        )
    }

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

#[cfg(test)]
#[path = "tabs_status_chips_tests.rs"]
mod tests;
