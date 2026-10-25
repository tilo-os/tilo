//! The people overlay: who is here, pinned out of the roster into a small
//! panel that floats over the windows.
//!
//! The roster dropdown ([`crate::collaborators`]) answers "who is here?"
//! once; pinned, the same rows stay in view while the person works. The
//! panel rests against an edge of the screen, wherever it was last dragged
//! to ([`dock`], the desk widgets' own edge mechanism), and never takes the keyboard: it only
//! answers the pointer over it.
//!
//! Each row is a face, a name and what the person is doing: their own
//! words first, else an automatic description of what they did (a
//! model's, or the host's plain reading), marked as such, with where they
//! are after it; else only where they are. Agents carry their tag and are
//! listed after the people.
//!
//! The person's own row leads the list ([`own_status`]): their face beside
//! a field with what they say they are doing, which they change right
//! there.
//!
//! Resting the pointer on a row (about a quarter second, so passing over
//! the list opens nothing) opens a peek beside the panel ([`peek`]): a
//! picture of their screen built from what their presence says, never a
//! stream of it, with Follow. Beside it a message box ([`composer`]) sends
//! them a direct message through Chat and shows the last few lines.
//!
//! The host supplies the rows (the roster's own, already filtered by what
//! each person lets others see), each person's screen and recent messages,
//! and applies the [`PeopleHit`]s: following goes through the host's one
//! follow mechanism, a message through Chat's own action.

mod composer;
mod dock;
mod own_status;
mod peek;
#[cfg(test)]
mod tests;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Size};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::Key;

use ui::icons::Icon;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::widget::{ButtonFace, ButtonRole, ChipStyle, FloatingPanelFace, ListRowFace, RowLeading};

use crate::collaborators::{LineSource, RosterRow};
use crate::i18n::{t, t_args};

pub use crate::desk_edges::DockEdge;

/// How long the pointer rests on a row before its peek opens: long enough
/// that passing over the list opens nothing, short enough to feel direct.
pub const HOVER_INTENT: Duration = Duration::from_millis(250);
/// How long the peek stays after the pointer left the row and the peek, so
/// a path from one to the other that clips the gap does not close it.
const LEAVE_GRACE: Duration = Duration::from_millis(220);
/// A face in a row.
const FACE: f64 = 28.0;

/// What a person's screen looks like, as their presence says: the windows
/// on the part of their workspace in front of them.
#[derive(Clone, Debug, PartialEq)]
pub struct PeerScreen {
    /// The roster id of the person.
    pub id: String,
    /// Their screen's width over its height.
    pub aspect: f64,
    pub windows: Vec<PeerWindow>,
}

/// One window on someone's screen.
#[derive(Clone, Debug, PartialEq)]
pub struct PeerWindow {
    /// Where it is on their screen, in the unit square.
    pub rect: Rect,
    /// The app's id, for its icon.
    pub app: String,
    /// The app's name.
    pub app_name: String,
    /// What it shows, when that is shared with this person: its title.
    /// `None` for a window on something not shared with them, which shows
    /// as its app and nothing more.
    pub title: Option<String>,
    /// The window on this device showing the same thing, when there is
    /// one: its live picture stands for theirs.
    pub picture: Option<u64>,
    /// The window in front of them.
    pub focused: bool,
}

impl PeerWindow {
    /// Whether what this window shows is shared with this person.
    #[must_use]
    pub fn shared(&self) -> bool {
        self.title.is_some() || self.picture.is_some()
    }
}

/// The last lines of a direct conversation with someone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerMessages {
    /// The roster id of the person.
    pub id: String,
    pub lines: Vec<MessageLine>,
}

/// One line of a direct conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageLine {
    /// This person wrote it.
    pub mine: bool,
    pub text: String,
}

/// What a peek showed of a window's live picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pictured {
    /// None to show: the window shows as its app.
    No,
    /// The window as it looks now.
    Live,
    /// The window as it looked when this device last showed it: it has
    /// been off screen since, and the peek marks it so.
    AsLastSeen,
}

/// Paints this device's live picture of a window ([`PeerWindow::picture`])
/// fitted into a rect, and says what it showed.
pub trait PeekPictures {
    fn paint(&mut self, window: u64, scene: &mut Scene, transform: Affine, rect: Rect) -> Pictured;
}

/// The live pictures the strip keeps of this device's windows, by window
/// group ([`ui::window_pictures`]): what every host's peek shows.
pub struct LivePictures;

impl PeekPictures for LivePictures {
    fn paint(&mut self, window: u64, scene: &mut Scene, transform: Affine, rect: Rect) -> Pictured {
        ui::window_pictures::with_picture(window, |picture| {
            let painted = ui::window_pictures::paint_fitted(picture, scene, transform, rect);
            if painted.area() <= 0.0 {
                Pictured::No
            } else if picture.current {
                Pictured::Live
            } else {
                Pictured::AsLastSeen
            }
        })
        .unwrap_or(Pictured::No)
    }
}

