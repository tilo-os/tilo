//! Collaborator roster popover.
//!
//! This component owns the popover's layout, painting and hit targets. The
//! shell host supplies a policy-filtered model and applies the semantic action
//! returned by [`CollaboratorsOverlay::hit_test`].

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace};

use crate::shell::state::WorkspaceTree;

const FOOTER_H: f64 = 40.0;
const WARNING_H: f64 = 24.0;
const ACTIONS_H: f64 = 40.0;
const INVITED_TAG: &str = "invited";
const INVITED_ROW_ALPHA: f32 = 0.55;

#[allow(
    clippy::cast_precision_loss,
    reason = "a popover can never contain enough rows to exceed f64 integer precision"
)]
fn row_count_as_f64(count: usize) -> f64 {
    count as f64
}

/// What a pointer press on the open popover means to the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollaboratorsHit {
    Invite,
    Join,
    RoomActivity,
    /// Inside the card, but not on an action.
    Absorb,
    /// Outside the card; the host should close it.
    Dismiss,
}

/// One row of the collaborators dropdown. People and agents are listed
/// together, but tagged apart so an agent is never reported as a person.
#[derive(Debug, Clone, PartialEq)]
pub struct RosterRow {
    pub label: String,
    pub initial: char,
    pub color: Color,
    pub is_agent: bool,
    pub role: Option<String>,
    pub invited: bool,
}

/// Immutable data and policy decisions needed to paint one frame.
pub struct CollaboratorsModel<'a> {
    pub host: Rect,
    pub rows: &'a [RosterRow],
    pub unavailable: Option<&'a str>,
    pub may_invite: bool,
}

/// The complete interactive surface of the collaborator popover.
#[derive(Debug, Default)]
pub struct CollaboratorsOverlay {
    panel: Rect,
    invite: Rect,
    join: Rect,
    activity: Rect,
}

