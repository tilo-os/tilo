//! The roster: who is in this workspace right now and what they are doing.
//!
//! One job. People, then agents, each a row with their face (in their
//! colour, dimmed while idle, ringed while in a call), their name, where
//! they are (the app or document) and what they are doing, and Follow.
//! A row opens in place to what they changed recently, with the way into
//! History. "Invite people" is at the bottom, because adding people belongs
//! to who is here. Your own name, status and what people see of you are on
//! your avatar's menu, not here.
//!
//! Built on the primitives: the floating panel ([`ui::widget::FloatingPanelFace`]),
//! list rows ([`ui::widget::ListRowFace`]) with a 40 px face and a text
//! stack, the agent tag chip, quiet buttons and the disclose motion. The
//! host supplies a policy-filtered model and applies the semantic action
//! returned by [`CollaboratorsOverlay::handle_event`].

use std::collections::HashMap;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use ui::icons::Icon;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{ButtonFace, ButtonRole, ChipStyle, FloatingPanelFace, ListRowFace, RowLeading};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use tilo_desktop_model::{Collaborator, DesktopTree};
use ui::TextRole;

use crate::i18n::{t, t_args};

/// Rows the roster shows before its list scrolls.
const VISIBLE_ROWS: usize = 8;
/// A face in a row.
const FACE: f64 = 40.0;
/// Room the trailing controls take: Wave (an icon button) and Follow.
const TRAILING_GAP: f64 = 4.0;

/// What a pointer press or a key on the open roster means to the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollaboratorsHit {
    Invite,
    /// Follow the person at this row index, or stop following them.
    ToggleFollow(usize),
    /// Wave at the person at this row index.
    Wave(usize),
    /// Join the voice channel the person at this row index is in.
    JoinCall(usize),
    /// Open or close what the person at this row index changed recently.
    ToggleDetails(usize),
    /// Open History at one change (row index, change index) of theirs.
    OpenChange(usize, usize),
    /// Open History at everything the person at this row index changed.
    OpenChanges(usize),
    /// Inside the panel, but not on an action.
    Absorb,
    /// Outside the panel; the host closes it.
    Dismiss,
}

/// One row of the roster. People and agents are listed apart, so an agent
/// is never reported as a person.
#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts about one participant (software, a bot, invited, followable), each read on its own"
)]
pub struct RosterRow {
    pub id: String,
    pub label: String,
    pub initial: char,
    pub color: Color,
    /// Software, not a person: an agent or a bot.
    pub is_agent: bool,
    /// One of the room's bots (then also [`Self::is_agent`]): tagged Bot,
    /// never Agent.
    pub is_bot: bool,
    pub role: Option<String>,
    pub invited: bool,
    /// Where they are: the app or document in front of them.
    pub activity: Option<String>,
    /// Short participant intention, marked by its [`LineSource`] so
    /// model-suggested words cannot be mistaken for a direct quote.
    pub status: Option<RosterStatus>,
    /// What they changed recently in this workspace, newest first. Shown
    /// only while the row is open; a row without any does not open.
    pub recent: Vec<RecentChange>,
    pub followable: bool,
    /// How available they say they are (idle and away come on their own).
    pub availability: ui::Availability,
    /// The voice channel they are in, if any.
    pub call: Option<String>,
    /// The voice channel of theirs this device may join from their row:
    /// one they are in and this person is not.
    pub join: Option<VoiceChannel>,
}

/// One voice channel's call, by the app, record and channel that hold it:
/// what joining it asks of the app that owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceChannel {
    pub app: String,
    pub record: String,
    pub channel: String,
}

/// One recent change of someone's, as History records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentChange {
    /// History's own summary of it ("Edited Welcome").
    pub text: String,
    /// How long ago, already worded ("4 min ago").
    pub when: String,
    /// The History entry it opens at; `None` opens their changes.
    pub entry: Option<[u8; 16]>,
}

/// Changes an open row lists before "See their changes" takes over.
pub const RECENT_CHANGES_SHOWN: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterStatus {
    pub text: String,
    pub source: LineSource,
}

/// Who wrote a line shown about someone.
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

/// What the roster shows this frame, decided by the host.
pub struct CollaboratorsModel<'a> {
    pub host: Rect,
    /// The workspace's name, the panel's title.
    pub workspace: &'a str,
    pub rows: &'a [RosterRow],
    /// Why nobody can be shown (the room is unreachable), instead of rows.
    pub unavailable: Option<&'a str>,
    /// Who can see or control which of your windows right now. Never hidden
    /// while it is true: it leads the list.
    pub exposure: Option<&'a str>,
    pub may_invite: bool,
    pub followed_id: Option<&'a str>,
    /// Ephemeral room-local attachments, expressed using roster ids.
    pub following_pairs: &'a [(String, String)],
    /// The roster id whose recent changes are open. Ignored when that row
    /// has nothing recent.
    pub expanded_id: Option<&'a str>,
    /// People watching a public workspace without joining it.
    pub watching: usize,
}

/// Direction of a transient attachment relative to one roster row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RosterFollowRelationship {
    Follows(String),
    FollowedBy(String),
}

impl CollaboratorsModel<'_> {
    /// The rows of who is here, not merely invited, as people and agents.
    fn present_groups(&self) -> (Vec<usize>, Vec<usize>) {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| !row.invited)
            .map(|(index, row)| (index, row.is_agent))
            .fold(
                (Vec::new(), Vec::new()),
                |(mut people, mut agents), (index, is_agent)| {
                    if is_agent {
                        agents.push(index);
                    } else {
                        people.push(index);
                    }
                    (people, agents)
                },
            )
    }

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

/// One painted row: which model row, where, and its controls.
#[derive(Clone, Debug, Default)]
struct RowPlace {
    index: usize,
    /// The row itself (its 64 px header).
    rect: Rect,
    follow: Rect,
    wave: Rect,
    join: Rect,
    /// The open part under the row, its changes and "See their changes".
    details: Rect,
    change_rows: Vec<Rect>,
    changes: Rect,
    /// The open part is fully open: its controls take presses and focus.
    settled: bool,
}

/// A section heading ("People", "Agents") where it was painted.
#[derive(Clone, Debug)]
struct Heading {
    text: String,
    rect: Rect,
}

/// The open roster: geometry and interaction state across frames.
#[derive(Default)]
pub struct CollaboratorsOverlay {
    host: Rect,
    panel: Rect,
    viewport: Rect,
    header: Rect,
    body: Rect,
    /// The body's content height, all rows laid out.
    body_height: f64,
    scroll: Option<ui::widget::ScrollRegion>,
    scroll_theme: Option<Theme>,
    rows: Vec<RowPlace>,
    headings: Vec<Heading>,
    /// The screen-exposure line, in content coordinates.
    exposure: Rect,
    shown_exposure: Option<String>,
    invite: Rect,
    /// The trailing slot every row reserves for Wave and Follow, and Join
    /// where anyone has a call to join.
    trailing_w: f64,
    /// What Join adds to [`Self::trailing_w`]: none when nobody has a call.
    join_extra: f64,
    /// The row whose recent changes are open, and how open (the disclose
    /// motion).
    opened: Option<String>,
    reveal: ui::motion::Presence,
    /// The open row's laid-out height when fully open.
    details_height: f64,
    keyboard_focus: Option<CollaboratorsHit>,
    targets: ui::widget::PaintedTargets,
    painted_actions: Vec<(String, CollaboratorsHit)>,
    held_submit: [bool; 2],
    /// What the last frame showed, for assistive technology and the host's
    /// revalidation.
    shown_rows: Vec<RosterRow>,
    shown_workspace: String,
    shown_expanded: Option<String>,
    shown_followed: Option<String>,
    motion: ui::tokens::EffectiveMotion,
    /// When someone here was first seen in a call, so their ring breathes
    /// from its start.
    call_since: Option<ui::time::Instant>,
}

