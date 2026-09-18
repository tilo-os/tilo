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
/// Roster row pitch.
const ROW_H: f64 = 48.0;
/// Minimum pointer target (HIG §9.6): every button in this popover —
/// Follow/Stop, Invite, Join — is at least this tall.
const MIN_TARGET: f64 = 32.0;
const INVITED_TAG: &str = "invited";
const INVITED_ROW_ALPHA: f32 = 0.55;

/// A compact arrow with an explicit head. Used where a relationship must
/// remain directional without turning a presence treatment into a label.
fn draw_direction_arrow(
    scene: &mut Scene,
    transform: Affine,
    from: Point,
    to: Point,
    color: Color,
) {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let length = dx.hypot(dy);
    if length <= f64::EPSILON {
        return;
    }
    let ux = dx / length;
    let uy = dy / length;
    let head = 3.5;
    let wing = 2.3;
    let base = Point::new(to.x - ux * head, to.y - uy * head);
    let left = Point::new(base.x - uy * wing, base.y + ux * wing);
    let right = Point::new(base.x + uy * wing, base.y - ux * wing);
    let stroke = vello::kurbo::Stroke::new(1.2);
    scene.stroke(
        &stroke,
        transform,
        color,
        None,
        &vello::kurbo::Line::new(from, to),
    );
    scene.stroke(
        &stroke,
        transform,
        color,
        None,
        &vello::kurbo::Line::new(left, to),
    );
    scene.stroke(
        &stroke,
        transform,
        color,
        None,
        &vello::kurbo::Line::new(right, to),
    );
}

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
    /// Toggle follow for the row at this index.
    ToggleFollow(usize),
    /// Inside the card, but not on an action.
    Absorb,
    /// Outside the card; the host should close it.
    Dismiss,
}

/// One row of the collaborators dropdown. People and agents are listed
/// together, but tagged apart so an agent is never reported as a person.
#[derive(Debug, Clone, PartialEq)]
pub struct RosterRow {
    pub id: String,
    pub label: String,
    pub initial: char,
    pub color: Color,
    pub is_agent: bool,
    pub role: Option<String>,
    pub invited: bool,
    /// Ephemeral current location, never a history summary.
    pub activity: Option<String>,
    pub followable: bool,
}

/// Immutable data and policy decisions needed to paint one frame.
pub struct CollaboratorsModel<'a> {
    pub host: Rect,
    pub rows: &'a [RosterRow],
    pub unavailable: Option<&'a str>,
    pub may_invite: bool,
    pub followed_id: Option<&'a str>,
    /// Ephemeral room-local attachments, expressed using roster ids.  The
    /// People surface can therefore show them when cursors are off-screen.
    pub following_pairs: &'a [(String, String)],
}

/// Direction of a transient attachment relative to one roster row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RosterFollowRelationship {
    Follows(String),
    FollowedBy(String),
}

impl CollaboratorsModel<'_> {
    #[must_use]
    pub fn relationship_for(&self, row_id: &str) -> Option<RosterFollowRelationship> {
        self.following_pairs.iter().find_map(|(follower, target)| {
            (follower == row_id)
                .then(|| RosterFollowRelationship::Follows(target.clone()))
                .or_else(|| {
                    (target == row_id)
                        .then(|| RosterFollowRelationship::FollowedBy(follower.clone()))
                })
        })
    }
}

struct RowFrame<'a> {
    row: &'a RosterRow,
    y: f64,
    followed: bool,
    relationship: Option<RosterFollowRelationship>,
    relationship_partner: Option<&'a RosterRow>,
    cursor: Option<Point>,
}