/// A host with no pictures: every window shows as its app.
pub struct NoPictures;

impl PeekPictures for NoPictures {
    fn paint(&mut self, _: u64, _: &mut Scene, _: Affine, _: Rect) -> Pictured {
        Pictured::No
    }
}

/// What the overlay shows this frame, decided by the host.
pub struct PeopleOverlayModel<'a> {
    /// The area the overlay floats in: the screen under the bar.
    pub host: Rect,
    /// What already waits at the edges (desk widgets, a call card): the
    /// panel steps past them as a widget does.
    pub occupied: &'a [Rect],
    /// The roster's rows, as the dropdown has them.
    pub rows: &'a [RosterRow],
    pub screens: &'a [PeerScreen],
    pub messages: &'a [PeerMessages],
    /// Whom this person follows, by roster id.
    pub followed_id: Option<&'a str>,
    /// Why a direct message cannot be sent from here, when it cannot; the
    /// message box says so instead of offering to send.
    pub messaging_unavailable: Option<&'a str>,
    /// This person, as everyone here reads them (their status their own
    /// words, or what is said of them automatically): their row leads the
    /// list, with a field to say what they are doing. `None` lists only
    /// the others.
    pub own: Option<&'a RosterRow>,
}

/// What a pointer press or a key on the overlay means to the host.
#[derive(Clone, Debug, PartialEq)]
pub enum PeopleHit {
    /// Follow the person with this roster id, or stop following them.
    ToggleFollow(String),
    /// Send `text` to the person with this roster id as a direct message.
    Send { to: String, text: String },
    /// Close the overlay: the roster goes back to the bar.
    Unpin,
    /// The panel was dragged to another edge; keep it where the desk
    /// widgets keep theirs.
    Moved(DockEdge),
    /// Say this in your own words as what you are doing; empty clears it.
    SetStatus(String),
}

/// One painted row.
#[derive(Clone, Debug)]
struct RowPlace {
    id: String,
    rect: Rect,
    follow: Rect,
}

/// The pinned people overlay: geometry and interaction across frames.
pub struct PeopleOverlay {
    dock: dock::Dock,
    /// The panel as drawn this frame.
    panel: Rect,
    header: Rect,
    unpin: Rect,
    rows: Vec<RowPlace>,
    headings: Vec<(String, Rect)>,
    /// The person the pointer rests on, and since when.
    hover: Option<(String, Instant)>,
    peek: peek::Peek,
    composer: composer::Composer,
    /// The person's own row: where it is, and the field saying what they
    /// are doing.
    own_row: Rect,
    own_status: own_status::OwnStatus,
    /// When the pointer left the row, peek and message box.
    left_at: Option<Instant>,
    cursor: Option<Point>,
    /// The overlay arriving and leaving.
    shown: ui::motion::Presence,
    targets: ui::widget::PaintedTargets,
    motion: ui::tokens::EffectiveMotion,
    /// What the last frame showed, for the host and assistive technology.
    shown_rows: Vec<RosterRow>,
    shown_followed: Option<String>,
}

impl std::fmt::Debug for PeopleOverlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeopleOverlay")
            .field("panel", &self.panel)
            .field("rows", &self.rows.len())
            .field("peek", &self.peek.person())
            .finish_non_exhaustive()
    }
}

impl Default for PeopleOverlay {
    fn default() -> Self {
        Self::new(DockEdge::Right)
    }
}

/// What a row says under a person's name: what they are doing, where they
/// are when that adds something, and whether software wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowLine {
    /// A call they are in, their own words, else an automatic description;
    /// else only where they are.
    pub doing: String,
    /// Where they are, under what they are doing: only with a description
    /// that does not already say it.
    pub place: Option<String>,
    /// Software wrote `doing` (a model's line or the host's reading), which
    /// the row marks so it never passes for their own words.
    pub automatic: bool,
}

/// The line under a person's name as a row shows it.
#[must_use]
pub fn row_line(row: &RosterRow) -> Option<RowLine> {
    let automatic = crate::collaborators::says_automatically(row);
    let (doing, _) = doing(row)?;
    let describes = row.call.is_some()
        || row
            .status
            .as_ref()
            .is_some_and(|status| !status.text.trim().is_empty());
    let place = row
        .activity
        .as_deref()
        .map(str::trim)
        .filter(|place| describes && !place.is_empty() && *place != doing)
        .map(str::to_owned);
    Some(RowLine {
        doing,
        place,
        automatic,
    })
}

