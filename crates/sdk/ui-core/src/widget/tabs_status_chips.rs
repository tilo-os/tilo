//! The bar's mark of a window an agent opened for its person: the agent, by its mark and name in
//! its presence colour, which opens the run that opened the window in History. Pointing at it, or
//! reaching it with the keyboard, says which run that was.
use super::{Point, Rect, TOOLBAR_LEAD_GAP, TabBar, TextRenderer, Theme};
use crate::a11y::AccessNode;
use crate::i18n::widget_format;
use crate::widget::{ChipFace, ChipLead, ChipStyle};
use tilo_desktop_model::ownership::OpenedBy;
use vello::Scene;
use vello::kurbo::Affine;

pub(super) const OPENED_BY_TARGET: &str = "tab-opened-by";

#[derive(Debug, Default)]
pub(super) struct OpenedByChip {
    runs: std::collections::HashMap<u64, OpenedBy>,
    painted: Option<Rect>,
    focused: bool,
    pressed: Option<(u64, String)>,
}

/// The agent's colour, as presence draws the agent everywhere: the run's own id, never its display
/// name's.
fn agent_colour(run: &OpenedBy) -> vello::peniko::Color {
    let (red, green, blue) = crate::presence::participant_color(&run.run, true, None);
    vello::peniko::Color::from_rgb8(red, green, blue)
}

fn opened_by_tip(run: &OpenedBy) -> String {
    let title = run.title.trim();
    if title.is_empty() || title == run.label {
        widget_format("tab-opened-by", &[("agent", run.label.as_str().into())])
    } else {
        widget_format("tab-opened-by-run", &[("run", title.into())])
    }
}

impl TabBar {
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

    pub fn opened_by(&self, id: u64) -> Option<&OpenedBy> {
        self.opened.runs.get(&id)
    }

    pub fn take_opened_by_press(&mut self) -> Option<(u64, String)> {
        self.opened.pressed.take()
    }

    pub(super) fn opened_by_hit(&self, at: Point) -> bool {
        self.opened.painted.is_some_and(|rect| rect.contains(at))
            && self.opened_by(self.active_id).is_some()
    }

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

    pub fn activate_opened_by(&mut self) -> bool {
        self.touch();
        match self.opened.painted {
            Some(rect) => self.press_opened_by(rect.center()),
            None => false,
        }
    }

    pub(crate) fn set_opened_by_focused(&mut self, focused: bool) {
        if self.opened.focused != focused {
            self.touch();
            self.opened.focused = focused;
        }
    }

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