impl CollaboratorsOverlay {
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        model: &CollaboratorsModel<'_>,
        cursor: Option<Point>,
    ) {
        self.layout(
            model.host,
            model.rows.len(),
            model.unavailable.is_some(),
            model.may_invite,
        );
        let rect = self.panel;
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let card = RoundedRect::from_rect(rect, f64::from(tokens.radii.lg));
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::shell::panel_ground(theme),
            None,
            &card,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &card,
        );
        text_renderer.draw_text(
            scene,
            "Collaborators",
            tokens.typography.scale.lg,
            colors.text_primary,
            transform.pre_translate((rect.x0 + 16.0, rect.y0 + 16.0).into()),
            None,
        );

        if let Some(reason) = model.unavailable {
            text_renderer.draw_text(
                scene,
                reason,
                tokens.typography.scale.sm,
                colors.semantic.danger,
                transform.pre_translate((rect.x0 + 16.0, rect.y0 + 56.0).into()),
                None,
            );
        }
        if model.rows.is_empty() && model.unavailable.is_none() {
            text_renderer.draw_text(
                scene,
                "No peers connected",
                tokens.typography.scale.sm,
                colors.text_dim,
                transform.pre_translate((rect.x0 + 16.0, rect.y0 + 56.0).into()),
                None,
            );
        }

        let list_top = if model.unavailable.is_some() {
            rect.y0 + 48.0 + WARNING_H
        } else {
            rect.y0 + 48.0
        };
        for (index, row) in model.rows.iter().enumerate() {
            self.draw_row(
                scene,
                transform,
                text_renderer,
                theme,
                row,
                list_top + row_count_as_f64(index) * 36.0,
            );
        }
        self.draw_footer(scene, transform, text_renderer, theme, cursor);
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    #[cfg(test)]
    pub fn panel_rect(&self) -> Rect {
        self.panel
    }

    pub fn action_rects(&self) -> (Rect, Rect) {
        (self.invite, self.join)
    }

    pub fn activity_rect(&self) -> Rect {
        self.activity
    }

    pub fn contains(&self, point: Point) -> bool {
        self.panel.area() > 0.0 && self.panel.contains(point)
    }

    pub fn hit_test(&self, point: Point) -> CollaboratorsHit {
        if self.invite.area() > 0.0 && self.invite.contains(point) {
            CollaboratorsHit::Invite
        } else if self.join.area() > 0.0 && self.join.contains(point) {
            CollaboratorsHit::Join
        } else if self.activity.area() > 0.0 && self.activity.contains(point) {
            CollaboratorsHit::RoomActivity
        } else if self.contains(point) {
            CollaboratorsHit::Absorb
        } else {
            CollaboratorsHit::Dismiss
        }
    }

    fn layout(&mut self, host: Rect, member_count: usize, unavailable: bool, may_invite: bool) {
        let rows = row_count_as_f64(member_count.max(1));
        let warning_h = if unavailable && member_count > 0 {
            WARNING_H
        } else {
            0.0
        };
        let actions_h = if may_invite { ACTIONS_H } else { 0.0 };
        let height = 48.0 + warning_h + rows * 36.0 + 16.0 + actions_h + FOOTER_H;
        let x1 = host.x1 - 16.0;
        let y0 = host.y0 + 48.0;
        self.panel = Rect::new(
            x1 - 260.0,
            y0,
            x1,
            y0 + height.min(420.0 + actions_h + FOOTER_H),
        );
        self.activity = Rect::new(
            self.panel.x0 + 8.0,
            self.panel.y1 - FOOTER_H + 4.0,
            self.panel.x1 - 8.0,
            self.panel.y1 - 8.0,
        );
        if may_invite {
            let y0 = self.panel.y1 - FOOTER_H - ACTIONS_H + 6.0;
            let gap = 8.0;
            let half = (self.panel.width() - 16.0 - gap) / 2.0;
            self.invite = Rect::new(
                self.panel.x0 + 8.0,
                y0,
                self.panel.x0 + 8.0 + half,
                y0 + 28.0,
            );
            self.join = Rect::new(self.invite.x1 + gap, y0, self.panel.x1 - 8.0, y0 + 28.0);
        } else {
            self.invite = Rect::ZERO;
            self.join = Rect::ZERO;
        }
    }

    fn draw_row(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        row: &RosterRow,
        row_y: f64,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let center = Point::new(self.panel.x0 + 28.0, row_y + 14.0);
        let avatar_rect = Rect::new(
            center.x - 11.0,
            center.y - 11.0,
            center.x + 11.0,
            center.y + 11.0,
        );
        if row.invited {
            let seat = vello::kurbo::Circle::new(center, 11.0);
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0).with_dashes(0.0, [3.0, 3.0]),
                transform,
                row.color.multiply_alpha(INVITED_ROW_ALPHA),
                None,
                &seat,
            );
            let initial = row.initial.to_string();
            let size = 22.0_f32 * 0.4;
            let (width, height) =
                text_renderer.measure_weighted(&initial, size, None, TextWeight::Semibold);
            text_renderer.draw_text_weighted(
                scene,
                &initial,
                size,
                colors.text_dim,
                transform.pre_translate(
                    (
                        center.x - f64::from(width) / 2.0,
                        center.y - f64::from(height) / 2.0,
                    )
                        .into(),
                ),
                None,
                TextWeight::Semibold,
            );
        } else {
            ui::widget::Avatar::draw_at(
                scene,
                transform,
                avatar_rect,
                &row.initial.to_string(),
                row.color,
                theme,
                text_renderer,
            );
        }
        text_renderer.draw_text(
            scene,
            &row.label,
            tokens.typography.scale.sm,
            if row.invited {
                colors.text_dim
            } else {
                colors.text_primary
            },
            transform.pre_translate((self.panel.x0 + 44.0, row_y + 7.0).into()),
            None,
        );
        if row.is_agent && !row.label.eq_ignore_ascii_case("agent") {
            text_renderer.draw_text(
                scene,
                "agent",
                tokens.typography.scale.xs,
                colors.text_dim,
                transform.pre_translate((self.panel.x1 - 56.0, row_y + 9.0).into()),
                None,
            );
        }
        let subtitle = if row.invited {
            Some(INVITED_TAG.to_string())
        } else {
            row.role.clone()
        };
        if let Some(subtitle) = subtitle {
            let (width, _) = text_renderer.measure_weighted(
                &subtitle,
                tokens.typography.scale.xs,
                None,
                TextWeight::Regular,
            );
            text_renderer.draw_text(
                scene,
                &subtitle,
                tokens.typography.scale.xs,
                colors.text_dim,
                transform
                    .pre_translate((self.panel.x1 - 16.0 - f64::from(width), row_y + 21.0).into()),
                None,
            );
        }
    }

    fn draw_footer(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        cursor: Option<Point>,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        if self.invite.area() > 0.0 {
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(self.invite, "Invite")
                    .primary()
                    .radius(f64::from(tokens.radii.sm))
                    .hovered(cursor.is_some_and(|point| self.invite.contains(point))),
            );
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(self.join, "Join")
                    .radius(f64::from(tokens.radii.sm))
                    .hovered(cursor.is_some_and(|point| self.join.contains(point))),
            );
        }

        let separator = vello::kurbo::Line::new(
            Point::new(self.activity.x0, self.activity.y0 - 4.0),
            Point::new(self.activity.x1, self.activity.y0 - 4.0),
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &separator,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surface_white_5,
            None,
            &RoundedRect::from_rect(self.activity, f64::from(tokens.radii.sm)),
        );
        let size = tokens.typography.scale.sm;
        let (_, height) = text_renderer.measure("Room activity", size, None);
        text_renderer.draw_text(
            scene,
            "Room activity",
            size,
            colors.text_primary,
            transform.pre_translate(
                (
                    self.activity.x0 + 12.0,
                    self.activity.center().y - f64::from(height) / 2.0,
                )
                    .into(),
            ),
            None,
        );
        let (width, height) = text_renderer.measure("\u{203a}", size, None);
        text_renderer.draw_text(
            scene,
            "\u{203a}",
            size,
            colors.text_dim,
            transform.pre_translate(
                (
                    self.activity.x1 - 12.0 - f64::from(width),
                    self.activity.center().y - f64::from(height) / 2.0,
                )
                    .into(),
            ),
            None,
        );
    }
}