/// The complete interactive surface of the collaborator popover.
#[derive(Debug, Default)]
pub struct CollaboratorsOverlay {
    panel: Rect,
    invite: Rect,
    join: Rect,
    activity: Rect,
    row_actions: Vec<Rect>,
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
        // Theme type tokens already carry the user's text scale, and this
        // popover lays out and paints with those resolved sizes; the
        // renderer must not scale them a second time (HIG §7a), so it is
        // reset for the pass exactly as the lens pass resets it, and
        // restored for the ordinary child widgets that follow.
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        self.draw_at_theme_scale(scene, transform, text_renderer, theme, model, cursor);
        text_renderer.set_text_scale(renderer_scale);
    }

    fn draw_at_theme_scale(
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
        for (target, row) in self.row_actions.iter_mut().zip(model.rows) {
            if !row.followable || target.y1 > self.activity.y0 - 4.0 {
                *target = Rect::ZERO;
            }
        }
        let rect = self.panel;
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        // Panel role (HIG §9.1/§9.5): the popover's corner is the Panel
        // radius and its edge is derived against the ground, not declared.
        let card = RoundedRect::from_rect(
            rect,
            ui::chrome::ChromeRole::Panel.radius(theme, rect.height()),
        );
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
            ui::chrome::shell::panel_border(theme),
            None,
            &card,
        );
        text_renderer.draw_text(
            scene,
            "People",
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
                "Only you are here",
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
            let relationship = model.relationship_for(&row.id);
            let relationship_partner = relationship.as_ref().and_then(|relationship| {
                let partner_id = match relationship {
                    RosterFollowRelationship::Follows(id)
                    | RosterFollowRelationship::FollowedBy(id) => id,
                };
                model
                    .rows
                    .iter()
                    .find(|candidate| candidate.id == partner_id.as_str())
            });
            self.draw_row(
                scene,
                transform,
                text_renderer,
                theme,
                &RowFrame {
                    row,
                    y: list_top + row_count_as_f64(index) * 48.0,
                    followed: model.followed_id == Some(row.id.as_str()),
                    relationship,
                    relationship_partner,
                    cursor,
                },
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

    #[cfg(test)]
    pub fn row_action_rects(&self) -> &[Rect] {
        &self.row_actions
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
        } else if let Some(index) = self
            .row_actions
            .iter()
            .position(|rect| rect.area() > 0.0 && rect.contains(point))
        {
            CollaboratorsHit::ToggleFollow(index)
        } else if self.contains(point) {
            CollaboratorsHit::Absorb
        } else {
            CollaboratorsHit::Dismiss
        }
    }

    /// The Follow/Stop button of the row whose top edge is `y`: one pointer
    /// target tall, centred in the row. Paint and hit-testing both read
    /// this, so they cannot drift apart.
    fn row_action_rect(panel: Rect, y: f64) -> Rect {
        let top = y + (ROW_H - MIN_TARGET) / 2.0;
        Rect::new(panel.x1 - 88.0, top, panel.x1 - 12.0, top + MIN_TARGET)
    }

    fn layout(&mut self, host: Rect, member_count: usize, unavailable: bool, may_invite: bool) {
        let rows = row_count_as_f64(member_count.max(1));
        let warning_h = if unavailable && member_count > 0 {
            WARNING_H
        } else {
            0.0
        };
        let actions_h = if may_invite { ACTIONS_H } else { 0.0 };
        let height = 48.0 + warning_h + rows * 48.0 + 16.0 + actions_h + FOOTER_H;
        let x1 = host.x1 - 16.0;
        let y0 = host.y0 + 48.0;
        self.panel = Rect::new(
            x1 - 320.0,
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
        self.row_actions = (0..member_count)
            .map(|index| {
                let y = self.panel.y0 + 48.0 + warning_h + row_count_as_f64(index) * 48.0;
                Self::row_action_rect(self.panel, y)
            })
            .collect();
        if may_invite {
            // Buttons are one pointer target tall (HIG §9.6: 32 minimum),
            // centred in the 40 px action band.
            let y0 = self.panel.y1 - FOOTER_H - ACTIONS_H + (ACTIONS_H - MIN_TARGET) / 2.0;
            let gap = 8.0;
            let half = (self.panel.width() - 16.0 - gap) / 2.0;
            self.invite = Rect::new(
                self.panel.x0 + 8.0,
                y0,
                self.panel.x0 + 8.0 + half,
                y0 + MIN_TARGET,
            );
            self.join = Rect::new(
                self.invite.x1 + gap,
                y0,
                self.panel.x1 - 8.0,
                y0 + MIN_TARGET,
            );
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
        frame: &RowFrame<'_>,
    ) {
        let tokens = &theme.tokens;
        if frame.followed {
            let active = RoundedRect::new(
                self.panel.x0 + 8.0,
                frame.y + 3.0,
                self.panel.x1 - 8.0,
                frame.y + 45.0,
                f64::from(tokens.radii.md),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                frame.row.color.multiply_alpha(0.12),
                None,
                &active,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                frame.row.color,
                None,
                &RoundedRect::new(
                    self.panel.x0 + 8.0,
                    frame.y + 11.0,
                    self.panel.x0 + 11.0,
                    frame.y + 37.0,
                    1.5,
                ),
            );
        }

        self.draw_row_identity(scene, transform, text_renderer, theme, frame);
        self.draw_row_text(scene, transform, text_renderer, theme, frame);
    }

    fn draw_row_identity(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        frame: &RowFrame<'_>,
    ) {
        let row = frame.row;
        let colors = &theme.tokens.colors;
        let center = Point::new(self.panel.x0 + 28.0, frame.y + 24.0);
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
            // A live spark belongs on the identity itself: it reads as
            // "here now" without turning the row into a status report.
            let spark = vello::kurbo::Circle::new(Point::new(center.x + 8.0, center.y + 8.0), 3.5);
            scene.stroke(
                &vello::kurbo::Stroke::new(2.0),
                transform,
                ui::chrome::shell::panel_ground(theme),
                None,
                &spark,
            );
            scene.fill(Fill::NonZero, transform, row.color, None, &spark);
            if let (Some(relationship), Some(partner)) =
                (frame.relationship.as_ref(), frame.relationship_partner)
            {
                // Two small, physically paired avatars make the relationship
                // legible even without a cursor. The arrowhead always lands
                // on the followed person, so direction is not inferred from
                // text or row order.
                let (from, to) = match relationship {
                    RosterFollowRelationship::Follows(_) => (
                        Point::new(center.x + 8.0, center.y - 7.0),
                        Point::new(center.x + 17.0, center.y - 12.0),
                    ),
                    RosterFollowRelationship::FollowedBy(_) => (
                        Point::new(center.x - 17.0, center.y + 12.0),
                        Point::new(center.x - 8.0, center.y + 7.0),
                    ),
                };
                let partner_center = match relationship {
                    RosterFollowRelationship::Follows(_) => to,
                    RosterFollowRelationship::FollowedBy(_) => from,
                };
                let partner_rect = Rect::new(
                    partner_center.x - 6.0,
                    partner_center.y - 6.0,
                    partner_center.x + 6.0,
                    partner_center.y + 6.0,
                );
                ui::widget::Avatar::draw_at(
                    scene,
                    transform,
                    partner_rect,
                    &partner.initial.to_string(),
                    partner.color,
                    theme,
                    text_renderer,
                );
                draw_direction_arrow(scene, transform, from, to, row.color.multiply_alpha(0.9));
            }
        }
    }

    fn draw_row_text(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        frame: &RowFrame<'_>,
    ) {
        let row = frame.row;
        let row_y = frame.y;
        let followed = frame.followed;
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let text_x = self.panel.x0 + 44.0;
        let label_x1 = if row.is_agent {
            self.panel.x1 - 142.0
        } else if row.followable {
            self.panel.x1 - 96.0
        } else {
            self.panel.x1 - 12.0
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the collaborator popover is only a few hundred logical pixels wide"
        )]
        let shown_label = text_renderer.elide_to_width(
            &row.label,
            tokens.typography.scale.sm,
            (label_x1 - text_x).max(0.0) as f32,
        );
        text_renderer.draw_text(
            scene,
            &shown_label,
            tokens.typography.scale.sm,
            if row.invited {
                colors.text_dim
            } else {
                colors.text_primary
            },
            transform.pre_translate((text_x, row_y + 8.0).into()),
            None,
        );
        if row.is_agent && !row.label.eq_ignore_ascii_case("agent") {
            text_renderer.draw_text(
                scene,
                "agent",
                tokens.typography.scale.xs,
                colors.text_dim,
                transform.pre_translate((self.panel.x1 - 136.0, row_y + 9.0).into()),
                None,
            );
        }
        let subtitle = if row.invited {
            Some(INVITED_TAG.to_string())
        } else if followed {
            row.activity.as_ref().map_or_else(
                || Some("Following".to_string()),
                |activity| Some(format!("Following · {activity}")),
            )
        } else if let Some(activity) = &row.activity {
            Some(activity.clone())
        } else {
            row.role.clone()
        };
        if let Some(subtitle) = subtitle {
            let subtitle_x1 = if row.followable {
                self.panel.x1 - 96.0
            } else {
                self.panel.x1 - 12.0
            };
            #[allow(
                clippy::cast_possible_truncation,
                reason = "the collaborator popover is only a few hundred logical pixels wide"
            )]
            let subtitle = text_renderer.elide_to_width(
                &subtitle,
                tokens.typography.scale.xs,
                (subtitle_x1 - text_x).max(0.0) as f32,
            );
            text_renderer.draw_text(
                scene,
                &subtitle,
                tokens.typography.scale.xs,
                colors.text_dim,
                transform.pre_translate((text_x, row_y + 27.0).into()),
                None,
            );
        }
        if row.followable {
            let action = Self::row_action_rect(self.panel, row_y);
            // Control role (HIG §9.1): a pressable body takes `radii.lg`;
            // `radii.sm` is for details drawn inside a control.
            // The pass paints with the renderer reset to 1× (see `draw`), so
            // the face is handed the theme-scaled label size explicitly.
            let face = ButtonFace::new(action, if followed { "Stop" } else { "Follow" })
                .radius(ui::chrome::ChromeRole::Control.radius(theme, action.height()))
                .text_size(tokens.typography.scale.base)
                .hovered(frame.cursor.is_some_and(|point| action.contains(point)));
            Button::paint(scene, transform, text_renderer, theme, &face);
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
            // Control role (HIG §9.1): pressable bodies take `radii.lg`.
            let radius = ui::chrome::ChromeRole::Control.radius(theme, self.invite.height());
            // The pass paints with the renderer reset to 1× (see `draw`), so
            // each face is handed the theme-scaled label size explicitly.
            let label_size = tokens.typography.scale.base;
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(self.invite, "Invite")
                    .primary()
                    .radius(radius)
                    .text_size(label_size)
                    .hovered(cursor.is_some_and(|point| self.invite.contains(point))),
            );
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(self.join, "Join")
                    .radius(radius)
                    .text_size(label_size)
                    .hovered(cursor.is_some_and(|point| self.join.contains(point))),
            );
        }

        // Separator inside a Panel (HIG §9.5): derived against the panel
        // ground and inset `s3` from both panel edges, never full-bleed into
        // the corner radius — it was a declared `border_standard` line that
        // vanished on the dark preset.
        ui::surface::separator(
            scene,
            transform,
            self.panel.x0,
            self.panel.x1,
            self.activity.y0 - 4.0,
            f64::from(tokens.spacing.s3),
            ui::chrome::shell::panel_ground(theme),
        );
        // Control role (HIG §9.1): the activity row takes the Control radius
        // (`radii.sm` is for details inside a control) and rests on the lift
        // floor with an edge derived from that floor.
        ui::surface::card(
            scene,
            transform,
            self.activity,
            ui::chrome::ChromeRole::Control.radius(theme, self.activity.height()),
            colors.lift(0.35),
        );
        let size = tokens.typography.scale.sm;
        let (_, height) = text_renderer.measure("Workspace activity", size, None);
        text_renderer.draw_text(
            scene,
            "Workspace activity",
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
    collaborator_roster_rows_with_local_user(tree, &crate::shell::state::local_user_id())
}