/// The line under a person's name: a call they are in, else their own
/// words, else what a model wrote of what they did, else where they are.
/// With who wrote it, so a model's line is never shown as their own.
#[must_use]
pub fn doing(row: &RosterRow) -> Option<(String, LineSource)> {
    if let Some(call) = &row.call {
        let text = if call.trim().is_empty() {
            t("people-panel-in-a-call")
        } else {
            t_args("people-panel-in-call", &[("channel", call.clone().into())])
        };
        return Some((text, LineSource::Observed));
    }
    if let Some(status) = row
        .status
        .as_ref()
        .filter(|status| !status.text.trim().is_empty())
    {
        return Some((status.text.clone(), status.source));
    }
    row.activity
        .as_ref()
        .filter(|activity| !activity.trim().is_empty())
        .map(|activity| (activity.clone(), LineSource::Observed))
}

impl PeopleOverlay {
    /// An overlay waiting at `edge` (as the person's settings keep it).
    #[must_use]
    pub fn new(edge: DockEdge) -> Self {
        Self {
            dock: dock::Dock::new(edge),
            panel: Rect::ZERO,
            header: Rect::ZERO,
            unpin: Rect::ZERO,
            rows: Vec::new(),
            headings: Vec::new(),
            hover: None,
            peek: peek::Peek::default(),
            composer: composer::Composer::default(),
            own_row: Rect::ZERO,
            own_status: own_status::OwnStatus::default(),
            left_at: None,
            cursor: None,
            shown: ui::motion::Presence::closed(),
            targets: ui::widget::PaintedTargets::default(),
            motion: ui::tokens::EffectiveMotion::default(),
            shown_rows: Vec::new(),
            shown_followed: None,
        }
    }

    /// The edge the overlay waits at.
    #[must_use]
    pub fn edge(&self) -> DockEdge {
        self.dock.edge()
    }

    /// Wait at `edge` from now on (as the person's settings keep it),
    /// without a motion.
    pub fn set_edge(&mut self, edge: DockEdge) {
        self.dock.set_edge(edge);
    }

    /// Show the overlay (it arrives) or let it go (it leaves).
    pub fn show(&mut self, open: bool) {
        if open == self.shown.is_open() {
            return;
        }
        let now = ui::time::now();
        self.shown.show(
            open,
            ui::motion::Appear::SPEC,
            self.motion,
            ui::motion::input_source(),
            now,
        );
        if !open {
            self.close_peek();
        }
        if let Some(until) = self.shown.settles_at() {
            ui::motion::want_frames_until(until);
        }
    }