impl std::fmt::Debug for CollaboratorsOverlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CollaboratorsOverlay")
            .field("panel", &self.panel)
            .field("rows", &self.rows.len())
            .finish_non_exhaustive()
    }
}

/// The line under a person's name saying what they are doing: a call they
/// are in, else what they wrote or are doing, else how available they are.
fn doing_line(row: &RosterRow) -> Option<String> {
    if let Some(call) = &row.call {
        return Some(if call.trim().is_empty() {
            t("people-panel-in-a-call")
        } else {
            t_args("people-panel-in-call", &[("channel", call.clone().into())])
        });
    }
    if let Some(status) = &row.status {
        return Some(status.text.clone());
    }
    match row.availability {
        ui::Availability::Available => None,
        ui::Availability::Focusing => Some(t("people-panel-focusing")),
        ui::Availability::Away => Some(t("people-panel-away")),
        ui::Availability::Idle => Some(t("people-panel-idle")),
    }
}

impl CollaboratorsOverlay {
    /// Paint the roster for `model` and lay out its targets.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &CollaboratorsModel<'_>,
        cursor: Option<Point>,
    ) {
        self.motion = theme.motion();
        // Keyboard focus stays with the person and control it was on, not
        // the row index, when people come, go or reorder.
        let focused = self.keyboard_focus.and_then(|hit| self.target_id(hit));
        self.follow_expansion(model);
        self.host = model.host;
        self.shown_rows = model.rows.to_vec();
        model.workspace.clone_into(&mut self.shown_workspace);
        self.shown_followed = model.followed_id.map(str::to_owned);
        self.shown_exposure = model.exposure.map(str::to_owned);
        self.layout(text, theme, model);
        self.keyboard_focus = focused.and_then(|id| {
            self.focus_order()
                .into_iter()
                .find(|hit| self.target_id(*hit).as_deref() == Some(id.as_str()))
        });
        self.configure_scroll(theme);
        self.paint(scene, transform, text, theme, model, cursor);
        self.register_targets(model.host);
    }

    /// Open the row the host says is open, on the disclose motion.
    fn follow_expansion(&mut self, model: &CollaboratorsModel<'_>) {
        let wanted = model
            .expanded_id
            .filter(|id| {
                model
                    .rows
                    .iter()
                    .any(|row| row.id == *id && !row.recent.is_empty())
            })
            .map(str::to_owned);
        if wanted != self.shown_expanded {
            let now = ui::time::now();
            if wanted.is_some() {
                self.opened.clone_from(&wanted);
                self.reveal = ui::motion::Presence::closed();
            }
            self.reveal.show(
                wanted.is_some(),
                ui::motion::Disclose::SPEC,
                self.motion,
                ui::motion::input_source(),
                now,
            );
            if let Some(until) = self.reveal.settles_at() {
                ui::motion::want_frames_until(until);
            }
            self.shown_expanded = wanted;
        }
        if self.shown_expanded.is_none() && !self.reveal.is_visible(ui::motion::frame_now()) {
            self.opened = None;
        }
    }

    /// Where every part goes this frame.
    fn layout(&mut self, text: &mut TextRenderer, theme: &Theme, model: &CollaboratorsModel<'_>) {
        let metrics = &theme.tokens.metrics;
        let spacing = &theme.tokens.spacing;
        let row_h = f64::from(metrics.row_avatar);
        let heading_h = f64::from(metrics.row_single);
        let width = f64::from(metrics.panel_width_list)
            .min((model.host.width() - 2.0 * f64::from(spacing.s4)).max(240.0));
        let header_h = ui::widget::TextStackFace::new(model.workspace)
            .secondary("0")
            .height(text, theme);
        let control = f64::from(metrics.bar_control_height);
        let (people, agents) = model.present_groups();
        let face = FloatingPanelFace::new()
            .header(header_h)
            .footer(if model.may_invite { control } else { 0.0 })
            .body_empty(false);
        // The open row's details: a line per change, then the link.
        let open_index = self
            .opened
            .as_deref()
            .and_then(|id| model.rows.iter().position(|row| row.id == id));
        let shown_changes = open_index
            .and_then(|index| model.rows.get(index))
            .map_or(0, |row| row.recent.len().min(RECENT_CHANGES_SHOWN));
        self.details_height = if shown_changes == 0 {
            0.0
        } else {
            heading_h * ui::num::f64_of_usize(shown_changes) + control + f64::from(spacing.s2)
        };
        let revealed = ui::motion::Disclose::reveal(
            self.reveal.progress(ui::motion::frame_now()),
            self.details_height,
        )
        .height;
        let sections = [&people, &agents]
            .into_iter()
            .filter(|group| !group.is_empty())
            .count();
        let mut body_h = if sections == 0 {
            row_h
        } else {
            [&people, &agents]
                .into_iter()
                .filter(|group| !group.is_empty())
                .map(|group| heading_h + row_h * ui::num::f64_of_usize(group.len()))
                .sum::<f64>()
                + f64::from(spacing.s2) * ui::num::f64_of_usize(sections.saturating_sub(1))
        };
        let exposure_h = model.exposure.map_or(0.0, |exposure| {
            ListRowFace::new(exposure)
                .two_line_title()
                .mark(Icon::ScreenShare)
                .height(text, theme)
        });
        if exposure_h > 0.0 && sections == 0 {
            body_h = 0.0;
        }
        let open_extra = if open_index.is_some() { revealed } else { 0.0 };
        self.body_height = exposure_h + body_h + open_extra;
        let anchor_top =
            model.host.y0 + f64::from(crate::topbar::topbar_height(theme)) + f64::from(spacing.s2);
        // Never taller than the output under the bar: the list scrolls.
        let chrome_h = face.height_for(1.0, theme) - 1.0;
        let room = (model.host.y1 - f64::from(spacing.s3) - anchor_top - chrome_h).max(row_h);
        let visible_body = (heading_h * 2.0 + row_h * ui::num::f64_of_usize(VISIBLE_ROWS))
            .min(self.body_height)
            .min(room);
        let panel_h = face.height_for(visible_body, theme);
        let right = model.host.x1 - f64::from(spacing.s3);
        self.panel = Rect::new(right - width, anchor_top, right, anchor_top + panel_h);
        let parts = face.layout(self.panel, theme);
        self.header = parts.header;
        self.body = parts.body;
        self.viewport = self.panel;
        self.invite = if model.may_invite {
            parts.footer
        } else {
            Rect::ZERO
        };

        self.place_rows(
            text,
            theme,
            model,
            &RowsPlan {
                people: &people,
                agents: &agents,
                open_index,
                shown_changes,
                revealed,
                exposure_h,
            },
        );
    }

    /// The rows, in content coordinates (before scrolling): the exposure
    /// notice, then each group's heading and rows, the open row's details
    /// under it.
    fn place_rows(
        &mut self,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &CollaboratorsModel<'_>,
        plan: &RowsPlan<'_>,
    ) {
        let metrics = &theme.tokens.metrics;
        let spacing = &theme.tokens.spacing;
        let row_h = f64::from(metrics.row_avatar);
        let heading_h = f64::from(metrics.row_single);
        let control = f64::from(metrics.bar_control_height);
        let RowsPlan {
            people,
            agents,
            open_index,
            shown_changes,
            revealed,
            exposure_h,
        } = *plan;
        self.rows.clear();
        self.headings.clear();
        let bleed = ui::chrome::row_bleed(theme);
        // The exposure notice, when there is one, heads the list.
        self.exposure = if exposure_h > 0.0 {
            Rect::new(
                self.body.x0 - bleed,
                self.body.y0,
                self.body.x1 + bleed,
                self.body.y0 + exposure_h,
            )
        } else {
            Rect::ZERO
        };
        let mut y = self.body.y0 + exposure_h.max(0.0);
        let widths = ButtonWidths::measure(text, theme);
        // Join (where anyone has a call to join), Wave and Follow sit in the
        // row's trailing slot, inside its disclosure chevron when it has one.
        let any_join = model.rows.iter().any(|row| row.join.is_some());
        self.trailing_w = widths.trailing(any_join, control);
        self.join_extra = if any_join {
            TRAILING_GAP + widths.join
        } else {
            0.0
        };
        // Follow stands in one column down the list: where any row has a
        // chevron, every row keeps its place free.
        let any_disclosure = model.rows.iter().any(|row| !row.recent.is_empty());
        for (key, group) in [
            ("people-panel-section-people", people),
            ("people-panel-section-agents", agents),
        ] {
            if group.is_empty() {
                continue;
            }
            if !self.headings.is_empty() {
                y += f64::from(spacing.s2);
            }
            self.headings.push(Heading {
                text: t(key),
                rect: Rect::new(self.body.x0, y, self.body.x1, y + heading_h),
            });
            y += heading_h;
            for &index in group {
                let rect = Rect::new(self.body.x0 - bleed, y, self.body.x1 + bleed, y + row_h);
                let Some(row) = model.rows.get(index) else {
                    continue;
                };
                let (mut place, follow_right) =
                    self.row_place(text, theme, (index, rect), row, any_disclosure, widths);
                // The open part's lines end where Follow ends.
                let line_right = follow_right + bleed;
                y += row_h;
                if open_index == Some(index) {
                    place.details = Rect::new(self.body.x0, y, self.body.x1, y + revealed);
                    // Under the name: each change a line of its own, then
                    // the way to everything they changed, its label on the
                    // same column. Laid out from the start so they fade in
                    // with the opening; they take presses once it is open.
                    let column = self.body.x0 + FACE + f64::from(spacing.s3);
                    place.settled = revealed >= self.details_height - 0.5;
                    place.change_rows = (0..shown_changes)
                        .map(|k| {
                            let top = y + heading_h * ui::num::f64_of_usize(k);
                            Rect::new(column - bleed, top, line_right, top + heading_h)
                        })
                        .collect();
                    let link_top = y + self.details_height - control - f64::from(spacing.s2);
                    let link_x = column - f64::from(spacing.s3);
                    place.changes = Rect::new(
                        link_x,
                        link_top,
                        link_x + widths.changes,
                        link_top + control,
                    );
                    y += revealed;
                }
                self.rows.push(place);
            }
        }
    }

    /// The row `index` at `rect` with its trailing buttons: Join where the
    /// person has a call to join, Follow where they can be followed, Wave
    /// for a person (never an agent). Also where Follow's slot ends, whether
    /// or not it shows.
    fn row_place(
        &self,
        text: &mut TextRenderer,
        theme: &Theme,
        (index, rect): (usize, Rect),
        row: &RosterRow,
        any_disclosure: bool,
        widths: ButtonWidths,
    ) -> (RowPlace, f64) {
        let (follow, wave) = self.trailing_slots(
            text,
            theme,
            rect,
            (&row.label, any_disclosure),
            widths.follow,
        );
        // Join stands beside Follow, Wave (shown on hover) before it.
        let control = f64::from(theme.tokens.metrics.bar_control_height);
        let (join, wave) = if row.join.is_some() {
            let join = Rect::new(
                follow.x0 - TRAILING_GAP - widths.join,
                follow.y0,
                follow.x0 - TRAILING_GAP,
                follow.y1,
            );
            let wave = Rect::new(
                join.x0 - TRAILING_GAP - control,
                join.y0,
                join.x0 - TRAILING_GAP,
                join.y1,
            );
            (join, wave)
        } else {
            (Rect::ZERO, wave)
        };
        let place = RowPlace {
            index,
            rect,
            follow: if row.followable { follow } else { Rect::ZERO },
            wave: if row.is_agent { Rect::ZERO } else { wave },
            join,
            ..RowPlace::default()
        };
        (place, follow.x1)
    }

    /// Follow and Wave in the trailing slot of the row labelled `label` at
    /// `rect`, inside the disclosure chevron's place when the list keeps
    /// one (`disclosure`).
    fn trailing_slots(
        &self,
        text: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        (label, disclosure): (&str, bool),
        follow_w: f64,
    ) -> (Rect, Rect) {
        let control = f64::from(theme.tokens.metrics.bar_control_height);
        let mut probe = ListRowFace::new(label).trailing_width(self.trailing_w);
        if disclosure {
            probe = probe.disclosure();
        }
        let right = probe
            .trailing_rect(rect, text, theme)
            .map_or_else(|| rect.x1 - ui::chrome::row_bleed(theme), |slot| slot.x1);
        let follow = Rect::new(
            right - follow_w,
            rect.center().y - control / 2.0,
            right,
            rect.center().y + control / 2.0,
        );
        let wave = Rect::new(
            follow.x0 - TRAILING_GAP - control,
            follow.y0,
            follow.x0 - TRAILING_GAP,
            follow.y1,
        );
        (follow, wave)
    }

    fn configure_scroll(&mut self, theme: &Theme) {
        let visible = self.body.height();
        // The bar runs the panel's whole edge, header to footer, and the
        // panel's fixed parts count as content that never scrolls away, so
        // the list's own range is unchanged.
        let fixed = self.panel.height() - visible;
        let content = self.body_height + fixed;
        let scroll = self
            .scroll
            .get_or_insert_with(ui::widget::ScrollRegion::vertical);
        scroll.set_viewport(
            Rect::new(self.body.x0, self.panel.y0, self.panel.x1, self.panel.y1),
            theme,
        );
        scroll.set_content_size(self.body.width(), content);
        self.scroll_theme = (self.body_height > visible + 0.5).then(|| theme.clone());
    }

    fn scroll_offset(&self) -> f64 {
        self.scroll.as_ref().map_or(0.0, |scroll| scroll.offset().y)
    }

    /// A content rect as it is on screen, the body scrolled.
    fn view_rect(&self, rect: Rect) -> Rect {
        if rect.area() <= 0.0 {
            return rect;
        }
        Affine::translate((0.0, -self.scroll_offset()))
            .transform_rect_bbox(rect)
            .intersect(self.body)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one paint pass keeps the panel, its rows and their controls in step"
    )]
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &CollaboratorsModel<'_>,
        cursor: Option<Point>,
    ) {
        let face = FloatingPanelFace::new()
            .header(self.header.height())
            .footer(if model.may_invite {
                self.invite.height()
            } else {
                0.0
            })
            .body_empty(false);
        let (_, inner) = face.paint(scene, transform, self.panel, 1.0, theme);
        let theme = &inner;
        let colors = &theme.tokens.colors;
        let here = model
            .rows
            .iter()
            .filter(|row| !row.invited)
            .count()
            .saturating_add(1);
        let here_text = if model.watching > 0 {
            t_args(
                "people-panel-here-watching",
                &[("count", here.into()), ("watching", model.watching.into())],
            )
        } else {
            t_args("people-panel-here-count", &[("count", here.into())])
        };
        ui::widget::TextStackFace::new(model.workspace)
            .secondary(&here_text)
            .paint(
                &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
                self.header,
            );

        // The body scrolls under the header and above the footer.
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.body.inflate(ui::chrome::row_bleed(theme), 0.0),
        );
        let scrolled = transform.pre_translate((0.0, -self.scroll_offset()).into());
        let cursor = cursor.map(|p| Point::new(p.x, p.y + self.scroll_offset()));
        if let Some(exposure) = model.exposure.filter(|_| self.exposure.area() > 0.0) {
            ListRowFace::new(exposure)
                .two_line_title()
                .mark(Icon::ScreenShare)
                .paint(
                    &mut ui::paint::PaintCtx::new(scene, scrolled, text, theme),
                    self.exposure,
                );
        }
        if self.rows.is_empty() && self.exposure.area() <= 0.0 {
            let empty = model
                .unavailable
                .map_or_else(|| t("people-panel-empty"), str::to_owned);
            let style = theme.text(TextRole::Body);
            let (w, h) = text.measure_style(&empty, None, &style);
            text.draw_style(
                scene,
                &empty,
                colors.text_dim,
                transform.pre_translate(
                    (
                        self.body.center().x - f64::from(w) / 2.0,
                        self.body.center().y - f64::from(h) / 2.0,
                    )
                        .into(),
                ),
                None,
                &style,
            );
        }
        for heading in &self.headings {
            let style = theme
                .text(TextRole::Caption)
                .with_weight(ui::text_render::TextWeight::Medium);
            let (_, h) = text.measure_style(&heading.text, None, &style);
            text.draw_style(
                scene,
                &heading.text,
                colors.text_dim,
                scrolled.pre_translate(
                    (
                        heading.rect.x0,
                        heading.rect.center().y - f64::from(h) / 2.0,
                    )
                        .into(),
                ),
                None,
                &style,
            );
        }
        let agent_tag = t("people-panel-agent-tag");
        let bot_tag = t("people-panel-bot-tag");
        let follow_label = t("people-panel-follow");
        let stop_label = t("people-panel-stop-following");
        let wave_label = t("people-panel-wave");
        let join_label = t("people-panel-join");
        let changes_label = t("people-panel-changes");
        let now = ui::time::now();
        let in_call = model.rows.iter().any(|row| row.call.is_some());
        let since = *self.call_since.get_or_insert(now);
        if !in_call {
            self.call_since = None;
        }
        let breath =
            ui::motion::Breathe::level(now.saturating_duration_since(since), theme.motion());
        for place in self.rows.clone() {
            let Some(row) = model.rows.get(place.index) else {
                continue;
            };
            let lines = (row.activity.clone().unwrap_or_default(), doing_line(row));
            let hovered = cursor.is_some_and(|p| place.rect.contains(p));
            let interaction = ui::widget::Interaction {
                hovered,
                pressed: false,
                focused: self.keyboard_focus == Some(CollaboratorsHit::ToggleDetails(place.index)),
                disabled: false,
            };
            let paint_face = |ctx: &mut ui::paint::PaintCtx<'_>, rect: Rect| {
                let mut face = ui::presence::Face::new(&row.label, &row.id, row.color);
                face.is_agent = row.is_agent;
                face.dimmed = row.availability.dims();
                face.in_call = row.call.as_ref().map(|_| breath);
                ui::presence::FaceStack {
                    side: FACE - 6.0,
                    overlap: 0.0,
                    max: 1,
                }
                .paint(
                    ctx,
                    (rect.center().x + FACE / 2.0, rect.center().y),
                    (&[face], 0),
                    ui::widget::SurfaceLevel::Floating.color(ctx.theme),
                );
            };
            // Software is tagged after its name: Bot for the room's bots,
            // Agent for agents.
            let tags = [(
                if row.is_bot {
                    bot_tag.as_str()
                } else {
                    agent_tag.as_str()
                },
                ChipStyle::Neutral,
            )];
            let mut face = ListRowFace::new(&row.label);
            if !lines.0.is_empty() {
                face = face.subtitle(&lines.0);
            }
            if let Some(doing) = &lines.1 {
                face = face.secondary(doing);
            }
            if row.is_agent {
                face = face.tags(&tags);
            }
            let mut face = face
                .leading(RowLeading::Paint(&paint_face))
                .leading_column(FACE)
                // Only a row with Join gives its name the less room.
                .trailing_width(if place.join.area() > 0.0 {
                    self.trailing_w
                } else {
                    self.trailing_w - self.join_extra
                })
                .interaction(interaction);
            if !row.recent.is_empty() {
                let turn = if self.opened.as_deref() == Some(row.id.as_str()) {
                    ui::motion::Disclose::reveal(self.reveal.progress(now), 1.0).turn
                } else {
                    0.0
                };
                face = face.disclosure_turned(turn);
            }
            face.paint(
                &mut ui::paint::PaintCtx::new(scene, scrolled, text, theme),
                place.rect,
            );
            if place.wave.area() > 0.0
                && (hovered || self.keyboard_focus == Some(CollaboratorsHit::Wave(place.index)))
            {
                ui::widget::Button::paint(
                    scene,
                    scrolled,
                    text,
                    theme,
                    &ButtonFace::icon_only(place.wave, Icon::Hand, &wave_label)
                        .role(ButtonRole::Plain)
                        .hovered(cursor.is_some_and(|p| place.wave.contains(p)))
                        .focused(self.keyboard_focus == Some(CollaboratorsHit::Wave(place.index))),
                );
            }
            if place.join.area() > 0.0 {
                ui::widget::Button::paint(
                    scene,
                    scrolled,
                    text,
                    theme,
                    &ButtonFace::new(place.join, &join_label)
                        .role(ButtonRole::Plain)
                        .hovered(cursor.is_some_and(|p| place.join.contains(p)))
                        .focused(
                            self.keyboard_focus == Some(CollaboratorsHit::JoinCall(place.index)),
                        ),
                );
            }
            if place.follow.area() > 0.0 {
                let following = model.followed_id == Some(row.id.as_str());
                ui::widget::Button::paint(
                    scene,
                    scrolled,
                    text,
                    theme,
                    &ButtonFace::new(
                        place.follow,
                        if following {
                            &stop_label
                        } else {
                            &follow_label
                        },
                    )
                    .role(ButtonRole::Plain)
                    .hovered(cursor.is_some_and(|p| place.follow.contains(p)))
                    .focused(
                        self.keyboard_focus == Some(CollaboratorsHit::ToggleFollow(place.index)),
                    ),
                );
            }
            if place.details.area() > 0.0 && !row.recent.is_empty() {
                let opacity =
                    ui::motion::Disclose::reveal(self.reveal.progress(now), self.details_height)
                        .opacity;
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    opacity,
                    scrolled,
                    &place.details,
                );
                for (k, (rect, change)) in place.change_rows.iter().zip(&row.recent).enumerate() {
                    let hit = CollaboratorsHit::OpenChange(place.index, k);
                    let mut line = ListRowFace::new(&change.text);
                    if !change.when.is_empty() {
                        line = line.meta(&change.when);
                    }
                    line.interaction(ui::widget::Interaction {
                        hovered: cursor.is_some_and(|p| rect.contains(p)),
                        pressed: false,
                        focused: self.keyboard_focus == Some(hit),
                        disabled: false,
                    })
                    .paint(
                        &mut ui::paint::PaintCtx::new(scene, scrolled, text, theme),
                        *rect,
                    );
                }
                if place.changes.area() > 0.0 {
                    ui::widget::Button::paint(
                        scene,
                        scrolled,
                        text,
                        theme,
                        &ButtonFace::new(place.changes, &changes_label)
                            .role(ButtonRole::Plain)
                            .hovered(cursor.is_some_and(|p| place.changes.contains(p)))
                            .focused(
                                self.keyboard_focus
                                    == Some(CollaboratorsHit::OpenChanges(place.index)),
                            ),
                    );
                }
                scene.pop_layer();
            }
        }
        scene.pop_layer();
        if let (Some(scroll), Some(scroll_theme)) =
            (self.scroll.as_ref(), self.scroll_theme.as_ref())
        {
            scroll.paint_bars(scene, transform, scroll_theme);
        }
        if self.invite.area() > 0.0 {
            let label = t("people-panel-invite-people");
            ui::widget::Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::new(self.invite, &label)
                    .icon(Icon::UserPlus)
                    .role(ButtonRole::Secondary)
                    .hovered(cursor.is_some_and(|p| self.invite.contains(p)))
                    .focused(self.keyboard_focus == Some(CollaboratorsHit::Invite)),
            );
        }
    }

    pub fn primary_vertical_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        self.scroll_theme.as_ref()?;
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
            self.register_targets(self.host);
        }
        result
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn target_id(&self, hit: CollaboratorsHit) -> Option<String> {
        let id_of = |index: usize| self.shown_rows.get(index).map(|row| row.id.clone());
        match hit {
            CollaboratorsHit::ToggleFollow(index) => id_of(index).map(|id| format!("follow:{id}")),
            CollaboratorsHit::Wave(index) => id_of(index).map(|id| format!("wave:{id}")),
            CollaboratorsHit::JoinCall(index) => id_of(index).map(|id| format!("join:{id}")),
            CollaboratorsHit::ToggleDetails(index) => {
                id_of(index).map(|id| format!("details:{id}"))
            }
            CollaboratorsHit::OpenChanges(index) => id_of(index).map(|id| format!("changes:{id}")),
            CollaboratorsHit::OpenChange(index, k) => {
                id_of(index).map(|id| format!("change:{id}:{k}"))
            }
            other => Some(format!("{other:?}")),
        }
    }

    fn register_targets(&mut self, host: Rect) {
        self.targets.begin_frame();
        self.painted_actions.clear();
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
                let rect = if hit == CollaboratorsHit::Invite {
                    rect
                } else {
                    self.view_rect(rect)
                };
                if rect.area() > 0.0 {
                    self.targets.target(&id, rect, "");
                    self.painted_actions.push((id, hit));
                }
            }
        }
        self.targets.end_frame();
    }

    pub fn interrupt(&mut self) {
        self.targets.cancel_pointer_press();
        self.targets.set_host_focus(false);
        self.keyboard_focus = None;
    }

    pub fn needs_animation_frame(&self) -> bool {
        let now = ui::time::now();
        self.reveal.is_moving(now)
            || self.scroll_theme.as_ref().is_some_and(|theme| {
                self.scroll
                    .as_ref()
                    .is_some_and(|scroll| scroll.needs_animation_frame(theme))
            })
    }

    pub fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let scrollbar = self.scroll_theme.as_ref().and_then(|theme| {
            self.scroll
                .as_ref()
                .and_then(|scroll| scroll.next_wake_deadline(theme))
        });
        // Someone here is in a call: their ring breathes, gently.
        let breath = self
            .shown_rows
            .iter()
            .any(|row| row.call.is_some())
            .then(|| ui::motion::Breathe::next_frame(ui::time::now(), self.motion))
            .flatten();
        scrollbar.into_iter().chain(breath).min()
    }

    pub fn wants_pointer_capture(&self) -> bool {
        self.targets.wants_pointer_capture()
    }

    /// Release/repeat edges still belong to the roster after its action
    /// changes the active surface. They must be consumed before ordinary
    /// modal routing.
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

    fn scroll_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool {
        let Some(theme) = self.scroll_theme.clone() else {
            return false;
        };
        let Some(scroll) = self.scroll.as_mut() else {
            return false;
        };
        let previous = scroll.offset();
        let changed = scroll.handle_event(event, ctx, &theme);
        if previous != scroll.offset() {
            self.targets.cancel_pointer_press();
        }
        changed && *ctx.handled
    }

    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        open: bool,
        cursor: Option<Point>,
    ) -> Option<CollaboratorsHit> {
        if !open {
            self.interrupt();
            if self.targets.wants_pointer_capture() {
                self.targets.handle_event(event, ctx);
                if matches!(
                    event,
                    WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
                ) {
                    *ctx.handled = true;
                }
            }
            return None;
        }
        if matches!(
            event,
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. }
        ) && let Some(point) = cursor
        {
            let moved = ui::synthetic_input::cursor_moved(
                point.x * ctx.scale_factor,
                point.y * ctx.scale_factor,
            );
            self.targets.handle_event(&moved, ctx);
            self.scroll_event(&moved, ctx);
        }
        if self.scroll_event(event, ctx) {
            self.register_targets(self.host);
            return None;
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = event {
            return self.handle_submit(event, key, ctx);
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
            Key::Named(NamedKey::Enter) => 0,
            Key::Named(NamedKey::Space) => 1,
            _ => return None,
        };
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
        self.panel
    }

    /// The Invite button, where it is painted.
    pub fn invite_rect(&self) -> Rect {
        self.invite
    }

    pub fn contains(&self, point: Point) -> bool {
        self.panel.area() > 0.0 && self.panel.contains(point)
    }

    /// The stable person behind a painted Follow button. The host must
    /// revalidate that person against its live roster before applying it.
    pub fn follow_target(&self, index: usize) -> Option<&str> {
        self.shown_rows
            .get(index)
            .filter(|row| row.followable)
            .map(|row| row.id.as_str())
    }

    /// The stable person behind a painted Wave button.
    pub fn wave_target(&self, index: usize) -> Option<&str> {
        self.shown_rows
            .get(index)
            .filter(|row| !row.is_agent && !row.invited)
            .map(|row| row.id.as_str())
    }

    /// The voice channel behind a painted Join button: the one the person
    /// at this row index is in. The host revalidates it against the room's
    /// calls before joining.
    pub fn join_target(&self, index: usize) -> Option<&VoiceChannel> {
        self.shown_rows.get(index).and_then(|row| row.join.as_ref())
    }

    /// The stable person behind an openable row. The host toggles its
    /// expanded id with this, never with the row index.
    pub fn details_target(&self, index: usize) -> Option<&str> {
        self.shown_rows
            .get(index)
            .filter(|row| !row.recent.is_empty())
            .map(|row| row.id.as_str())
    }

    /// The person and History entry behind a painted change line.
    pub fn change_target(&self, index: usize, change: usize) -> Option<(&str, Option<[u8; 16]>)> {
        let row = self.shown_rows.get(index)?;
        let change = row.recent.get(change)?;
        Some((row.label.as_str(), change.entry))
    }

    /// The name History knows the person at this row index by, for "See
    /// their changes".
    pub fn changes_author(&self, index: usize) -> Option<&str> {
        self.shown_rows
            .get(index)
            .filter(|row| !row.recent.is_empty())
            .map(|row| row.label.as_str())
    }

    /// What a press at `point` (on screen) means.
    pub fn hit_test(&self, point: Point) -> CollaboratorsHit {
        if !self.contains(point) {
            return CollaboratorsHit::Dismiss;
        }
        if self.invite.contains(point) {
            return CollaboratorsHit::Invite;
        }
        if !self.body.contains(point) {
            return CollaboratorsHit::Absorb;
        }
        let point = Point::new(point.x, point.y + self.scroll_offset());
        for place in &self.rows {
            if place.follow.contains(point) {
                return CollaboratorsHit::ToggleFollow(place.index);
            }
            if place.wave.contains(point) {
                return CollaboratorsHit::Wave(place.index);
            }
            if place.join.contains(point) {
                return CollaboratorsHit::JoinCall(place.index);
            }
            if place.details.contains(point) {
                if place.settled && place.changes.contains(point) {
                    return CollaboratorsHit::OpenChanges(place.index);
                }
                if let Some(k) = place
                    .change_rows
                    .iter()
                    .position(|rect| place.settled && rect.contains(point))
                {
                    return CollaboratorsHit::OpenChange(place.index, k);
                }
                return CollaboratorsHit::Absorb;
            }
            if place.rect.contains(point) && self.details_target(place.index).is_some() {
                return CollaboratorsHit::ToggleDetails(place.index);
            }
        }
        CollaboratorsHit::Absorb
    }

    /// Every control in reading order: each row's body, Wave and Follow,
    /// its changes link when open, then Invite.
    fn focus_order(&self) -> Vec<CollaboratorsHit> {
        let mut order = Vec::new();
        for place in &self.rows {
            if self.details_target(place.index).is_some() {
                order.push(CollaboratorsHit::ToggleDetails(place.index));
            }
            if place.join.area() > 0.0 {
                order.push(CollaboratorsHit::JoinCall(place.index));
            }
            if place.wave.area() > 0.0 {
                order.push(CollaboratorsHit::Wave(place.index));
            }
            if place.follow.area() > 0.0 {
                order.push(CollaboratorsHit::ToggleFollow(place.index));
            }
            if !place.settled {
                continue;
            }
            order.extend(
                (0..place.change_rows.len()).map(|k| CollaboratorsHit::OpenChange(place.index, k)),
            );
            if place.changes.area() > 0.0 {
                order.push(CollaboratorsHit::OpenChanges(place.index));
            }
        }
        if self.invite.area() > 0.0 {
            order.push(CollaboratorsHit::Invite);
        }
        order
    }

    /// Where `hit` is, in content coordinates (Invite: on screen).
    fn rect_for_hit(&self, hit: CollaboratorsHit) -> Option<Rect> {
        let place = |index: usize| self.rows.iter().find(|place| place.index == index);
        match hit {
            CollaboratorsHit::Invite => Some(self.invite),
            CollaboratorsHit::ToggleFollow(index) => place(index).map(|place| place.follow),
            CollaboratorsHit::Wave(index) => place(index).map(|place| place.wave),
            CollaboratorsHit::JoinCall(index) => place(index).map(|place| place.join),
            CollaboratorsHit::ToggleDetails(index) => place(index).map(|place| place.rect),
            CollaboratorsHit::OpenChanges(index) => place(index).map(|place| place.changes),
            CollaboratorsHit::OpenChange(index, k) => {
                place(index).and_then(|place| place.change_rows.get(k).copied())
            }
            CollaboratorsHit::Absorb | CollaboratorsHit::Dismiss => None,
        }
        .filter(|rect| rect.area() > 0.0)
    }

    /// Bring the focused control into view.
    fn reveal_focus(&mut self) {
        let Some(rect) = self
            .keyboard_focus
            .filter(|hit| *hit != CollaboratorsHit::Invite)
            .and_then(|hit| self.rect_for_hit(hit))
        else {
            return;
        };
        // In the bar's terms (the whole panel): the rect, with the panel's
        // fixed parts below it, fits the panel when the rect fits the list.
        let fixed = self.panel.height() - self.body.height();
        let rect = Affine::translate((-self.body.x0, -self.body.y0))
            .transform_rect_bbox(rect.inflate(3.0, 3.0));
        let content = Rect::new(rect.x0, rect.y0, rect.x1, rect.y1 + fixed);
        if let Some(scroll) = &mut self.scroll {
            scroll.scroll_into_view(content);
        }
    }

    /// Arrows, Tab, Home and End move through the controls; Enter or Space
    /// activates the focused one; Escape closes.
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

    /// The roster for assistive technology: who is here, each with where
    /// they are and what they are doing, then every control.
    pub fn accessibility_snapshot(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};
        let present: Vec<&RosterRow> = self.shown_rows.iter().filter(|row| !row.invited).collect();
        let people = present
            .iter()
            .enumerate()
            .map(|(index, row)| {
                AccessNode::new(Role::ListItem)
                    .with_name(row.label.clone())
                    .with_description(spoken_details(row))
                    .with_position(index.saturating_add(1), present.len())
            })
            .collect();
        let mut children = vec![
            AccessNode::new(Role::List)
                .with_name(t("a11y-shell-people-here-now"))
                .with_children(people),
        ];

        for hit in self.focus_order() {
            let focused = self.keyboard_focus == Some(hit);
            let name_of = |index: usize| {
                self.shown_rows
                    .get(index)
                    .map_or_else(String::new, |row| row.label.clone())
            };
            let mut expanded = None;
            let (name, description) = match hit {
                CollaboratorsHit::ToggleFollow(index) => (
                    t_args("a11y-shell-follow", &[("name", name_of(index).into())]),
                    t("a11y-shell-follow-description"),
                ),
                CollaboratorsHit::Wave(index) => (
                    t_args("a11y-shell-wave", &[("name", name_of(index).into())]),
                    t("a11y-shell-wave-description"),
                ),
                CollaboratorsHit::JoinCall(index) => (
                    t_args("a11y-shell-join-call", &[("name", name_of(index).into())]),
                    t("a11y-shell-join-call-description"),
                ),
                CollaboratorsHit::ToggleDetails(index) => {
                    expanded = Some(self.shown_rows.get(index).is_some_and(|row| {
                        self.shown_expanded.as_deref() == Some(row.id.as_str())
                    }));
                    (
                        t_args(
                            "a11y-shell-people-recent-toggle",
                            &[("name", name_of(index).into())],
                        ),
                        t("a11y-shell-people-recent-toggle-description"),
                    )
                }
                CollaboratorsHit::OpenChange(index, k) => (
                    self.shown_rows
                        .get(index)
                        .and_then(|row| row.recent.get(k))
                        .map_or_else(String::new, |change| {
                            if change.when.is_empty() {
                                change.text.clone()
                            } else {
                                format!("{}, {}", change.text, change.when)
                            }
                        }),
                    t("a11y-shell-people-change-description"),
                ),
                CollaboratorsHit::OpenChanges(index) => (
                    t_args(
                        "a11y-shell-people-changes",
                        &[("name", name_of(index).into())],
                    ),
                    t("a11y-shell-people-changes-description"),
                ),
                CollaboratorsHit::Invite => {
                    (t("a11y-shell-invite"), t("a11y-shell-invite-description"))
                }
                CollaboratorsHit::Absorb | CollaboratorsHit::Dismiss => continue,
            };
            let mut state = AccessState::new().focusable(true).focused(focused);
            if let Some(expanded) = expanded {
                state = state.expanded(expanded);
            }
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(name)
                    .with_description(description)
                    .with_state(state),
            );
        }
        // After the controls, so a control's path stays its focus position.
        if let Some(exposure) = &self.shown_exposure {
            children.push(AccessNode::new(Role::StaticText).with_name(exposure.clone()));
        }
        AccessNode::new(Role::Dialog)
            .with_name(t("a11y-shell-people"))
            .with_description(format!(
                "{}. {}",
                self.shown_workspace,
                t("a11y-shell-people-description")
            ))
            .with_children(children)
    }

    pub fn accessibility_rect_for_path(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [] | [0] => Some(self.panel_rect()),
            [0, row] => self.rows.get(*row).map(|place| self.view_rect(place.rect)),
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
}

