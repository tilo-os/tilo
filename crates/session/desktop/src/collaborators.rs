//! Collaborator roster popover.
//!
//! This component owns the popover's layout, painting and hit targets. The
//! host supplies a policy-filtered model and applies the semantic action
//! returned by [`CollaboratorsOverlay::hit_test`].

use std::collections::HashMap;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

use ui::agent_identity::{AgentIdentityStyle, draw_agent_identity_mark};
use ui::icons::Icon;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, ControlSize, FieldFace, SegmentedFace, TextInput, Widget};
use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use tilo_desktop_model::{Collaborator, DesktopTree};
use ui::TextRole;
use ui::widget::ButtonRole;

const FOOTER_H: f64 = 40.0;
const ACTIONS_H: f64 = 40.0;
/// Minimum pointer target (HIG §9.6): every button in this popover —
/// Follow/Stop, Invite, Join — is at least this tall.
const MIN_TARGET: f64 = 32.0;
const INVITED_ROW_ALPHA: f32 = 0.55;
const ROSTER_PAGE_SIZE: usize = 3;
const ROSTER_PAGER_H: f64 = 36.0;

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
    EditStatus,
    SetStatusSharing(ui::StatusSharing),
    /// Toggle follow for the row at this index.
    ToggleFollow(usize),
    /// Open the durable member or agent settings destination for this row.
    OpenSettings(usize),
    PreviousPeople,
    NextPeople,
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
    /// Human membership role. Agents never inherit this as ambient authority.
    pub role: Option<String>,
    /// Honest summary of an agent's explicit scoped-authority preset.
    pub authority: Option<String>,
    pub invited: bool,
    /// Ephemeral current location, never a history summary.
    pub activity: Option<String>,
    /// Short participant intention. `automatic` is rendered with a sparkle so
    /// model-suggested words cannot be mistaken for a direct quote.
    pub status: Option<RosterStatus>,
    pub followable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterStatus {
    pub text: String,
    pub automatic: bool,
}

/// Immutable data and policy decisions needed to paint one frame.
pub struct CollaboratorsModel<'a> {
    pub host: Rect,
    pub rows: &'a [RosterRow],
    pub own_initial: char,
    pub own_color: Color,
    pub unavailable: Option<&'a str>,
    pub may_invite: bool,
    pub followed_id: Option<&'a str>,
    /// Ephemeral room-local attachments, expressed using roster ids.  The
    /// People surface can therefore show them when cursors are off-screen.
    pub following_pairs: &'a [(String, String)],
    pub own_status: Option<&'a str>,
    /// Local participant's ephemeral semantic action, shown separately from
    /// their longer-lived intent/status so the two cannot be conflated.
    pub own_activity: Option<&'a str>,
    pub own_status_automatic: bool,
    pub status_sharing: ui::StatusSharing,
    pub status_editing: bool,
    pub status_selection: ui::text_selection::Selection,
    /// Compositor-authoritative disclosure of exactly who can currently see
    /// or control which shared surface. Presence alone never implies this.
    pub screen_exposure: Option<&'a str>,
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
    index: usize,
    row: &'a RosterRow,
    y: f64,
    followed: bool,
    relationship: Option<RosterFollowRelationship>,
    relationship_partner: Option<&'a RosterRow>,
    cursor: Option<Point>,
}

#[derive(Default)]
struct StatusField(Option<TextInput>);

impl std::fmt::Debug for StatusField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StatusField")
            .finish_non_exhaustive()
    }
}