/// Builds the same presence rows using an explicitly supplied local principal.
pub(crate) fn collaborator_roster_rows_with_local_user(
    tree: &WorkspaceTree,
    local_user: &str,
) -> Vec<RosterRow> {
    let roster: &[crate::shell::state::Collaborator] = match tree.active_workspace() {
        Some(workspace) if !workspace.collaborators.is_empty() => &workspace.collaborators,
        _ => &tree.collaborators,
    };
    let colors = crate::shell::state::roster_colors_with_local_user(roster, local_user);
    let agent_members = tree
        .active_workspace()
        .map_or_else(Vec::new, crate::shell::state::Workspace::agent_members);
    let rows: Vec<RosterRow> = roster
        .iter()
        .map(|participant| {
            let agent_role = agent_members
                .iter()
                .find(|(agent, _)| agent.eq_ignore_ascii_case(&participant.name))
                .map(|(_, role)| *role);
            let is_agent = agent_role.is_some();
            let (red, green, blue) = colors
                .get(&participant.id)
                .copied()
                .unwrap_or_else(|| ui::presence::user_color(&participant.id));
            let has_view = participant.active_tab.is_some();
            RosterRow {
                id: participant.id.clone(),
                initial: roster_initial(&participant.name),
                label: participant.name.clone(),
                color: Color::from_rgb8(red, green, blue),
                is_agent,
                role: agent_role
                    .map(|role| crate::shell::state::role_label(role).to_string())
                    .or_else(|| participant.role_id.clone()),
                invited: participant.invited,
                activity: participant
                    .active_tab
                    .and_then(|tab| active_tab_title(tree, tab))
                    .map(|title| format!("Viewing {title}"))
                    .or_else(|| {
                        (!participant.invited)
                            .then(|| if is_agent { "Connected" } else { "Here now" }.to_string())
                    }),
                // A headless agent has no pointer/viewport to follow merely
                // because its session is connected. Follow becomes available
                // only when it has published a real view; semantic action
                // targets remain visible through interaction presence.
                followable: !participant.invited && (!is_agent || has_view),
            }
        })
        .collect();
    // The collaborators popover is immediate presence, not membership
    // administration. An agent granted access but not connected belongs in
    // Members; once its live session arrives in `roster`, the name match
    // above tags it as an agent and exposes the same cursor/follow affordance
    // through truthful semantic activity rather than a fabricated cursor.
    rows
}

