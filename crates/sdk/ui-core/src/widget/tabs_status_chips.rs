//! The bar's mark of a window an agent opened for its person: the agent, by
//! its mark and name in its presence colour, which opens the run that opened
//! the window in History. Pointing at it, or reaching it with the keyboard,
//! says which run that was.
//!
//! The mark is a stop of the window's keyboard order of its own
//! ([`super::super::window_group`] lays it out right after the bar): a press
//! from the pointer, the keyboard or assistive technology is reported by
//! [`TabBar::take_opened_by_press`], and the host opens History, since only
//! the host knows the runs.

use super::{Point, Rect, TOOLBAR_LEAD_GAP, TabBar, TextRenderer, Theme};
use crate::a11y::AccessNode;
use crate::i18n::widget_format;
use crate::widget::{ChipFace, ChipLead, ChipStyle};
use tilo_desktop_model::ownership::OpenedBy;
use vello::Scene;
use vello::kurbo::Affine;

/// The accessibility action target of the agent that opened the window.
pub(super) const OPENED_BY_TARGET: &str = "tab-opened-by";

/// The bar's part in the mark: the windows agents opened, by tab, where the
/// mark was last painted, whether the keyboard is on it, and a press waiting
/// for the host.
#[derive(Debug, Default)]
pub(super) struct OpenedByChip {
    runs: std::collections::HashMap<u64, OpenedBy>,
    painted: Option<Rect>,
    focused: bool,
    pressed: Option<(u64, String)>,
}

/// The agent's colour, as presence draws the agent everywhere.
fn agent_colour(run: &OpenedBy) -> vello::peniko::Color {
    let (red, green, blue) = collab_types::presence::agent_rgb(&run.label);
    vello::peniko::Color::from_rgb8(red, green, blue)
}

/// What the agent's mark says when it is pointed at or focused: the run that
/// opened the window, as History names it. The mark names the agent already;
/// a run that goes by nothing else says only that the agent opened it.
fn opened_by_tip(run: &OpenedBy) -> String {
    let title = run.title.trim();
    if title.is_empty() || title == run.label {
        widget_format("tab-opened-by", &[("agent", run.label.as_str().into())])
    } else {
        widget_format("tab-opened-by-run", &[("run", title.into())])
    }
}

impl TabBar {
    /// Mark `id` as a window the agent run `run` opened for its person, or
    /// clear the mark. `true` when that changed what the bar shows.
    pub fn set_opened_by(&mut self, id: u64, run: Option<OpenedBy>) -> bool {
        let changed = match run {
            Some(run) => self.opened.runs.insert(id, run.clone()) != Some(run),
            None => self.opened.runs.remove(&id).is_some(),
        };
        if changed {
            self.touch();
        }
        changed
    }

    /// The agent run that opened the window `id`, when one did.
    pub fn opened_by(&self, id: u64) -> Option<&OpenedBy> {
        self.opened.runs.get(&id)
    }

    /// A press on the agent's mark, once: the tab it was on and the run to
    /// open in History.
    pub fn take_opened_by_press(&mut self) -> Option<(u64, String)> {
        self.opened.pressed.take()
    }

    /// Whether `at` is on the agent's mark, where it was painted.
    pub(super) fn opened_by_hit(&self, at: Point) -> bool {
        self.opened.painted.is_some_and(|rect| rect.contains(at))
            && self.opened_by(self.active_id).is_some()
    }

    /// A press at `at` on the agent's mark: report its run. Whether it was
    /// on the mark.
    pub(super) fn press_opened_by(&mut self, at: Point) -> bool {
        self.touch();
        if !self.opened_by_hit(at) {
            return false;
        }
        let Some(run) = self.opened_by(self.active_id).map(|run| run.run.clone()) else {
            return false;
        };
        self.opened.pressed = Some((self.active_id, run));
        true
    }

    /// The agent's mark activated without a pointer. Whether the bar shows
    /// it.
    pub fn activate_opened_by(&mut self) -> bool {
        self.touch();
        match self.opened.painted {
            Some(rect) => self.press_opened_by(rect.center()),
            None => false,
        }
    }

    /// The keyboard came to the agent's mark, or left it: the mark wears the
    /// toolkit's focus ring while it is there.
    pub(crate) fn set_opened_by_focused(&mut self, focused: bool) {
        if self.opened.focused != focused {
            self.touch();
            self.opened.focused = focused;
        }
    }