/// The complete interactive surface of the collaborator popover.
#[derive(Debug, Default)]
pub struct CollaboratorsOverlay {
    panel: Rect,
    viewport: Rect,
    host: Rect,
    scroll_gutter: f64,
    scroll: Option<ui::widget::ScrollRegion>,
    scroll_theme: Option<Theme>,
    status_input: StatusField,
    header_height: f64,
    exposure_height: f64,
    exposure_line_height: f64,
    warning_height: f64,
    status_height: f64,
    status_block_height: f64,
    mode_height: f64,
    stacked_modes: bool,
    follow_width: f64,
    follow_height: f64,
    activity_height: f64,
    control_height: f64,
    row_height: f64,
    invite: Rect,
    join: Rect,
    activity: Rect,
    row_actions: Vec<Rect>,
    status: Rect,
    status_modes: Vec<(ui::StatusSharing, Rect)>,
    row_rects: Vec<Rect>,
    roster_offset: usize,
    roster_range: std::ops::Range<usize>,
    roster_page_size: usize,
    previous_people: Rect,
    next_people: Rect,
    keyboard_focus: Option<CollaboratorsHit>,
    targets: ui::widget::PaintedTargets,
    painted_actions: Vec<(String, CollaboratorsHit)>,
    held_submit: [bool; 2],
    accessibility_rows: Vec<RosterRow>,
    accessibility_status: Option<String>,
    accessibility_own_activity: Option<String>,
    accessibility_status_automatic: bool,
    accessibility_sharing: ui::StatusSharing,
    accessibility_editing: bool,
    accessibility_screen_exposure: Option<String>,
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
        // Focus names the person the user selected, not their changing
        // position in the live roster.
        let focus = match self.keyboard_focus {
            Some(CollaboratorsHit::ToggleFollow(index)) => self
                .accessibility_rows
                .get(index)
                .and_then(|selected| {
                    model
                        .rows
                        .iter()
                        .position(|row| row.id == selected.id && row.followable)
                })
                .map(CollaboratorsHit::ToggleFollow),
            Some(CollaboratorsHit::OpenSettings(index)) => self
                .accessibility_rows
                .get(index)
                .and_then(|selected| model.rows.iter().position(|row| row.id == selected.id))
                .map(CollaboratorsHit::OpenSettings),
            other => other,
        };
        let previous_focus_rect = self.focused_rect();
        let previous_panel = self.panel;
        let previous_host = self.host;
        self.keyboard_focus = focus;
        self.measure_controls(text_renderer, theme, model);
        self.layout(
            model.host,
            model.rows.len(),
            model.unavailable.is_some(),
            model.may_invite,
            model.screen_exposure.is_some(),
        );
        if self.panel != previous_panel || model.host != previous_host {
            self.cancel_pointer_gesture();
        }
        self.configure_scroll(theme, model);
        self.partition_sharing(text_renderer, theme);
        self.accessibility_rows.clear();
        self.accessibility_rows.extend_from_slice(model.rows);
        self.accessibility_status = model.own_status.map(str::to_string);
        self.accessibility_own_activity = model.own_activity.map(str::to_string);
        self.accessibility_status_automatic = model.own_status_automatic;
        self.accessibility_sharing = model.status_sharing;
        self.accessibility_editing = model.status_editing;
        self.accessibility_screen_exposure = model.screen_exposure.map(str::to_string);
        for (target, row) in self.row_actions.iter_mut().zip(model.rows) {
            if !row.followable || target.y1 > self.activity.y0 - 4.0 {
                *target = Rect::ZERO;
            }
        }
        self.keyboard_focus = self
            .reconcile_pager_focus(focus)
            .filter(|hit| self.rect_for_hit(*hit).is_some());
        if previous_focus_rect != self.focused_rect() {
            self.reveal_focus();
        }
        self.register_targets(model.host);
        let rect = self.viewport;
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
        let cursor = cursor.map(|point| self.content_point(point));
        if let Some(scroll) = self.scroll.take() {
            scroll.paint(scene, transform, theme, |scene, content_transform| {
                let content_transform =
                    content_transform.pre_translate((-self.panel.x0, -self.panel.y0).into());
                self.draw_contents(
                    scene,
                    content_transform,
                    text_renderer,
                    theme,
                    model,
                    cursor,
                );
            });
            self.scroll = Some(scroll);
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "a paint routine that lays its elements out top to bottom from shared running offsets"
    )]
    fn draw_contents(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        model: &CollaboratorsModel<'_>,
        cursor: Option<Point>,
    ) {
        let rect = self.panel;
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        text_renderer.draw_style(
            scene,
            &crate::i18n::t("a11y-shell-people"),
            colors.text_primary,
            transform.pre_translate((rect.x0 + 16.0, rect.y0 + 16.0).into()),
            None,
            &tokens.typography.style(TextRole::Heading),
        );
        let live_rows = model
            .rows
            .iter()
            .filter(|row| !row.invited)
            .collect::<Vec<_>>();
        let here_count = live_rows.len().saturating_add(1);
        let here = crate::i18n::t_args("people-panel-here-now", &[("count", here_count.into())]);
        let (here_width, _) =
            text_renderer.measure_style(&here, None, &tokens.typography.style(TextRole::Caption));
        let here_x = rect.x1 - 14.0 - f64::from(here_width);
        text_renderer.draw_style(
            scene,
            &here,
            colors.text_dim,
            transform.pre_translate((here_x, rect.y0 + 20.0).into()),
            None,
            &tokens.typography.style(TextRole::Caption),
        );
        let dot_colors = std::iter::once(model.own_color)
            .chain(live_rows.iter().map(|row| row.color))
            .take(4)
            .collect::<Vec<_>>();
        let dots_x = here_x - 8.0 - row_count_as_f64(dot_colors.len()) * 7.0;
        for (index, color) in dot_colors.into_iter().enumerate() {
            let x = dots_x + row_count_as_f64(index) * 7.0;
            let dot = vello::kurbo::Circle::new(Point::new(x + 3.0, rect.y0 + 25.0), 3.0);
            scene.fill(Fill::NonZero, transform, color, None, &dot);
        }
        self.draw_own_status(scene, transform, text_renderer, theme, model, cursor);
        if let Some(exposure) = model.screen_exposure {
            let banner = Rect::new(
                rect.x0 + 12.0,
                rect.y0 + self.header_height.max(48.0) + self.status_block_height.max(92.0),
                rect.x1 - 12.0,
                rect.y0
                    + self.header_height.max(48.0)
                    + self.status_block_height.max(92.0)
                    + self.exposure_height.max(44.0)
                    - 4.0,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                colors.intent.inverted.multiply_alpha(0.12),
                None,
                &RoundedRect::from_rect(banner, f64::from(tokens.radii.detail)),
            );
            text_renderer.draw_style(
                scene,
                &crate::i18n::t("people-panel-screen-visible"),
                colors.intent.inverted,
                transform.pre_translate((banner.x0 + 8.0, banner.y0 + 5.0).into()),
                None,
                &tokens.typography.style(TextRole::Caption),
            );
            text_renderer.draw_style(
                scene,
                exposure,
                colors.text_primary,
                transform.pre_translate(
                    (banner.x0 + 8.0, banner.y0 + 7.0 + self.exposure_line_height).into(),
                ),
                Some(Self::disclosure_width(theme)),
                &tokens.typography.style(TextRole::Caption),
            );
        }

        let roster_top = rect.y0
            + self.header_height.max(48.0)
            + self.status_block_height.max(92.0)
            + if model.screen_exposure.is_some() {
                self.exposure_height.max(44.0)
            } else {
                0.0
            };
        if let Some(reason) = model.unavailable {
            text_renderer.draw_style(
                scene,
                reason,
                colors.semantic.danger,
                transform.pre_translate((rect.x0 + 16.0, roster_top + 4.0).into()),
                Some(Self::disclosure_width(theme)),
                &tokens.typography.style(TextRole::Caption),
            );
        }
        if model.rows.is_empty() && model.unavailable.is_none() {
            text_renderer.draw_style(
                scene,
                &crate::i18n::t("people-panel-only-you"),
                colors.text_dim,
                transform.pre_translate((rect.x0 + 16.0, roster_top + 4.0).into()),
                None,
                &tokens.typography.style(TextRole::Caption),
            );
        }

        let list_top = rect.y0
            + self.header_height.max(48.0)
            + self.status_block_height.max(92.0)
            + if model.screen_exposure.is_some() {
                self.exposure_height.max(44.0)
            } else {
                0.0
            }
            + if model.unavailable.is_some() {
                self.warning_height.max(24.0)
            } else {
                0.0
            };
        for (visible_index, index) in self.roster_range.clone().enumerate() {
            #[expect(
                clippy::indexing_slicing,
                reason = "layout() above clamps roster_range to model.rows.len()"
            )]
            let row = &model.rows[index];
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
                    index,
                    row,
                    y: list_top + row_count_as_f64(visible_index) * self.row_height.max(64.0),
                    followed: model.followed_id == Some(row.id.as_str()),
                    relationship,
                    relationship_partner,
                    cursor,
                },
            );
        }
        self.draw_roster_pager(
            scene,
            transform,
            text_renderer,
            theme,
            model.rows.len(),
            cursor,
        );
        self.draw_footer(scene, transform, text_renderer, theme, cursor);
        if let Some(rect) = self
            .focused_rect()
            .filter(|_| !matches!(self.keyboard_focus, Some(CollaboratorsHit::ToggleFollow(_))))
        {
            scene.stroke(
                &vello::kurbo::Stroke::new(2.0),
                transform,
                theme.tokens.colors.intent.inverted_hover,
                None,
                &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.detail)),
            );
        }
    }

    fn configure_scroll(&mut self, theme: &Theme, model: &CollaboratorsModel<'_>) {
        let available = (model.host.y1 - self.panel.y0 - 8.0).max(1.0);
        let gutter = if self.panel.height() > available {
            f64::from(theme.tokens.metrics.scrollbar_gutter)
        } else {
            0.0
        };
        if (gutter - self.scroll_gutter).abs() > f64::EPSILON {
            self.scroll_gutter = gutter;
            self.layout(
                model.host,
                model.rows.len(),
                model.unavailable.is_some(),
                model.may_invite,
                model.screen_exposure.is_some(),
            );
        }
        self.viewport = Rect::new(
            self.panel.x0,
            self.panel.y0,
            self.panel.x1 + gutter,
            self.panel.y0 + self.panel.height().min(available),
        );
        let scroll = self
            .scroll
            .get_or_insert_with(ui::widget::ScrollRegion::vertical);
        scroll.set_viewport(self.viewport, theme);
        scroll.set_content_size(self.panel.width(), self.panel.height());
        self.scroll_theme = (gutter > 0.0).then(|| theme.clone());
    }

    fn scroll_offset(&self) -> f64 {
        self.scroll.as_ref().map_or(0.0, |scroll| scroll.offset().y)
    }

    fn view_rect(&self, rect: Rect) -> Rect {
        if rect.area() <= 0.0 {
            return rect;
        }
        Affine::translate((0.0, -self.scroll_offset())).transform_rect_bbox(rect)
    }

    fn content_point(&self, point: Point) -> Point {
        Point::new(point.x, point.y + self.scroll_offset())
    }

    fn reconcile_pager_focus(&self, focus: Option<CollaboratorsHit>) -> Option<CollaboratorsHit> {
        match focus {
            Some(CollaboratorsHit::NextPeople) if self.next_people.area() <= 0.0 => {
                (self.previous_people.area() > 0.0).then_some(CollaboratorsHit::PreviousPeople)
            }
            Some(CollaboratorsHit::PreviousPeople) if self.previous_people.area() <= 0.0 => {
                (self.next_people.area() > 0.0).then_some(CollaboratorsHit::NextPeople)
            }
            other => other,
        }
    }

    fn reveal_focus(&mut self) {
        let Some(rect) = self.focused_rect() else {
            return;
        };
        let content = Affine::translate((-self.panel.x0, -self.panel.y0))
            .transform_rect_bbox(rect.inflate(3.0, 3.0));
        if let Some(scroll) = &mut self.scroll {
            let previous = scroll.offset();
            scroll.scroll_into_view(content);
            if previous != scroll.offset() {
                self.targets.cancel_pointer_press();
                self.status_input.0 = None;
            }
        }
    }

    fn scroll_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool {
        let Some(theme) = self.scroll_theme.as_ref() else {
            return false;
        };
        let Some(scroll) = self.scroll.as_mut() else {
            return false;
        };
        if let WindowEvent::KeyboardInput { event: key, .. } = event {
            if !matches!(
                key.logical_key,
                Key::Named(NamedKey::PageUp | NamedKey::PageDown)
            ) {
                return false;
            }
            scroll.set_focused(true);
        }
        let previous = scroll.offset();
        let changed = scroll.handle_event(event, ctx, theme);
        if previous != scroll.offset() {
            self.targets.cancel_pointer_press();
            self.status_input.0 = None;
        }
        changed && *ctx.handled
    }

    pub fn primary_vertical_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        self.scroll.as_ref()?.primary_vertical_scroll()
    }

    pub fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        let Some(scroll) = &mut self.scroll else {
            return ui::widget::PrimaryScrollResult::Unsupported;
        };
        let result = scroll.apply_primary_vertical_scroll(action);
        if matches!(result, ui::widget::PrimaryScrollResult::Changed(_)) {
            self.targets.cancel_pointer_press();
            self.status_input.0 = None;
            self.register_targets(self.host);
        }
        result
    }

    pub fn clear(&mut self) {
        self.interrupt();
        let mut targets = std::mem::take(&mut self.targets);
        targets.begin_frame();
        let held_submit = self.held_submit;
        *self = Self::default();
        self.targets = targets;
        self.held_submit = held_submit;
    }

    fn target_id(&self, hit: CollaboratorsHit) -> Option<String> {
        match hit {
            CollaboratorsHit::ToggleFollow(index) => {
                self.follow_target(index).map(|id| format!("follow:{id}"))
            }
            CollaboratorsHit::OpenSettings(index) => self
                .settings_target(index)
                .map(|(id, _)| format!("settings:{id}")),
            other => Some(format!("{other:?}")),
        }
    }

    fn interaction(&self, hit: CollaboratorsHit) -> ui::widget::Interaction {
        self.target_id(hit)
            .map_or_else(ui::widget::Interaction::default, |id| {
                self.targets.interaction(&id)
            })
    }

    fn register_targets(&mut self, host: Rect) {
        let armed = self
            .painted_actions
            .iter()
            .find(|(id, _)| self.targets.interaction(id).pressed)
            .map(|(id, _)| id.clone());
        self.targets.begin_frame();
        self.painted_actions.clear();
        // Non-action regions own the whole gesture too, so an outside click
        // can dismiss without handing its release to the app behind People.
        for (hit, rect) in [
            (CollaboratorsHit::Dismiss, host),
            (CollaboratorsHit::Absorb, self.panel_rect()),
        ] {
            if let Some(id) = self.target_id(hit) {
                self.targets.target(&id, rect, "");
                self.painted_actions.push((id, hit));
            }
        }
        for hit in self.focus_order() {
            if let (Some(id), Some(rect)) = (self.target_id(hit), self.rect_for_hit(hit)) {
                let rect = self.view_rect(rect).intersect(self.panel_rect());
                if rect.area() > 0.0 {
                    self.targets.target(&id, rect, "");
                    self.painted_actions.push((id, hit));
                }
            }
        }
        if let Some(scroll) = &self.scroll
            && scroll.overflows_vertically()
        {
            let rect = Rect::new(
                scroll.content_rect().x1,
                self.viewport.y0,
                self.viewport.x1,
                self.viewport.y1,
            );
            self.targets.target("scrollbar", rect, "");
            self.painted_actions
                .push(("scrollbar".into(), CollaboratorsHit::Absorb));
        }
        if armed.is_some_and(|id| {
            !self
                .painted_actions
                .iter()
                .any(|(candidate, _)| *candidate == id)
        }) {
            self.targets.cancel_pointer_press();
        }
    }

    fn cancel_pointer_gesture(&mut self) {
        self.status_input.0 = None;
        if let Some(scroll) = self.scroll.take()
            && let Some(theme) = &self.scroll_theme
        {
            let mut reset = ui::widget::ScrollRegion::vertical();
            reset.set_viewport(self.viewport, theme);
            reset.set_content_size(self.panel.width(), self.panel.height());
            reset.set_offset(scroll.offset());
            self.scroll = Some(reset);
        }
        self.targets.cancel_pointer_press();
    }

    pub fn interrupt(&mut self) {
        self.cancel_pointer_gesture();
        self.targets.set_host_focus(false);
        self.keyboard_focus = None;
    }

    pub fn needs_animation_frame(&self) -> bool {
        self.scroll_theme.as_ref().is_some_and(|theme| {
            self.scroll
                .as_ref()
                .is_some_and(|scroll| scroll.needs_animation_frame(theme))
        })
    }

    pub fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let caret = self
            .status_input
            .0
            .as_ref()
            .and_then(Widget::next_wake_deadline);
        let scrollbar = self.scroll_theme.as_ref().and_then(|theme| {
            self.scroll
                .as_ref()
                .and_then(|scroll| scroll.next_wake_deadline(theme))
        });
        caret.into_iter().chain(scrollbar).min()
    }

    pub fn wants_pointer_capture(&self) -> bool {
        self.targets.wants_pointer_capture()
    }

    /// Release/repeat edges still belong to People after its action changes
    /// the active surface. They must be consumed before ordinary modal routing.
    pub fn consume_sequence_edge(&mut self, event: &WindowEvent, open: bool) -> bool {
        if !open
            || matches!(
                event,
                WindowEvent::Focused(false) | WindowEvent::CursorLeft { .. }
            )
        {
            self.interrupt();
        }
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return false;
        };
        let index = match key.logical_key {
            Key::Named(NamedKey::Enter) => 0,
            Key::Named(NamedKey::Space) => 1,
            _ => return false,
        };
        let Some(held) = self.held_submit.get_mut(index) else {
            return false;
        };
        if *held && (key.repeat || key.state == ElementState::Released) {
            if key.state == ElementState::Released {
                *held = false;
            }
            return true;
        }
        false
    }

    /// Pointer editing reuses `TextInput`'s caret, word selection and drag rules;
    /// the host remains authoritative for the text and keyboard edits.
    pub fn status_pointer_selection(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        cursor: Option<Point>,
    ) -> Option<ui::text_selection::Selection> {
        if !matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput {
                    button: MouseButton::Left,
                    ..
                }
        ) {
            return None;
        }
        let point = self.content_point(cursor?);
        let input = self.status_input.0.as_mut()?;
        let moved = WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(
                point.x * ctx.scale_factor,
                point.y * ctx.scale_factor,
            ),
        };
        input.handle_event(&moved, ctx);
        if matches!(event, WindowEvent::MouseInput { .. }) {
            input.handle_event(event, ctx);
        }
        let (anchor, cursor) = input
            .selection_anchor_focus_public()
            .unwrap_or_else(|| (input.cursor_pos(), input.cursor_pos()));
        Some(ui::text_selection::Selection { anchor, cursor })
    }

    fn handle_closed_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.interrupt();
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        ) {
            self.targets = ui::widget::PaintedTargets::new();
            return;
        }
        if self.targets.wants_pointer_capture() {
            self.targets.handle_event(event, ctx);
            if matches!(
                event,
                WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
            ) {
                *ctx.handled = true;
            }
        }
    }

    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        open: bool,
        cursor: Option<Point>,
    ) -> Option<CollaboratorsHit> {
        if !open {
            self.handle_closed_event(event, ctx);
            return None;
        }
        if matches!(
            event,
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. }
        ) {
            if let Some(point) = cursor {
                let moved = WindowEvent::CursorMoved {
                    device_id: DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(
                        point.x * ctx.scale_factor,
                        point.y * ctx.scale_factor,
                    ),
                };
                self.targets.handle_event(&moved, ctx);
                self.scroll_event(&moved, ctx);
            } else {
                let owned = self.targets.wants_pointer_capture();
                self.interrupt();
                if matches!(
                    event,
                    WindowEvent::MouseInput {
                        state: ElementState::Released,
                        ..
                    }
                ) {
                    self.targets.handle_event(event, ctx);
                }
                *ctx.handled = open || owned;
                return None;
            }
        }
        let scrolled = self.scroll_event(event, ctx);
        if let WindowEvent::KeyboardInput { event: key, .. } = event {
            if scrolled {
                self.register_targets(self.host);
                return None;
            }
            return self.handle_submit(event, key, ctx);
        }
        if scrolled {
            self.targets.handle_event(event, ctx);
            self.take_activated_hit();
            self.register_targets(self.host);
            return None;
        }
        if !matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::CursorLeft { .. }
                | WindowEvent::Focused(false)
                | WindowEvent::MouseInput {
                    button: MouseButton::Left,
                    ..
                }
        ) {
            return None;
        }
        self.targets.handle_event(event, ctx);
        if matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput {
                    button: MouseButton::Left,
                    ..
                }
        ) {
            *ctx.handled = true;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            ..
        } = event
        {
            self.keyboard_focus = self
                .targets
                .focused()
                .and_then(|id| {
                    self.painted_actions
                        .iter()
                        .find(|(candidate, _)| candidate == id)
                        .map(|(_, hit)| *hit)
                })
                .filter(|hit| !matches!(hit, CollaboratorsHit::Absorb | CollaboratorsHit::Dismiss));
        }
        self.take_activated_hit()
    }

    fn handle_submit(
        &mut self,
        event: &WindowEvent,
        key: &winit::event::KeyEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> Option<CollaboratorsHit> {
        if key.logical_key == Key::Named(NamedKey::Escape) && key.state == ElementState::Pressed {
            self.interrupt();
            *ctx.handled = true;
            return Some(CollaboratorsHit::Dismiss);
        }
        let index = match key.logical_key {
            Key::Named(NamedKey::Enter) => Some(0),
            Key::Named(NamedKey::Space) => Some(1),
            _ => None,
        };
        let index = index?;
        *ctx.handled = true;
        let held = self.held_submit.get_mut(index)?;
        if key.state == ElementState::Released {
            *held = false;
            return None;
        }
        *held = true;
        if key.repeat {
            return None;
        }
        self.reveal_focus();
        self.register_targets(self.host);
        let focus = self.keyboard_focus.and_then(|hit| self.target_id(hit))?;
        self.targets.set_host_focus(true);
        self.targets.focus(&focus);
        self.targets.handle_event(event, ctx);
        self.take_activated_hit()
    }

    fn take_activated_hit(&mut self) -> Option<CollaboratorsHit> {
        self.targets.take_activated().into_iter().find_map(|id| {
            self.painted_actions
                .iter()
                .find(|(candidate, _)| *candidate == id)
                .map(|(_, hit)| *hit)
        })
    }

    pub fn panel_rect(&self) -> Rect {
        if self.viewport.area() > 0.0 {
            self.viewport
        } else {
            self.panel
        }
    }

    pub fn action_rects(&self) -> (Rect, Rect) {
        (self.view_rect(self.invite), self.view_rect(self.join))
    }

    pub fn activity_rect(&self) -> Rect {
        self.view_rect(self.activity)
    }

    #[cfg(test)]
    pub fn row_action_rects(&self) -> &[Rect] {
        &self.row_actions
    }

    pub fn contains(&self, point: Point) -> bool {
        self.panel_rect().area() > 0.0 && self.panel_rect().contains(point)
    }

    /// The stable person behind a painted Follow button. The host must
    /// revalidate that person against its live roster before applying Follow.
    pub fn follow_target(&self, index: usize) -> Option<&str> {
        self.row_actions
            .get(index)
            .filter(|rect| rect.area() > 0.0)
            .and_then(|_| self.accessibility_rows.get(index))
            .filter(|row| row.followable)
            .map(|row| row.id.as_str())
    }

    /// Stable identity and kind behind a painted durable-settings row action.
    /// The host revalidates both against its fresh workspace roster before it
    /// opens either Members or Agents.
    pub fn settings_target(&self, index: usize) -> Option<(&str, bool)> {
        if self.settings_rect(index).area() <= 0.0 {
            return None;
        }
        self.accessibility_rows
            .get(index)
            .map(|row| (row.id.as_str(), row.is_agent))
    }

    pub fn hit_test(&self, point: Point) -> CollaboratorsHit {
        if !self.contains(point) {
            return CollaboratorsHit::Dismiss;
        }
        let point = self.content_point(point);
        if self.invite.area() > 0.0 && self.invite.contains(point) {
            CollaboratorsHit::Invite
        } else if self.join.area() > 0.0 && self.join.contains(point) {
            CollaboratorsHit::Join
        } else if self.activity.area() > 0.0 && self.activity.contains(point) {
            CollaboratorsHit::RoomActivity
        } else if self.status.area() > 0.0 && self.status.contains(point) {
            CollaboratorsHit::EditStatus
        } else if let Some((mode, _)) = self
            .status_modes
            .iter()
            .find(|(_, rect)| rect.contains(point))
        {
            CollaboratorsHit::SetStatusSharing(*mode)
        } else if let Some(index) = self
            .row_actions
            .iter()
            .position(|rect| rect.area() > 0.0 && rect.contains(point))
        {
            CollaboratorsHit::ToggleFollow(index)
        } else if let Some(index) = self.row_rects.iter().enumerate().find_map(|(index, _)| {
            let rect = self.settings_rect(index);
            (rect.area() > 0.0 && rect.contains(point)).then_some(index)
        }) {
            CollaboratorsHit::OpenSettings(index)
        } else if self.previous_people.area() > 0.0 && self.previous_people.contains(point) {
            CollaboratorsHit::PreviousPeople
        } else if self.next_people.area() > 0.0 && self.next_people.contains(point) {
            CollaboratorsHit::NextPeople
        } else {
            CollaboratorsHit::Absorb
        }
    }

    /// The Follow/Stop button of the row whose top edge is `y`: one pointer
    /// target tall, centred in the row. Paint and hit-testing both read
    /// this, so they cannot drift apart.
    fn row_action_rect(&self, panel: Rect, y: f64) -> Rect {
        let height = self.follow_height.max(MIN_TARGET);
        let top = y + (self.row_height.max(64.0) - height) / 2.0;
        Rect::new(
            panel.x1 - 12.0 - self.follow_width.max(76.0),
            top,
            panel.x1 - 12.0,
            top + height,
        )
    }

    fn settings_rect(&self, index: usize) -> Rect {
        let Some(row) = self.row_rects.get(index).copied() else {
            return Rect::ZERO;
        };
        if row.area() <= 0.0 {
            return Rect::ZERO;
        }
        let x1 = self
            .row_actions
            .get(index)
            .filter(|action| action.area() > 0.0)
            .map_or(row.x1 - 8.0, |action| action.x0 - 4.0);
        Rect::new(row.x0 + 8.0, row.y0 + 2.0, x1, row.y1 - 2.0)
    }

    fn focus_order(&self) -> Vec<CollaboratorsHit> {
        let mut order = Vec::new();
        if self.status.area() > 0.0 {
            order.push(CollaboratorsHit::EditStatus);
        }
        order.extend(
            self.status_modes
                .iter()
                .map(|(mode, _)| CollaboratorsHit::SetStatusSharing(*mode)),
        );
        for index in 0..self.row_rects.len() {
            if self.settings_rect(index).area() > 0.0 {
                order.push(CollaboratorsHit::OpenSettings(index));
            }
            if self
                .row_actions
                .get(index)
                .is_some_and(|rect| rect.area() > 0.0)
            {
                order.push(CollaboratorsHit::ToggleFollow(index));
            }
        }
        if self.previous_people.area() > 0.0 {
            order.push(CollaboratorsHit::PreviousPeople);
        }
        if self.next_people.area() > 0.0 {
            order.push(CollaboratorsHit::NextPeople);
        }
        if self.invite.area() > 0.0 {
            order.extend([CollaboratorsHit::Invite, CollaboratorsHit::Join]);
        }
        if self.activity.area() > 0.0 {
            order.push(CollaboratorsHit::RoomActivity);
        }
        order
    }

    fn rect_for_hit(&self, hit: CollaboratorsHit) -> Option<Rect> {
        match hit {
            CollaboratorsHit::Invite => Some(self.invite),
            CollaboratorsHit::Join => Some(self.join),
            CollaboratorsHit::RoomActivity => Some(self.activity),
            CollaboratorsHit::EditStatus => Some(self.status),
            CollaboratorsHit::SetStatusSharing(mode) => self
                .status_modes
                .iter()
                .find_map(|(candidate, rect)| (*candidate == mode).then_some(*rect)),
            CollaboratorsHit::ToggleFollow(index) => self.row_actions.get(index).copied(),
            CollaboratorsHit::OpenSettings(index) => Some(self.settings_rect(index)),
            CollaboratorsHit::PreviousPeople => Some(self.previous_people),
            CollaboratorsHit::NextPeople => Some(self.next_people),
            CollaboratorsHit::Absorb | CollaboratorsHit::Dismiss => None,
        }
        .filter(|rect| rect.area() > 0.0)
    }

    fn focused_rect(&self) -> Option<Rect> {
        self.keyboard_focus
            .and_then(|focus| self.rect_for_hit(focus))
    }

    pub fn key(&mut self, key: &Key, backwards: bool) -> Option<CollaboratorsHit> {
        if matches!(key, Key::Named(NamedKey::Escape)) {
            self.keyboard_focus = None;
            return Some(CollaboratorsHit::Dismiss);
        }
        if matches!(key, Key::Named(NamedKey::Enter | NamedKey::Space)) {
            return self.keyboard_focus;
        }
        let reverse = matches!(key, Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp))
            || (backwards && matches!(key, Key::Named(NamedKey::Tab)));
        if !matches!(
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
        ) {
            return None;
        }
        let order = self.focus_order();
        if order.is_empty() {
            return None;
        }
        let current = self
            .keyboard_focus
            .and_then(|focus| order.iter().position(|candidate| *candidate == focus));
        let last = order.len().saturating_sub(1);
        let next = if matches!(key, Key::Named(NamedKey::Home)) {
            0
        } else if matches!(key, Key::Named(NamedKey::End)) {
            last
        } else if reverse {
            current
                .and_then(|index| index.checked_sub(1))
                .unwrap_or(last)
        } else {
            current
                .and_then(|index| index.checked_add(1))
                .filter(|index| *index < order.len())
                .unwrap_or(0)
        };
        self.keyboard_focus = order.get(next).copied();
        self.reveal_focus();
        self.register_targets(self.host);
        None
    }

    #[expect(
        clippy::too_many_lines,
        reason = "a declarative accessibility tree for every panel control"
    )]
    pub fn accessibility_snapshot(&self) -> ui::a11y::AccessNode {
        use crate::i18n::{t, t_args};
        use ui::a11y::{AccessNode, AccessState, Role};

        let total_people = self.accessibility_rows.len();
        let people = self
            .roster_range
            .clone()
            .filter_map(|index| {
                self.accessibility_rows.get(index).map(|row| {
                    let mut details = Vec::new();
                    if row.is_agent {
                        details.push(t("a11y-shell-people-agent"));
                    }
                    if row.invited {
                        details.push(t("a11y-shell-people-invited"));
                    } else {
                        details.push(t("a11y-shell-people-present"));
                    }
                    if let Some(activity) = &row.activity {
                        details.push(t_args(
                            "a11y-shell-people-now",
                            &[("activity", activity.into())],
                        ));
                    }
                    if let Some(authority) = &row.authority {
                        details.push(authority.clone());
                    } else if let Some(role) = &row.role {
                        details.push(role.clone());
                    }
                    if let Some(status) = &row.status {
                        details.push(if status.automatic {
                            t_args(
                                "a11y-shell-people-intent-inferred",
                                &[("status", (&status.text).into())],
                            )
                        } else {
                            t_args(
                                "a11y-shell-people-intent",
                                &[("status", (&status.text).into())],
                            )
                        });
                    }
                    AccessNode::new(Role::ListItem)
                        .with_name(row.label.clone())
                        .with_description(details.join("; "))
                        .with_position(index.saturating_add(1), total_people)
                })
            })
            .collect();
        let mut children = vec![
            AccessNode::new(Role::List)
                .with_name(t("a11y-shell-people-here-now"))
                .with_children(people),
        ];
        for hit in self.focus_order() {
            let focused = self.keyboard_focus == Some(hit);
            let (role, name, description, checked) = match hit {
                CollaboratorsHit::EditStatus => {
                    let mut description = if self.accessibility_status_automatic {
                        t("a11y-shell-status-inferred")
                    } else {
                        t("a11y-shell-status-set")
                    };
                    if let Some(activity) = &self.accessibility_own_activity {
                        description.push_str("; ");
                        description.push_str(&t_args(
                            "a11y-shell-people-now",
                            &[("activity", activity.into())],
                        ));
                    }
                    (
                        if self.accessibility_editing {
                            Role::TextInput
                        } else {
                            Role::Button
                        },
                        t("a11y-shell-status-name"),
                        description,
                        None,
                    )
                }
                CollaboratorsHit::SetStatusSharing(mode) => (
                    Role::RadioButton,
                    match mode {
                        ui::StatusSharing::Automatic => t("a11y-shell-status-automatic"),
                        ui::StatusSharing::ManualOnly => t("a11y-shell-status-manual"),
                        ui::StatusSharing::Off => t("a11y-shell-status-off"),
                    },
                    match mode {
                        ui::StatusSharing::Automatic => {
                            t("a11y-shell-status-automatic-description")
                        }
                        ui::StatusSharing::ManualOnly => t("a11y-shell-status-manual-description"),
                        ui::StatusSharing::Off => t("a11y-shell-status-off-description"),
                    },
                    Some(self.accessibility_sharing == mode),
                ),
                CollaboratorsHit::ToggleFollow(index) => {
                    let row = self.accessibility_rows.get(index);
                    (
                        Role::Button,
                        row.map_or_else(
                            || t("a11y-shell-follow-collaborator"),
                            |row| t_args("a11y-shell-follow", &[("name", (&row.label).into())]),
                        ),
                        t("a11y-shell-follow-description"),
                        None,
                    )
                }
                CollaboratorsHit::OpenSettings(index) => {
                    let row = self.accessibility_rows.get(index);
                    (
                        Role::Button,
                        row.map_or_else(
                            || t("a11y-shell-open-member-settings"),
                            |row| {
                                t_args(
                                    if row.is_agent {
                                        "a11y-shell-open-agent-settings"
                                    } else {
                                        "a11y-shell-open-member-settings-for"
                                    },
                                    &[("name", (&row.label).into())],
                                )
                            },
                        ),
                        row.map_or_else(
                            || t("a11y-shell-open-member-settings-description"),
                            |row| {
                                t(if row.is_agent {
                                    "a11y-shell-open-agent-settings-description"
                                } else {
                                    "a11y-shell-open-member-settings-description"
                                })
                            },
                        ),
                        None,
                    )
                }
                CollaboratorsHit::PreviousPeople => (
                    Role::Button,
                    t("a11y-shell-people-previous"),
                    t("a11y-shell-people-previous-description"),
                    None,
                ),
                CollaboratorsHit::NextPeople => (
                    Role::Button,
                    t("a11y-shell-people-next"),
                    t("a11y-shell-people-next-description"),
                    None,
                ),
                CollaboratorsHit::Invite => (
                    Role::Button,
                    t("a11y-shell-invite"),
                    t("a11y-shell-invite-description"),
                    None,
                ),
                CollaboratorsHit::Join => (
                    Role::Button,
                    t("a11y-shell-join"),
                    t("a11y-shell-join-description"),
                    None,
                ),
                CollaboratorsHit::RoomActivity => (
                    Role::Button,
                    t("a11y-shell-workspace-activity"),
                    t("a11y-shell-workspace-activity-description"),
                    None,
                ),
                CollaboratorsHit::Absorb | CollaboratorsHit::Dismiss => continue,
            };
            let mut state = AccessState::new().focusable(true).focused(focused);
            if let Some(checked) = checked {
                state = state.checked(checked);
            }
            let mut node = AccessNode::new(role)
                .with_name(name)
                .with_description(description)
                .with_state(state);
            if hit == CollaboratorsHit::EditStatus
                && let Some(value) = self.accessibility_status.as_ref()
            {
                node = node.with_value(value.clone());
            }
            children.push(node);
        }
        let mut description = t("a11y-shell-people-description");
        if let Some(exposure) = &self.accessibility_screen_exposure {
            description.push_str("; ");
            description.push_str(&t_args(
                "a11y-shell-people-screen-visible",
                &[("exposure", exposure.into())],
            ));
        }
        AccessNode::new(Role::Dialog)
            .with_name(t("a11y-shell-people"))
            .with_description(description)
            .with_children(children)
    }

    pub fn accessibility_rect_for_path(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [] | [0] => Some(self.panel_rect()),
            [0, row] => self
                .roster_range
                .start
                .checked_add(*row)
                .filter(|index| *index < self.roster_range.end)
                .and_then(|index| self.row_rects.get(index))
                .filter(|rect| rect.area() > 0.0)
                .map(|rect| self.view_rect(*rect)),
            [control] if *control > 0 => self
                .focus_order()
                .get(control.saturating_sub(1))
                .and_then(|hit| self.rect_for_hit(*hit))
                .map(|rect| self.view_rect(rect)),
            _ => None,
        }
    }

    pub fn focus_accessibility_path(&mut self, path: &[usize]) -> bool {
        let [control] = path else {
            return false;
        };
        if *control == 0 {
            return false;
        }
        let Some(hit) = self.focus_order().get(control.saturating_sub(1)).copied() else {
            return false;
        };
        self.keyboard_focus = Some(hit);
        self.reveal_focus();
        self.register_targets(self.host);
        true
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one layout pass derives every panel rect from the same running offsets"
    )]
    fn layout(
        &mut self,
        host: Rect,
        member_count: usize,
        unavailable: bool,
        may_invite: bool,
        has_exposure: bool,
    ) {
        self.host = host;
        let warning_h = if unavailable {
            self.warning_height.max(24.0)
        } else {
            0.0
        };
        let actions_h = if may_invite {
            (self.control_height + 8.0).max(ACTIONS_H)
        } else {
            0.0
        };
        let pager_h = (self.control_height + 8.0).max(ROSTER_PAGER_H);
        let exposure_h = if has_exposure {
            self.exposure_height.max(44.0)
        } else {
            0.0
        };
        let footer_h = (self.activity_height + 8.0).max(FOOTER_H);
        let fixed_height = self.header_height.max(48.0)
            + self.status_block_height.max(92.0)
            + exposure_h
            + warning_h
            + 16.0
            + actions_h
            + footer_h;
        let y0 = host.y0 + 48.0;
        let available = (host.y1 - y0 - 8.0).max(1.0);
        self.roster_page_size =
            self.page_size_for_height(member_count, available, fixed_height, pager_h);
        let focused_person = match self.keyboard_focus {
            Some(CollaboratorsHit::ToggleFollow(index) | CollaboratorsHit::OpenSettings(index)) => {
                Some(index)
            }
            _ => None,
        };
        if let Some(index) = focused_person.filter(|index| *index < member_count) {
            self.roster_offset = index
                .checked_div(self.roster_page_size)
                .unwrap_or(0)
                .saturating_mul(self.roster_page_size);
        } else if self.roster_offset >= member_count {
            self.roster_offset = 0;
        } else {
            self.roster_offset = self
                .roster_offset
                .checked_div(self.roster_page_size)
                .unwrap_or(0)
                .saturating_mul(self.roster_page_size);
        }
        let paged = member_count > self.roster_page_size;
        let end = self
            .roster_offset
            .saturating_add(self.roster_page_size)
            .min(member_count);
        self.roster_range = self.roster_offset..end;
        let rows = row_count_as_f64(self.roster_range.len().max(1));
        let height =
            fixed_height + rows * self.row_height.max(64.0) + if paged { pager_h } else { 0.0 };
        let x1 = host.x1 - 16.0 - self.scroll_gutter;
        self.panel = Rect::new(x1 - (320.0 - self.scroll_gutter), y0, x1, y0 + height);
        self.activity = Rect::new(
            self.panel.x0 + 8.0,
            self.panel.y1 - footer_h + 4.0,
            self.panel.x1 - 8.0,
            self.panel.y1 - 4.0,
        );
        self.row_actions = (0..member_count)
            .map(|index| {
                let Some(visible_index) = index.checked_sub(self.roster_offset) else {
                    return Rect::ZERO;
                };
                if visible_index >= self.roster_range.len() {
                    return Rect::ZERO;
                }
                let y = self.panel.y0
                    + self.header_height.max(48.0)
                    + self.status_block_height.max(92.0)
                    + exposure_h
                    + warning_h
                    + row_count_as_f64(visible_index) * self.row_height.max(64.0);
                self.row_action_rect(self.panel, y)
            })
            .collect();
        self.row_rects = (0..member_count)
            .map(|index| {
                let Some(visible_index) = index.checked_sub(self.roster_offset) else {
                    return Rect::ZERO;
                };
                if visible_index >= self.roster_range.len() {
                    return Rect::ZERO;
                }
                let y = self.panel.y0
                    + self.header_height.max(48.0)
                    + self.status_block_height.max(92.0)
                    + exposure_h
                    + warning_h
                    + row_count_as_f64(visible_index) * self.row_height.max(64.0);
                Rect::new(
                    self.panel.x0 + 8.0,
                    y,
                    self.panel.x1 - 8.0,
                    y + self.row_height.max(64.0),
                )
            })
            .collect();
        if paged {
            let pager_y = self.panel.y0
                + self.header_height.max(48.0)
                + self.status_block_height.max(92.0)
                + exposure_h
                + warning_h
                + rows * self.row_height.max(64.0);
            let center = self.panel.x0 + self.panel.width() / 2.0;
            self.previous_people = if self.roster_offset > 0 {
                Rect::new(
                    self.panel.x0 + 12.0,
                    pager_y + 4.0,
                    center - 38.0,
                    pager_y + 4.0 + self.control_height.max(MIN_TARGET),
                )
            } else {
                Rect::ZERO
            };
            self.next_people = if end < member_count {
                Rect::new(
                    center + 38.0,
                    pager_y + 4.0,
                    self.panel.x1 - 12.0,
                    pager_y + 4.0 + self.control_height.max(MIN_TARGET),
                )
            } else {
                Rect::ZERO
            };
        } else {
            self.previous_people = Rect::ZERO;
            self.next_people = Rect::ZERO;
        }
        if may_invite {
            // Shared control metrics preserve the minimum target as text grows.
            let control_h = self.control_height.max(MIN_TARGET);
            let y0 = self.panel.y1 - footer_h - actions_h + (actions_h - control_h) / 2.0;
            let gap = 8.0;
            let half = (self.panel.width() - 16.0 - gap) / 2.0;
            self.invite = Rect::new(
                self.panel.x0 + 8.0,
                y0,
                self.panel.x0 + 8.0 + half,
                y0 + self.control_height.max(MIN_TARGET),
            );
            self.join = Rect::new(
                self.invite.x1 + gap,
                y0,
                self.panel.x1 - 8.0,
                y0 + self.control_height.max(MIN_TARGET),
            );
        } else {
            self.invite = Rect::ZERO;
            self.join = Rect::ZERO;
        }
        self.status = Rect::new(
            self.panel.x0 + 44.0,
            self.panel.y0 + self.header_height.max(48.0) + 2.0,
            self.panel.x1 - 12.0,
            self.panel.y0 + self.header_height.max(48.0) + 2.0 + self.status_height.max(28.0),
        );
        let mode_top = self.status.y1 + 4.0;
        let mode_width = (self.panel.width() - 32.0) / 3.0;
        self.status_modes = [
            ui::StatusSharing::Automatic,
            ui::StatusSharing::ManualOnly,
            ui::StatusSharing::Off,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, mode)| {
            let index = row_count_as_f64(index);
            let rect = if self.stacked_modes {
                let top = mode_top + index * (self.mode_height + 4.0);
                Rect::new(
                    self.panel.x0 + 12.0,
                    top,
                    self.panel.x1 - 12.0,
                    top + self.mode_height,
                )
            } else {
                Rect::new(
                    self.panel.x0 + 12.0 + index * mode_width,
                    mode_top,
                    self.panel.x0 + 8.0 + (index + 1.0) * mode_width,
                    mode_top + self.mode_height.max(24.0),
                )
            };
            (mode, rect)
        })
        .collect();
    }

    fn page_size_for_height(
        &self,
        member_count: usize,
        available: f64,
        fixed_height: f64,
        pager_height: f64,
    ) -> usize {
        let largest = member_count.clamp(1, ROSTER_PAGE_SIZE);
        (1..=largest)
            .rev()
            .find(|page_size| {
                let paged = member_count > *page_size;
                let height = fixed_height
                    + row_count_as_f64(*page_size) * self.row_height.max(64.0)
                    + if paged { pager_height } else { 0.0 };
                height <= available + 0.5
            })
            .unwrap_or(1)
    }

    pub fn change_people_page(&mut self, forward: bool, member_count: usize) {
        self.targets.cancel_pointer_press();
        let page_size = self.roster_page_size.max(1);
        if forward {
            if self.roster_offset.saturating_add(page_size) < member_count {
                self.roster_offset = self.roster_offset.saturating_add(page_size);
            }
        } else {
            self.roster_offset = self.roster_offset.saturating_sub(page_size);
        }
        self.keyboard_focus = self.keyboard_focus.filter(|hit| {
            matches!(
                hit,
                CollaboratorsHit::PreviousPeople | CollaboratorsHit::NextPeople
            )
        });
    }

    fn draw_roster_pager(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        member_count: usize,
        cursor: Option<Point>,
    ) {
        if member_count <= self.roster_page_size.max(1) {
            return;
        }
        let label = format!(
            "{}–{} of {member_count}",
            self.roster_range.start.saturating_add(1),
            self.roster_range.end
        );
        let (width, _) = text_renderer.measure_style(&label, None, &theme.text(TextRole::Caption));
        let y = self.previous_people.y0.max(self.next_people.y0);
        text_renderer.draw_style(
            scene,
            &label,
            theme.tokens.colors.text_dim,
            transform.pre_translate(
                (
                    self.panel.x0 + self.panel.width() / 2.0 - f64::from(width) / 2.0,
                    y + 7.0,
                )
                    .into(),
            ),
            None,
            &theme.text(TextRole::Caption),
        );
        for (rect, icon, name, hit) in [
            (
                self.previous_people,
                Icon::ChevronLeft,
                crate::i18n::t("people-panel-previous-page"),
                CollaboratorsHit::PreviousPeople,
            ),
            (
                self.next_people,
                Icon::ChevronRight,
                crate::i18n::t("people-panel-next-page"),
                CollaboratorsHit::NextPeople,
            ),
        ] {
            if rect.area() <= 0.0 {
                continue;
            }
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(rect, "")
                    .icon(icon)
                    .accessible_name(&name)
                    .pressed(self.interaction(hit).pressed)
                    .hovered(cursor.is_some_and(|point| rect.contains(point))),
            );
        }
    }

    fn partition_sharing(&mut self, text: &mut TextRenderer, theme: &Theme) {
        if self.stacked_modes {
            return;
        }
        let labels = Self::sharing_labels();
        let labels = labels.iter().map(String::as_str).collect::<Vec<_>>();
        let top = self.status.y1 + 4.0;
        let band = Rect::new(
            self.panel.x0 + 12.0,
            top,
            self.panel.x1 - 12.0,
            top + self.mode_height,
        );
        let rects = SegmentedFace::partition(band, &labels, ControlSize::Regular, text, theme);
        for ((_, target), rect) in self.status_modes.iter_mut().zip(rects) {
            *target = rect;
        }
    }

    fn sharing_labels() -> [String; 3] {
        [
            "people-panel-sharing-auto",
            "people-panel-sharing-manual",
            "people-panel-sharing-hidden",
        ]
        .map(crate::i18n::t)
    }

    fn disclosure_width(theme: &Theme) -> f32 {
        280.0 - theme.tokens.metrics.scrollbar_gutter
    }

    fn measure_controls(
        &mut self,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &CollaboratorsModel<'_>,
    ) {
        let (_, heading_height) = text.measure_style(
            &crate::i18n::t("a11y-shell-people"),
            None,
            &theme.text(TextRole::Heading),
        );
        self.header_height = (f64::from(heading_height) + 24.0).max(48.0);
        let caption = theme.text(TextRole::Caption);
        let (_, line) = text.measure_style(
            &crate::i18n::t("people-panel-screen-visible"),
            None,
            &caption,
        );
        self.exposure_line_height = f64::from(line);
        self.exposure_height = model.screen_exposure.map_or(44.0, |detail| {
            let (_, height) =
                text.measure_style(detail, Some(Self::disclosure_width(theme)), &caption);
            f64::from(line + height) + 16.0
        });
        self.warning_height = model.unavailable.map_or(24.0, |reason| {
            let (_, height) =
                text.measure_style(reason, Some(Self::disclosure_width(theme)), &caption);
            f64::from(height) + 8.0
        });
        self.control_height = ButtonFace::height(ControlSize::Regular, theme);
        self.activity_height =
            ui::widget::ListRowFace::new(&crate::i18n::t("a11y-shell-workspace-activity"))
                .disclosure()
                .height(text, theme);
        self.status_height = FieldFace::height(ControlSize::Regular, theme);
        self.mode_height = SegmentedFace::height(ControlSize::Regular, theme);
        let labels = Self::sharing_labels();
        let labels = labels.iter().map(String::as_str).collect::<Vec<_>>();
        self.stacked_modes =
            SegmentedFace::intrinsic_width(&labels, ControlSize::Regular, text, theme)
                > 288.0 - f64::from(theme.tokens.metrics.scrollbar_gutter);
        let sharing_height = if self.stacked_modes {
            self.mode_height * 3.0 + 8.0
        } else {
            self.mode_height
        };
        self.status_block_height = 2.0
            + self.status_height
            + 4.0
            + sharing_height
            + f64::from(theme.text(TextRole::Caption).size)
            + 20.0;
        self.follow_height = ButtonFace::height(ControlSize::Large, theme);
        self.row_height = (self.follow_height + 16.0).max(64.0);
        self.follow_width = ["people-panel-follow", "people-panel-stop-following"]
            .into_iter()
            .map(|key| {
                ButtonFace::new(Rect::ZERO, &crate::i18n::t(key))
                    .control_size(ControlSize::Large)
                    .intrinsic_width(text, theme)
            })
            .fold(76.0, f64::max);
    }

    fn paint_status_field(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &CollaboratorsModel<'_>,
        cursor: Option<Point>,
    ) {
        let shown = model.own_status.unwrap_or_default();
        let placeholder = crate::i18n::t("people-panel-status-empty");
        let face = FieldFace::new(self.status, shown)
            .placeholder(&placeholder)
            .text_role(TextRole::Caption)
            .focused(model.status_editing)
            .hovered(cursor.is_some_and(|point| self.status.contains(point)));
        if !model.status_editing {
            self.status_input.0 = None;
            face.paint(scene, transform, text, theme);
            return;
        }
        face.paint_chrome(scene, transform, theme);
        let input = self.status_input.0.get_or_insert_with(|| {
            TextInput::new(Vec::new())
                .with_plain_chrome()
                .with_spellcheck(false)
                .with_autocorrect(false)
                .with_text_role(TextRole::Caption)
        });
        input.set_text(shown);
        let selected = input
            .selection_anchor_focus_public()
            .unwrap_or_else(|| (input.cursor_pos(), input.cursor_pos()));
        if selected != (model.status_selection.anchor, model.status_selection.cursor) {
            input.set_selection(model.status_selection.anchor, model.status_selection.cursor);
        }
        input.set_focus(true);
        input.apply_layout_external(face.text_rect(theme));
        let input_theme = theme
            .clone()
            .with_text_scale(theme.text_scale * theme.ui_scale);
        input.draw(
            scene,
            transform,
            text,
            &input_theme,
            &ui::presence::PresenceCtx::default(),
        );
    }

    fn paint_sharing(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        sharing: ui::StatusSharing,
        cursor: Option<Point>,
    ) {
        let labels = Self::sharing_labels();
        let rects = self
            .status_modes
            .iter()
            .map(|(_, rect)| *rect)
            .collect::<Vec<_>>();
        let selected = self
            .status_modes
            .iter()
            .position(|(mode, _)| *mode == sharing);
        if self.stacked_modes {
            for ((mode, rect), label) in self.status_modes.iter().zip(&labels) {
                Button::paint(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    &ButtonFace::new(*rect, label)
                        .toggled(*mode == sharing)
                        .hovered(cursor.is_some_and(|point| rect.contains(point)))
                        .pressed(
                            self.interaction(CollaboratorsHit::SetStatusSharing(*mode))
                                .pressed,
                        )
                        .focused(
                            self.keyboard_focus == Some(CollaboratorsHit::SetStatusSharing(*mode)),
                        ),
                );
            }
        } else {
            let labels = labels.iter().map(String::as_str).collect::<Vec<_>>();
            let face = SegmentedFace::new(&rects, &labels, selected.unwrap_or(0)).hovered(
                cursor.and_then(|point| rects.iter().position(|rect| rect.contains(point))),
            );
            ui::widget::ToggleGroup::paint(scene, transform, text_renderer, theme, &face);
        }
    }

    fn draw_own_status(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        model: &CollaboratorsModel<'_>,
        cursor: Option<Point>,
    ) {
        let colors = &theme.tokens.colors;
        let avatar = Rect::new(
            self.panel.x0 + 12.0,
            self.status.y0 + 1.0,
            self.panel.x0 + 38.0,
            self.status.y0 + 27.0,
        );
        ui::widget::AvatarFace::new(&model.own_initial.to_string(), "")
            .presence(model.own_color)
            .paint(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                avatar,
            );
        let spark = vello::kurbo::Circle::new(Point::new(avatar.x1 - 2.0, avatar.y1 - 2.0), 3.5);
        scene.stroke(
            &vello::kurbo::Stroke::new(2.0),
            transform,
            ui::chrome::shell::panel_ground(theme),
            None,
            &spark,
        );
        scene.fill(Fill::NonZero, transform, model.own_color, None, &spark);
        if model.own_status_automatic && !model.status_editing {
            let inset = f64::from(theme.tokens.spacing.s1) / 2.0;
            let side = f64::from(theme.text(TextRole::Caption).size).min(avatar.width() / 2.0);
            let badge = Rect::new(
                avatar.x0 - inset,
                avatar.y0 - inset,
                avatar.x0 - inset + side,
                avatar.y0 - inset + side,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                ui::chrome::shell::panel_ground(theme),
                None,
                &RoundedRect::from_rect(
                    badge.inflate(inset, inset),
                    f64::from(theme.tokens.radii.detail),
                ),
            );
            Icon::Sparkles.draw(scene, transform, badge, colors.intent.inverted);
        }

        self.paint_status_field(scene, transform, text_renderer, theme, model, cursor);
        self.paint_sharing(
            scene,
            transform,
            text_renderer,
            theme,
            model.status_sharing,
            cursor,
        );
        if let Some(activity) = model.own_activity {
            let dot = vello::kurbo::Circle::new(
                Point::new(
                    self.status.x0 + 8.0,
                    self.panel.y0
                        + self.header_height.max(48.0)
                        + self.status_block_height.max(92.0)
                        - 22.0,
                ),
                3.0,
            );
            scene.fill(Fill::NonZero, transform, model.own_color, None, &dot);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a non-negative on-screen width in logical pixels; f32 holds it with sub-pixel precision"
            )]
            let activity = text_renderer.elide_to_width_style(
                activity,
                (self.status.width() - 20.0).max(0.0) as f32,
                &theme.text(TextRole::Caption),
            );
            text_renderer.draw_style(
                scene,
                &activity,
                colors.text_dim,
                transform.pre_translate(
                    (
                        self.status.x0 + 16.0,
                        self.panel.y0
                            + self.header_height.max(48.0)
                            + self.status_block_height.max(92.0)
                            - 28.0,
                    )
                        .into(),
                ),
                None,
                &theme.text(TextRole::Caption),
            );
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
                frame.y + self.row_height.max(64.0) - 3.0,
                f64::from(tokens.radii.control),
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
                    frame.y + self.row_height.max(64.0) - 11.0,
                    1.5,
                ),
            );
        }

        self.draw_row_identity(scene, transform, text_renderer, theme, frame);
        self.draw_row_text(scene, transform, text_renderer, theme, frame);
        let settings = self.settings_rect(frame.index);
        if settings.area() > 0.0 {
            let size = 12.0;
            let color = if self
                .interaction(CollaboratorsHit::OpenSettings(frame.index))
                .hovered
            {
                theme.tokens.colors.text_primary
            } else {
                theme.tokens.colors.text_dim
            };
            Icon::Settings2.draw(
                scene,
                transform,
                Rect::new(
                    settings.x1 - size,
                    settings.center().y - size / 2.0,
                    settings.x1,
                    settings.center().y + size / 2.0,
                ),
                color,
            );
        }
    }

    fn draw_row_identity(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        frame: &RowFrame<'_>,
    ) {
        self.draw_row_avatar(scene, transform, text_renderer, theme, frame);
        self.draw_row_relationship(scene, transform, text_renderer, theme, frame);
    }

    fn draw_row_avatar(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        frame: &RowFrame<'_>,
    ) {
        let row = frame.row;
        let colors = &theme.tokens.colors;
        let center = Point::new(self.panel.x0 + 28.0, frame.y + 29.0);
        let avatar_rect = Rect::new(
            center.x - 11.0,
            center.y - 11.0,
            center.x + 11.0,
            center.y + 11.0,
        );
        if row.is_agent {
            draw_agent_identity_mark(
                scene,
                transform,
                avatar_rect,
                row.color,
                theme,
                if row.invited {
                    AgentIdentityStyle::Outline {
                        alpha: INVITED_ROW_ALPHA,
                    }
                } else {
                    AgentIdentityStyle::Solid
                },
            );
        } else if row.invited {
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
            ui::widget::AvatarFace::new(&row.initial.to_string(), "")
                .presence(row.color)
                .paint(
                    &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                    avatar_rect,
                );
        }
    }

    fn draw_row_relationship(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        frame: &RowFrame<'_>,
    ) {
        let row = frame.row;
        let center = Point::new(self.panel.x0 + 28.0, frame.y + 29.0);
        if !row.invited {
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
                if partner.is_agent {
                    draw_agent_identity_mark(
                        scene,
                        transform,
                        partner_rect,
                        partner.color,
                        theme,
                        if partner.invited {
                            AgentIdentityStyle::Outline {
                                alpha: INVITED_ROW_ALPHA,
                            }
                        } else {
                            AgentIdentityStyle::Solid
                        },
                    );
                } else {
                    ui::widget::AvatarFace::new(&partner.initial.to_string(), "")
                        .presence(partner.color)
                        .paint(
                            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                            partner_rect,
                        );
                }
                draw_direction_arrow(scene, transform, from, to, row.color.multiply_alpha(0.9));
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "a paint routine that lays its elements out top to bottom from shared running offsets"
    )]
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
        let settings = self.settings_rect(frame.index);
        // Text ends before the painted profile glyph. Because `settings.x1`
        // follows the measured Follow control, localized labels and text
        // scaling cannot make the two controls overlap.
        let row_text_x1 = if settings.area() > 0.0 {
            settings.x1 - 16.0
        } else if row.followable {
            self.panel.x1 - self.follow_width.max(76.0) - 20.0
        } else {
            self.panel.x1 - 12.0
        };
        let agent_tag = crate::i18n::t("people-panel-agent-tag");
        let agent_tag_x = if row.is_agent && !row.label.eq_ignore_ascii_case(&agent_tag) {
            let (width, _) = text_renderer.measure_style(
                &agent_tag,
                None,
                &tokens.typography.style(TextRole::Caption),
            );
            Some(row_text_x1 - f64::from(width))
        } else {
            None
        };
        let label_x1 = agent_tag_x.map_or(row_text_x1, |x| x - 6.0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the collaborator popover is only a few hundred logical pixels wide"
        )]
        let shown_label = text_renderer.elide_to_width_style(
            &row.label,
            (label_x1 - text_x).max(0.0) as f32,
            &tokens.typography.style(TextRole::Caption),
        );
        let has_activity = row.invited
            || followed
            || row.activity.is_some()
            || row.role.is_some()
            || row.authority.is_some();
        let label_y = if has_activity || row.status.is_some() {
            row_y + 8.0
        } else {
            row_y + 22.0
        };
        text_renderer.draw_style(
            scene,
            &shown_label,
            if row.invited {
                colors.text_dim
            } else {
                colors.text_primary
            },
            transform.pre_translate((text_x, label_y).into()),
            None,
            &tokens.typography.style(TextRole::Caption),
        );
        if let Some(agent_tag_x) = agent_tag_x {
            text_renderer.draw_style(
                scene,
                &agent_tag,
                colors.text_dim,
                transform.pre_translate((agent_tag_x, row_y + 9.0).into()),
                None,
                &tokens.typography.style(TextRole::Caption),
            );
        }
        let subtitle = if row.invited {
            Some(crate::i18n::t("people-panel-invited-tag"))
        } else if followed {
            row.activity.as_ref().map_or_else(
                || Some(crate::i18n::t("people-panel-following")),
                |activity| {
                    Some(crate::i18n::t_args(
                        "people-panel-following-activity",
                        &[("activity", activity.as_str().into())],
                    ))
                },
            )
        } else if let Some(activity) = &row.activity {
            Some(activity.clone())
        } else {
            row.authority.clone().or_else(|| row.role.clone())
        };
        if let Some(subtitle) = subtitle {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "the collaborator popover is only a few hundred logical pixels wide"
            )]
            let subtitle = text_renderer.elide_to_width_style(
                &subtitle,
                (row_text_x1 - text_x).max(0.0) as f32,
                &tokens.typography.style(TextRole::Caption),
            );
            text_renderer.draw_style(
                scene,
                &subtitle,
                colors.text_dim,
                transform.pre_translate((text_x, row_y + 27.0).into()),
                None,
                &tokens.typography.style(TextRole::Caption),
            );
        }
        if let Some(status) = &row.status {
            let icon = if status.automatic {
                Icon::Sparkles
            } else {
                Icon::Pencil
            };
            let icon_rect = Rect::new(
                text_x,
                row_y + if has_activity { 45.0 } else { 29.0 },
                text_x + 12.0,
                row_y + if has_activity { 57.0 } else { 41.0 },
            );
            icon.draw(scene, transform, icon_rect, colors.intent.inverted);
            let status_text_x = text_x + 17.0;
            #[allow(
                clippy::cast_possible_truncation,
                reason = "the collaborator popover is only a few hundred logical pixels wide"
            )]
            let shown = text_renderer.elide_to_width_style(
                &status.text,
                (row_text_x1 - status_text_x).max(0.0) as f32,
                &tokens.typography.style(TextRole::Caption),
            );
            text_renderer.draw_style(
                scene,
                &shown,
                colors.intent.inverted,
                transform.pre_translate(
                    (
                        status_text_x,
                        row_y + if has_activity { 45.0 } else { 29.0 },
                    )
                        .into(),
                ),
                None,
                &tokens.typography.style(TextRole::Caption),
            );
        }
        if row.followable {
            let action = self.row_action_rect(self.panel, row_y);
            // Control role (HIG §9.1): a pressable body takes `radii.control`;
            // `radii.detail` is for details drawn inside a control.
            // The pass paints with the renderer reset to 1× (see `draw`), so
            // the face is handed the theme-scaled label size explicitly.
            let label = crate::i18n::t(if followed {
                "people-panel-stop-following"
            } else {
                "people-panel-follow"
            });
            let face = ButtonFace::new(action, &label)
                .control_size(ui::widget::ControlSize::Large)
                .pressed(
                    self.targets
                        .interaction(&format!("follow:{}", row.id))
                        .pressed,
                )
                .focused(matches!(
                    self.keyboard_focus,
                    Some(CollaboratorsHit::ToggleFollow(index))
                        if self.follow_target(index) == Some(row.id.as_str())
                ))
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
        if self.invite.area() > 0.0 {
            let invite_label = crate::i18n::t("people-panel-invite");
            let join_label = crate::i18n::t("people-panel-join");
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(self.invite, &invite_label)
                    .role(ButtonRole::Primary)
                    .pressed(self.interaction(CollaboratorsHit::Invite).pressed)
                    .hovered(cursor.is_some_and(|point| self.invite.contains(point))),
            );
            Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &ButtonFace::new(self.join, &join_label)
                    .pressed(self.interaction(CollaboratorsHit::Join).pressed)
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
        let title = crate::i18n::t("a11y-shell-workspace-activity");
        let mut interaction = self.interaction(CollaboratorsHit::RoomActivity);
        interaction.hovered = cursor.is_some_and(|point| self.activity.contains(point));
        interaction.focused = self.keyboard_focus == Some(CollaboratorsHit::RoomActivity);
        ui::widget::ListRowFace::new(&title)
            .disclosure()
            .interaction(interaction)
            .paint(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                self.activity,
            );
    }
}

