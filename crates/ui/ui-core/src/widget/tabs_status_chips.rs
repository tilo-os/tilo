//! The bar's mark of a window an agent opened for its person: the agent, by its mark and name in
//! its presence colour, which opens the invocation that opened the window in History. Pointing at it, or
//! reaching it with the keyboard, says whom the agent opened it for and which invocation that was.
use super::{Point, Rect, TOOLBAR_LEAD_GAP, TabBar, TextRenderer, Theme};
use crate::widget::{ChipFace, ChipLead, ChipStyle};
use tilo_actions::opened_by::OpenedBy;
use tilo_ui_base::a11y::AccessNode;
use tilo_ui_base::i18n::widget_format;
use vello::Scene;
use vello::kurbo::Affine;

pub(super) const OPENED_BY_TARGET: &str = "tab-opened-by";

#[derive(Debug, Default)]
pub(super) struct OpenedByChip {
    runs: std::collections::HashMap<u64, OpenedBy>,
    /// The person each window's agent opened it for, by the name this page knows them by.
    opened_for: std::collections::HashMap<u64, String>,
    painted: Option<Rect>,
    pressed: Option<(u64, String)>,
}

/// The agent's colour, as presence draws the agent everywhere: by the agent, never by its display
/// name or by the session that opened the window.
fn agent_colour(opened: &OpenedBy) -> vello::peniko::Color {
    let (red, green, blue) = tilo_ui_base::presence::member_color(&opened.agent);
    vello::peniko::Color::from_rgb8(red, green, blue)
}

/// What pointing at the mark says: who opened the window, and for whom when the page knows them,
/// then on a line of its own the step that opened it, when the agent titled one.
fn opened_by_tip(opened: &OpenedBy, opened_for: Option<&str>) -> String {
    let agent = opened.label.as_str();
    let who = match opened_for
        .map(str::trim)
        .filter(|person| !person.is_empty())
    {
        Some(person) => widget_format(
            "tab-opened-by-for",
            &[("agent", agent.into()), ("person", person.into())],
        ),
        None => widget_format("tab-opened-by", &[("agent", agent.into())]),
    };
    let title = opened.title.trim();
    if title.is_empty() || title == opened.label {
        who
    } else {
        let step = widget_format("tab-opened-by-run", &[("run", title.into())]);
        format!("{who}\n{step}")
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

    /// Name the person the agent that opened `id` works for, as this page knows them, or forget
    /// them. `true` when that changed.
    pub fn set_opened_for(&mut self, id: u64, person: Option<String>) -> bool {
        let changed = match person {
            Some(person) => self.opened.opened_for.insert(id, person.clone()) != Some(person),
            None => self.opened.opened_for.remove(&id).is_some(),
        };
        if changed {
            self.touch();
        }
        changed
    }

    fn opened_tip(&self, id: u64) -> Option<String> {
        let opened = self.opened_by(id)?;
        Some(opened_by_tip(
            opened,
            self.opened.opened_for.get(&id).map(String::as_str),
        ))
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

    pub(super) fn opened_by_stop(&self) -> Option<(Rect, AccessNode)> {
        let rect = self.opened.painted?;
        let run = self.opened_by(self.active_id)?;
        let node = AccessNode::new(tilo_ui_base::a11y::Role::Button)
            .with_name(run.label.clone())
            .with_description(self.opened_tip(self.active_id).unwrap_or_default())
            .with_action_target(OPENED_BY_TARGET)
            .with_bounds(rect);
        Some((rect, node))
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
        let (x0, limit, cy) = match self.mark_span() {
            Some(spare) => spare,
            None if self.shown_toolbar().is_some() => return None,
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

    fn mark_span(&self) -> Option<(f64, f64, f64)> {
        self.shown_toolbar()?;
        if self.toolbar_row.is_editing() {
            return None;
        }
        let shown = self.toolbar_row.location_content_end()?;
        let parts = self.toolbar_row.parts();
        let location = parts.location;
        let after = parts
            .views
            .iter()
            .chain(&parts.actions)
            .chain(&parts.reactions)
            .chain([&parts.views_menu, &parts.overflow, &parts.reaction_add])
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
        let lifted = rect.contains(self.last_cursor);
        let face = ChipFace::new(rect, &label)
            .lead(ChipLead::Agent(colour))
            .style(ChipStyle::Identity(colour))
            .hovered(lifted);
        if lifted { face } else { face.bare() }.paint(scene, transform, tr, theme);
        self.opened.painted = Some(rect);
    }
}

#[cfg(test)]
#[path = "tabs_status_chips_tests.rs"]
mod tests;
