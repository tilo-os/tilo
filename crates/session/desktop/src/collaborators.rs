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

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use ui::icons::Icon;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{ButtonFace, ButtonRole, ChipStyle, FloatingPanelFace, ListRowFace, RowLeading};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::TextRole;

use crate::here::Screen;

use crate::i18n::{t, t_args};

mod roster;
use roster::spoken_details;
mod screen;
pub use roster::{
    AGENT_ROLE_MARKER, PeopleNames, collaborator_is_agent, collaborator_roster_rows,
    collaborator_roster_rows_named, in_app, roster_colors_with_local_user,
};

/// Rows the roster shows before its list scrolls.
const VISIBLE_ROWS: usize = 8;
/// A face in a row.
const FACE: f64 = 40.0;
/// Workspace the trailing controls take: Wave (an icon button) and Follow.
const TRAILING_GAP: f64 = 4.0;

/// What a pointer press or a key on the open roster means to the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollaboratorsHit {
    Invite,
    /// Keep the list on screen: the host pins it out of the bar as the people overlay.
    Pin,
    /// Follow the person at this row index.
    Follow(usize),
    /// Stop following the person at this row index.
    Unfollow(usize),
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
    /// Bring forward the window the software at this row index works in.
    OpenTab(usize),
    /// Inside the panel, but not on an action.
    Absorb,
    /// Outside the panel; the host closes it.
    Dismiss,
}

/// One row of the roster.
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
    /// One of the workspace's bots (then also [`Self::is_agent`]): tagged Bot, never Agent.
    pub is_bot: bool,
    pub role: Option<String>,
    pub invited: bool,
    /// Where they are: the app or document in front of them.
    pub activity: Option<String>,
    /// The window of this workspace a piece of software is working in: its tab, and what that tab is
    /// called here. Software has no screen to mirror, so its row shows this instead, and pressing it
    /// brings that window forward.
    pub working_in: Option<(u64, String)>,
    /// Short participant intention, marked by its [`LineSource`] so model-suggested words cannot be mistaken for a direct quote.
    pub status: Option<RosterStatus>,
    /// What they changed recently in this workspace, newest first: a row with none does not open.
    pub recent: Vec<RecentChange>,
    pub followable: bool,
    /// How available they say they are (idle and away come on their own).
    pub availability: ui::Availability,
    /// The voice channel they are in, if any.
    pub call: Option<String>,
    /// The voice channel of theirs this device may join from their row: one they are in and this person is not.
    pub join: Option<VoiceChannel>,
    /// Another device of this person, listed among their devices.
    pub own_device: bool,
}

impl RosterRow {
    /// This person as their own row shows them (the pinned overlay's first): by `id` , `label` and `color` , with what everyone here reads of them, their own words or what is said of them automatically.
    #[must_use]
    pub fn own(id: String, label: String, color: Color, status: Option<RosterStatus>) -> Self {
        Self {
            id,
            initial: label
                .chars()
                .next()
                .map_or('\u{2022}', |c| c.to_uppercase().next().unwrap_or(c)),
            label,
            color,
            is_agent: false,
            is_bot: false,
            role: None,
            invited: false,
            activity: None,
            working_in: None,
            status: status.filter(|status| !status.text.trim().is_empty()),
            recent: Vec::new(),
            followable: false,
            availability: ui::Availability::Available,
            call: None,
            join: None,
            own_device: false,
        }
    }
}

/// One voice channel's call, by the app, record and channel that hold it: what joining it asks of the app that owns it.
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

/// Changes an open row lists before "Their changes" takes over.
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

/// What the roster shows this frame, decided by the host.
pub struct CollaboratorsModel<'a> {
    pub host: Rect,
    /// The workspace's name, the panel's title.
    pub workspace: &'a str,
    pub rows: &'a [RosterRow],
    pub may_invite: bool,
    /// The host can keep the list on screen as the people overlay: the header offers to.
    pub may_pin: bool,
    /// The host keeps the roster on screen now: the header offers to put it back in the bar, and each person's screen stands a mirror under their row.
    pub pinned: bool,
    /// Where the pinned roster stands, as its host placed it (dragged to an edge): `None` places it under the bar at the host's right edge.
    pub panel: Option<Rect>,
    /// Each person's screen, by roster id (`None`: they keep where they are to themselves), mirrored under their card.
    pub screens: &'a [(String, Option<Screen>)],
    pub followed_id: Option<&'a str>,
    /// Ephemeral workspace-local attachments, expressed using roster ids.
    pub following_pairs: &'a [(String, String)],
    /// The roster id whose recent changes are open. Ignored when that row has nothing recent.
    pub expanded_id: Option<&'a str>,
    /// Whom this person waved at a moment ago, by roster id: their row shows the wave sent.
    pub waved: &'a [String],
    /// When the wave stops showing: the row it went to is drawn again without one then.
    pub waved_until: Option<ui::time::Instant>,
}

/// Direction of a transient attachment relative to one roster row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RosterFollowRelationship {
    Follows(String),
    FollowedBy(String),
}

