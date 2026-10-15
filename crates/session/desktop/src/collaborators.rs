//! Collaborator roster popover.
//!
//! This component owns the popover's layout, painting and hit targets. The
//! host supplies a policy-filtered model and applies the semantic action
//! returned by [`CollaboratorsOverlay::hit_test`].

use std::collections::HashMap;
use ui::num::f64_of_usize;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

use ui::agent_identity::{AgentIdentityStyle, draw_agent_identity_mark};
use ui::icons::Icon;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, ControlSize, FieldFace, TextInput, Widget};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use tilo_desktop_model::{Collaborator, DesktopTree};
use ui::TextRole;
use ui::widget::ButtonRole;

const FOOTER_H: f64 = 40.0;
const ACTIONS_H: f64 = 40.0;
const INVITED_ROW_ALPHA: f32 = 0.55;
/// Rows the People popover shows before its list scrolls.
const VISIBLE_ROWS: usize = 8;
/// An expanded row shows at most this many lines of its recent summary, so
/// one talkative participant cannot push everyone else off the page.
const RECENT_MAX_LINES: usize = 3;
/// The row's text column (avatar gutter) and the inset of a status-style
/// line's text past its 12 px icon.
const ROW_TEXT_INSET: f64 = 44.0;
const ROW_ICON_TEXT_INSET: f64 = 17.0;

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

/// What a pointer press on the open popover means to the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollaboratorsHit {
    Invite,
    Join,
    RoomActivity,
    /// Change the name others see. Only painted when the host offers it.
    Rename,
    EditStatus,
    /// Show or hide the choice of what to share. The host calls
    /// [`CollaboratorsOverlay::toggle_sharing_choices`].
    ChangeSharing,
    SetStatusSharing(ui::StatusSharing),
    /// Toggle follow for the row at this index.
    ToggleFollow(usize),
    /// Show or hide what the person at this row index did recently. Raised
    /// by the row body (never its Follow button), only for rows that have a
    /// recent summary; the host toggles [`CollaboratorsModel::expanded_id`].
    ToggleDetails(usize),
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
    /// Short participant intention, marked by its [`LineSource`] so
    /// model-suggested words cannot be mistaken for a direct quote.
    pub status: Option<RosterStatus>,
    /// What the participant did in the last few minutes, as a short summary.
    /// Shown only while the row is expanded, marked like `status`. A row
    /// without one is not expandable.
    pub recent: Option<RosterStatus>,
    pub followable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterStatus {
    pub text: String,
    pub source: LineSource,
}

/// Who wrote a line shown about someone. It decides the line's marker: only
/// software-written words carry the generated sparkle, and nothing the
/// person did not type carries the pencil.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineSource {
    /// The person typed it.
    Written,
    /// A model (or another generator) wrote it from what they did.
    Generated,
    /// Plainly read off what they did ("Editing Welcome in Notes").
    Observed,
}

impl LineSource {
    #[must_use]
    pub fn generated_if(generated: bool) -> Self {
        if generated {
            Self::Generated
        } else {
            Self::Observed
        }
    }
}

/// Immutable data and policy decisions needed to paint one frame.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent host policy and state flags for one frame, not a state machine"
)]
pub struct CollaboratorsModel<'a> {
    pub host: Rect,
    pub rows: &'a [RosterRow],
    /// The name others see for this person (a guest's given name, such as
    /// "Brave Fox"); its first letter is the avatar's.
    pub own_label: &'a str,
    pub own_color: Color,
    /// Whether the host can rename this person: the name then carries an
    /// edit affordance that raises [`CollaboratorsHit::Rename`].
    pub own_rename: bool,
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
    /// Whether `own_activity` is model prose rather than read off actions.
    pub own_activity_generated: bool,
    pub own_status_automatic: bool,
    pub status_sharing: ui::StatusSharing,
    pub status_editing: bool,
    pub status_selection: ui::text_selection::Selection,
    /// Compositor-authoritative disclosure of exactly who can currently see
    /// or control which shared surface. Presence alone never implies this.
    pub screen_exposure: Option<&'a str>,
    /// The roster id whose recent summary is shown. Ignored when that row
    /// has no `recent` summary.
    pub expanded_id: Option<&'a str>,
    /// People following along without taking part (spectators). They have
    /// no row; the header counts them so the room's audience is never
    /// hidden.
    pub watching: usize,
    /// Whether the host can edit the own status. When false the status is
    /// shown read-only and raises no [`CollaboratorsHit::EditStatus`]; the
    /// sharing modes stay.
    pub status_editable: bool,
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

/// Floor for a roster row: three caption lines plus a margin.
const ROW_MIN_H: f64 = 44.0;

/// Narrowest the quiet follow control reserves at a row's trailing edge.
const FOLLOW_MIN_W: f64 = 56.0;

struct RowFrame<'a> {
    row: &'a RosterRow,
    index: usize,
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent layout and accessibility facts retained between paint and input"
)]
pub struct CollaboratorsOverlay {
    panel: Rect,
    /// The panel's width, `metrics.panel_width_list` (HIG 9.6) narrowed to
    /// the host less its margins, resolved when the controls are measured so
    /// text wraps at the width it is painted at.
    panel_width: f64,
    /// The floating Panel margin it keeps from the host's edges.
    panel_margin: f64,
    /// `metrics.min_pointer_target` (HIG 9.6): every button in this popover
    /// (Follow/Stop, Invite, Join) is at least this tall.
    min_target: f64,
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
    /// The own block, top to bottom: the name line, the status field (only
    /// where the host edits it), the live line, the sharing line and, while
    /// open, the sharing choices.
    name_height: f64,
    own_name_width: f64,
    own_label: String,
    own_avatar: Rect,
    rename_offered: bool,
    /// The name and its edit mark: the Rename target, zero when the host
    /// cannot rename.
    own_name: Rect,
    shows_own_activity: bool,
    own_line_height: f64,
    own_activity_top: f64,
    sharing_line_height: f64,
    sharing_text_width: f64,
    sharing_top: f64,
    /// The "Change" link that opens the sharing choices.
    sharing_change: Rect,
    sharing_open: bool,
    /// The sharing modes this host offers: writing your own needs a host
    /// that edits the status.
    offered_modes: Vec<ui::StatusSharing>,
    follow_width: f64,
    follow_height: f64,
    activity_height: f64,
    control_height: f64,
    row_height: f64,
    /// Index (into the model rows) of the expanded row, the extra height its
    /// recent summary adds, and the summary's laid-out lines.
    expanded_row: Option<usize>,
    expanded_extra: f64,
    expanded_lines: Vec<String>,
    /// Whether "+N watching" moved under "Here now" for lack of room.
    watching_wraps: bool,
    /// The host cannot edit the own status here: it paints, but is no target.
    status_read_only: bool,
    invite: Rect,
    join: Rect,
    activity: Rect,
    row_actions: Vec<Rect>,
    status: Rect,
    status_modes: Vec<(ui::StatusSharing, Rect)>,
    row_rects: Vec<Rect>,
    /// Row bodies that toggle the recent summary; zero for rows without one.
    row_details: Vec<Rect>,
    /// The popover's height when it shows at most [`VISIBLE_ROWS`] rows.
    visible_height: f64,
    keyboard_focus: Option<CollaboratorsHit>,
    targets: ui::widget::PaintedTargets,
    painted_actions: Vec<(String, CollaboratorsHit)>,
    held_submit: [bool; 2],
    accessibility_rows: Vec<RosterRow>,
    accessibility_status: Option<String>,
    accessibility_own_label: String,
    accessibility_own_activity: Option<String>,
    accessibility_status_automatic: bool,
    accessibility_sharing: ui::StatusSharing,
    accessibility_editing: bool,
    accessibility_screen_exposure: Option<String>,
    accessibility_expanded: Option<String>,
    accessibility_watching: usize,
}