/// How wide `label` is as a quiet button's label.
fn text_width(text: &mut TextRenderer, label: &str, theme: &Theme) -> f64 {
    f64::from(
        text.measure_style(
            label,
            None,
            &theme
                .text(TextRole::Label)
                .with_weight(ui::text_render::TextWeight::Medium),
        )
        .0,
    )
}

/// Every person and agent in the active workspace as the roster lists them.
pub fn collaborator_roster_rows(desktop: &DesktopTree, local_user: &str) -> Vec<RosterRow> {
    collaborator_roster_rows_named(desktop, local_user, None).0
}

/// [`collaborator_roster_rows`], with people who share a name numbered
/// ([`ui::presence::ParticipantLabels`]) — `own_name`, this person's own,
/// among them — and what this person is shown as.
pub fn collaborator_roster_rows_named(
    desktop: &DesktopTree,
    local_user: &str,
    own_name: Option<&str>,
) -> (Vec<RosterRow>, Option<String>) {
    let mut rows = unnumbered_roster_rows(desktop, local_user);
    let own_name = own_name.map(str::trim).filter(|name| !name.is_empty());
    let policy = desktop
        .active_workspace()
        .map(|workspace| &workspace.policy);
    let joined = |id: &str| policy.and_then(|policy| policy.joined_at(id));
    let labels = ui::presence::ParticipantLabels::by_joining(
        rows.iter()
            .filter(|row| !row.is_agent && row.id != local_user)
            .map(|row| (row.id.as_str(), row.label.as_str()))
            .chain(own_name.map(|name| (local_user, name)))
            .map(|(id, name)| (id, name, joined(id))),
    );
    for row in rows.iter_mut().filter(|row| !row.is_agent) {
        row.label = labels.label(&row.id, &row.label);
    }
    (rows, own_name.map(|name| labels.label(local_user, name)))
}