/// Rows the dropdown and the presence publisher agree are in the room, with
/// `local_user` the principal this device speaks as.
pub fn collaborator_roster_rows(desktop: &DesktopTree, local_user: &str) -> Vec<RosterRow> {
    // Presence publication updates the active workspace and the legacy
    // tree-wide mirror together, but readers can observe one side of that
    // handoff before the other (and old control clients still update only
    // the mirror). Merge by stable participant id, preferring the room row's
    // richer activity fields, so nobody temporarily disappears from People.
    let mut roster = desktop
        .active_workspace()
        .map(|workspace| workspace.collaborators.clone())
        .unwrap_or_default();
    for participant in &desktop.collaborators {
        if !roster.iter().any(|row| row.id == participant.id) {
            roster.push(participant.clone());
        }
    }
    let colors = roster_colors_with_local_user(&roster, local_user);
    let agent_members = desktop.active_workspace().map_or_else(
        Vec::new,
        crate::authority::WorkspaceAuthority::agent_members,
    );
    let rows: Vec<RosterRow> = roster
        .iter()
        .map(|participant| {
            let agent_membership = agent_members.iter().find(|(agent, _)| {
                agent.eq_ignore_ascii_case(&participant.id)
                    || agent.eq_ignore_ascii_case(&participant.name)
            });
            let is_agent = agent_membership.is_some();
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
                role: if is_agent {
                    None
                } else {
                    participant.role_id.as_deref().map(display_role_id)
                },
                authority: agent_membership.and_then(|(agent, _)| {
                    desktop.active_workspace().map(|workspace| {
                        let preset =
                            crate::workspace_settings::agent_authority_preset(workspace, agent);
                        crate::i18n::t_args(
                            "people-panel-agent-capability-preset",
                            &[("preset", preset.label().into())],
                        )
                    })
                }),
                invited: participant.invited,
                activity: participant
                    .active_tab
                    .and_then(|tab| active_tab_title(desktop, tab))
                    .map(|title| {
                        crate::i18n::t_args("people-panel-viewing", &[("title", title.into())])
                    }),
                status: None,
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
    // Workspace Settings' Agents destination. Once its live session arrives
    // in `roster`, the identity match above tags it as an agent and exposes
    // the same cursor/follow affordance
    // through truthful semantic activity rather than a fabricated cursor.
    rows
}

/// A recorded role id as people read it; an id this model does not define
/// shows as recorded.
fn display_role_id(id: &str) -> String {
    tilo_desktop_model::role_from_id(id)
        .map_or_else(|| id.to_string(), crate::workspace_settings::role_label)
}

/// What colour each person on `roster` is drawn in, with `local` the
/// principal this device speaks as. One function so cursor and avatar cannot
/// disagree and read as two participants: both go through
/// [`ui::presence::assign_peer_colors`], which settles the whole room at
/// once. An explicit [`Collaborator::cursor_color`] outranks anything
/// computed and is held back from the assignment; the local user is
/// included in the settled set (never the avatar stack) so every host
/// computes the same one.
pub fn roster_colors_with_local_user(
    roster: &[Collaborator],
    local: &str,
) -> HashMap<String, (u8, u8, u8)> {
    let mut explicit: HashMap<String, (u8, u8, u8)> = HashMap::new();
    for c in roster {
        if let Some(rgb) = c
            .cursor_color
            .as_deref()
            .and_then(ui::presence::parse_hex_color)
        {
            explicit.insert(c.id.clone(), rgb);
        }
    }
    let mut ids: Vec<&str> = roster
        .iter()
        .filter(|c| !explicit.contains_key(&c.id))
        .map(|c| c.id.as_str())
        .collect();
    if !explicit.contains_key(local) {
        ids.push(local);
    }
    let reserved: Vec<(u8, u8, u8)> = explicit.values().copied().collect();
    let mut assigned = ui::presence::assign_peer_colors(&ids, &reserved);
    assigned.extend(explicit);
    assigned
}

fn roster_initial(name: &str) -> char {
    name.chars()
        .next()
        .map_or('\u{2022}', |character| character.to_ascii_uppercase())
}

fn active_tab_title(desktop: &DesktopTree, tab_id: u64) -> Option<String> {
    desktop
        .active_workspace()?
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
    use crate::authority::WorkspaceAuthority as _;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn collaborators_footer_and_buttons_keep_token_geometry_on_every_palette() {
        /// HIG §9.5: the footer separator (derived against the Panel ground) and
        /// the activity row's edge (derived against its lift floor) must both
        /// register on every shipped palette.
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
        fn footer_keeps_a_token_gap_around_the_activity_row() {
            let s1 = f64::from(ui::tokens::Spacing::BASE.s1);
            let mut overlay = CollaboratorsOverlay::default();
            overlay.layout(Rect::new(0.0, 0.0, 1280.0, 800.0), 2, false, true, false);
            let panel = overlay.panel_rect();
            let activity = overlay.activity_rect();
            let (invite, _join) = overlay.action_rects();
            let separator_y = activity.y0 - 4.0;
            assert!(
                separator_y - invite.y1 >= s1,
                "separator clears the action band"
            );
            assert!(activity.y0 - separator_y >= s1, "row clears the separator");
            assert!(panel.y1 - activity.y1 >= s1, "row clears the panel edge");
            assert!(activity.x0 - panel.x0 >= s1 && panel.x1 - activity.x1 >= s1);
        }

        fn every_button_is_at_least_one_pointer_target_and_centred_in_its_band() {
            let mut overlay = CollaboratorsOverlay::default();
            overlay.layout(Rect::new(0.0, 0.0, 1200.0, 800.0), 3, false, true, false);
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
                let row = overlay.row_rects[index];
                assert!(
                    (rect.center().y - row.center().y).abs() < f64::EPSILON,
                    "row {index}'s button is centred in its row"
                );
                assert!(rect.y0 >= row.y0 && rect.y1 <= row.y1);
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

        fn crowded_rosters_page_without_putting_rows_under_the_footer() {
            let mut overlay = CollaboratorsOverlay::default();
            let host = Rect::new(0.0, 0.0, 1200.0, 800.0);
            overlay.layout(host, 8, false, true, false);

            assert_eq!(
                overlay
                    .row_action_rects()
                    .iter()
                    .filter(|rect| rect.area() > 0.0)
                    .count(),
                ROSTER_PAGE_SIZE
            );
            assert!(overlay.next_people.area() > 0.0);
            assert_eq!(
                overlay.hit_test(overlay.next_people.center()),
                CollaboratorsHit::NextPeople
            );
            assert!(
                overlay
                    .row_rects
                    .iter()
                    .filter(|rect| rect.area() > 0.0)
                    .all(|rect| rect.y1 < overlay.activity.y0)
            );

            overlay.change_people_page(true, 8);
            overlay.layout(host, 8, false, true, false);
            assert_eq!(overlay.roster_range, 3..6);
            assert!(overlay.previous_people.area() > 0.0);
            assert_eq!(
                overlay.hit_test(overlay.row_action_rects()[3].center()),
                CollaboratorsHit::ToggleFollow(3)
            );

            overlay.change_people_page(true, 8);
            overlay.layout(host, 8, false, true, false);
            assert_eq!(overlay.roster_range, 6..8);
            assert_eq!(
                overlay
                    .row_action_rects()
                    .iter()
                    .filter(|rect| rect.area() > 0.0)
                    .count(),
                2
            );
            assert_eq!(overlay.next_people, Rect::ZERO);
        }

        footer_separator_and_row_edge_register_on_every_palette();
        footer_keeps_a_token_gap_around_the_activity_row();
        every_button_is_at_least_one_pointer_target_and_centred_in_its_band();
        crowded_rosters_page_without_putting_rows_under_the_footer();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn collaborators_accessibility_and_roster_separate_identity_action_and_intent() {
        fn agent_classification_uses_stable_identity_not_display_label() {
            use collab_types::caps::{CapOverride, Subject, Verb};
            use tilo_desktop_model::{Workspace, WorkspaceId};

            let mut workspace = Workspace::new(WorkspaceId(71), "Shared");
            workspace.owner = Some("owner".into());
            workspace
                .grant_as(
                    &"owner".to_string(),
                    CapOverride::membership(
                        Subject::agent("scribe"),
                        workspace.id.raw().to_string(),
                        Verb::Write,
                    ),
                )
                .expect("owner admits agent");
            workspace
                .collaborators
                .push(Collaborator::new("scribe", "Release Notes Agent"));
            let desktop = DesktopTree {
                active: workspace.id,
                workspaces: vec![workspace],
                ..DesktopTree::default()
            };

            let rows = collaborator_roster_rows(&desktop, "local");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].label, "Release Notes Agent");
            assert!(
                rows[0].is_agent,
                "a renamed agent must never appear as a person"
            );
            assert_eq!(rows[0].role, None);
            assert_eq!(
                rows[0].authority.as_deref(),
                Some("Capability preset: Operator")
            );

            let mut overlay = CollaboratorsOverlay::default();
            overlay.layout(Rect::new(0.0, 0.0, 1200.0, 800.0), 1, false, false, false);
            overlay.accessibility_rows = rows;
            let snapshot = overlay.accessibility_snapshot();
            assert!(snapshot.children.iter().any(|node| {
                node.role == ui::a11y::Role::Button
                    && node.name.as_deref() == Some("Review access for Release Notes Agent")
                    && node.description.as_deref().is_some_and(|description| {
                        description.contains("explicit scoped capabilities")
                    })
            }));
        }

        fn accessibility_distinguishes_live_action_from_inferred_intent() {
            let mut overlay = CollaboratorsOverlay::default();
            overlay.layout(Rect::new(0.0, 0.0, 1200.0, 800.0), 1, false, false, false);
            overlay.accessibility_rows = vec![RosterRow {
                id: "ada".into(),
                label: "Ada".into(),
                initial: 'A',
                color: Color::from_rgb8(1, 2, 3),
                is_agent: false,
                role: None,
                authority: None,
                invited: false,
                activity: Some("Editing · Q2 finance report".into()),
                status: Some(RosterStatus {
                    text: "Preparing the board review".into(),
                    automatic: true,
                }),
                followable: true,
            }];
            let snapshot = overlay.accessibility_snapshot();
            let person = snapshot
                .children
                .first()
                .and_then(|list| list.children.first())
                .expect("the live roster is exposed as a list");
            let description = person.description.as_deref().unwrap_or_default();
            assert!(description.contains("Now: Editing · Q2 finance report"));
            assert!(description.contains("Intent, inferred: Preparing the board review"));
        }

        fn accessibility_keeps_own_live_action_separate_from_own_intent() {
            let mut overlay = CollaboratorsOverlay::default();
            overlay.layout(Rect::new(0.0, 0.0, 1200.0, 800.0), 0, false, false, false);
            overlay.accessibility_status = Some("Closing the Q3 forecast".into());
            overlay.accessibility_own_activity = Some("Reconciling · Q3 forecast".into());
            let snapshot = overlay.accessibility_snapshot();
            let status = snapshot
                .children
                .iter()
                .find(|node| node.name.as_deref() == Some("Your collaboration status"))
                .expect("the self status control is exposed");
            assert!(
                status.description.as_deref().is_some_and(
                    |description| description.contains("Now: Reconciling · Q3 forecast")
                )
            );
        }

        fn screen_exposure_is_persistent_layout_and_accessibility_content() {
            let host = Rect::new(0.0, 0.0, 1200.0, 800.0);
            let mut ordinary = CollaboratorsOverlay::default();
            ordinary.layout(host, 0, false, false, false);
            let ordinary_height = ordinary.panel.height();

            let mut exposed = CollaboratorsOverlay::default();
            exposed.layout(host, 0, false, false, true);
            exposed.accessibility_screen_exposure =
                Some("Ada (person) views Draft; Scribe (agent) controls Terminal".into());

            assert_eq!(exposed.panel.height(), ordinary_height + 44.0);
            let description = exposed
                .accessibility_snapshot()
                .description
                .expect("People dialog has a privacy description");
            assert!(description.contains("Screen visible now"));
            assert!(description.contains("Ada (person) views Draft"));
            assert!(description.contains("Scribe (agent) controls Terminal"));
        }

        fn roster_model_keeps_follow_direction_when_cursors_are_not_visible() {
            let rows = vec![
                RosterRow {
                    id: "ada".into(),
                    label: "Ada".into(),
                    initial: 'A',
                    color: Color::from_rgb8(1, 2, 3),
                    is_agent: false,
                    role: None,
                    authority: None,
                    invited: false,
                    activity: None,
                    status: None,
                    followable: true,
                },
                RosterRow {
                    id: "bob".into(),
                    label: "Bob".into(),
                    initial: 'B',
                    color: Color::from_rgb8(4, 5, 6),
                    is_agent: false,
                    role: None,
                    authority: None,
                    invited: false,
                    activity: None,
                    status: None,
                    followable: true,
                },
            ];
            let pairs = vec![("ada".to_string(), "bob".to_string())];
            let model = CollaboratorsModel {
                host: Rect::ZERO,
                rows: &rows,
                own_initial: 'Q',
                own_color: Color::from_rgb8(7, 8, 9),
                unavailable: None,
                may_invite: false,
                followed_id: None,
                following_pairs: &pairs,
                own_status: None,
                own_activity: None,
                own_status_automatic: false,
                status_sharing: ui::StatusSharing::Automatic,
                status_editing: false,
                status_selection: ui::text_selection::Selection::default(),
                screen_exposure: None,
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

        agent_classification_uses_stable_identity_not_display_label();
        accessibility_distinguishes_live_action_from_inferred_intent();
        accessibility_keeps_own_live_action_separate_from_own_intent();
        screen_exposure_is_persistent_layout_and_accessibility_content();
        roster_model_keeps_follow_direction_when_cursors_are_not_visible();
    }

    #[test]
    fn collaborators_hit_targets_follow_painted_geometry_and_policy() {
        fn hit_testing_uses_the_same_cached_geometry_as_paint() {
            let mut overlay = CollaboratorsOverlay::default();
            overlay.layout(Rect::new(0.0, 0.0, 1200.0, 800.0), 2, false, true, false);
            let (invite, join) = overlay.action_rects();
            assert_eq!(overlay.hit_test(invite.center()), CollaboratorsHit::Invite);
            assert_eq!(overlay.hit_test(join.center()), CollaboratorsHit::Join);
            assert_eq!(
                overlay.hit_test(overlay.row_action_rects()[0].center()),
                CollaboratorsHit::ToggleFollow(0),
            );
            assert_eq!(
                overlay.hit_test(overlay.settings_rect(0).center()),
                CollaboratorsHit::OpenSettings(0),
            );
            assert_eq!(
                overlay.hit_test(overlay.activity_rect().center()),
                CollaboratorsHit::RoomActivity
            );
            assert_eq!(
                overlay.hit_test(Point::new(
                    overlay.panel_rect().x0 + 2.0,
                    overlay.panel_rect().y0 + 2.0,
                )),
                CollaboratorsHit::Absorb
            );
            assert_eq!(overlay.hit_test(Point::ZERO), CollaboratorsHit::Dismiss);
        }

        fn policy_removes_invite_geometry_instead_of_only_hiding_it() {
            let mut overlay = CollaboratorsOverlay::default();
            let host = Rect::new(0.0, 0.0, 1200.0, 800.0);
            overlay.layout(host, 1, false, true, false);
            assert!(overlay.action_rects().0.area() > 0.0);
            overlay.layout(host, 1, false, false, false);
            assert_eq!(overlay.action_rects(), (Rect::ZERO, Rect::ZERO));
        }

        fn clearing_drops_every_invisible_hit_target() {
            let mut overlay = CollaboratorsOverlay::default();
            overlay.layout(Rect::new(0.0, 0.0, 1200.0, 800.0), 1, false, true, false);
            overlay.clear();
            assert_eq!(overlay.panel_rect(), Rect::ZERO);
            assert_eq!(overlay.action_rects(), (Rect::ZERO, Rect::ZERO));
            assert_eq!(overlay.activity_rect(), Rect::ZERO);
            assert!(overlay.row_action_rects().is_empty());
        }

        fn own_status_and_privacy_modes_are_first_class_hit_targets() {
            let mut overlay = CollaboratorsOverlay::default();
            overlay.layout(Rect::new(0.0, 0.0, 1200.0, 800.0), 2, false, true, false);
            assert_eq!(
                overlay.hit_test(overlay.status.center()),
                CollaboratorsHit::EditStatus
            );
            for (mode, rect) in &overlay.status_modes {
                assert_eq!(
                    overlay.hit_test(rect.center()),
                    CollaboratorsHit::SetStatusSharing(*mode)
                );
            }
        }

        fn keyboard_reaches_status_privacy_follow_and_activity_without_pointer_input() {
            let mut overlay = CollaboratorsOverlay::default();
            overlay.layout(Rect::new(0.0, 0.0, 1200.0, 800.0), 1, false, true, false);

            assert_eq!(overlay.key(&Key::Named(NamedKey::Home), false), None);
            assert_eq!(
                overlay.key(&Key::Named(NamedKey::Enter), false),
                Some(CollaboratorsHit::EditStatus)
            );
            assert_eq!(overlay.key(&Key::Named(NamedKey::End), false), None);
            assert_eq!(
                overlay.key(&Key::Named(NamedKey::Space), false),
                Some(CollaboratorsHit::RoomActivity)
            );
            assert_eq!(
                overlay.key(&Key::Named(NamedKey::Escape), false),
                Some(CollaboratorsHit::Dismiss)
            );
        }

        hit_testing_uses_the_same_cached_geometry_as_paint();
        policy_removes_invite_geometry_instead_of_only_hiding_it();
        clearing_drops_every_invisible_hit_target();
        own_status_and_privacy_modes_are_first_class_hit_targets();
        keyboard_reaches_status_privacy_follow_and_activity_without_pointer_input();
    }

    fn roster_focus_rows() -> Vec<RosterRow> {
        ["Ada", "Bob"]
            .into_iter()
            .map(|name| RosterRow {
                id: name.to_ascii_lowercase(),
                label: name.into(),
                initial: name.chars().next().expect("fixture name"),
                color: Color::from_rgb8(114, 139, 180),
                is_agent: false,
                role: None,
                authority: None,
                invited: false,
                activity: None,
                status: None,
                followable: true,
            })
            .collect()
    }

    fn paint_roster_focus(overlay: &mut CollaboratorsOverlay, rows: &[RosterRow]) {
        paint_roster_focus_in(overlay, rows, Rect::new(0.0, 0.0, 800.0, 600.0));
    }

    fn paint_roster_focus_in(overlay: &mut CollaboratorsOverlay, rows: &[RosterRow], host: Rect) {
        overlay.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &CollaboratorsModel {
                host,
                rows,
                own_initial: 'Q',
                own_color: Color::from_rgb8(170, 120, 130),
                unavailable: None,
                may_invite: false,
                followed_id: None,
                following_pairs: &[],
                own_status: None,
                own_activity: None,
                own_status_automatic: false,
                status_sharing: ui::StatusSharing::Automatic,
                status_editing: false,
                status_selection: ui::text_selection::Selection::default(),
                screen_exposure: None,
            },
            None,
        );
    }

    fn collaborator_control_index(overlay: &CollaboratorsOverlay, name: &str) -> usize {
        overlay
            .accessibility_snapshot()
            .children
            .iter()
            .position(|node| node.name.as_deref() == Some(name))
            .expect("named collaborator control is published")
    }

    fn focus_collaborator_control(overlay: &mut CollaboratorsOverlay, name: &str) {
        let index = collaborator_control_index(overlay, name);
        assert!(overlay.focus_accessibility_path(&[index]));
    }

    #[test]
    fn roster_follow_focus_tracks_a_person_across_painted_reordering() {
        let mut overlay = CollaboratorsOverlay::default();
        let mut people = roster_focus_rows();
        paint_roster_focus(&mut overlay, &people);
        focus_collaborator_control(&mut overlay, "Follow Ada");
        people.swap(0, 1);
        paint_roster_focus(&mut overlay, &people);
        assert_eq!(
            overlay.key(&Key::Named(NamedKey::Enter), false),
            Some(CollaboratorsHit::ToggleFollow(1)),
            "Ada survives at the new index"
        );
        let snapshot = overlay.accessibility_snapshot();
        let focused = snapshot
            .children
            .iter()
            .find(|node| node.state.focused)
            .expect("surviving Ada control retains focus");
        assert_eq!(focused.name.as_deref(), Some("Follow Ada"));
    }

    #[test]
    fn roster_follow_focus_does_not_jump_to_a_replacement_person() {
        let mut overlay = CollaboratorsOverlay::default();
        let mut people = roster_focus_rows();
        paint_roster_focus(&mut overlay, &people);
        focus_collaborator_control(&mut overlay, "Follow Ada");
        people.remove(0);
        paint_roster_focus(&mut overlay, &people);
        assert_eq!(overlay.key(&Key::Named(NamedKey::Enter), false), None);
        assert_eq!(overlay.key(&Key::Named(NamedKey::Tab), false), None);
        assert_eq!(
            overlay.key(&Key::Named(NamedKey::Enter), false),
            Some(CollaboratorsHit::EditStatus),
            "navigation explicitly selects a new control"
        );
        assert_eq!(
            overlay.key(&Key::Named(NamedKey::Escape), false),
            Some(CollaboratorsHit::Dismiss)
        );
    }

    #[test]
    fn roster_follow_focus_clears_when_the_person_is_no_longer_followable() {
        let mut overlay = CollaboratorsOverlay::default();
        let mut people = roster_focus_rows();
        paint_roster_focus(&mut overlay, &people);
        focus_collaborator_control(&mut overlay, "Follow Ada");
        people.first_mut().expect("Ada").followable = false;
        paint_roster_focus(&mut overlay, &people);
        assert_eq!(overlay.key(&Key::Named(NamedKey::Enter), false), None);
        assert!(
            !overlay
                .accessibility_snapshot()
                .children
                .iter()
                .any(|node| node.state.focused)
        );
    }

    #[test]
    fn roster_follow_focus_survives_resize_and_clears_on_page_change() {
        let mut overlay = CollaboratorsOverlay::default();
        let mut people = roster_focus_rows();
        paint_roster_focus(&mut overlay, &people);
        focus_collaborator_control(&mut overlay, "Follow Ada");
        let viewport = Rect::new(0.0, 0.0, 640.0, 480.0);
        paint_roster_focus_in(&mut overlay, &people, viewport);
        assert_eq!(
            overlay.key(&Key::Named(NamedKey::Enter), false),
            Some(CollaboratorsHit::ToggleFollow(0))
        );
        let ada = collaborator_control_index(&overlay, "Follow Ada");
        let focus = overlay
            .accessibility_rect_for_path(&[ada])
            .expect("Ada after resize");
        assert!(
            viewport.contains(focus.origin()) && viewport.contains(Point::new(focus.x1, focus.y1))
        );
        for name in ["Carol", "Dana"] {
            let mut row = people.first().expect("fixture Ada").clone();
            row.id = name.to_ascii_lowercase();
            row.label = name.into();
            people.push(row);
        }
        overlay.change_people_page(true, people.len());
        paint_roster_focus_in(&mut overlay, &people, viewport);
        assert_eq!(overlay.key(&Key::Named(NamedKey::Enter), false), None);
        overlay.change_people_page(false, people.len());
        paint_roster_focus_in(&mut overlay, &people, viewport);
        assert_eq!(overlay.key(&Key::Named(NamedKey::Enter), false), None);
        focus_collaborator_control(&mut overlay, "Follow Ada");
        assert_eq!(
            overlay.key(&Key::Named(NamedKey::Enter), false),
            Some(CollaboratorsHit::ToggleFollow(0))
        );
    }
}