/// Rows the dropdown and compositor presence publisher agree are in the room.
pub fn collaborator_roster_rows(tree: &WorkspaceTree) -> Vec<RosterRow> {
    let roster: &[crate::shell::state::Collaborator] = match tree.active_workspace() {
        Some(workspace) if !workspace.collaborators.is_empty() => &workspace.collaborators,
        _ => &tree.collaborators,
    };
    let colors = crate::shell::state::roster_colors(roster);
    let mut rows: Vec<RosterRow> = roster
        .iter()
        .map(|participant| {
            let (red, green, blue) = colors
                .get(&participant.id)
                .copied()
                .unwrap_or_else(|| ui::presence::user_color(&participant.id));
            RosterRow {
                initial: roster_initial(&participant.name),
                label: participant.name.clone(),
                color: Color::from_rgb8(red, green, blue),
                is_agent: false,
                role: participant.role_id.clone(),
                invited: participant.invited,
            }
        })
        .collect();
    if let Some(workspace) = tree.active_workspace() {
        for (agent, role) in workspace.agent_members() {
            let (red, green, blue) = ui::presence::user_color(&agent);
            rows.push(RosterRow {
                initial: roster_initial(&agent),
                label: agent,
                color: Color::from_rgb8(red, green, blue),
                is_agent: true,
                role: Some(crate::shell::state::role_label(role).to_string()),
                invited: false,
            });
        }
    }
    rows
}

fn roster_initial(name: &str) -> char {
    name.chars()
        .next()
        .map_or('\u{2022}', |character| character.to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_testing_uses_the_same_cached_geometry_as_paint() {
        let mut overlay = CollaboratorsOverlay::default();
        overlay.layout(Rect::new(0.0, 0.0, 1200.0, 800.0), 2, false, true);
        let (invite, join) = overlay.action_rects();
        assert_eq!(overlay.hit_test(invite.center()), CollaboratorsHit::Invite);
        assert_eq!(overlay.hit_test(join.center()), CollaboratorsHit::Join);
        assert_eq!(
            overlay.hit_test(overlay.activity_rect().center()),
            CollaboratorsHit::RoomActivity
        );
        assert_eq!(
            overlay.hit_test(overlay.panel_rect().center()),
            CollaboratorsHit::Absorb
        );
        assert_eq!(overlay.hit_test(Point::ZERO), CollaboratorsHit::Dismiss);
    }

    #[test]
    fn policy_removes_invite_geometry_instead_of_only_hiding_it() {
        let mut overlay = CollaboratorsOverlay::default();
        let host = Rect::new(0.0, 0.0, 1200.0, 800.0);
        overlay.layout(host, 1, false, true);
        assert!(overlay.action_rects().0.area() > 0.0);
        overlay.layout(host, 1, false, false);
        assert_eq!(overlay.action_rects(), (Rect::ZERO, Rect::ZERO));
    }

    #[test]
    fn clearing_drops_every_invisible_hit_target() {
        let mut overlay = CollaboratorsOverlay::default();
        overlay.layout(Rect::new(0.0, 0.0, 1200.0, 800.0), 1, false, true);
        overlay.clear();
        assert_eq!(overlay.panel_rect(), Rect::ZERO);
        assert_eq!(overlay.action_rects(), (Rect::ZERO, Rect::ZERO));
        assert_eq!(overlay.activity_rect(), Rect::ZERO);
    }
}