fn unnumbered_roster_rows(desktop: &DesktopTree, local_user: &str) -> Vec<RosterRow> {
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
            let is_bot = workspace.is_some_and(|workspace| is_room_bot(workspace, participant));
            let is_agent = is_bot || agent_role.is_some() || has_agent_marker(participant);
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
                is_bot,
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
                recent: Vec::new(),
                followable: !participant.invited && (!is_agent || has_view),
                availability: ui::Availability::Available,
                call: None,
                join: None,
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

/// Whether `participant` is one of `workspace`'s bots, as the room's
/// governance names them.
fn is_room_bot(workspace: &tilo_desktop_model::Workspace, participant: &Collaborator) -> bool {
    workspace.bots.iter().any(|bot| bot.is(&participant.id))
}

/// Whether `participant` is an agent: admitted to `workspace` as one (by id
/// or display name), one of its bots, or marked with [`AGENT_ROLE_MARKER`].
/// Every presence surface asks this, so software never shows as a person on
/// one of them.
#[must_use]
pub fn collaborator_is_agent(
    workspace: Option<&tilo_desktop_model::Workspace>,
    participant: &Collaborator,
    now: u64,
) -> bool {
    has_agent_marker(participant)
        || workspace.is_some_and(|workspace| {
            is_room_bot(workspace, participant)
                || workspace.agent_role_at(&participant.id, now).is_some()
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

/// What a screen reader says about a present row beyond its name: an
/// agent, what they are on now, what they are doing.
fn spoken_details(row: &RosterRow) -> String {
    let mut details = Vec::new();
    if row.is_agent {
        details.push(t("a11y-shell-people-agent"));
    }
    if let Some(activity) = &row.activity {
        details.push(t_args(
            "a11y-shell-people-now",
            &[("activity", activity.into())],
        ));
    }
    if let Some(doing) = doing_line(row) {
        details.push(doing);
    }
    details.join("; ")
}

/// What [`CollaboratorsOverlay::place_rows`] places: the groups' row indices
/// and the open row's details as the layout measured them.
#[derive(Clone, Copy)]
struct RowsPlan<'p> {
    people: &'p [usize],
    agents: &'p [usize],
    open_index: Option<usize>,
    shown_changes: usize,
    revealed: f64,
    exposure_h: f64,
}

/// The widths of a row's text buttons, each its label plus its padding:
/// Follow (wide enough for either of its labels), the changes link and Join.
#[derive(Clone, Copy)]
struct ButtonWidths {
    follow: f64,
    changes: f64,
    join: f64,
}

impl ButtonWidths {
    fn measure(text: &mut TextRenderer, theme: &Theme) -> Self {
        let pad = 2.0 * f64::from(theme.tokens.spacing.s3);
        let follow = text_width(text, &t("people-panel-follow"), theme).max(text_width(
            text,
            &t("people-panel-stop-following"),
            theme,
        ));
        Self {
            follow: follow + pad,
            changes: text_width(text, &t("people-panel-changes"), theme) + pad,
            join: text_width(text, &t("people-panel-join"), theme) + pad,
        }
    }

    /// The width a row's trailing slot keeps: Follow and Wave (a `control`
    /// square), and Join where any row has a call to join (`any_join`).
    fn trailing(self, any_join: bool, control: f64) -> f64 {
        let join = if any_join {
            TRAILING_GAP + self.join
        } else {
            0.0
        };
        self.follow + TRAILING_GAP + control + join
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, label: &str) -> RosterRow {
        RosterRow {
            id: id.into(),
            label: label.into(),
            initial: label.chars().next().unwrap_or('?'),
            color: Color::from_rgb8(0x25, 0x63, 0xeb),
            is_agent: false,
            is_bot: false,
            role: None,
            invited: false,
            activity: Some(format!("Viewing {label}'s notes")),
            status: None,
            recent: Vec::new(),
            followable: true,
            availability: ui::Availability::Available,
            call: None,
            join: None,
        }
    }

    fn paint(overlay: &mut CollaboratorsOverlay, rows: &[RosterRow], expanded: Option<&str>) {
        let theme = Theme::dark();
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        overlay.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &CollaboratorsModel {
                host: Rect::new(0.0, 0.0, 1400.0, 900.0),
                workspace: "Lounge",
                rows,
                unavailable: None,
                exposure: None,
                may_invite: true,
                followed_id: None,
                following_pairs: &[],
                expanded_id: expanded,
                watching: 0,
            },
            None,
        );
    }

    /// Voice is one click away: someone in a call this person can join
    /// shows "In a call" and offers Join on their row, reached by pointer
    /// and keyboard alike and named for assistive technology; someone in no
    /// call offers none.
    #[test]
    fn a_person_in_a_call_offers_join_on_their_row() {
        let mut ada = row("ada", "Ada");
        ada.call = Some("voice".into());
        ada.join = Some(VoiceChannel {
            app: "so.tilo.chat".into(),
            record: String::new(),
            channel: "c_voice".into(),
        });
        let rows = vec![ada, row("ben", "Ben")];
        let mut overlay = CollaboratorsOverlay::default();
        paint(&mut overlay, &rows, None);
        assert_eq!(
            doing_line(&rows[0]),
            Some(t_args(
                "people-panel-in-call",
                &[("channel", "voice".into())]
            ))
        );
        let ada_place = overlay.rows[0].clone();
        assert!(ada_place.join.area() > 0.0, "Ada's row offers Join");
        assert!(
            ada_place.join.x1 <= ada_place.follow.x0 && ada_place.wave.x1 <= ada_place.join.x0,
            "Join stands beside Follow, Wave before it"
        );
        assert_eq!(
            overlay.hit_test(ada_place.join.center()),
            CollaboratorsHit::JoinCall(0)
        );
        assert_eq!(overlay.join_target(0), rows[0].join.as_ref());
        assert_eq!(overlay.rows[1].join, Rect::ZERO, "Ben is in no call");
        assert!(
            overlay
                .focus_order()
                .contains(&CollaboratorsHit::JoinCall(0))
        );
        assert!(
            !overlay
                .focus_order()
                .contains(&CollaboratorsHit::JoinCall(1))
        );
    }

    #[test]
    fn a_rooms_bot_is_tagged_as_a_bot_never_as_a_person() {
        use tilo_desktop_model::{RoomBot, Workspace, WorkspaceId};

        let device = "ab".repeat(32);
        let mut workspace = Workspace::new(WorkspaceId(72), "Lounge");
        workspace.bots.push(RoomBot {
            name: "Standup".into(),
            installation: Some("6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6".into()),
            device: Some(device.clone()),
            entry: Some("so.tilo.bots.standup".into()),
            version: Some("1.2.0".into()),
            added_by: "owner".into(),
            requested: true,
            paused: false,
        });
        workspace
            .collaborators
            .push(Collaborator::new(device.clone(), "Standup"));
        workspace
            .collaborators
            .push(Collaborator::new("ada", "Ada"));
        let desktop = DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        };

        let rows = collaborator_roster_rows(&desktop, "local");
        let bot = rows.iter().find(|row| row.id == device).unwrap();
        assert!(bot.is_bot && bot.is_agent, "a bot is software, tagged Bot");
        let ada = rows.iter().find(|row| row.id == "ada").unwrap();
        assert!(!ada.is_bot && !ada.is_agent);
        assert!(collaborator_is_agent(
            desktop.active_workspace(),
            &Collaborator::new(device, "Standup"),
            0
        ));
    }

    #[test]
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
    }

    #[test]
    fn the_model_keeps_follow_direction_when_cursors_are_not_visible() {
        let rows = vec![row("ada", "Ada"), row("bob", "Bob")];
        let pairs = vec![("ada".to_string(), "bob".to_string())];
        let model = CollaboratorsModel {
            host: Rect::ZERO,
            workspace: "Lounge",
            rows: &rows,
            unavailable: None,
            exposure: None,
            may_invite: false,
            followed_id: None,
            following_pairs: &pairs,
            expanded_id: None,
            watching: 0,
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

    /// People, then agents, each with Follow on its row and Wave for
    /// people; Invite at the bottom; a press on a row's controls is that
    /// control's, on its body opens what they changed.
    #[test]
    fn the_roster_lists_people_then_agents_with_their_actions_and_invite() {
        let mut agent = row("scribe", "Release Notes Agent");
        agent.is_agent = true;
        let mut ada = row("ada", "Ada");
        ada.recent = vec![RecentChange {
            text: "Rewrote the intro and two headings".into(),
            when: "4 min ago".into(),
            entry: Some([1; 16]),
        }];
        let mut invited = row("cy", "Cy");
        invited.invited = true;
        let rows = vec![agent, ada, row("ben", "Ben"), invited];
        let mut overlay = CollaboratorsOverlay::default();
        paint(&mut overlay, &rows, None);

        let order: Vec<usize> = overlay.rows.iter().map(|place| place.index).collect();
        assert_eq!(
            order,
            vec![1, 2, 0],
            "people first, then agents; the invited are not here"
        );
        assert_eq!(
            overlay
                .headings
                .iter()
                .map(|h| h.text.as_str())
                .collect::<Vec<_>>(),
            vec![
                t("people-panel-section-people"),
                t("people-panel-section-agents")
            ]
        );
        let ada_place = overlay.rows[0].clone();
        assert_eq!(
            overlay.hit_test(ada_place.follow.center()),
            CollaboratorsHit::ToggleFollow(1)
        );
        assert_eq!(
            overlay.hit_test(ada_place.wave.center()),
            CollaboratorsHit::Wave(1)
        );
        assert_eq!(
            overlay.hit_test(Point::new(
                ada_place.rect.x0 + 80.0,
                ada_place.rect.center().y
            )),
            CollaboratorsHit::ToggleDetails(1),
            "Ada has recent changes: her row opens"
        );
        let ben_place = overlay.rows[1].clone();
        assert_eq!(
            overlay.hit_test(Point::new(
                ben_place.rect.x0 + 80.0,
                ben_place.rect.center().y
            )),
            CollaboratorsHit::Absorb,
            "nothing recent: nothing to open"
        );
        assert_eq!(overlay.rows[2].wave, Rect::ZERO, "nobody waves at software");
        assert_eq!(
            overlay.hit_test(overlay.invite_rect().center()),
            CollaboratorsHit::Invite
        );
        assert_eq!(
            overlay.hit_test(Point::new(5.0, 800.0)),
            CollaboratorsHit::Dismiss
        );
        assert_eq!(overlay.wave_target(1), Some("ada"));
        assert_eq!(overlay.wave_target(0), None);
    }

    /// Only you: the roster says nobody else is here, and still offers
    /// Invite.
    #[test]
    fn only_you_says_nobody_else_is_here_and_offers_invite() {
        let mut overlay = CollaboratorsOverlay::default();
        paint(&mut overlay, &[], None);
        assert!(overlay.rows.is_empty());
        assert!(overlay.invite_rect().area() > 0.0);
        let snapshot = overlay.accessibility_snapshot();
        assert!(
            snapshot
                .children
                .iter()
                .any(|node| node.name.as_deref() == Some(t("a11y-shell-invite").as_str())),
            "Invite is a control"
        );
    }

    /// An open row shows what the person changed with the way into History;
    /// the keyboard reaches every control without a pointer.
    #[test]
    fn an_open_row_leads_to_history_and_the_keyboard_reaches_every_control() {
        let _settled = ui::motion::testing::settled();
        let mut ada = row("ada", "Ada");
        ada.recent = vec![
            RecentChange {
                text: "Rewrote the intro".into(),
                when: "4 min ago".into(),
                entry: Some([1; 16]),
            },
            RecentChange {
                text: "Added a heading".into(),
                when: "9 min ago".into(),
                entry: Some([2; 16]),
            },
        ];
        let rows = vec![ada, row("ben", "Ben")];
        let mut overlay = CollaboratorsOverlay::default();
        paint(&mut overlay, &rows, Some("ada"));
        paint(&mut overlay, &rows, Some("ada"));
        let changes = overlay.rows[0].changes;
        assert!(changes.area() > 0.0, "the open row has its link to History");
        assert_eq!(
            overlay.hit_test(changes.center()),
            CollaboratorsHit::OpenChanges(0)
        );
        assert_eq!(
            overlay.rows[0].change_rows.len(),
            2,
            "each change is listed"
        );
        let second = overlay.rows[0].change_rows[1];
        assert_eq!(
            overlay.hit_test(second.center()),
            CollaboratorsHit::OpenChange(0, 1),
            "each change opens History at itself"
        );
        assert_eq!(overlay.change_target(0, 1), Some(("Ada", Some([2; 16]))));

        let mut reached = Vec::new();
        for _ in 0..overlay.focus_order().len() {
            overlay.key(&Key::Named(NamedKey::Tab), false);
            reached.push(overlay.keyboard_focus);
        }
        for wanted in [
            CollaboratorsHit::ToggleDetails(0),
            CollaboratorsHit::Wave(0),
            CollaboratorsHit::ToggleFollow(0),
            CollaboratorsHit::OpenChange(0, 0),
            CollaboratorsHit::OpenChange(0, 1),
            CollaboratorsHit::OpenChanges(0),
            CollaboratorsHit::Wave(1),
            CollaboratorsHit::ToggleFollow(1),
            CollaboratorsHit::Invite,
        ] {
            assert!(reached.contains(&Some(wanted)), "{wanted:?} is reachable");
        }
        assert_eq!(
            overlay.key(&Key::Named(NamedKey::Escape), false),
            Some(CollaboratorsHit::Dismiss)
        );
    }

    /// What someone is doing: a call first, then what they wrote, then how
    /// available they are; nothing for someone simply here.
    #[test]
    fn what_someone_is_doing_reads_their_call_status_or_availability() {
        let mut ada = row("ada", "Ada");
        assert_eq!(doing_line(&ada), None);
        ada.availability = ui::Availability::Idle;
        assert_eq!(doing_line(&ada), Some(t("people-panel-idle")));
        ada.status = Some(RosterStatus {
            text: "Reviewing the deck".into(),
            source: LineSource::Written,
        });
        assert_eq!(doing_line(&ada).as_deref(), Some("Reviewing the deck"));
        ada.call = Some("voice".into());
        assert_eq!(
            doing_line(&ada),
            Some(t_args(
                "people-panel-in-call",
                &[("channel", "voice".into())]
            ))
        );
        ada.call = Some(String::new());
        assert_eq!(doing_line(&ada), Some(t("people-panel-in-a-call")));
    }
}