    /// Whether the overlay is shown or still leaving.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.shown.is_visible(ui::motion::frame_now())
    }

    /// The person whose peek is open, by roster id: the host reads their
    /// recent messages for the message box.
    #[must_use]
    pub fn peeked(&self) -> Option<&str> {
        self.peek.person()
    }

    /// Whether the message box or the field of what this person is doing
    /// holds the keyboard.
    #[must_use]
    pub fn composing(&self) -> bool {
        self.composer.is_focused() || self.own_status.is_focused()
    }

    /// Whether `point` is on the overlay, its peek or its message box.
    #[must_use]
    pub fn contains(&self, point: Point) -> bool {
        self.is_visible()
            && (self.panel.contains(point)
                || self.peek.covers(point)
                || self.composer.covers(point))
    }

    pub fn wants_pointer_capture(&self) -> bool {
        self.dock.is_held() || self.targets.wants_pointer_capture()
    }

    /// Whether anything is moving: the overlay arriving, the panel settling
    /// on its edge, the peek opening.
    #[must_use]
    pub fn needs_animation_frame(&self) -> bool {
        let now = ui::motion::frame_now();
        self.shown.is_moving(now)
            || self.dock.is_moving(now)
            || self.peek.is_moving(now)
            // A peek's windows wait for their first live pictures.
            || (self.peek.person().is_some() && ui::window_pictures::owes_any())
    }

    /// When the overlay next needs drawing without input: a pointer at
    /// rest on a row opens its peek then, one that left closes it then.
    #[must_use]
    pub fn next_wake_deadline(&self) -> Option<Instant> {
        let open_at = self
            .hover
            .as_ref()
            .filter(|(id, _)| self.peek.person() != Some(id.as_str()))
            .and_then(|(_, since)| since.checked_add(HOVER_INTENT));
        let close_at = self.left_at.and_then(|at| at.checked_add(LEAVE_GRACE));
        [
            open_at,
            close_at,
            self.dock.settles_at(),
            self.peek.settles_at(),
            self.composer.next_wake_deadline(),
            self.own_status.next_wake_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn close_peek(&mut self) {
        self.peek.close(self.motion);
        self.composer.blur();
        self.hover = None;
        self.left_at = None;
    }

    /// Paint the overlay for `model`, `pictures` drawing the live pictures
    /// of windows shared with this person.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &PeopleOverlayModel<'_>,
        pictures: &mut dyn PeekPictures,
    ) {
        self.motion = theme.motion();
        let now = ui::motion::frame_now();
        self.shown_rows = model.rows.to_vec();
        self.shown_followed = model.followed_id.map(str::to_owned);
        if !self.is_visible() {
            self.panel = Rect::ZERO;
            self.rows.clear();
            self.targets.begin_frame();
            self.targets.end_frame();
            return;
        }
        self.settle_hover(model, now);
        let said = model
            .own
            .and_then(|own| own.status.as_ref())
            .filter(|status| status.source == LineSource::Written)
            .map_or("", |status| status.text.as_str());
        self.own_status.sync(said);
        self.layout(text, theme, model, now);
        let arrive = self.shown.progress(now);
        let pose = ui::motion::Appear::pose(
            arrive,
            ui::motion::Appear::origin(self.panel, self.panel.center()),
            self.motion,
        );
        let placed = pose.applied_to(transform);
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            pose.opacity,
            placed,
            &ui::widget::floating_surface::painted_bounds(self.panel, theme),
        );
        self.paint_panel(scene, placed, text, theme, model);
        scene.pop_layer();
        let anchor = self
            .peek
            .person()
            .and_then(|id| self.rows.iter().find(|row| row.id == id))
            .map(|row| row.rect);
        if let (Some(id), Some(anchor)) = (self.peek.person().map(str::to_owned), anchor) {
            let row = model.rows.iter().find(|row| row.id == id);
            let screen = model.screens.iter().find(|screen| screen.id == id);
            let messages = model
                .messages
                .iter()
                .find(|messages| messages.id == id)
                .map_or(&[][..], |messages| messages.lines.as_slice());
            if let Some(row) = row {
                let following = model.followed_id == Some(id.as_str());
                let card = self.peek.draw(
                    scene,
                    transform,
                    text,
                    theme,
                    &peek::PeekModel {
                        host: model.host,
                        panel: self.panel,
                        anchor,
                        edge: self.dock.edge(),
                        row,
                        screen,
                        following,
                        cursor: self.cursor,
                    },
                    pictures,
                );
                self.composer.draw(
                    scene,
                    transform,
                    text,
                    theme,
                    &composer::ComposerModel {
                        host: model.host,
                        peek: card,
                        opening: self.peek.openness(now),
                        name: &row.label,
                        lines: messages,
                        unavailable: model.messaging_unavailable,
                        cursor: self.cursor,
                    },
                );
            }
        } else if self.peek.person().is_some() {
            // Their row is gone: so is their peek.
            self.close_peek();
        }
        self.register_targets();
    }

    /// Open the peek of the person the pointer has rested on long enough;
    /// close it once the pointer has been away long enough.
    fn settle_hover(&mut self, model: &PeopleOverlayModel<'_>, now: Instant) {
        if self.dock.is_dragging() {
            return;
        }
        if let Some((id, since)) = &self.hover
            && self.peek.person() != Some(id.as_str())
            && now.saturating_duration_since(*since) >= HOVER_INTENT
            && model.rows.iter().any(|row| row.id == *id)
        {
            let id = id.clone();
            self.open_peek(&id);
        }
        if let Some(left) = self.left_at
            && now.saturating_duration_since(left) >= LEAVE_GRACE
            && !self.composer.holds_peek()
        {
            self.close_peek();
        }
    }

    fn open_peek(&mut self, id: &str) {
        let now = ui::time::now();
        if self.peek.person() != Some(id) {
            self.composer.reset_for(id);
        }
        self.peek.open(id, self.motion, now);
        self.left_at = None;
    }

    /// Where every part of the panel goes this frame.
    fn layout(
        &mut self,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &PeopleOverlayModel<'_>,
        now: Instant,
    ) {
        let metrics = &theme.tokens.metrics;
        let width = (f64::from(metrics.panel_width_utility) * 0.8).round();
        let control = f64::from(metrics.bar_control_height);
        let heading_h = f64::from(metrics.row_single) * 0.75;
        let row_h = Self::row_height(text, theme);
        let placed_h = Self::placed_row_height(text, theme);
        let height_of = |index: usize| {
            let placed = model
                .rows
                .get(index)
                .and_then(row_line)
                .is_some_and(|line| line.place.is_some());
            if placed { placed_h } else { row_h }
        };
        let (people, agents) = groups(model.rows);
        let own_h = Self::own_height(theme, row_h);
        let mut body_h: f64 = people.iter().map(|&index| height_of(index)).sum();
        if !agents.is_empty() {
            body_h += heading_h + agents.iter().map(|&index| height_of(index)).sum::<f64>();
        }
        if people.is_empty() && agents.is_empty() {
            body_h = row_h;
        }
        if model.own.is_some() {
            body_h += own_h;
        }
        let face = FloatingPanelFace::new().header(control).body_empty(false);
        let margin = crate::desk_edges::MARGIN;
        let room = (model.host.height() - 2.0 * margin).max(row_h);
        let height = face.height_for(body_h, theme).min(room);
        let panel = self.dock.frame(
            model.host,
            Size::new(width, height),
            model.occupied,
            self.motion,
            now,
        );
        self.panel = panel;
        let parts = face.layout(panel, theme);
        self.header = parts.header;
        self.unpin = Rect::new(
            parts.header.x1 - control,
            parts.header.center().y - control / 2.0,
            parts.header.x1,
            parts.header.center().y + control / 2.0,
        );
        let bleed = ui::chrome::row_bleed(theme);
        let follow_w = control;
        self.rows.clear();
        self.headings.clear();
        let mut y = parts.body.y0;
        self.own_row = if model.own.is_some() {
            let own = Rect::new(parts.body.x0 - bleed, y, parts.body.x1 + bleed, y + own_h);
            y += own_h;
            own
        } else {
            Rect::ZERO
        };
        let place_rows = |rows: &[usize], this: &mut Self, y: &mut f64| {
            for &index in rows {
                let Some(row) = model.rows.get(index) else {
                    continue;
                };
                let row_h = height_of(index);
                if *y + row_h > parts.body.y1 + 0.5 {
                    break;
                }
                let rect = Rect::new(parts.body.x0 - bleed, *y, parts.body.x1 + bleed, *y + row_h);
                let follow = if row.followable {
                    Rect::new(
                        parts.body.x1 - follow_w,
                        rect.center().y - control / 2.0,
                        parts.body.x1,
                        rect.center().y + control / 2.0,
                    )
                } else {
                    Rect::ZERO
                };
                this.rows.push(RowPlace {
                    id: row.id.clone(),
                    rect,
                    follow,
                });
                *y += row_h;
            }
        };
        place_rows(&people, self, &mut y);
        if !agents.is_empty() && y + heading_h + row_h <= parts.body.y1 + 0.5 {
            self.headings.push((
                t("people-panel-section-agents"),
                Rect::new(parts.body.x0, y, parts.body.x1, y + heading_h),
            ));
            y += heading_h;
            place_rows(&agents, self, &mut y);
        }
    }

    /// The person's own row: their face beside the field, with room above
    /// and below it.
    fn own_height(theme: &Theme, row_h: f64) -> f64 {
        (own_status::height(theme) + 2.0 * f64::from(theme.tokens.spacing.s2)).max(row_h)
    }

    fn row_height(text: &mut TextRenderer, theme: &Theme) -> f64 {
        ListRowFace::new("Ag")
            .secondary("Ag")
            .leading_column(FACE)
            .height(text, theme)
    }

    /// A row that says what someone is doing and, under it, where they are.
    fn placed_row_height(text: &mut TextRenderer, theme: &Theme) -> f64 {
        ListRowFace::new("Ag")
            .subtitle("Ag")
            .secondary("Ag")
            .leading_column(FACE)
            .height(text, theme)
    }

    fn paint_panel(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &PeopleOverlayModel<'_>,
    ) {
        let control = f64::from(theme.tokens.metrics.bar_control_height);
        let face = FloatingPanelFace::new().header(control).body_empty(false);
        let (_, inner) = face.paint(scene, transform, self.panel, 1.0, theme);
        let theme = &inner;
        let colors = &theme.tokens.colors;
        self.paint_header(scene, transform, text, theme, model);
        for (heading, rect) in &self.headings {
            let style = theme
                .text(ui::TextRole::Caption)
                .with_weight(ui::text_render::TextWeight::Medium);
            let (_, h) = text.measure_style(heading, None, &style);
            text.draw_style(
                scene,
                heading,
                colors.text_dim,
                transform.pre_translate((rect.x0, rect.center().y - f64::from(h) / 2.0).into()),
                None,
                &style,
            );
        }
        if let Some(own) = model.own {
            self.paint_own(scene, transform, text, theme, own);
        }
        if self.rows.is_empty() {
            let empty = t("people-panel-empty");
            let style = theme.text(ui::TextRole::Caption);
            let (w, h) = text.measure_style(&empty, None, &style);
            let top = self.header.y1.max(self.own_row.y1);
            let body = Rect::new(self.panel.x0, top, self.panel.x1, self.panel.y1);
            text.draw_style(
                scene,
                &empty,
                colors.text_dim,
                transform.pre_translate(
                    (
                        body.center().x - f64::from(w) / 2.0,
                        body.center().y - f64::from(h) / 2.0,
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
        for place in self.rows.clone() {
            let Some(row) = model.rows.iter().find(|row| row.id == place.id) else {
                continue;
            };
            self.paint_row(
                scene,
                transform,
                text,
                theme,
                (row, &place),
                model.followed_id == Some(row.id.as_str()),
                (&agent_tag, &bot_tag, &follow_label, &stop_label),
            );
        }
    }

    /// The header: a grip saying the panel moves, how many are here, and
    /// the way back to the bar.
    fn paint_header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &PeopleOverlayModel<'_>,
    ) {
        let colors = &theme.tokens.colors;
        let present = model.rows.iter().filter(|row| !row.invited).count();
        let title = t_args("people-overlay-title", &[("count", present.into())]);
        let style = theme
            .text(ui::TextRole::Label)
            .with_weight(ui::text_render::TextWeight::Medium);
        let (_, h) = text.measure_style(&title, None, &style);
        // A grip of dots says the panel can be moved.
        let grip = Rect::new(
            self.header.x0,
            self.header.center().y - 6.0,
            self.header.x0 + 8.0,
            self.header.center().y + 6.0,
        );
        for (dx, dy) in [
            (2.0, 2.0),
            (6.0, 2.0),
            (2.0, 6.0),
            (6.0, 6.0),
            (2.0, 10.0),
            (6.0, 10.0),
        ] {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                colors.text_tertiary,
                None,
                &vello::kurbo::Circle::new((grip.x0 + dx, grip.y0 + dy), 1.1),
            );
        }
        text.draw_style(
            scene,
            &title,
            colors.text_dim,
            transform.pre_translate(
                (
                    grip.x1 + f64::from(theme.tokens.spacing.s2),
                    self.header.center().y - f64::from(h) / 2.0,
                )
                    .into(),
            ),
            None,
            &style,
        );
        let unpin_label = t("people-overlay-unpin");
        ui::widget::Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::icon_only(self.unpin, Icon::X, &unpin_label)
                .role(ButtonRole::Plain)
                .hovered(self.cursor.is_some_and(|p| self.unpin.contains(p))),
        );
    }

    /// The person's own row: their face, and the field saying what they
    /// are doing.
    fn paint_own(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        own: &RosterRow,
    ) {
        let bleed = ui::chrome::row_bleed(theme);
        let inner = self.own_row.inset((-bleed, 0.0));
        let face_rect = Rect::new(inner.x0, inner.y0, inner.x0 + FACE, inner.y1);
        let mut face = ui::presence::Face::new(&own.label, &own.id, own.color);
        face.dimmed = own.availability.dims();
        ui::presence::FaceStack {
            side: FACE - 6.0,
            overlap: 0.0,
            max: 1,
        }
        .paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
            (face_rect.center().x + FACE / 2.0, face_rect.center().y),
            (&[face], 0),
            ui::widget::SurfaceLevel::Floating.color(theme),
        );
        let gap = f64::from(theme.tokens.spacing.s2);
        let field_h = own_status::height(theme);
        let field = Rect::new(
            face_rect.x1 + gap,
            inner.center().y - field_h / 2.0,
            inner.x1,
            inner.center().y + field_h / 2.0,
        );
        self.own_status
            .draw(scene, transform, text, theme, field, self.cursor);
    }

    fn paint_row(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        (row, place): (&RosterRow, &RowPlace),
        following: bool,
        (agent_tag, bot_tag, follow_label, stop_label): (&str, &str, &str, &str),
    ) {
        let hovered = self.cursor.is_some_and(|p| place.rect.contains(p));
        let peeked = self.peek.person() == Some(row.id.as_str());
        let line = row_line(row);
        let paint_face = |ctx: &mut ui::paint::PaintCtx<'_>, rect: Rect| {
            let mut face = ui::presence::Face::new(&row.label, &row.id, row.color);
            face.is_agent = row.is_agent;
            face.dimmed = row.availability.dims();
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
        let tags = [(
            if row.is_bot { bot_tag } else { agent_tag },
            ChipStyle::Neutral,
        )];
        let mut face = ListRowFace::new(&row.label)
            .leading(RowLeading::Paint(&paint_face))
            .leading_column(FACE)
            .trailing_width(if place.follow.area() > 0.0 {
                place.follow.width()
            } else {
                0.0
            })
            .selected(peeked)
            .interaction(ui::widget::Interaction {
                hovered: hovered || peeked,
                pressed: false,
                focused: false,
                disabled: false,
            });
        if row.is_agent {
            face = face.tags(&tags);
        }
        if let Some(line) = &line {
            face = match &line.place {
                // Where they are stays a quieter line of its own under
                // what they are doing.
                Some(place) => face.subtitle(&line.doing).secondary(place),
                None => face.secondary(&line.doing),
            };
            // Software's words about someone are marked as such: never
            // shown as if they had said them.
            if line.automatic {
                face = face.mark(Icon::Sparkles);
            }
        }
        face.paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text, theme),
            place.rect,
        );
        // Following someone shows on their row without a hover; Follow
        // itself waits for the pointer, so the list stays quiet.
        if place.follow.area() > 0.0 && (hovered || following || peeked) {
            ui::widget::Button::paint(
                scene,
                transform,
                text,
                theme,
                &ButtonFace::icon_only(
                    place.follow,
                    if following { Icon::EyeOff } else { Icon::Eye },
                    if following { stop_label } else { follow_label },
                )
                .role(ButtonRole::Plain)
                .toggled(following)
                .hovered(self.cursor.is_some_and(|p| place.follow.contains(p))),
            );
        }
        if following {
            // Their colour beside their name: whose view this is.
            let mark = Rect::new(
                place.rect.x0 + 1.0,
                place.rect.y0 + 10.0,
                place.rect.x0 + 3.0,
                place.rect.y1 - 10.0,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                row.color,
                None,
                &vello::kurbo::RoundedRect::from_rect(mark, 1.0),
            );
        }
    }

    /// The targets this frame painted, for presses and assistive
    /// technology.
    fn register_targets(&mut self) {
        self.targets.begin_frame();
        self.targets
            .target("unpin", self.unpin, t("people-overlay-unpin"));
        if self.own_row.area() > 0.0 {
            self.targets.target(
                "own-status",
                self.own_status.field,
                t("people-overlay-own-status"),
            );
            if let Some(clear) = self.own_status.clear_rect() {
                self.targets
                    .target("own-clear", clear, t("people-overlay-own-status-clear"));
            }
        }
        for place in &self.rows {
            let label = self
                .shown_rows
                .iter()
                .find(|row| row.id == place.id)
                .map_or_else(String::new, |row| row.label.clone());
            self.targets
                .target(format!("row:{}", place.id), place.rect, label.clone());
            if place.follow.area() > 0.0 {
                let following = self.shown_followed.as_deref() == Some(place.id.as_str());
                let name = if following {
                    t_args("a11y-people-stop-following", &[("name", label.into())])
                } else {
                    t_args("a11y-people-follow", &[("name", label.into())])
                };
                self.targets
                    .target(format!("follow:{}", place.id), place.follow, name);
            }
        }
        if let Some(id) = self.peek.person() {
            for (target, rect) in self.peek.targets() {
                self.targets
                    .target(format!("peek-{target}:{id}"), rect, String::new());
            }
            if let Some(rect) = self.composer.send_rect() {
                self.targets
                    .target(format!("send:{id}"), rect, t("people-overlay-send"));
            }
        }
        self.targets.end_frame();
    }

    /// Text an input method committed while the message box holds the
    /// keyboard.
    pub fn commit_text(&mut self, text: &str) -> bool {
        self.own_status.commit_text(text) || self.composer.commit_text(text)
    }

    /// A key while the message box holds the keyboard. Escape closes the
    /// peek; Enter sends. `None` when the key is not the overlay's.
    pub fn key(
        &mut self,
        key: &Key,
        mods: winit::keyboard::ModifiersState,
    ) -> Option<Option<PeopleHit>> {
        if self.own_status.is_focused() {
            return match self.own_status.key(key, mods) {
                own_status::Keyed::Ignored => None,
                own_status::Keyed::Consumed | own_status::Keyed::Cancel => Some(None),
                own_status::Keyed::Save(words) => Some(Some(PeopleHit::SetStatus(words))),
            };
        }
        if !self.composer.is_focused() {
            return None;
        }
        match self.composer.key(key, mods) {
            composer::Keyed::Ignored => None,
            composer::Keyed::Consumed => Some(None),
            composer::Keyed::Cancel => {
                self.close_peek();
                Some(None)
            }
            composer::Keyed::Send(text) => {
                let to = self.peek.person()?.to_owned();
                Some(Some(PeopleHit::Send { to, text }))
            }
        }
    }

    /// The message went: clear the box, keep it open for the next one.
    pub fn sent(&mut self) {
        self.composer.clear();
    }

    /// Route `event` (in logical points through `scale`). Marks it handled
    /// when the pointer is over the overlay; never takes the keyboard
    /// unless the message box was pressed.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        handled: &mut bool,
    ) -> Option<PeopleHit> {
        if !self.is_visible() {
            return None;
        }
        let now = ui::time::now();
        let mut hit = None;
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = Point::new(position.x / scale, position.y / scale);
                self.cursor = Some(point);
                if self.dock.is_held() {
                    if self.dock.drag(point) {
                        self.targets.cancel_pointer_press();
                        self.hover = None;
                        if self.peek.person().is_some() && !self.composer.holds_peek() {
                            self.close_peek();
                        }
                    }
                    *handled = true;
                } else {
                    self.track_hover(point, now);
                }
            }
            WindowEvent::CursorLeft { .. } | WindowEvent::Focused(false) => {
                self.cursor = None;
                self.dock.cancel();
                self.hover = None;
                if self.peek.person().is_some() && !self.composer.holds_peek() {
                    self.left_at.get_or_insert(now);
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let point = self.cursor;
                match state {
                    ElementState::Pressed => {
                        if let Some(point) = point {
                            hit = self.press(point, handled);
                        }
                    }
                    ElementState::Released => {
                        if self.dock.is_held() {
                            *handled = true;
                            if let Some(edge) = point.and_then(|point| self.dock.release(point)) {
                                self.targets.cancel_pointer_press();
                                hit = Some(PeopleHit::Moved(edge));
                            } else {
                                self.dock.cancel();
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        let mut pointer_handled = false;
        self.targets
            .handle_pointer_event(event, scale, &mut pointer_handled);
        *handled |= pointer_handled;
        if hit.is_none() {
            hit = self.take_activated();
        }
        if matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
        ) && self.cursor.is_some_and(|point| self.contains(point))
        {
            *handled = true;
        }
        hit
    }

    /// A press at `point`: on the message box it takes the keyboard; on the
    /// panel it may start a drag; anywhere else it lets go of the message
    /// box (and with it the peek, unless the pointer is on it).
    fn press(&mut self, point: Point, handled: &mut bool) -> Option<PeopleHit> {
        if self.own_row.area() > 0.0 && self.own_status.field_contains(point) {
            self.composer.blur();
            self.own_status.focus();
            *handled = true;
            return None;
        }
        // Anywhere else lets go of the field, putting back what was said.
        if self.own_status.is_focused()
            && !self
                .own_status
                .clear_rect()
                .is_some_and(|clear| clear.contains(point))
        {
            self.own_status.cancel();
        }
        if self.composer.field_contains(point) {
            self.composer.focus();
            *handled = true;
            return None;
        }
        if self.panel.contains(point) {
            let on_control = self.unpin.contains(point)
                || self.own_row.contains(point)
                || self.rows.iter().any(|row| row.follow.contains(point));
            if !on_control {
                self.dock.press(point);
            }
            *handled = true;
            return None;
        }
        if self.peek.covers(point) || self.composer.covers(point) {
            *handled = true;
            return None;
        }
        // A press elsewhere is the person going back to their work.
        if self.peek.person().is_some() {
            self.close_peek();
        }
        None
    }

    /// The pointer moved to `point`: the row it rests on starts its hover
    /// intent; leaving the row, peek and message box starts the grace.
    fn track_hover(&mut self, point: Point, now: Instant) {
        let over_row = self
            .rows
            .iter()
            .find(|row| row.rect.contains(point))
            .map(|row| row.id.clone());
        let on_peek = self.peek.covers(point) || self.composer.covers(point);
        match &over_row {
            Some(id) => {
                if self.hover.as_ref().is_none_or(|(hovered, _)| hovered != id) {
                    self.hover = Some((id.clone(), now));
                    if let Some(until) = now.checked_add(HOVER_INTENT) {
                        ui::motion::want_frames_until(until);
                    }
                }
            }
            None => self.hover = None,
        }
        let near_peeked = over_row
            .as_deref()
            .is_some_and(|id| self.peek.person() == Some(id));
        if on_peek || near_peeked || over_row.is_some() {
            self.left_at = None;
        } else if self.peek.person().is_some() && !self.composer.holds_peek() {
            self.left_at.get_or_insert(now);
        }
    }

    fn take_activated(&mut self) -> Option<PeopleHit> {
        let activated = self.targets.take_activated();
        for id in activated {
            if id == "unpin" {
                return Some(PeopleHit::Unpin);
            }
            if id == "own-clear" {
                self.own_status.cleared();
                return Some(PeopleHit::SetStatus(String::new()));
            }
            if id == "own-status" {
                self.composer.blur();
                self.own_status.focus();
                continue;
            }
            if let Some(person) = id.strip_prefix("follow:") {
                return Some(PeopleHit::ToggleFollow(person.to_owned()));
            }
            if let Some(person) = id.strip_prefix("peek-follow:") {
                return Some(PeopleHit::ToggleFollow(person.to_owned()));
            }
            if let Some(person) = id.strip_prefix("row:") {
                // A click opens the peek at once: a touch has no hover.
                let person = person.to_owned();
                self.open_peek(&person);
                continue;
            }
            if let Some(person) = id.strip_prefix("send:")
                && let Some(text) = self.composer.text_to_send()
            {
                return Some(PeopleHit::Send {
                    to: person.to_owned(),
                    text,
                });
            }
        }
        None
    }

    /// The painted targets, for assistive technology.
    #[must_use]
    pub fn accessibility_children(&self) -> Vec<ui::a11y::AccessNode> {
        self.targets.accessibility_children()
    }

    /// What the last frame showed of the rows.
    #[must_use]
    pub fn shown_rows(&self) -> &[RosterRow] {
        &self.shown_rows
    }

    /// The panel as drawn.
    #[must_use]
    pub fn panel_rect(&self) -> Rect {
        self.panel
    }
}

/// The present rows' indices: people, then agents.
fn groups(rows: &[RosterRow]) -> (Vec<usize>, Vec<usize>) {
    let mut people = Vec::new();
    let mut agents = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if row.invited || row.own_device {
            continue;
        }
        if row.is_agent {
            agents.push(index);
        } else {
            people.push(index);
        }
    }
    (people, agents)
}