fn roster_initial(name: &str) -> char {
    name.chars()
        .next()
        .map_or('\u{2022}', |character| character.to_ascii_uppercase())
}

fn active_tab_title(tree: &WorkspaceTree, tab_id: u64) -> Option<String> {
    tree.active_workspace()?
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .find(|tab| tab.id == tab_id)
        .map(|tab| tab.title.trim())
        .filter(|title| !title.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HIG §9.5: the footer separator (derived against the Panel ground) and
    /// the activity row's edge (derived against its lift floor) must both
    /// register on every shipped palette.
    #[test]
    fn footer_separator_and_row_edge_register_on_every_palette() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let ground = ui::chrome::shell::panel_ground(&theme);
            let floor = theme.tokens.colors.lift(0.35);
            for (what, fill) in [("separator", ground), ("activity row", floor)] {
                let delta = ui::surface::edge_delta(ui::surface::edge_on(fill), fill);
                assert!(
                    delta >= ui::tokens::colors::HAIRLINE_MIN_DELTA,
                    "{}: the {what} must separate from its ground (Δ luminance {delta:.4})",
                    theme.name
                );
            }
        }
    }

    /// Nothing in the footer touches (HIG §7): the separator clears the
    /// action band, the row clears the separator and the panel's edge, and
    /// the row keeps its inset from the panel sides.
    #[test]
    fn footer_keeps_a_token_gap_around_the_activity_row() {
        let s1 = f64::from(ui::tokens::Spacing::BASE.s1);
        let mut overlay = CollaboratorsOverlay::default();
        overlay.layout(Rect::new(0.0, 0.0, 1280.0, 800.0), 2, false, true);
        let panel = overlay.panel_rect();
        let activity = overlay.activity_rect();
        let (invite, _join) = overlay.action_rects();
        let separator_y = activity.y0 - 4.0;
        assert!(separator_y - invite.y1 >= s1, "separator clears the action band");
        assert!(activity.y0 - separator_y >= s1, "row clears the separator");
        assert!(panel.y1 - activity.y1 >= s1, "row clears the panel edge");
        assert!(activity.x0 - panel.x0 >= s1 && panel.x1 - activity.x1 >= s1);
    }

    #[test]
    fn every_button_is_at_least_one_pointer_target_and_centred_in_its_band() {
        let mut overlay = CollaboratorsOverlay::default();
        overlay.layout(Rect::new(0.0, 0.0, 1200.0, 800.0), 3, false, true);
        let (invite, join) = overlay.action_rects();
        for rect in [invite, join]
            .into_iter()
            .chain(overlay.row_action_rects().iter().copied())
        {
            assert!(
                rect.height() >= MIN_TARGET && rect.width() >= MIN_TARGET,
                "{rect:?} is at least 32 x 32"
            );
        }
        for (index, rect) in overlay.row_action_rects().iter().enumerate() {
            let row_top = overlay.panel_rect().y0 + 48.0 + row_count_as_f64(index) * ROW_H;
            assert!(
                (rect.center().y - (row_top + ROW_H / 2.0)).abs() < f64::EPSILON,
                "row {index}'s button is centred in its row"
            );
            assert!(rect.y0 >= row_top && rect.y1 <= row_top + ROW_H);
        }
        let band_top = overlay.panel_rect().y1 - FOOTER_H - ACTIONS_H;
        assert!(invite.y0 >= band_top && invite.y1 <= band_top + ACTIONS_H);
        assert!(
            (invite.center().y - (band_top + ACTIONS_H / 2.0)).abs() < f64::EPSILON,
            "footer buttons are centred in the action band"
        );
        assert_eq!(invite.y0, join.y0);
        assert_eq!(invite.y1, join.y1);
    }

    #[test]
    fn hit_testing_uses_the_same_cached_geometry_as_paint() {
        let mut overlay = CollaboratorsOverlay::default();
        overlay.layout(Rect::new(0.0, 0.0, 1200.0, 800.0), 2, false, true);
        let (invite, join) = overlay.action_rects();
        assert_eq!(overlay.hit_test(invite.center()), CollaboratorsHit::Invite);
        assert_eq!(overlay.hit_test(join.center()), CollaboratorsHit::Join);
        assert_eq!(
            overlay.hit_test(overlay.row_action_rects()[0].center()),
            CollaboratorsHit::ToggleFollow(0),
        );
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
        assert!(overlay.row_action_rects().is_empty());
    }

    #[test]
    fn roster_model_keeps_follow_direction_when_cursors_are_not_visible() {
        let rows = vec![
            RosterRow {
                id: "ada".into(),
                label: "Ada".into(),
                initial: 'A',
                color: Color::from_rgb8(1, 2, 3),
                is_agent: false,
                role: None,
                invited: false,
                activity: None,
                followable: true,
            },
            RosterRow {
                id: "bob".into(),
                label: "Bob".into(),
                initial: 'B',
                color: Color::from_rgb8(4, 5, 6),
                is_agent: false,
                role: None,
                invited: false,
                activity: None,
                followable: true,
            },
        ];
        let pairs = vec![("ada".to_string(), "bob".to_string())];
        let model = CollaboratorsModel {
            host: Rect::ZERO,
            rows: &rows,
            unavailable: None,
            may_invite: false,
            followed_id: None,
            following_pairs: &pairs,
        };
        assert_eq!(
            model.relationship_for("ada"),
            Some(RosterFollowRelationship::Follows("bob".into()))
        );
        assert_eq!(
            model.relationship_for("bob"),
            Some(RosterFollowRelationship::FollowedBy("ada".into()))
        );
    }
}