    /// Where the agent's mark of the window in front was last painted, and
    /// how assistive technology reads it: a button naming the agent,
    /// described by its run, that opens the run in History. What the
    /// window's keyboard stop for the mark stands on.
    pub(crate) fn opened_by_stop(&self) -> Option<(Rect, AccessNode)> {
        let rect = self.opened.painted?;
        let run = self.opened_by(self.active_id)?;
        let node = AccessNode::new(crate::a11y::Role::Button)
            .with_name(run.label.clone())
            .with_description(opened_by_tip(run))
            .with_action_target(OPENED_BY_TARGET)
            .with_bounds(rect);
        Some((rect, node))
    }

    /// Say in which run the agent opened the window in front, under the
    /// agent's mark, while the pointer rests on it or the keyboard is on it:
    /// in the agent's colour, as the holder of a tab is named. Within `room`,
    /// the part of the screen the window shows in
    /// ([`crate::widget::tooltip::on_screen`]).
    pub(crate) fn draw_opened_by_tip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        room: Rect,
    ) {
        let keyboard = self.opened.focused && crate::focus_visible::shown();
        let Some(rect) = self
            .opened
            .painted
            .filter(|rect| keyboard || rect.contains(self.last_cursor))
        else {
            return;
        };
        let Some(run) = self.opened_by(self.active_id) else {
            return;
        };
        let colour = theme.tokens.colors.presence.of(agent_colour(run));
        crate::widget::tooltip::draw_person_tooltip_on(
            scene,
            transform,
            tr,
            theme,
            (rect, crate::widget::tooltip::TooltipSide::Below),
            (&opened_by_tip(run), colour),
            room,
        );
    }

    /// Where the agent's mark of the window in front stands, with its label:
    /// after the bar's pills, or, where the window's app put its slots in
    /// the bar, right after its location. `None` when no agent opened the
    /// window in front, a tab is being dragged, or there is no room for it.
    pub(super) fn opened_by_chip(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<(Rect, String)> {
        let run = self.opened_by(self.active_id)?;
        if self.pending_drag.is_some() || self.tab_rects.len() != self.tabs.len() {
            return None;
        }
        let label = run.label.clone();
        let (x0, limit, cy) = match self.mark_room() {
            Some(room) => room,
            None if self.active_slots().is_some() => return None,
            None => {
                let tab_row = self.tab_row();
                (
                    self.status_chips_start() + TOOLBAR_LEAD_GAP,
                    self.tab_row_limit(tab_row),
                    tab_row.center().y,
                )
            }
        };
        let lead = ChipLead::Agent(agent_colour(run));
        let width = ChipFace::intrinsic_width(&label, lead, false, tr, theme);
        let height = ChipFace::height(theme);
        (x0 + width <= limit).then(|| {
            (
                Rect::from_origin_size(Point::new(x0, cy - height / 2.0), (width, height)),
                label,
            )
        })
    }

    /// Where the mark may stand in the window in front whose app put its
    /// slots in the bar: right after what its location shows, before
    /// whatever stands after the location, on the location's line. `None`
    /// without slots, while the location is edited, or with no location to
    /// follow.
    fn mark_room(&self) -> Option<(f64, f64, f64)> {
        self.active_slots()?;
        if self.toolbar_row.is_editing() {
            return None;
        }
        let shown = self.toolbar_row.location_content_end()?;
        let slots = self.toolbar_row.slots();
        let location = slots.location;
        let after = slots
            .views
            .iter()
            .chain(&slots.actions)
            .chain(&slots.reactions)
            .chain([&slots.views_menu, &slots.overflow, &slots.reaction_add])
            .filter(|rect| rect.area() > 0.0 && rect.x0 >= location.x1)
            .map(|rect| rect.x0)
            .fold(self.toolbar_row_rect().x1, f64::min);
        Some((
            shown + TOOLBAR_LEAD_GAP,
            after - TOOLBAR_LEAD_GAP,
            location.center().y,
        ))
    }

    /// Paint the mark where it stands: the agent's face and name alone,
    /// quieter than the title beside it, and its pill in the agent's colour
    /// when pointed at or reached with the keyboard, as the button it is.
    pub(super) fn draw_opened_by_chip(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.opened.painted = None;
        let Some((rect, label)) = self.opened_by_chip(tr, theme) else {
            return;
        };
        let Some(run) = self.opened_by(self.active_id) else {
            return;
        };
        let colour = agent_colour(run);
        let focused = self.opened.focused && crate::focus_visible::shown();
        let lifted = focused || rect.contains(self.last_cursor);
        let face = ChipFace::new(rect, &label)
            .lead(ChipLead::Agent(colour))
            .style(ChipStyle::Identity(colour))
            .hovered(lifted)
            .focused(focused);
        if lifted { face } else { face.bare() }.paint(scene, transform, tr, theme);
        self.opened.painted = Some(rect);
    }
}

#[cfg(test)]
#[path = "tabs_status_chips_tests.rs"]
mod tests;