impl CollaboratorsModel<'_> {
    /// The rows of who is here, not merely invited, under their headings: people, agents, and this person's own other devices.
    fn present_groups(&self) -> [(&'static str, Vec<usize>); 3] {
        let mut groups = [
            ("people-panel-section-people", Vec::new()),
            ("people-panel-section-agents", Vec::new()),
            ("people-panel-section-devices", Vec::new()),
        ];
        for (index, row) in self.rows.iter().enumerate().filter(|(_, row)| !row.invited) {
            let group = if row.own_device {
                2
            } else {
                usize::from(row.is_agent)
            };
            if let Some((_, rows)) = groups.get_mut(group) {
                rows.push(index);
            }
        }
        groups
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

/// Say on each follower's row whom they follow, from `pairs` of `(follower, followed)` roster ids: "Following Ada", or "Following you" when they follow this person (`own_id`).
pub fn note_following(rows: &mut [RosterRow], pairs: &[(String, String)], own_id: Option<&str>) {
    for (follower, followed) in pairs {
        // `None` for this person: "you".
        let person = if own_id == Some(followed.as_str()) {
            None
        } else if let Some(row) = rows.iter().find(|row| &row.id == followed) {
            Some(row.label.clone())
        } else {
            continue;
        };
        let Some(row) = rows.iter_mut().find(|row| &row.id == follower) else {
            continue;
        };
        let place = row
            .activity
            .take()
            .filter(|activity| !activity.trim().is_empty());
        row.activity = Some(match (place, person) {
            (None, None) => t("people-follows-you"),
            (None, Some(person)) => t_args("people-follows", &[("person", person.into())]),
            (Some(place), None) => t_args("people-follows-you-where", &[("place", place.into())]),
            (Some(place), Some(person)) => t_args(
                "people-follows-where",
                &[("place", place.into()), ("person", person.into())],
            ),
        });
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
    /// What software works in, under its row (software has no screen to mirror).
    working_in: Rect,
    /// The one line under a person's name saying why their screen is not shown there.
    said: Rect,
    /// The open part under the row, its changes and "Their changes".
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
    invite: Rect,
    /// The header's Keep on screen button (Put back in the bar when pinned).
    pin: Rect,
    /// Where each row's mirror of that person's screen stands.
    mirrors: Vec<(usize, Rect)>,
    /// The trailing slot every row reserves for Wave and Follow, and Join where anyone has a call to join.
    trailing_w: f64,
    /// What Join adds to [`Self::trailing_w`]: none when nobody has a call.
    join_extra: f64,
    /// The row whose recent changes are open, and how open (the disclose motion).
    opened: Option<String>,
    reveal: ui::motion::Presence,
    /// The open row's laid-out height when fully open.
    details_height: f64,
    keyboard_focus: Option<CollaboratorsHit>,
    targets: ui::widget::PaintedTargets,
    painted_actions: Vec<(String, CollaboratorsHit)>,
    held_submit: [bool; 2],
    /// What the last frame showed, for assistive technology and the host's revalidation.
    shown_rows: Vec<RosterRow>,
    /// The last frame kept the roster on screen: its header button puts it back in the bar.
    shown_pinned: bool,
    shown_workspace: String,
    shown_expanded: Option<String>,
    shown_followed: Option<String>,
    /// Whom the last frame showed waved at.
    shown_waved: Vec<String>,
    /// When the wave it shows stops showing, so the row that carries it is drawn again without one.
    shown_waved_until: Option<ui::time::Instant>,
    motion: ui::tokens::EffectiveMotion,
    /// How the host draws the roster now, while it arrives: input is hit-tested there (`Pose::to_local`), not where it will rest.
    drawn: ui::motion::Pose,
    /// When someone here was first seen in a call, so their ring breathes from its start.
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

/// The line under a person's name saying what they are doing: a call they are in, else what they wrote or are doing, else how available they are.
#[must_use]
pub fn doing_line(row: &RosterRow) -> Option<String> {
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

/// Whether the line under someone's name is software's words about them (a model's, or the host's reading of what they did), not theirs: it carries the automatic mark.
#[must_use]
pub fn says_automatically(row: &RosterRow) -> bool {
    row.call.is_none()
        && row.status.as_ref().is_some_and(|status| {
            status.source != LineSource::Written && !status.text.trim().is_empty()
        })
}

/// How tall each person's mirror of their screen stands: what the panel's room allows, in the screen's own shape, and none at all when that leaves no room for a picture.
fn mirror_height(model: &CollaboratorsModel<'_>, width: f64, row_h: f64, theme: &Theme) -> f64 {
    if !model.pinned {
        return 0.0;
    }
    let room = (model.host.height() * 0.5 - row_h).max(0.0);
    // Software has no screen to mirror, so its rows take none of the room people's mirrors share.
    let people = model.rows.iter().filter(|row| !row.is_agent).count();
    let each = room / ui::num::f64_of_usize(people.max(1));
    let natural = (width - 2.0 * FloatingPanelFace::body_inset(theme)) / 1.6;
    if each < 64.0 { 0.0 } else { each.min(natural) }
}

/// Software's where-it-works line: what it is doing now (its own now line, as its name and its card
/// say it) and, where there is one, the record it is working in.
fn working_in_line(row: &RosterRow) -> (String, Option<String>) {
    let record = row
        .working_in
        .as_ref()
        .map_or_else(String::new, |(_, title)| title.clone());
    match doing_line(row) {
        Some(now) if !now.trim().is_empty() => {
            let record = (!record.is_empty()).then_some(record);
            (now, record)
        }
        _ => (record, None),
    }
}

/// What stands under a row beyond its line: a person's picture of their screen, a line saying why
/// their screen is not shown there, or software's line naming the window it works in.
fn under_height(
    row: &RosterRow,
    screen: Option<&Screen>,
    (mirror_h, in_row): (f64, bool),
    theme: &Theme,
) -> f64 {
    let line = f64::from(theme.tokens.metrics.bar_control_height);
    if row.is_agent {
        // Its line under the row names the window it works in, or, with none, says what it is
        // doing: in the row, under its name, where that holds the words whole (`in_row`), else on
        // a line of its own under the row rather than cut to "Reading Launch notes in…".
        let doing = doing_line(row).is_some_and(|now| !now.trim().is_empty());
        return if row.working_in.is_some() {
            line
        } else if doing && !in_row {
            // One caption line, which stands right under the name (`place_rows`).
            f64::from(theme.text(TextRole::Caption).line_height_px())
        } else {
            0.0
        };
    }
    if sharing(row, screen) {
        mirror_h
    } else if mirror_h > 0.0 {
        line
    } else {
        0.0
    }
}

/// Whether `row` is a person with a picture of their screen to show: they share where they are, and
/// something stands open there.
fn sharing(row: &RosterRow, screen: Option<&Screen>) -> bool {
    !row.is_agent && screen.is_some_and(|screen| !screen.windows.is_empty())
}

/// `row`'s screen as the desk holds it: `None` while they keep where they are to themselves.
fn screen_of<'a>(model: &'a CollaboratorsModel<'_>, row: &RosterRow) -> Option<&'a Screen> {
    model
        .screens
        .iter()
        .find(|(id, _)| *id == row.id)
        .and_then(|(_, screen)| screen.as_ref())
}

/// Why a person's screen is not drawn under their row: they keep where they are private, or nothing
/// stands open on it. Said under their name, so it never says the name again.
fn screen_line(screen: Option<&Screen>) -> String {
    t(if screen.is_some() {
        "people-panel-nothing-open"
    } else {
        "people-panel-screen-private"
    })
}

/// The panel's header line: who is here besides this person, counted as the list is (the others, never this person, who is the "you" of the words; "2 here" over one row read as someone missing).
fn here_line(others: usize) -> String {
    t_args("people-panel-here-count", &[("count", others.into())])
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
        // Keyboard focus stays with the person and control it was on, not the row index, when
        // people come, go or reorder.
        let focused = self.keyboard_focus.and_then(|hit| self.target_id(hit));
        self.follow_expansion(model);
        self.host = model.host;
        self.shown_rows = model.rows.to_vec();
        self.shown_pinned = model.pinned;
        model.workspace.clone_into(&mut self.shown_workspace);
        self.shown_followed = model.followed_id.map(str::to_owned);
        self.shown_waved = model.waved.to_vec();
        self.shown_waved_until = model.waved_until;
        self.layout(text, theme, model);
        self.keyboard_focus = focused.and_then(|id| {
            self.focus_order()
                .into_iter()
                .find(|hit| self.target_id(*hit).as_deref() == Some(id.as_str()))
        });
        self.configure_scroll(theme);
        let cursor = cursor.map(|point| self.drawn.to_local(point));
        self.paint(scene, transform, text, theme, model, cursor);
        self.register_targets(model.panel.unwrap_or(model.host));
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
        let width = model.panel.map_or_else(
            || {
                f64::from(metrics.panel_width_list)
                    .min((model.host.width() - 2.0 * f64::from(spacing.s4)).max(240.0))
            },
            |panel| panel.width(),
        );
        let header_h = ui::widget::TextStackFace::new(model.workspace)
            .secondary("0")
            .height(text, theme);
        let control = f64::from(metrics.bar_control_height);
        let groups = model.present_groups();
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
        let sections = groups.iter().filter(|(_, group)| !group.is_empty()).count();
        // A label earns its line only where it tells one section from another: with a single
        // section the header already says who is here.
        let labelled = sections > 1;
        let mirror_h = mirror_height(model, width, row_h, theme);
        // What stands under each row: a person's picture of their screen, one line saying why there
        // is none, software's where-it-works line, or nothing.
        self.measure_trailing(text, theme, model);
        let under: Vec<f64> = model
            .rows
            .iter()
            .map(|row| {
                let in_row = self.doing_fits_in_row(text, theme, row, width);
                under_height(row, screen_of(model, row), (mirror_h, in_row), theme)
            })
            .collect();
        let card_h = |index: usize| {
            row_h
                + under.get(index).copied().map_or_else(
                    || 0.0,
                    |under| {
                        if under > 0.0 {
                            under + f64::from(spacing.s2)
                        } else {
                            0.0
                        }
                    },
                )
        };
        let tallest = (0..model.rows.len()).map(card_h).fold(row_h, f64::max);
        let body_h = if sections == 0 {
            model.rows.first().map_or(0.0, |_| card_h(0))
        } else {
            let heading = if labelled { heading_h } else { 0.0 };
            let gap = if labelled {
                f64::from(spacing.s2) * ui::num::f64_of_usize(sections.saturating_sub(1))
            } else {
                0.0
            };
            groups
                .iter()
                .filter(|(_, group)| !group.is_empty())
                .map(|(_, group)| heading + group.iter().map(|index| card_h(*index)).sum::<f64>())
                .sum::<f64>()
                + gap
        };
        let open_extra = if open_index.is_some() { revealed } else { 0.0 };
        self.body_height = body_h + open_extra;
        self.place_panel(theme, model, &face, width, tallest);
        self.place_rows(
            text,
            theme,
            model,
            &RowsPlan {
                groups: &groups,
                labelled,
                open_index,
                shown_changes,
                revealed,
                under: &under,
            },
        );
    }

    /// The panel itself: its rect (where its host placed it, else under the bar at the right edge), its header and pin, its body and its footer.
    fn place_panel(
        &mut self,
        theme: &Theme,
        model: &CollaboratorsModel<'_>,
        face: &FloatingPanelFace,
        width: f64,
        card_h: f64,
    ) {
        let metrics = &theme.tokens.metrics;
        let spacing = &theme.tokens.spacing;
        let row_h = f64::from(metrics.row_avatar);
        let heading_h = f64::from(metrics.row_single);
        let control = f64::from(metrics.bar_control_height);
        let anchor_top =
            model.host.y0 + f64::from(crate::topbar::topbar_height(theme)) + f64::from(spacing.s2);
        // Never taller than the output under the bar: the list scrolls.
        let chrome_h = face.height_for(1.0, theme) - 1.0;
        let room = (model.host.y1 - f64::from(spacing.s3) - anchor_top - chrome_h).max(row_h);
        let visible_body = (heading_h * 2.0 + card_h * ui::num::f64_of_usize(VISIBLE_ROWS))
            .min(self.body_height)
            .min(room);
        let panel_h = face.height_for(visible_body, theme);
        self.panel = model.panel.map_or_else(
            || {
                let right = model.host.x1 - f64::from(spacing.s3);
                Rect::new(right - width, anchor_top, right, anchor_top + panel_h)
            },
            |panel| Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + panel_h),
        );
        let parts = face.layout(self.panel, theme);
        self.header = parts.header;
        self.pin = if model.may_pin || model.pinned {
            Rect::new(
                parts.header.x1 - control,
                parts.header.center().y - control / 2.0,
                parts.header.x1,
                parts.header.center().y + control / 2.0,
            )
        } else {
            Rect::ZERO
        };
        self.body = parts.body;
        self.viewport = self.panel;
        self.invite = if model.may_invite {
            parts.footer
        } else {
            Rect::ZERO
        };
    }

    /// The rows, in content coordinates (before scrolling): each group's heading and its cards, the open row's details under it.
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
            groups,
            labelled,
            open_index,
            shown_changes,
            revealed,
            under,
        } = *plan;
        self.rows.clear();
        self.headings.clear();
        self.mirrors.clear();
        let gap = f64::from(spacing.s2);
        let bleed = ui::chrome::row_bleed(theme);
        let mut y = self.body.y0;
        let widths = self.measure_trailing(text, theme, model);
        // Follow stands in one column down the list: where any row has a chevron, every row keeps
        // its place free.
        let any_disclosure = model.rows.iter().any(|row| !row.recent.is_empty());
        for (key, group) in groups {
            if group.is_empty() {
                continue;
            }
            if labelled {
                if !self.headings.is_empty() {
                    y += f64::from(spacing.s2);
                }
                self.headings.push(Heading {
                    text: t(key),
                    rect: Rect::new(self.body.x0, y, self.body.x1, y + heading_h),
                });
                y += heading_h;
            }
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
                let under_h = under.get(index).copied().unwrap_or(0.0);
                if under_h > 0.0 {
                    let strip = Rect::new(self.body.x0, y + gap, self.body.x1, y + gap + under_h);
                    // A person's screen is a picture of where they are, with room for one; where
                    // there is none, their row says why in a line. Software's line names the window
                    // it works in, which pressing brings forward.
                    if sharing(row, screen_of(model, row)) {
                        self.mirrors.push((index, strip));
                    } else if row.is_agent && row.working_in.is_some() {
                        place.working_in = strip;
                    } else {
                        // The line stands in the column their name does, under it. Software's
                        // line says what it is doing: a second line of its row, risen into the
                        // row's own room under the name rather than floating a control's height
                        // below it.
                        let lift = if row.is_agent {
                            gap + f64::from(spacing.s3)
                        } else {
                            0.0
                        };
                        place.said = Rect::new(
                            self.body.x0 + FACE + f64::from(spacing.s3),
                            strip.y0 - lift,
                            strip.x1,
                            strip.y1 - lift,
                        );
                    }
                    y += gap + under_h;
                }
                if open_index == Some(index) {
                    place.details = Rect::new(self.body.x0, y, self.body.x1, y + revealed);
                    // Under the name: each change a line of its own, then the way to everything
                    // they changed, its label on the same column. Laid out from the start so they
                    // fade in with the opening; they take presses once it is open.
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

    /// The row `index` at `rect` with its trailing buttons: Join where the person has a call to join, Follow where they can be followed, Wave for a person (never an agent).
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
            wave: if row.is_agent || row.own_device {
                Rect::ZERO
            } else {
                wave
            },
            join,
            ..RowPlace::default()
        };
        (place, follow.x1)
    }

    /// Join (where anyone has a call to join), Wave and Follow sit in a row's trailing slot, inside
    /// its disclosure chevron when it has one: the room the list keeps for them.
    fn measure_trailing(
        &mut self,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &CollaboratorsModel<'_>,
    ) -> ButtonWidths {
        let widths = ButtonWidths::measure(text, theme);
        let control = f64::from(theme.tokens.metrics.bar_control_height);
        let any_join = model.rows.iter().any(|row| row.join.is_some());
        self.trailing_w = widths.trailing(any_join, control);
        self.join_extra = if any_join {
            TRAILING_GAP + widths.join
        } else {
            0.0
        };
        widths
    }

    /// The trailing room the row keeps for the controls it has: Follow, Wave (never for software
    /// or the person's own devices) and Join where it has a call to join.
    fn row_trailing(&self, row: &RosterRow, joins: bool, theme: &Theme) -> f64 {
        let mut trailing = self.trailing_w;
        if !joins {
            trailing -= self.join_extra;
        }
        if row.is_agent || row.own_device {
            trailing -= TRAILING_GAP + f64::from(theme.tokens.metrics.bar_control_height);
        }
        trailing.max(0.0)
    }

    /// Whether what the agent `row` is doing reads whole in its row, under its name, in a panel
    /// `width` wide: the column between its face and Follow.
    fn doing_fits_in_row(
        &self,
        text: &mut TextRenderer,
        theme: &Theme,
        row: &RosterRow,
        width: f64,
    ) -> bool {
        let Some(doing) = doing_line(row).filter(|now| !now.trim().is_empty()) else {
            return true;
        };
        let inset = FloatingPanelFace::body_inset(theme);
        let bleed = ui::chrome::row_bleed(theme);
        let rect = Rect::new(
            inset - bleed,
            0.0,
            width - inset + bleed,
            f64::from(theme.tokens.metrics.row_avatar),
        );
        let probe = ListRowFace::new(&row.label)
            .leading_column(FACE)
            .trailing_width(self.row_trailing(row, row.join.is_some(), theme));
        let start = probe.text_column(rect, text, theme);
        let end = probe
            .trailing_rect(rect, text, theme)
            .map_or(rect.x1 - bleed, |slot| {
                slot.x0 - f64::from(theme.tokens.spacing.s2)
            });
        let needs = f64::from(
            text.measure_style(&doing, None, &theme.text(TextRole::Caption))
                .0,
        );
        needs <= end - start
    }

    /// Follow and Wave in the trailing slot of the row labelled `label` at `rect` , inside the disclosure chevron's place when the list keeps one (`disclosure`).
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
        // The bar runs the panel's whole edge, header to footer, and the panel's fixed parts count
        // as content that never scrolls away, so the list's own range is unchanged.
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
        let here_text = here_line(
            model
                .rows
                .iter()
                .filter(|row| !row.invited && !row.own_device)
                .count(),
        );
        let title = if self.pin.area() > 0.0 {
            Rect::new(
                self.header.x0,
                self.header.y0,
                self.pin.x0 - f64::from(theme.tokens.spacing.s2),
                self.header.y1,
            )
        } else {
            self.header
        };
        ui::widget::TextStackFace::new(model.workspace)
            .secondary(&here_text)
            .paint(
                &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
                title,
            );
        if self.pin.area() > 0.0 {
            let (icon, label) = if model.pinned {
                (Icon::X, t("people-panel-unpin"))
            } else {
                (Icon::Pin, t("people-panel-pin"))
            };
            ui::widget::Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::icon_only(self.pin, icon, &label)
                    .hovered(cursor.is_some_and(|p| self.pin.contains(p)))
                    .focused(self.keyboard_focus == Some(CollaboratorsHit::Pin)),
            );
        }

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
        if self.rows.is_empty() {
            let empty = t("people-panel-empty");
            ui::widget::TextLineFace::styled(&empty, &theme.text(TextRole::Body), TextRole::Body)
                .paint(
                    &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
                    self.body,
                    colors.text_dim,
                );
        }
        for heading in &self.headings {
            let style = theme
                .text(TextRole::Caption)
                .with_weight(ui::text_render::TextWeight::Medium);
            ui::widget::TextLineFace::styled(&heading.text, &style, TextRole::Caption).paint(
                &mut ui::paint::PaintCtx::new(scene, scrolled, text, theme),
                heading.rect,
                colors.text_dim,
            );
        }
        let agent_tag = t("people-panel-agent-tag");
        let bot_tag = t("people-panel-bot-tag");
        let follow_label = t("people-panel-follow");
        let stop_label = t("people-panel-stop-following");
        let wave_label = t("people-panel-wave");
        let sent_label = t("people-panel-waved");
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
            let mirror = self
                .mirrors
                .iter()
                .find(|(index, _)| *index == place.index)
                .map(|(_, rect)| *rect);
            // The card is its row and what stands under it: hovered or opened, they lift together.
            let under = [mirror, Some(place.working_in), Some(place.said)]
                .into_iter()
                .flatten()
                .find(|rect| rect.area() > 0.0);
            let hovered = cursor.is_some_and(|p| {
                place.rect.contains(p) || under.is_some_and(|under| under.contains(p))
            });
            let opened = model.expanded_id == Some(row.id.as_str());
            let following = model.followed_id == Some(row.id.as_str());
            let card = under.map_or(place.rect, |under| place.rect.union(under));
            let lifted = hovered || opened || following;
            if under.is_some() && lifted {
                ui::widget::paint_surface(
                    scene,
                    scrolled,
                    card.inset(-2.0),
                    ui::widget::SurfaceLevel::Raised,
                    theme,
                );
            }
            let interaction = ui::widget::Interaction {
                // A card lifts as one piece: the row keeps no ground of its own.
                hovered: hovered && mirror.is_none(),
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
            // Software is tagged after its name: Bot for the workspace's bots,
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
            // The line under the row says it whole where software's window line stands there:
            // the row's own line is elided to a caption column beside Wave and Stop, and a
            // person reading "Reading Launch notes in…" has been told nothing.
            if let Some(doing) = &lines.1
                && place.working_in.area() <= 0.0
                && place.said.area() <= 0.0
            {
                face = face.secondary(doing);
            }
            // Words software wrote about someone are marked as such: never shown as if they had
            // said them.
            if says_automatically(row) {
                face = face.mark(ui::icons::Icon::Sparkles);
            }
            if row.is_agent {
                face = face.tags(&tags);
            }
            let mut face = face
                .leading(RowLeading::Paint(&paint_face))
                .leading_column(FACE)
                // Only a row with Join gives its name the less room, and software, which is never
                // waved at, keeps no room for Wave.
                .trailing_width(self.row_trailing(row, place.join.area() > 0.0, theme))
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
            // The open row (a press on their face in the bar opens it) is their card: its wave
            // shows without a hover.
            let opened = model.expanded_id == Some(row.id.as_str());
            // A wave just sent stays shown as sent, so the waver knows it went.
            let waved = model.waved.contains(&row.id);
            if place.wave.area() > 0.0
                && (hovered
                    || opened
                    || waved
                    || self.keyboard_focus == Some(CollaboratorsHit::Wave(place.index)))
            {
                let (icon, label) = if waved {
                    (Icon::Check, &sent_label)
                } else {
                    (Icon::Hand, &wave_label)
                };
                ui::widget::Button::paint(
                    scene,
                    scrolled,
                    text,
                    theme,
                    &ButtonFace::icon_only(place.wave, icon, label)
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
                        self.keyboard_focus
                            == Some(if following {
                                CollaboratorsHit::Unfollow(place.index)
                            } else {
                                CollaboratorsHit::Follow(place.index)
                            }),
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
            if place.working_in.area() > 0.0 {
                // What software is doing now, and the window it does it in: pressing the line brings
                // that window forward.
                let (line, record) = working_in_line(row);
                let mut face = ListRowFace::new(&line);
                // The record the line already names is said once: "Reading Launch notes in Notes"
                // and a "Notes" after it is the same place twice.
                if let Some(record) = &record
                    && !line.contains(record.as_str())
                {
                    face = face.meta(record);
                }
                face.interaction(ui::widget::Interaction {
                    hovered: cursor.is_some_and(|p| place.working_in.contains(p)),
                    pressed: false,
                    focused: self.keyboard_focus == Some(CollaboratorsHit::OpenTab(place.index)),
                    disabled: false,
                })
                .paint(
                    &mut ui::paint::PaintCtx::new(scene, scrolled, text, theme),
                    place.working_in,
                );
            }
            if place.said.area() > 0.0 {
                // Why their screen is not drawn here, in one quiet line under their name: the card
                // says it without holding a picture of nothing. Software with no window to name
                // says there what it is doing, whole.
                let line = if row.is_agent {
                    doing_line(row).unwrap_or_default()
                } else {
                    screen_line(screen_of(model, row))
                };
                let style = theme.text(TextRole::Caption);
                ui::widget::TextLineFace::styled(&line, &style, TextRole::Caption)
                    .start()
                    .paint(
                        &mut ui::paint::PaintCtx::new(scene, scrolled, text, theme),
                        place.said,
                        colors.text_dim,
                    );
            }
            if let Some(mirror) = mirror
                && let Some(screen) = screen_of(model, row)
            {
                screen::picture(scene, scrolled, text, theme, mirror, row, screen);
            }
            if following && mirror.is_some() {
                // Their colour runs the card's whole height: whose view this is. It is the
                // person's own colour, which the theme cannot name.
                let bar = Rect::new(card.x0 + 1.0, card.y0 + 6.0, card.x0 + 3.0, card.y1 - 6.0);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    scrolled,
                    row.color,
                    None,
                    &bar,
                );
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

    /// Where the panel's header stands: the grip that drags a roster kept on screen to another edge.
    #[must_use]
    pub fn body_rect(&self) -> Rect {
        self.body
    }

    pub fn header_rect(&self) -> Rect {
        self.header
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
            CollaboratorsHit::Follow(index) => id_of(index).map(|id| format!("follow:{id}")),
            CollaboratorsHit::Unfollow(index) => id_of(index).map(|id| format!("unfollow:{id}")),
            CollaboratorsHit::Wave(index) => id_of(index).map(|id| format!("wave:{id}")),
            CollaboratorsHit::JoinCall(index) => id_of(index).map(|id| format!("join:{id}")),
            CollaboratorsHit::ToggleDetails(index) => {
                id_of(index).map(|id| format!("details:{id}"))
            }
            CollaboratorsHit::OpenChanges(index) => id_of(index).map(|id| format!("changes:{id}")),
            CollaboratorsHit::OpenTab(index) => id_of(index).map(|id| format!("work:{id}")),
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
                let rect = if matches!(hit, CollaboratorsHit::Invite | CollaboratorsHit::Pin) {
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

    /// Step a glide after a flick to the frame's time, from the host's `update` ; returns whether the roster moved.
    pub fn advance_scroll(&mut self) -> bool {
        let moved = self
            .scroll
            .as_mut()
            .is_some_and(ui::widget::ScrollRegion::advance);
        if moved {
            self.targets.cancel_pointer_press();
        }
        moved
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
        [scrollbar, breath, self.shown_waved_until]
            .into_iter()
            .flatten()
            .filter(|at| *at > ui::time::now())
            .min()
    }

    pub fn wants_pointer_capture(&self) -> bool {
        self.targets.wants_pointer_capture()
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
        // While it arrives, the pointer acts on what is drawn under it.
        let drawn = self.drawn;
        let cursor = cursor.map(|point| drawn.to_local(point));
        let posed;
        let event = match event {
            WindowEvent::CursorMoved { position, .. } if !drawn.is_rest() => {
                let scale = ctx.scale_factor;
                let local = drawn.to_local(Point::new(position.x / scale, position.y / scale));
                posed = ui::synthetic_input::cursor_moved(local.x * scale, local.y * scale);
                &posed
            }
            _ => event,
        };
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

    /// The stable person behind a painted Wave button.
    pub fn wave_target(&self, index: usize) -> Option<&str> {
        self.shown_rows
            .get(index)
            .filter(|row| !row.is_agent && !row.own_device && !row.invited)
            .map(|row| row.id.as_str())
    }

    /// The voice channel behind a painted Join button: the one the person at this row index is in.
    pub fn join_target(&self, index: usize) -> Option<&VoiceChannel> {
        self.shown_rows.get(index).and_then(|row| row.join.as_ref())
    }

    /// Where a painted row's header stands, so a card about that person opens under it.
    pub fn row_rect(&self, index: usize) -> Option<Rect> {
        self.rows
            .iter()
            .find(|row| row.index == index)
            .map(|row| row.rect)
    }

    /// The stable person behind an openable row: the host toggles the expanded row by it, never by the row's place.
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

    /// What a press at `point` (on screen) means.
    pub fn hit_test(&self, point: Point) -> CollaboratorsHit {
        if !self.contains(point) {
            return CollaboratorsHit::Dismiss;
        }
        if self.invite.contains(point) {
            return CollaboratorsHit::Invite;
        }
        if self.pin.contains(point) {
            return CollaboratorsHit::Pin;
        }
        if !self.body.contains(point) {
            return CollaboratorsHit::Absorb;
        }
        let point = Point::new(point.x, point.y + self.scroll_offset());
        for place in &self.rows {
            if place.follow.contains(point) {
                return if self.is_followed(place.index) {
                    CollaboratorsHit::Unfollow(place.index)
                } else {
                    CollaboratorsHit::Follow(place.index)
                };
            }
            if place.wave.contains(point) {
                return CollaboratorsHit::Wave(place.index);
            }
            if place.join.contains(point) {
                return CollaboratorsHit::JoinCall(place.index);
            }
            if place.working_in.contains(point) {
                return CollaboratorsHit::OpenTab(place.index);
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

    /// Every control in reading order: Keep on screen in the header, each row's body, Wave and Follow, its changes link when open, then Invite.
    fn focus_order(&self) -> Vec<CollaboratorsHit> {
        let mut order = Vec::new();
        if self.pin.area() > 0.0 {
            order.push(CollaboratorsHit::Pin);
        }
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
            if place.working_in.area() > 0.0 {
                order.push(CollaboratorsHit::OpenTab(place.index));
            }
            if place.follow.area() > 0.0 {
                order.push(if self.is_followed(place.index) {
                    CollaboratorsHit::Unfollow(place.index)
                } else {
                    CollaboratorsHit::Follow(place.index)
                });
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

    /// Whether the person at `index` is the one this device follows now.
    fn is_followed(&self, index: usize) -> bool {
        self.shown_rows
            .get(index)
            .is_some_and(|row| self.shown_followed.as_deref() == Some(row.id.as_str()))
    }

    /// Where `hit` is, in content coordinates (Invite and Pin: on screen).
    fn rect_for_hit(&self, hit: CollaboratorsHit) -> Option<Rect> {
        let place = |index: usize| self.rows.iter().find(|place| place.index == index);
        match hit {
            CollaboratorsHit::Invite => Some(self.invite),
            CollaboratorsHit::Pin => Some(self.pin),
            CollaboratorsHit::Follow(index) | CollaboratorsHit::Unfollow(index) => {
                place(index).map(|place| place.follow)
            }
            CollaboratorsHit::Wave(index) => place(index).map(|place| place.wave),
            CollaboratorsHit::JoinCall(index) => place(index).map(|place| place.join),
            CollaboratorsHit::ToggleDetails(index) => place(index).map(|place| place.rect),
            CollaboratorsHit::OpenChanges(index) => place(index).map(|place| place.changes),
            CollaboratorsHit::OpenTab(index) => place(index).map(|place| place.working_in),
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
            .filter(|hit| !matches!(hit, CollaboratorsHit::Invite | CollaboratorsHit::Pin))
            .and_then(|hit| self.rect_for_hit(hit))
        else {
            return;
        };
        // In the bar's terms (the whole panel): the rect, with the panel's fixed parts below it,
        // fits the panel when the rect fits the list.
        let fixed = self.panel.height() - self.body.height();
        let rect = Affine::translate((-self.body.x0, -self.body.y0))
            .transform_rect_bbox(rect.inflate(3.0, 3.0));
        let content = Rect::new(rect.x0, rect.y0, rect.x1, rect.y1 + fixed);
        if let Some(scroll) = &mut self.scroll {
            scroll.scroll_into_view(content);
        }
    }

    /// Arrows, Tab, Home and End move through the controls; Enter or Space activates the focused one; Escape closes.
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

    /// What the wave on row `index` is called: sent, or to send.
    fn wave_name(&self, index: usize) -> String {
        let row = self.shown_rows.get(index);
        let name = row.map_or_else(String::new, |row| row.label.clone());
        let key = if row.is_some_and(|row| self.shown_waved.contains(&row.id)) {
            "a11y-shell-waved"
        } else {
            "a11y-shell-wave"
        };
        t_args(key, &[("name", name.into())])
    }

    /// The roster for assistive technology: who is here, each with where they are and what they are doing, then every control.
    pub fn accessibility_snapshot(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut children = vec![self.spoken_people()];

        for hit in self.focus_order() {
            let focused = self.keyboard_focus == Some(hit);
            let name_of = |index: usize| {
                self.shown_rows
                    .get(index)
                    .map_or_else(String::new, |row| row.label.clone())
            };
            let mut expanded = None;
            let (name, description) = match hit {
                CollaboratorsHit::Follow(index) => (
                    t_args("a11y-shell-follow", &[("name", name_of(index).into())]),
                    t("a11y-shell-follow-description"),
                ),
                CollaboratorsHit::Unfollow(index) => (
                    t_args(
                        "a11y-shell-stop-following",
                        &[("name", name_of(index).into())],
                    ),
                    t("a11y-shell-follow-description"),
                ),
                CollaboratorsHit::Wave(index) => {
                    (self.wave_name(index), t("a11y-shell-wave-description"))
                }
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
                // The window software works in, by what it is called here.
                CollaboratorsHit::OpenTab(index) => (
                    self.working_in_name(index),
                    t("a11y-shell-people-change-description"),
                ),
                CollaboratorsHit::Invite => {
                    (t("a11y-shell-invite"), t("a11y-shell-invite-description"))
                }
                CollaboratorsHit::Pin => {
                    if self.shown_pinned {
                        (t("people-panel-unpin"), t("a11y-people-unpin"))
                    } else {
                        (t("people-panel-pin"), t("a11y-people-pin"))
                    }
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
        AccessNode::new(Role::Dialog)
            .with_name(t("a11y-shell-people"))
            .with_description(format!(
                "{}. {}",
                self.shown_workspace,
                t("a11y-shell-people-description")
            ))
            .with_children(children)
    }

    /// What software's where-it-works line says, as a reader hears it: the window's name here.
    fn working_in_name(&self, index: usize) -> String {
        let Some(row) = self.shown_rows.get(index) else {
            return String::new();
        };
        match working_in_line(row) {
            (line, Some(record)) => format!("{line}, {record}"),
            (line, None) => line,
        }
    }

    /// [`Self::accessibility_snapshot`] with each control's bounds and its focus place as the reader's target (`people:<n>`), which [`Self::hit_for_target`] turns back into what it does.
    fn spoken_people(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, Role};
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
        AccessNode::new(Role::List)
            .with_name(t("a11y-shell-people-here-now"))
            .with_children(people)
    }

    pub fn accessibility_with_targets(&self) -> ui::a11y::AccessNode {
        let order = self.focus_order();
        let mut node = self.accessibility_snapshot().with_bounds(self.panel_rect());
        let controls = node
            .children
            .iter_mut()
            .skip(1)
            .filter(|child| child.role == ui::a11y::Role::Button);
        for (index, (child, hit)) in controls.zip(&order).enumerate() {
            let mut control =
                std::mem::replace(child, ui::a11y::AccessNode::new(ui::a11y::Role::Button))
                    .with_action_target(format!("people:{index}"));
            let rect = self.rect_for_hit(*hit).map(|rect| match hit {
                CollaboratorsHit::Invite | CollaboratorsHit::Pin => rect,
                _ => self.view_rect(rect),
            });
            if let Some(rect) = rect {
                control = control.with_bounds(rect);
            }
            *child = control;
        }
        node
    }

    /// What the control a reader pressed (`people:<n>`, from [`Self::accessibility_with_targets`]) does.
    #[must_use]
    pub fn hit_for_target(&self, target: &str) -> Option<CollaboratorsHit> {
        let index: usize = target.strip_prefix("people:")?.parse().ok()?;
        self.focus_order().get(index).copied()
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

/// What [`CollaboratorsOverlay::place_rows`] places: the groups' row indices and the open row's details as the layout measured them.
#[derive(Clone, Copy)]
struct RowsPlan<'p> {
    /// What stands under each row beyond its line, by row: a person's picture of their screen, a
    /// line saying why there is none, software's where-it-works line, or nothing.
    under: &'p [f64],
    groups: &'p [(&'static str, Vec<usize>)],
    /// Whether the sections are labelled: only where there is more than one to tell apart.
    labelled: bool,
    open_index: Option<usize>,
    shown_changes: usize,
    revealed: f64,
}

/// The widths of a row's text buttons, each its label plus its padding: Follow (wide enough for either of its labels), the changes link and Join.
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

    /// The width a row's trailing slot keeps: Follow and Wave (a `control` square), and Join where any row has a call to join (`any_join`).
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
mod tests;