impl CollaboratorsOverlay {
    /// The minimum pointer target: the theme's, once a frame measured it,
    /// the base token before that.
    fn min_target(&self) -> f64 {
        if self.min_target > 0.0 {
            self.min_target
        } else {
            f64::from(ui::tokens::Metrics::BASE.min_pointer_target)
        }
    }

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
            Some(CollaboratorsHit::ToggleDetails(index)) => self
                .accessibility_rows
                .get(index)
                .and_then(|selected| {
                    model
                        .rows
                        .iter()
                        .position(|row| row.id == selected.id && row.recent.is_some())
                })
                .map(CollaboratorsHit::ToggleDetails),
            other => other,
        };
        let previous_focus_rect = self.focused_rect();
        let previous_panel = self.panel;
        let previous_host = self.host;
        // A choice that took effect closes the choices: the sharing line
        // now says what was chosen.
        if self.sharing_open && model.status_sharing != self.accessibility_sharing {
            self.sharing_open = false;
        }
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
        self.retain_frame(model);
        self.keyboard_focus = focus.filter(|hit| self.rect_for_hit(*hit).is_some());
        if previous_focus_rect != self.focused_rect() {
            self.reveal_focus();
        }
        self.register_targets(model.host);
        self.paint_frame(scene, transform, text_renderer, theme, model, cursor);
    }

    /// Keep what input and accessibility need between paints, and drop row
    /// targets the policy or the footer rule out.
    fn retain_frame(&mut self, model: &CollaboratorsModel<'_>) {
        self.accessibility_rows.clear();
        self.accessibility_rows.extend_from_slice(model.rows);
        self.accessibility_status = model.own_status.map(str::to_string);
        model
            .own_label
            .clone_into(&mut self.accessibility_own_label);
        self.accessibility_own_activity = model.own_activity.map(str::to_string);
        self.accessibility_status_automatic = model.own_status_automatic;
        self.accessibility_sharing = model.status_sharing;
        self.accessibility_editing = model.status_editing;
        self.accessibility_screen_exposure = model.screen_exposure.map(str::to_string);
        self.accessibility_expanded = self
            .expanded_row
            .and_then(|index| model.rows.get(index))
            .map(|row| row.id.clone());
        self.accessibility_watching = model.watching;
        self.status_read_only = !model.status_editable;
        for (target, row) in self.row_actions.iter_mut().zip(model.rows) {
            if !row.followable || target.y1 > self.activity.y0 - 4.0 {
                *target = Rect::ZERO;
            }
        }
        let footer = self.activity.y0 - 4.0;
        self.row_details = self
            .row_rects
            .iter()
            .zip(model.rows)
            .map(|(rect, row)| {
                if row.recent.is_some() && !row.invited && rect.area() > 0.0 && rect.y1 <= footer {
                    *rect
                } else {
                    Rect::ZERO
                }
            })
            .collect();
    }

    fn paint_frame(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        model: &CollaboratorsModel<'_>,
        cursor: Option<Point>,
    ) {
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
        if self.scroll_offset() > 0.0 {
            // The list scrolls under the heading; the count stays in view.
            let band = Rect::new(
                self.panel.x0 + 1.0,
                self.panel.y0 + 1.0,
                self.panel.x1 - 1.0,
                self.panel.y0 + self.header_height.max(48.0),
            );
            let radius =
                (ui::chrome::ChromeRole::Panel.radius(theme, rect.height()) - 1.0).max(0.0);
            scene.fill(
                Fill::NonZero,
                transform,
                ui::chrome::shell::panel_ground(theme),
                None,
                &RoundedRect::from_rect(
                    band,
                    vello::kurbo::RoundedRectRadii::new(radius, radius, 0.0, 0.0),
                ),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                ui::chrome::shell::panel_border(theme),
                None,
                &Rect::new(band.x0, band.y1 - 1.0, band.x1, band.y1),
            );
            self.draw_header(scene, transform, text_renderer, theme, model);
        }
    }

    /// The heading with the "here now" count and who is watching. Drawn
    /// with the content, and again pinned over it once the list scrolls.
    fn draw_header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        model: &CollaboratorsModel<'_>,
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
        let dots_x = here_x - 8.0 - f64_of_usize(dot_colors.len()) * 7.0;
        for (index, color) in dot_colors.into_iter().enumerate() {
            let x = dots_x + f64_of_usize(index) * 7.0;
            let dot = vello::kurbo::Circle::new(Point::new(x + 3.0, rect.y0 + 25.0), 3.0);
            scene.fill(Fill::NonZero, transform, color, None, &dot);
        }
        if model.watching > 0 {
            // Spectators have no row; the header names them beside the live
            // count, or under it when the heading leaves no room.
            let watching = Self::watching_label(model.watching);
            let caption = tokens.typography.style(TextRole::Caption);
            let (width, height) = text_renderer.measure_style(&watching, None, &caption);
            let at = if self.watching_wraps {
                (
                    rect.x1 - 14.0 - f64::from(width),
                    rect.y0 + 22.0 + f64::from(height),
                )
            } else {
                (dots_x - 8.0 - f64::from(width), rect.y0 + 20.0)
            };
            text_renderer.draw_style(
                scene,
                &watching,
                colors.text_dim,
                transform.pre_translate(at.into()),
                None,
                &caption,
            );
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
        self.draw_header(scene, transform, text_renderer, theme, model);
        let rect = self.panel;
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        self.draw_own_status(scene, transform, text_renderer, theme, model, cursor);
        if let Some(exposure) = model.screen_exposure {
            let banner = Rect::new(
                rect.x0 + 12.0,
                rect.y0 + self.header_height.max(48.0) + self.status_block_height,
                rect.x1 - 12.0,
                rect.y0
                    + self.header_height.max(48.0)
                    + self.status_block_height
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
                Some(self.disclosure_width(theme)),
                &tokens.typography.style(TextRole::Caption),
            );
        }

        let roster_top = rect.y0
            + self.header_height.max(48.0)
            + self.status_block_height
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
                Some(self.disclosure_width(theme)),
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

        for (index, (row, row_rect)) in model.rows.iter().zip(&self.row_rects).enumerate() {
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
            let y = row_rect.y0;
            self.draw_row(
                scene,
                transform,
                text_renderer,
                theme,
                &RowFrame {
                    row,
                    index,
                    y,
                    followed: model.followed_id == Some(row.id.as_str()),
                    relationship,
                    relationship_partner,
                    cursor,
                },
            );
        }
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
        let available = (model.host.y1 - self.panel.y0 - self.panel_margin)
            .min(self.visible_height)
            .max(1.0);
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
            CollaboratorsHit::ToggleDetails(index) => {
                self.details_target(index).map(|id| format!("details:{id}"))
            }
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
        let moved = ui::synthetic_input::cursor_moved(
            point.x * ctx.scale_factor,
            point.y * ctx.scale_factor,
        );
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
                let moved = ui::synthetic_input::cursor_moved(
                    point.x * ctx.scale_factor,
                    point.y * ctx.scale_factor,
                );
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

    fn status_editable(&self) -> bool {
        !self.status_read_only && self.status.area() > 0.0
    }

    /// The stable person behind a painted, expandable row body. The host
    /// toggles its expanded id with this, never with the row index.
    pub fn details_target(&self, index: usize) -> Option<&str> {
        self.row_details
            .get(index)
            .filter(|rect| rect.area() > 0.0)
            .and_then(|_| self.accessibility_rows.get(index))
            .filter(|row| row.recent.is_some())
            .map(|row| row.id.as_str())
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
        } else if self.own_name.area() > 0.0 && self.own_name.contains(point) {
            CollaboratorsHit::Rename
        } else if self.status_editable() && self.status.contains(point) {
            CollaboratorsHit::EditStatus
        } else if self.sharing_change.area() > 0.0 && self.sharing_change.contains(point) {
            CollaboratorsHit::ChangeSharing
        } else if let Some((mode, _)) = self
            .status_modes
            .iter()
            .find(|(_, rect)| rect.area() > 0.0 && rect.contains(point))
        {
            CollaboratorsHit::SetStatusSharing(*mode)
        } else if let Some(index) = self
            .row_actions
            .iter()
            .position(|rect| rect.area() > 0.0 && rect.contains(point))
        {
            CollaboratorsHit::ToggleFollow(index)
        } else if let Some(index) = self
            .row_details
            .iter()
            .position(|rect| rect.area() > 0.0 && rect.contains(point))
        {
            CollaboratorsHit::ToggleDetails(index)
        } else {
            CollaboratorsHit::Absorb
        }
    }

    /// The Follow/Stop button of the row whose top edge is `y`: one pointer
    /// target tall, centred in the row. Paint and hit-testing both read
    /// this, so they cannot drift apart.
    /// One roster row's height: the follow control with a token margin, never
    /// below what three caption lines need.
    fn row_h(&self) -> f64 {
        self.row_height.max(ROW_MIN_H)
    }

    fn row_action_rect(&self, panel: Rect, y: f64) -> Rect {
        let height = self.follow_height.max(self.min_target());
        let top = y + (self.row_h() - height) / 2.0;
        Rect::new(
            panel.x1 - 12.0 - self.follow_width.max(FOLLOW_MIN_W),
            top,
            panel.x1 - 12.0,
            top + height,
        )
    }

    fn focus_order(&self) -> Vec<CollaboratorsHit> {
        let mut order = Vec::new();
        if self.own_name.area() > 0.0 {
            order.push(CollaboratorsHit::Rename);
        }
        if self.status_editable() {
            order.push(CollaboratorsHit::EditStatus);
        }
        if self.sharing_change.area() > 0.0 {
            order.push(CollaboratorsHit::ChangeSharing);
        }
        order.extend(
            self.status_modes
                .iter()
                .filter(|(_, rect)| rect.area() > 0.0)
                .map(|(mode, _)| CollaboratorsHit::SetStatusSharing(*mode)),
        );
        // Each row's body comes before its Follow button, in reading order;
        // registering it first also leaves Follow on top for the pointer.
        for index in 0..self.row_actions.len().max(self.row_details.len()) {
            if self
                .row_details
                .get(index)
                .is_some_and(|rect| rect.area() > 0.0)
            {
                order.push(CollaboratorsHit::ToggleDetails(index));
            }
            if self
                .row_actions
                .get(index)
                .is_some_and(|rect| rect.area() > 0.0)
            {
                order.push(CollaboratorsHit::ToggleFollow(index));
            }
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
            CollaboratorsHit::Rename => Some(self.own_name),
            CollaboratorsHit::EditStatus => self.status_editable().then_some(self.status),
            CollaboratorsHit::ChangeSharing => Some(self.sharing_change),
            CollaboratorsHit::SetStatusSharing(mode) => self
                .status_modes
                .iter()
                .find_map(|(candidate, rect)| (*candidate == mode).then_some(*rect)),
            CollaboratorsHit::ToggleFollow(index) => self.row_actions.get(index).copied(),
            CollaboratorsHit::ToggleDetails(index) => self.row_details.get(index).copied(),
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

        let people = self
            .accessibility_rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
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
                if let Some(status) = &row.status {
                    details.push(match status.source {
                        LineSource::Generated => t_args(
                            "a11y-shell-people-intent-inferred",
                            &[("status", (&status.text).into())],
                        ),
                        LineSource::Written => t_args(
                            "a11y-shell-people-intent",
                            &[("status", (&status.text).into())],
                        ),
                        LineSource::Observed => t_args(
                            "a11y-shell-people-now",
                            &[("activity", (&status.text).into())],
                        ),
                    });
                }
                if let Some(recent) = row
                    .recent
                    .as_ref()
                    .filter(|_| self.accessibility_expanded.as_deref() == Some(row.id.as_str()))
                {
                    details.push(t_args(
                        if recent.source == LineSource::Generated {
                            "a11y-shell-people-recent-inferred"
                        } else {
                            "a11y-shell-people-recent"
                        },
                        &[("summary", (&recent.text).into())],
                    ));
                }
                AccessNode::new(Role::ListItem)
                    .with_name(row.label.clone())
                    .with_description(details.join("; "))
                    .with_position(index.saturating_add(1), self.accessibility_rows.len())
            })
            .collect();
        let mut children = vec![
            AccessNode::new(Role::List)
                .with_name(t("a11y-shell-people-here-now"))
                .with_children(people),
        ];
        for hit in self.focus_order() {
            let focused = self.keyboard_focus == Some(hit);
            let mut expanded = None;
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
                CollaboratorsHit::Rename => (
                    Role::Button,
                    t_args(
                        "a11y-shell-rename",
                        &[("name", (&self.accessibility_own_label).into())],
                    ),
                    t("a11y-shell-rename-description"),
                    None,
                ),
                CollaboratorsHit::ChangeSharing => {
                    expanded = Some(self.sharing_open);
                    (
                        Role::Button,
                        t("a11y-shell-sharing-change"),
                        Self::sharing_line(self.accessibility_sharing),
                        None,
                    )
                }
                CollaboratorsHit::SetStatusSharing(mode) => (
                    Role::RadioButton,
                    Self::sharing_choice(mode),
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
                CollaboratorsHit::ToggleDetails(index) => {
                    let row = self.accessibility_rows.get(index);
                    let open = row.is_some_and(|row| {
                        self.accessibility_expanded.as_deref() == Some(row.id.as_str())
                    });
                    expanded = Some(open);
                    (
                        Role::Button,
                        t_args(
                            "a11y-shell-people-recent-toggle",
                            &[(
                                "name",
                                row.map_or_else(String::new, |row| row.label.clone()).into(),
                            )],
                        ),
                        t("a11y-shell-people-recent-toggle-description"),
                        None,
                    )
                }
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
            if let Some(expanded) = expanded {
                state = state.expanded(expanded);
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
        if self.accessibility_watching > 0 {
            description.push_str("; ");
            description.push_str(&t_args(
                "a11y-shell-people-watching",
                &[("count", self.accessibility_watching.into())],
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
            [0, row] => self.row_rects.get(*row).map(|rect| self.view_rect(*rect)),
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

    fn layout(
        &mut self,
        host: Rect,
        member_count: usize,
        unavailable: bool,
        may_invite: bool,
        has_exposure: bool,
    ) {
        self.host = host;
        let uniform = self.row_h();
        let resting = move |_: usize| uniform;
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
        let exposure_h = if has_exposure {
            self.exposure_height.max(44.0)
        } else {
            0.0
        };
        let footer_h = (self.activity_height + 8.0).max(FOOTER_H);
        // Only the expanded row adds its summary's height; rows below it
        // move down by exactly that much.
        let expanded = self.expanded_row.filter(|index| *index < member_count);
        let extra = if expanded.is_some() {
            self.expanded_extra
        } else {
            0.0
        };
        let list_top =
            self.header_height.max(48.0) + self.status_block_height + exposure_h + warning_h;
        let mut offsets = Vec::with_capacity(member_count);
        let mut y = list_top;
        for index in 0..member_count {
            offsets.push(y);
            y += resting(index) + if expanded == Some(index) { extra } else { 0.0 };
        }
        let rows_h = if member_count == 0 {
            resting(0)
        } else {
            y - list_top
        };
        let tail_h = 16.0 + actions_h + footer_h;
        let height = list_top + rows_h + tail_h;
        // Everyone is listed; past VISIBLE_ROWS the popover stops growing and
        // its list scrolls under the pinned header.
        let visible_rows_h = offsets
            .get(VISIBLE_ROWS)
            .map_or(rows_h, |top| top - list_top);
        self.visible_height = list_top + visible_rows_h + tail_h;
        // Under the bar at the Panel margin, its trailing edge (scroll gutter
        // included) the same margin in from the host's: never off a narrow
        // host. The height scrolls inside the host (`configure_scroll`).
        let (width, margin) = if self.panel_width > 0.0 {
            (self.panel_width, self.panel_margin)
        } else {
            (
                f64::from(ui::tokens::Metrics::BASE.panel_width_list)
                    .min(host.width() - 2.0 * f64::from(ui::tokens::Spacing::BASE.s2))
                    .max(1.0),
                f64::from(ui::tokens::Spacing::BASE.s2),
            )
        };
        let x1 = host.x1 - margin - self.scroll_gutter;
        let y0 = host.y0 + f64::from(crate::topbar::TOPBAR_HEIGHT_PX) + margin;
        self.panel = Rect::new(
            (x1 - (width - self.scroll_gutter)).max(host.x0 + margin),
            y0,
            x1,
            y0 + height,
        );
        self.activity = Rect::new(
            self.panel.x0 + 8.0,
            self.panel.y1 - footer_h + 4.0,
            self.panel.x1 - 8.0,
            self.panel.y1 - 4.0,
        );
        self.row_actions = offsets
            .iter()
            .map(|top| self.row_action_rect(self.panel, self.panel.y0 + top))
            .collect();
        self.row_rects = offsets
            .iter()
            .enumerate()
            .map(|(index, top)| {
                let y = self.panel.y0 + top;
                Rect::new(
                    self.panel.x0 + 8.0,
                    y,
                    self.panel.x1 - 8.0,
                    y + resting(index) + if expanded == Some(index) { extra } else { 0.0 },
                )
            })
            .collect();
        if may_invite {
            // Shared control metrics preserve the minimum target as text grows.
            let control_h = self.control_height.max(self.min_target());
            let y0 = self.panel.y1 - footer_h - actions_h + (actions_h - control_h) / 2.0;
            let gap = 8.0;
            let half = (self.panel.width() - 16.0 - gap) / 2.0;
            self.invite = Rect::new(
                self.panel.x0 + 8.0,
                y0,
                self.panel.x0 + 8.0 + half,
                y0 + self.control_height.max(self.min_target()),
            );
            self.join = Rect::new(
                self.invite.x1 + gap,
                y0,
                self.panel.x1 - 8.0,
                y0 + self.control_height.max(self.min_target()),
            );
        } else {
            self.invite = Rect::ZERO;
            self.join = Rect::ZERO;
        }
        self.layout_own_block();
    }

    /// Place the own block's parts from its measured heights, top down.
    fn layout_own_block(&mut self) {
        let gap = 4.0;
        let text_x = self.panel.x0 + ROW_TEXT_INSET;
        let mut y = self.panel.y0 + self.header_height.max(48.0) + gap;
        self.own_avatar = Rect::from_center_size(
            (self.panel.x0 + 25.0, y + self.name_height / 2.0),
            (26.0, 26.0),
        );
        // The name and its pencil, as wide as they paint: the whole name
        // is the target, never the empty band beside it.
        self.own_name = if self.rename_offered {
            Rect::new(
                text_x - 4.0,
                y,
                (text_x + self.own_name_width + 6.0 + 14.0 + 4.0).min(self.panel.x1 - 12.0),
                y + self.name_height,
            )
        } else {
            Rect::ZERO
        };
        y += self.name_height + gap;
        if self.status_height > 0.0 {
            self.status = Rect::new(text_x, y, self.panel.x1 - 12.0, y + self.status_height);
            y += self.status_height + gap;
        } else {
            self.status = Rect::ZERO;
        }
        self.own_activity_top = y;
        if self.shows_own_activity {
            y += self.own_line_height + gap;
        }
        self.sharing_top = y;
        self.sharing_change = Rect::new(
            text_x - 4.0,
            y,
            (text_x + self.sharing_text_width + 4.0).min(self.panel.x1 - 12.0),
            y + self.sharing_line_height,
        );
        y += self.sharing_line_height + gap;
        self.status_modes = self
            .offered_modes
            .iter()
            .map(|mode| {
                if !self.sharing_open {
                    return (*mode, Rect::ZERO);
                }
                // One single-choice list: rows touch, the chosen one lifted
                // and checked.
                let rect = Rect::new(text_x - 8.0, y, self.panel.x1 - 8.0, y + self.mode_height);
                y += self.mode_height;
                (*mode, rect)
            })
            .collect();
    }

    /// Open or close the choice of what to share ([`CollaboratorsHit::ChangeSharing`]).
    /// Choosing a mode closes it again on the next paint.
    pub fn toggle_sharing_choices(&mut self) {
        self.targets.cancel_pointer_press();
        self.sharing_open = !self.sharing_open;
        if !self.sharing_open
            && matches!(
                self.keyboard_focus,
                Some(CollaboratorsHit::SetStatusSharing(_))
            )
        {
            self.keyboard_focus = Some(CollaboratorsHit::ChangeSharing);
        }
    }

    /// What the sharing line says for `mode`: plain words, never a mode
    /// name.
    fn sharing_line(mode: ui::StatusSharing) -> String {
        crate::i18n::t(match mode {
            ui::StatusSharing::Automatic => "people-panel-sharing-line-auto",
            ui::StatusSharing::ManualOnly => "people-panel-sharing-line-manual",
            ui::StatusSharing::Off => "people-panel-sharing-line-off",
        })
    }

    /// The label of the choice that selects `mode`.
    fn sharing_choice(mode: ui::StatusSharing) -> String {
        crate::i18n::t(match mode {
            ui::StatusSharing::Automatic => "people-panel-sharing-choice-auto",
            ui::StatusSharing::ManualOnly => "people-panel-sharing-choice-manual",
            ui::StatusSharing::Off => "people-panel-sharing-choice-off",
        })
    }

    /// The sharing line with its link: "Sharing what you're doing · Change".
    fn sharing_parts(mode: ui::StatusSharing) -> (String, String) {
        (
            format!("{} \u{b7} ", Self::sharing_line(mode)),
            crate::i18n::t("people-panel-sharing-change"),
        )
    }

    /// A disclosure or warning line: the panel less its 20 px insets and
    /// the scroll gutter.
    fn disclosure_width(&self, theme: &Theme) -> f32 {
        ui::num::f32_of_f64(
            (self.panel_width - 40.0 - f64::from(theme.tokens.metrics.scrollbar_gutter)).max(0.0),
        )
    }

    fn measure_controls(
        &mut self,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &CollaboratorsModel<'_>,
    ) {
        self.panel_margin = ui::widget::floating_surface::viewport_margin(theme);
        self.min_target = f64::from(theme.tokens.metrics.min_pointer_target);
        self.panel_width = f64::from(theme.tokens.metrics.panel_width_list)
            .min(model.host.width() - 2.0 * self.panel_margin)
            .max(1.0);
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
                text.measure_style(detail, Some(self.disclosure_width(theme)), &caption);
            f64::from(line + height) + 16.0
        });
        self.warning_height = model.unavailable.map_or(24.0, |reason| {
            let (_, height) =
                text.measure_style(reason, Some(self.disclosure_width(theme)), &caption);
            f64::from(height) + 8.0
        });
        self.control_height = ButtonFace::height(ControlSize::Regular, theme);
        self.activity_height =
            ui::widget::ListRowFace::new(&crate::i18n::t("a11y-shell-workspace-activity"))
                .disclosure()
                .height(text, theme);
        self.measure_own_block(text, theme, model);
        self.follow_height = ButtonFace::height(ControlSize::Regular, theme);
        self.row_height = (self.follow_height + 8.0).max(ROW_MIN_H);
        self.follow_width = ["people-panel-follow", "people-panel-stop-following"]
            .into_iter()
            .map(|key| {
                ButtonFace::new(Rect::ZERO, &crate::i18n::t(key))
                    .role(ui::widget::ButtonRole::Plain)
                    .control_size(ControlSize::Regular)
                    .intrinsic_width(text, theme)
            })
            .fold(FOLLOW_MIN_W, f64::max);
        self.measure_expanded(text, theme, model);
        self.measure_watching(text, theme, model);
    }

    /// Measure the own block: the name line, the status field where the host
    /// edits it, the live line, the sharing line and the open choices.
    fn measure_own_block(
        &mut self,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &CollaboratorsModel<'_>,
    ) {
        let name_style = theme.text(TextRole::Body).strong();
        let caption = theme.text(TextRole::Caption);
        let caption_line = f64::from(caption.line_height_px());
        self.own_line_height = caption_line;
        model.own_label.clone_into(&mut self.own_label);
        self.rename_offered = model.own_rename;
        let (name_width, name_line) = text.measure_style(model.own_label, None, &name_style);
        self.own_name_width = f64::from(name_width);
        self.name_height = f64::from(name_line).max(26.0);
        self.status_height = if model.status_editable {
            FieldFace::height(ControlSize::Regular, theme)
        } else {
            0.0
        };
        self.mode_height =
            ui::widget::ListRowFace::new(&Self::sharing_choice(model.status_sharing))
                .checked(true)
                .height(text, theme)
                .max(f64::from(theme.tokens.metrics.min_pointer_target));
        self.offered_modes = [
            ui::StatusSharing::Automatic,
            ui::StatusSharing::ManualOnly,
            ui::StatusSharing::Off,
        ]
        .into_iter()
        .filter(|mode| model.status_editable || *mode != ui::StatusSharing::ManualOnly)
        .collect();
        let (line, change) = Self::sharing_parts(model.status_sharing);
        self.sharing_text_width = f64::from(text.measure_style(&line, None, &caption).0)
            + f64::from(text.measure_style(&change, None, &caption).0);
        // The link is one comfortable target tall, not just its text.
        self.sharing_line_height = (caption_line + 8.0).max(24.0);
        let gap = f64::from(theme.tokens.spacing.s1);
        let mut height = gap + self.name_height + gap;
        if model.status_editable {
            height += self.status_height + gap;
        }
        self.shows_own_activity = model.own_activity.is_some();
        if self.shows_own_activity {
            height += caption_line + gap;
        }
        height += self.sharing_line_height;
        if self.sharing_open {
            height += gap + f64_of_usize(self.offered_modes.len()) * self.mode_height;
        }
        self.status_block_height = height + f64::from(theme.tokens.spacing.s3);
    }

    /// Lay out the expanded row's recent summary: wrapped under the row's
    /// status, clamped to [`RECENT_MAX_LINES`].
    fn measure_expanded(
        &mut self,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &CollaboratorsModel<'_>,
    ) {
        self.expanded_row = None;
        self.expanded_extra = 0.0;
        self.expanded_lines.clear();
        let Some((index, recent)) = model.expanded_id.and_then(|id| {
            model.rows.iter().enumerate().find_map(|(index, row)| {
                (row.id == id && !row.invited)
                    .then_some(row.recent.as_ref())
                    .flatten()
                    .map(|recent| (index, recent))
            })
        }) else {
            return;
        };
        let caption = theme.text(TextRole::Caption);
        self.expanded_lines = text.clamp_to_lines_style(
            &recent.text,
            self.recent_width(theme),
            RECENT_MAX_LINES,
            &caption,
        );
        if self.expanded_lines.is_empty() {
            return;
        }
        self.expanded_row = Some(index);
        let followed = model.followed_id == Some(id_of(model, index));
        let top = model
            .rows
            .get(index)
            .map_or(0.0, |row| self.recent_top(row, followed, theme));
        self.expanded_extra = (top - self.row_h()).max(0.0)
            + f64_of_usize(self.expanded_lines.len()) * f64::from(caption.line_height_px())
            + f64::from(theme.tokens.spacing.s2);
    }

    /// Where a row's recent summary starts, from the row's top: under the
    /// resting row, or under the status line when large text pushes that
    /// lower, so the two never overlap.
    fn recent_top(&self, row: &RosterRow, followed: bool, theme: &Theme) -> f64 {
        let resting = self.row_h() - f64::from(theme.tokens.spacing.s1);
        if row.status.is_none() {
            return resting;
        }
        let has_activity = row.invited || followed || row.activity.is_some() || row.role.is_some();
        let status_y = if has_activity { 45.0 } else { 29.0 };
        resting.max(status_y + f64::from(theme.text(TextRole::Caption).line_height_px()) + 2.0)
    }

    /// "+N watching" sits beside "Here now" unless that would run into the
    /// heading; then it takes a second header line.
    fn measure_watching(
        &mut self,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &CollaboratorsModel<'_>,
    ) {
        self.watching_wraps = false;
        if model.watching == 0 {
            return;
        }
        let caption = theme.text(TextRole::Caption);
        let (heading, _) = text.measure_style(
            &crate::i18n::t("a11y-shell-people"),
            None,
            &theme.text(TextRole::Heading),
        );
        let here_count = model
            .rows
            .iter()
            .filter(|row| !row.invited)
            .count()
            .saturating_add(1);
        let (here, _) = text.measure_style(
            &crate::i18n::t_args("people-panel-here-now", &[("count", here_count.into())]),
            None,
            &caption,
        );
        let (watching, line) =
            text.measure_style(&Self::watching_label(model.watching), None, &caption);
        let dots = f64_of_usize(here_count.min(4)) * 7.0 + 8.0;
        let width = self.panel_width - f64::from(theme.tokens.metrics.scrollbar_gutter);
        let needed = 16.0
            + f64::from(heading)
            + f64::from(theme.tokens.spacing.s3)
            + f64::from(watching)
            + 8.0
            + dots
            + f64::from(here)
            + 14.0;
        if needed > width {
            self.watching_wraps = true;
            self.header_height = self
                .header_height
                .max(22.0 + f64::from(line) * 2.0 + f64::from(theme.tokens.spacing.s2));
        }
    }

    fn watching_label(count: usize) -> String {
        crate::i18n::t_args("people-panel-watching", &[("count", count.into())])
    }

    /// Width of a status-style line under a row: from past its icon to the
    /// panel's trailing inset. Below the Follow button, so it spans it.
    fn recent_width(&self, theme: &Theme) -> f32 {
        // The text column inset, the icon's text inset and the panel's
        // 12 px trailing inset (`ROW_TEXT_INSET + ROW_ICON_TEXT_INSET + 12`).
        ui::num::f32_of_f64(
            (self.panel_width
                - f64::from(theme.tokens.metrics.scrollbar_gutter)
                - (ROW_TEXT_INSET + ROW_ICON_TEXT_INSET + 12.0))
                .max(0.0),
        )
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
        // A host that cannot edit shows the status read-only: no
        // invitation to type, no hover.
        let placeholder = if self.status_read_only {
            String::new()
        } else {
            crate::i18n::t("people-panel-status-empty")
        };
        let face = FieldFace::new(self.status, shown)
            .placeholder(&placeholder)
            .text_role(TextRole::Caption)
            .focused(model.status_editing)
            .hovered(
                !self.status_read_only && cursor.is_some_and(|point| self.status.contains(point)),
            );
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

    /// The sharing line, its "Change" link and, while open, the choices.
    fn paint_sharing(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        sharing: ui::StatusSharing,
        cursor: Option<Point>,
    ) {
        let colors = &theme.tokens.colors;
        let caption = theme.text(TextRole::Caption);
        let (lead, change) = Self::sharing_parts(sharing);
        let link = self.sharing_change;
        let hovered = cursor.is_some_and(|point| link.contains(point));
        if hovered || self.interaction(CollaboratorsHit::ChangeSharing).pressed {
            scene.fill(
                Fill::NonZero,
                transform,
                colors.text_primary.multiply_alpha(0.06),
                None,
                &RoundedRect::from_rect(link, f64::from(theme.tokens.radii.detail)),
            );
        }
        let y = self.sharing_top
            + (self.sharing_line_height - f64::from(caption.line_height_px())) / 2.0;
        let x = link.x0 + 4.0;
        let (lead_width, _) = text_renderer.measure_style(&lead, None, &caption);
        text_renderer.draw_style(
            scene,
            &lead,
            colors.text_dim,
            transform.pre_translate((x, y).into()),
            None,
            &caption,
        );
        text_renderer.draw_style(
            scene,
            &change,
            colors.intent.inverted,
            transform.pre_translate((x + f64::from(lead_width), y).into()),
            None,
            &caption,
        );
        for (mode, rect) in self
            .status_modes
            .iter()
            .filter(|(_, rect)| rect.area() > 0.0)
        {
            let hit = CollaboratorsHit::SetStatusSharing(*mode);
            let mut interaction = self.interaction(hit);
            interaction.hovered = cursor.is_some_and(|point| rect.contains(point));
            interaction.focused = self.keyboard_focus == Some(hit);
            let label = Self::sharing_choice(*mode);
            ui::widget::ListRowFace::new(&label)
                .checked(*mode == sharing)
                .selected(*mode == sharing)
                .interaction(interaction)
                .paint(
                    &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                    *rect,
                );
        }
    }

    /// The own name, in the text column beside the avatar, with a pencil
    /// when the host can rename.
    fn paint_own_name(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rename: bool,
        cursor: Option<Point>,
    ) {
        let colors = &theme.tokens.colors;
        let style = theme.text(TextRole::Body).strong();
        let target = Rect::new(
            self.panel.x0 + ROW_TEXT_INSET - 4.0,
            self.own_avatar.center().y - self.name_height / 2.0,
            self.own_name.x1,
            self.own_avatar.center().y + self.name_height / 2.0,
        );
        let hovered = rename && cursor.is_some_and(|point| target.contains(point));
        if hovered || (rename && self.interaction(CollaboratorsHit::Rename).pressed) {
            scene.fill(
                Fill::NonZero,
                transform,
                colors.text_primary.multiply_alpha(0.06),
                None,
                &RoundedRect::from_rect(target, f64::from(theme.tokens.radii.detail)),
            );
        }
        let x = self.panel.x0 + ROW_TEXT_INSET;
        let pencil = if rename { 6.0 + 14.0 } else { 0.0 };
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a non-negative on-screen width in logical pixels; f32 holds it with sub-pixel precision"
        )]
        let shown = text_renderer.elide_to_width_style(
            &self.own_label,
            (self.panel.x1 - 12.0 - x - pencil).max(0.0) as f32,
            &style,
        );
        let (width, height) = text_renderer.measure_style(&shown, None, &style);
        let top = target.y0 + (self.name_height - f64::from(height)) / 2.0;
        text_renderer.draw_style(
            scene,
            &shown,
            colors.text_primary,
            transform.pre_translate((x, top).into()),
            None,
            &style,
        );
        if rename {
            let icon_x = x + f64::from(width) + 6.0;
            let centre = target.y0 + self.name_height / 2.0;
            Icon::Pencil.draw(
                scene,
                transform,
                Rect::new(icon_x, centre - 7.0, icon_x + 14.0, centre + 7.0),
                if hovered {
                    colors.text_primary
                } else {
                    colors.text_dim
                },
            );
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
        let avatar = self.own_avatar;
        let initial: Option<String> = model
            .own_label
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_uppercase().collect());
        ui::widget::AvatarFace::new(initial.as_deref().unwrap_or_default(), "")
            .presence(model.own_color)
            .paint(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                avatar,
            );
        if initial.is_none() {
            // An identity with no letter to show (a bare key) would leave a
            // blank disc; a person glyph says who it is instead.
            Icon::User.draw(
                scene,
                transform,
                avatar.inflate(-6.0, -6.0),
                colors.surfaces.base,
            );
        }
        let spark = vello::kurbo::Circle::new(Point::new(avatar.x1 - 2.0, avatar.y1 - 2.0), 3.5);
        scene.stroke(
            &vello::kurbo::Stroke::new(2.0),
            transform,
            ui::chrome::shell::panel_ground(theme),
            None,
            &spark,
        );
        scene.fill(Fill::NonZero, transform, model.own_color, None, &spark);
        self.paint_own_name(
            scene,
            transform,
            text_renderer,
            theme,
            model.own_rename,
            cursor,
        );
        if self.status.area() > 0.0 {
            self.paint_status_field(scene, transform, text_renderer, theme, model, cursor);
            if model.own_status_automatic && !model.status_editing {
                // The status in the field was suggested, not typed.
                let side = 12.0;
                let badge = Rect::new(
                    self.status.x1 - 8.0 - side,
                    self.status.center().y - side / 2.0,
                    self.status.x1 - 8.0,
                    self.status.center().y + side / 2.0,
                );
                Icon::Sparkles.draw(scene, transform, badge, colors.intent.inverted);
            }
        }
        if let Some(activity) = model.own_activity {
            let caption = theme.text(TextRole::Caption);
            let x = self.panel.x0 + ROW_TEXT_INSET;
            let top = self.own_activity_top;
            let line = f64::from(caption.line_height_px());
            // Model prose carries the sparkle; a line read off what this
            // person did carries only their colour.
            if model.own_activity_generated {
                Icon::Sparkles.draw(
                    scene,
                    transform,
                    Rect::from_center_size((x + 6.0, top + line / 2.0), (12.0, 12.0)),
                    colors.intent.inverted,
                );
            } else {
                let dot = vello::kurbo::Circle::new(Point::new(x + 6.0, top + line / 2.0), 3.0);
                scene.fill(Fill::NonZero, transform, model.own_color, None, &dot);
            }
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a non-negative on-screen width in logical pixels; f32 holds it with sub-pixel precision"
            )]
            let activity = text_renderer.elide_to_width_style(
                activity,
                (self.panel.x1 - 12.0 - x - ROW_ICON_TEXT_INSET).max(0.0) as f32,
                &caption,
            );
            text_renderer.draw_style(
                scene,
                &activity,
                if model.own_activity_generated {
                    colors.intent.inverted
                } else {
                    colors.text_dim
                },
                transform.pre_translate((x + ROW_ICON_TEXT_INSET, top).into()),
                None,
                &caption,
            );
        }
        self.paint_sharing(
            scene,
            transform,
            text_renderer,
            theme,
            model.status_sharing,
            cursor,
        );
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
        let details = self
            .row_details
            .get(frame.index)
            .copied()
            .filter(|rect| rect.area() > 0.0);
        if let Some(body) = details.filter(|body| {
            !frame.followed
                && frame.cursor.is_some_and(|point| {
                    body.contains(point)
                        && !self
                            .row_actions
                            .get(frame.index)
                            .is_some_and(|action| action.contains(point))
                })
        }) {
            scene.fill(
                Fill::NonZero,
                transform,
                tokens
                    .colors
                    .lift_on(ui::chrome::shell::panel_ground(theme), 0.5),
                None,
                &RoundedRect::from_rect(body, f64::from(tokens.radii.control)),
            );
        }
        if frame.followed {
            let active = RoundedRect::new(
                self.panel.x0 + 8.0,
                frame.y + 3.0,
                self.panel.x1 - 8.0,
                frame.y + self.row_h() - 3.0,
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
                    frame.y + self.row_h() - 11.0,
                    1.5,
                ),
            );
        }

        self.draw_row_identity(scene, transform, text_renderer, theme, frame);
        self.draw_row_text(scene, transform, text_renderer, theme, frame);
    }

    /// The avatar's centre: level with the row's lines, never lower than
    /// on a three-line row.
    fn avatar_center_y(&self, frame: &RowFrame<'_>) -> f64 {
        frame.y + self.row_h() / 2.0
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
        let center = Point::new(self.panel.x0 + 28.0, self.avatar_center_y(frame));
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
        let center = Point::new(self.panel.x0 + 28.0, self.avatar_center_y(frame));
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
        let text_x = self.panel.x0 + ROW_TEXT_INSET;
        let text_x1 = self.row_text_x1(frame);
        let label_x1 = if row.is_agent {
            (self.panel.x1 - 142.0).min(text_x1)
        } else {
            text_x1
        };
        let caption = tokens.typography.style(TextRole::Caption);
        let line_h = f64::from(text_renderer.measure_style("Ag", None, &caption).1) + 2.0;
        let agent_tag = crate::i18n::t("people-panel-agent-tag");
        // The role sits on the right of the name line, so the left column is
        // the person (name, then what they are doing) and the role reads as a
        // label for the row, not as a second line of the person.
        let role = if row.invited || row.is_agent {
            None
        } else {
            row.role.as_deref()
        };
        // Two lines at most, so a row stays one compact height: a person's own
        // status, when set, is the second line; otherwise what they are doing.
        let status_line = row.status.as_ref();
        let subtitle = if status_line.is_some() && !row.invited {
            None
        } else if row.invited {
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
        } else if row.is_agent {
            None
        } else if role.is_none() {
            row.role.clone()
        } else {
            None
        };
        let lines =
            1_usize.saturating_add(usize::from(subtitle.is_some() || status_line.is_some()));
        let block_h = line_h * f64_of_usize(lines);
        let top = row_y + (self.row_h() - block_h) / 2.0;
        let right = label_x1;
        let role_shown = role.map(|role| {
            let shown = text_renderer.elide_to_width_style(role, 96.0, &caption);
            let (width, _) = text_renderer.measure_style(&shown, None, &caption);
            (shown, f64::from(width))
        });
        let role_reserve = role_shown.as_ref().map_or(0.0, |(_, width)| width + 8.0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the collaborator popover is only a few hundred logical pixels wide"
        )]
        let shown_label = text_renderer.elide_to_width_style(
            &row.label,
            (right - text_x - role_reserve).max(0.0) as f32,
            &caption,
        );
        text_renderer.draw_style(
            scene,
            &shown_label,
            if row.invited {
                colors.text_dim
            } else {
                colors.text_primary
            },
            transform.pre_translate((text_x, top).into()),
            None,
            &caption,
        );
        if let Some((role, width)) = role_shown {
            text_renderer.draw_style(
                scene,
                &role,
                colors.text_dim,
                transform.pre_translate((right - width, top).into()),
                None,
                &caption,
            );
        }
        if row.is_agent && !row.label.eq_ignore_ascii_case(&agent_tag) {
            text_renderer.draw_style(
                scene,
                &agent_tag,
                colors.text_dim,
                transform.pre_translate((self.panel.x1 - 136.0, top).into()),
                None,
                &caption,
            );
        }
        let mut next_line = top + line_h;
        let detail_x1 = if row.followable {
            self.panel.x1 - self.follow_width.max(FOLLOW_MIN_W) - 20.0
        } else {
            self.panel.x1 - 12.0
        };
        if let Some(subtitle) = subtitle {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "the collaborator popover is only a few hundred logical pixels wide"
            )]
            let subtitle = text_renderer.elide_to_width_style(
                &subtitle,
                (detail_x1 - text_x).max(0.0) as f32,
                &caption,
            );
            text_renderer.draw_style(
                scene,
                &subtitle,
                colors.text_dim,
                transform.pre_translate((text_x, next_line).into()),
                None,
                &caption,
            );
            next_line += line_h;
        }
        if let Some(status) = &row.status {
            let icon_rect = Rect::new(text_x, next_line, text_x + 12.0, next_line + 12.0);
            // Only model prose gets the sparkle and only typed words the
            // pencil; a line read off what they did gets their colour.
            match status.source {
                LineSource::Generated => {
                    Icon::Sparkles.draw(scene, transform, icon_rect, colors.intent.inverted);
                }
                LineSource::Written => {
                    Icon::Pencil.draw(scene, transform, icon_rect, colors.intent.inverted);
                }
                LineSource::Observed => {
                    let dot = vello::kurbo::Circle::new(icon_rect.center(), 3.0);
                    scene.fill(Fill::NonZero, transform, row.color, None, &dot);
                }
            }
            let status_text_x = text_x + ROW_ICON_TEXT_INSET;
            #[allow(
                clippy::cast_possible_truncation,
                reason = "the collaborator popover is only a few hundred logical pixels wide"
            )]
            let shown = text_renderer.elide_to_width_style(
                &status.text,
                (detail_x1 - status_text_x).max(0.0) as f32,
                &caption,
            );
            text_renderer.draw_style(
                scene,
                &shown,
                colors.intent.inverted,
                transform.pre_translate((status_text_x, next_line).into()),
                None,
                &caption,
            );
        }
        self.draw_row_details(scene, transform, text_renderer, theme, frame);
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
                .role(ui::widget::ButtonRole::Plain)
                .control_size(ui::widget::ControlSize::Regular)
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

    /// Where a row's text column ends: before its Follow button, and before
    /// the disclosure chevron of an expandable row.
    fn row_text_x1(&self, frame: &RowFrame<'_>) -> f64 {
        let expandable = self
            .row_details
            .get(frame.index)
            .is_some_and(|rect| rect.area() > 0.0);
        if frame.row.followable {
            self.panel.x1 - self.follow_width.max(FOLLOW_MIN_W) - 20.0
        } else if expandable {
            self.panel.x1 - 28.0
        } else {
            self.panel.x1 - 12.0
        }
    }

    /// An expandable row's disclosure chevron and, while expanded, its
    /// recent summary under the status line. Rows without a summary draw
    /// neither, so nothing suggests a click that does nothing.
    fn draw_row_details(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        frame: &RowFrame<'_>,
    ) {
        if !self
            .row_details
            .get(frame.index)
            .is_some_and(|rect| rect.area() > 0.0)
        {
            return;
        }
        let colors = &theme.tokens.colors;
        let expanded = self.expanded_row == Some(frame.index);
        // The chevron sits in the gap before the Follow button (or the
        // trailing inset), centred on the row's resting height.
        let chevron_x1 = if frame.row.followable {
            self.panel.x1 - self.follow_width.max(FOLLOW_MIN_W) - 16.0
        } else {
            self.panel.x1 - 14.0
        };
        let center_y = frame.y + self.row_h() / 2.0;
        Icon::draw(
            if expanded {
                Icon::ChevronUp
            } else {
                Icon::ChevronDown
            },
            scene,
            transform,
            Rect::new(
                chevron_x1 - 12.0,
                center_y - 6.0,
                chevron_x1,
                center_y + 6.0,
            ),
            colors.text_dim,
        );
        let Some(recent) = frame.row.recent.as_ref().filter(|_| expanded) else {
            return;
        };
        let caption = theme.text(TextRole::Caption);
        let line_height = f64::from(caption.line_height_px());
        let text_x = self.panel.x0 + ROW_TEXT_INSET;
        let top = frame.y + self.recent_top(frame.row, frame.followed, theme);
        // Model prose carries the same sparkle as an inferred status, so a
        // summary is never read as the person's own words.
        let (icon, ink) = if recent.source == LineSource::Generated {
            (Icon::Sparkles, colors.intent.inverted)
        } else {
            (Icon::History, colors.text_dim)
        };
        icon.draw(
            scene,
            transform,
            Rect::new(text_x, top + 1.0, text_x + 12.0, top + 13.0),
            ink,
        );
        for (line_index, line) in self.expanded_lines.iter().enumerate() {
            text_renderer.draw_style(
                scene,
                line,
                ink,
                transform.pre_translate(
                    (
                        text_x + ROW_ICON_TEXT_INSET,
                        top + f64_of_usize(line_index) * line_height,
                    )
                        .into(),
                ),
                None,
                &caption,
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
    let workspace = desktop.active_workspace();
    let now = tilo_clock::unix_now();
    let rows: Vec<RosterRow> = roster
        .iter()
        .map(|participant| {
            let agent_role = workspace.and_then(|workspace| {
                workspace
                    .agent_role_at(&participant.id, now)
                    .or_else(|| workspace.agent_role_at(&participant.name, now))
            });
            let is_agent = agent_role.is_some() || has_agent_marker(participant);
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
                    .map(crate::workspace_settings::role_label)
                    .or_else(|| {
                        participant
                            .role_id
                            .as_deref()
                            .filter(|_| !has_agent_marker(participant))
                            .map(display_role_id)
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
                recent: None,
                followable: !participant.invited && (!is_agent || has_view),
            }
        })
        .collect();
    // The collaborators popover is immediate presence, not membership
    // administration. An agent granted access but not connected belongs in
    // Members; once its live session arrives in `roster`, the identity match
    // above tags it as an agent and exposes the same cursor/follow affordance
    // through truthful semantic activity rather than a fabricated cursor.
    rows
}

/// The role id a room without workspace grants (an ephemeral, hosted
/// roster) sets on a participant that is software, not a person.
pub const AGENT_ROLE_MARKER: &str = "agent";

fn has_agent_marker(participant: &Collaborator) -> bool {
    participant.role_id.as_deref() == Some(AGENT_ROLE_MARKER)
}

/// Whether `participant` is an agent: admitted to `workspace` as one (by id
/// or display name), or marked with [`AGENT_ROLE_MARKER`]. Every presence
/// surface asks this, so an agent never shows as a person on one of them.
#[must_use]
pub fn collaborator_is_agent(
    workspace: Option<&tilo_desktop_model::Workspace>,
    participant: &Collaborator,
    now: u64,
) -> bool {
    has_agent_marker(participant)
        || workspace.is_some_and(|workspace| {
            workspace.agent_role_at(&participant.id, now).is_some()
                || workspace.agent_role_at(&participant.name, now).is_some()
        })
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

fn id_of<'a>(model: &CollaboratorsModel<'a>, index: usize) -> &'a str {
    model.rows.get(index).map_or("", |row| row.id.as_str())
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
                    rect.height() >= f64::from(ui::tokens::Metrics::BASE.min_pointer_target)
                        && rect.width() >= f64::from(ui::tokens::Metrics::BASE.min_pointer_target),
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

        fn crowded_rosters_list_everyone_and_scroll_past_eight_rows() {
            let mut overlay = CollaboratorsOverlay::default();
            let host = Rect::new(0.0, 0.0, 1200.0, 1600.0);
            overlay.layout(host, 12, false, true, false);

            assert_eq!(
                overlay
                    .row_action_rects()
                    .iter()
                    .filter(|rect| rect.area() > 0.0)
                    .count(),
                12,
                "everyone is listed; there are no pages"
            );
            assert!(
                overlay
                    .row_rects
                    .iter()
                    .all(|rect| rect.y1 < overlay.activity.y0)
            );
            assert_eq!(
                overlay.hit_test(overlay.row_action_rects()[11].center()),
                CollaboratorsHit::ToggleFollow(11)
            );
            let visible_bottom = overlay.panel.y0 + overlay.visible_height;
            assert!(
                overlay.panel.y1 > visible_bottom,
                "twelve people overflow the popover"
            );
            let tail = overlay.panel.y1 - overlay.row_rects[11].y1;
            assert!(
                (overlay.row_rects[7].y1 + tail - visible_bottom).abs() < 1e-9,
                "the capped popover shows exactly eight rows"
            );

            overlay.layout(host, 5, false, true, false);
            assert!(
                (overlay.panel.y0 + overlay.visible_height - overlay.panel.y1).abs() < 1e-9,
                "a short roster never scrolls"
            );
        }

        footer_separator_and_row_edge_register_on_every_palette();
        footer_keeps_a_token_gap_around_the_activity_row();
        every_button_is_at_least_one_pointer_target_and_centred_in_its_band();
        crowded_rosters_list_everyone_and_scroll_past_eight_rows();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn collaborators_accessibility_and_roster_separate_identity_action_and_intent() {
        fn agent_classification_uses_stable_identity_not_display_label() {
            use tilo_desktop_model::{Workspace, WorkspaceId};

            let mut workspace = Workspace::new(WorkspaceId(71), "Shared");
            workspace.set_owner("owner");
            assert!(
                !crate::workspace_settings::admit_agent(
                    &mut workspace,
                    &"owner".to_string(),
                    "scribe",
                    crate::workspace_settings::AgentPreset::Operator,
                )
                .is_empty(),
                "owner admits agent"
            );
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
            assert_eq!(rows[0].role.as_deref(), Some("Editor"));
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
                invited: false,
                activity: Some("Editing · Q2 finance report".into()),
                status: Some(RosterStatus {
                    text: "Preparing the board review".into(),
                    source: LineSource::Generated,
                }),
                recent: None,
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
            paint_roster_focus(&mut overlay, &[]);
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
                    invited: false,
                    activity: None,
                    status: None,
                    recent: None,
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
                    status: None,
                    recent: None,
                    followable: true,
                },
            ];
            let pairs = vec![("ada".to_string(), "bob".to_string())];
            let model = CollaboratorsModel {
                host: Rect::ZERO,
                rows: &rows,
                own_label: "Quinn",
                own_rename: false,
                own_color: Color::from_rgb8(7, 8, 9),
                unavailable: None,
                may_invite: false,
                followed_id: None,
                following_pairs: &pairs,
                own_status: None,
                own_activity: None,
                own_activity_generated: false,
                own_status_automatic: false,
                status_sharing: ui::StatusSharing::Automatic,
                status_editing: false,
                status_selection: ui::text_selection::Selection::default(),
                screen_exposure: None,
                expanded_id: None,
                watching: 0,
                status_editable: true,
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
                overlay.hit_test(overlay.activity_rect().center()),
                CollaboratorsHit::RoomActivity
            );
            assert_eq!(
                overlay.hit_test(overlay.panel_rect().center()),
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
            let people = roster_focus_rows();
            paint_roster_focus(&mut overlay, &people);
            assert_eq!(
                overlay.hit_test(overlay.status.center()),
                CollaboratorsHit::EditStatus
            );
            assert!(
                overlay
                    .status_modes
                    .iter()
                    .all(|(_, rect)| rect.area() == 0.0),
                "the choices stay closed until asked for"
            );
            assert_eq!(
                overlay.hit_test(overlay.sharing_change.center()),
                CollaboratorsHit::ChangeSharing
            );
            overlay.toggle_sharing_choices();
            paint_roster_focus(&mut overlay, &people);
            assert_eq!(overlay.status_modes.len(), 3);
            for (mode, rect) in &overlay.status_modes {
                assert_eq!(
                    overlay.hit_test(rect.center()),
                    CollaboratorsHit::SetStatusSharing(*mode)
                );
            }
        }

        fn keyboard_reaches_status_privacy_follow_and_activity_without_pointer_input() {
            let mut overlay = CollaboratorsOverlay::default();
            paint_roster_focus(&mut overlay, &roster_focus_rows());

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
                invited: false,
                activity: None,
                status: None,
                recent: None,
                followable: true,
            })
            .collect()
    }

    /// Paint the own block as a host that edits no status but renames (the
    /// browser): `sharing` is the current choice.
    fn paint_guest(overlay: &mut CollaboratorsOverlay, sharing: ui::StatusSharing) {
        overlay.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &CollaboratorsModel {
                host: Rect::new(0.0, 0.0, 800.0, 600.0),
                rows: &[],
                own_label: "Brave Fox",
                own_rename: true,
                own_color: Color::from_rgb8(170, 120, 130),
                unavailable: None,
                may_invite: false,
                followed_id: None,
                following_pairs: &[],
                own_status: None,
                own_activity: Some("Editing Welcome in Notes"),
                own_activity_generated: false,
                own_status_automatic: false,
                status_sharing: sharing,
                status_editing: false,
                status_selection: ui::text_selection::Selection::default(),
                screen_exposure: None,
                expanded_id: None,
                watching: 0,
                status_editable: false,
            },
            None,
        );
    }

    #[test]
    fn a_guest_sees_their_name_and_plain_sharing_choices() {
        let mut overlay = CollaboratorsOverlay::default();
        paint_guest(&mut overlay, ui::StatusSharing::Automatic);
        assert_eq!(overlay.status, Rect::ZERO, "no status field it cannot edit");
        assert_eq!(
            overlay.hit_test(overlay.own_name.center()),
            CollaboratorsHit::Rename
        );
        let snapshot = overlay.accessibility_snapshot();
        let change = snapshot
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Change what you share"))
            .expect("the sharing line is a control");
        assert_eq!(
            change.description.as_deref(),
            Some("Sharing what you're doing")
        );
        assert!(
            snapshot
                .children
                .iter()
                .any(|node| node.name.as_deref() == Some("Change your name, Brave Fox"))
        );

        overlay.toggle_sharing_choices();
        paint_guest(&mut overlay, ui::StatusSharing::Automatic);
        let offered: Vec<_> = overlay.status_modes.iter().map(|(mode, _)| *mode).collect();
        assert_eq!(
            offered,
            [ui::StatusSharing::Automatic, ui::StatusSharing::Off],
            "writing your own needs a host that edits the status"
        );
        let off = overlay.status_modes[1].1;
        assert_eq!(
            overlay.hit_test(off.center()),
            CollaboratorsHit::SetStatusSharing(ui::StatusSharing::Off)
        );
        // The host applied the choice: the choices close and the line says
        // what was chosen.
        paint_guest(&mut overlay, ui::StatusSharing::Off);
        assert!(
            overlay
                .status_modes
                .iter()
                .all(|(_, rect)| rect.area() == 0.0)
        );
        let snapshot = overlay.accessibility_snapshot();
        assert!(
            snapshot.children.iter().any(|node| {
                node.description.as_deref() == Some("Not sharing what you're doing")
            })
        );
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
                own_label: "Quinn",
                own_rename: false,
                own_color: Color::from_rgb8(170, 120, 130),
                unavailable: None,
                may_invite: false,
                followed_id: None,
                following_pairs: &[],
                own_status: None,
                own_activity: None,
                own_activity_generated: false,
                own_status_automatic: false,
                status_sharing: ui::StatusSharing::Automatic,
                status_editing: false,
                status_selection: ui::text_selection::Selection::default(),
                screen_exposure: None,
                expanded_id: None,
                watching: 0,
                status_editable: true,
            },
            None,
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test walks collapsed, expanded and read-only paints of one roster"
    )]
    fn roster_recent_details_watching_and_read_only_status() {
        fn paint(
            overlay: &mut CollaboratorsOverlay,
            rows: &[RosterRow],
            expanded_id: Option<&str>,
            status_editable: bool,
        ) {
            overlay.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
                &CollaboratorsModel {
                    host: Rect::new(0.0, 0.0, 800.0, 700.0),
                    rows,
                    own_label: "Quinn",
                    own_rename: false,
                    own_color: Color::from_rgb8(170, 120, 130),
                    unavailable: None,
                    may_invite: false,
                    followed_id: None,
                    following_pairs: &[],
                    own_status: Some("Reviewing"),
                    own_activity: None,
                    own_activity_generated: false,
                    own_status_automatic: false,
                    status_sharing: ui::StatusSharing::Automatic,
                    status_editing: false,
                    status_selection: ui::text_selection::Selection::default(),
                    screen_exposure: None,
                    expanded_id,
                    watching: 2,
                    status_editable,
                },
                None,
            );
        }
        fn person(snapshot: &ui::a11y::AccessNode, index: usize) -> String {
            snapshot.children[0].children[index]
                .description
                .clone()
                .unwrap_or_default()
        }

        let mut people = roster_focus_rows();
        people[0].recent = Some(RosterStatus {
            text: "Rewrote the intro and resolved two comments in Notes".into(),
            source: LineSource::Generated,
        });
        let mut overlay = CollaboratorsOverlay::default();
        paint(&mut overlay, &people, None, true);
        let ada = overlay.row_rects[0];
        let bob = overlay.row_rects[1];
        let body = Point::new(ada.x0 + 60.0, ada.center().y);
        assert_eq!(overlay.hit_test(body), CollaboratorsHit::ToggleDetails(0));
        assert_eq!(
            overlay.hit_test(overlay.row_actions[0].center()),
            CollaboratorsHit::ToggleFollow(0),
            "Follow stays its own target inside an expandable row"
        );
        assert_eq!(
            overlay.hit_test(Point::new(bob.x0 + 60.0, bob.center().y)),
            CollaboratorsHit::Absorb,
            "a row without a summary has nothing to expand"
        );
        assert_eq!(overlay.details_target(0), Some("ada"));
        let order = overlay.focus_order();
        let details = order
            .iter()
            .position(|hit| *hit == CollaboratorsHit::ToggleDetails(0));
        let follow = order
            .iter()
            .position(|hit| *hit == CollaboratorsHit::ToggleFollow(0));
        assert!(
            details.is_some() && details < follow,
            "keyboard reaches the row before Follow"
        );
        let collapsed = overlay.accessibility_snapshot();
        assert!(!person(&collapsed, 0).contains("Recently"));
        assert!(
            collapsed
                .description
                .as_deref()
                .is_some_and(|text| text.contains("2 people watching")),
            "spectators are announced with the room"
        );

        paint(&mut overlay, &people, Some("ada"), true);
        let grown = overlay.row_rects[0];
        assert!(grown.height() > ada.height(), "the expanded row grows");
        assert!(
            (overlay.row_rects[1].y0 - bob.y0 - (grown.height() - ada.height())).abs()
                < f64::EPSILON,
            "the next row moves down by exactly the summary"
        );
        assert!(
            !overlay.expanded_lines.is_empty() && overlay.expanded_lines.len() <= RECENT_MAX_LINES
        );
        let expanded = overlay.accessibility_snapshot();
        assert!(person(&expanded, 0).contains(
            "Recently, summarized: Rewrote the intro and resolved two comments in Notes"
        ));
        assert!(
            expanded
                .children
                .iter()
                .any(|node| node.state.expanded == Some(true)),
            "the disclosure reports its state"
        );

        paint(&mut overlay, &people, None, false);
        assert_ne!(
            overlay.hit_test(overlay.status.center()),
            CollaboratorsHit::EditStatus,
            "a host that cannot edit shows the status read-only"
        );
        assert!(
            !overlay
                .focus_order()
                .contains(&CollaboratorsHit::EditStatus)
        );
        assert!(
            overlay
                .focus_order()
                .contains(&CollaboratorsHit::ChangeSharing),
            "the sharing choice stays"
        );
    }

    #[test]
    fn roster_follow_focus_tracks_a_person_across_painted_reordering() {
        let mut overlay = CollaboratorsOverlay::default();
        let mut people = roster_focus_rows();
        paint_roster_focus(&mut overlay, &people);
        assert!(overlay.focus_accessibility_path(&[3]));
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
        assert!(overlay.focus_accessibility_path(&[3]));
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
        assert!(overlay.focus_accessibility_path(&[3]));
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
    fn roster_follow_focus_survives_resize_and_arrivals() {
        let mut overlay = CollaboratorsOverlay::default();
        let mut people = roster_focus_rows();
        paint_roster_focus(&mut overlay, &people);
        assert!(overlay.focus_accessibility_path(&[3]));
        let viewport = Rect::new(0.0, 0.0, 640.0, 480.0);
        paint_roster_focus_in(&mut overlay, &people, viewport);
        assert_eq!(
            overlay.key(&Key::Named(NamedKey::Enter), false),
            Some(CollaboratorsHit::ToggleFollow(0))
        );
        let focus = overlay
            .accessibility_rect_for_path(&[3])
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
        paint_roster_focus_in(&mut overlay, &people, viewport);
        assert_eq!(
            overlay.key(&Key::Named(NamedKey::Enter), false),
            Some(CollaboratorsHit::ToggleFollow(0)),
            "people arriving keep the focus where it was"
        );
    }
}
