//! The strip switcher: the strip ribbon grown into labels when the pointer
//! means it.
//!
//! It has one job, the strips of the workspace shown. At rest it is the
//! ribbon on the content's right edge: one small mark per strip, the focused
//! one long, and the face of whoever else is on a strip beside its mark.
//! Reaching for it lays each strip's name out as a label level with its mark
//! (no panel around them: the labels are the switcher), with the faces of the
//! people on it, and a "New strip" label under the last. Workspaces are not
//! here; they are switched, made and joined from the workspace menu.
//!
//! - A wheel or two-finger scroll over the ribbon or its labels moves to the
//!   strip above or below.
//! - A click on a mark or a label moves to that strip.
//! - A double click on a label renames its strip in place; "New strip" makes
//!   a strip, moves to it and puts its name in place for typing.
//!
//! Opening follows aim, not contact ([`EdgeAim`]): a pointer that comes to
//! rest on the ribbon opens the labels, one sweeping past the screen edge on
//! its way somewhere else does not, and a label lights under the pointer only
//! once it settles there. The labels go the moment the pointer leaves them,
//! unless a name is being typed: a name field never lives in a surface that
//! hover alone keeps up, so naming pins the labels until Enter, Escape or a
//! press elsewhere. Super+S opens them for the keyboard: arrows choose,
//! Enter takes, F2 renames, Escape closes.
//!
//! The state is pure: every method takes the instant it reasons about, and
//! the host turns a returned [`Command`] into the change on the desktop.

use std::time::{Duration, Instant};

use tilo_desktop::text_entry::{self, Edit, Mods};
use ui::TextRole;
use ui::chrome::shell::{RIBBON_SEG_W, StripRibbon};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::text_selection::Selection;
use ui::theme::Theme;
use ui::widget::{EdgeAim, FieldFace, TextInput, Widget as _};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::shell::i18n::{t, t_args};
use crate::shell::search::providers::actions::{STRIP_NAME_MAX_CHARS, valid_strip_name};
use crate::shell::state::StripId;
use tilo_desktop_model::DesktopTree;

/// A label's height: a small control.
const LABEL_H: f64 = ui::tokens::Metrics::BASE.bar_control_height as f64;
/// Room inside a label around its text.
const LABEL_PAD_X: f64 = ui::tokens::Spacing::BASE.s3 as f64;
/// Between a label and the ribbon mark it names.
const LABEL_GAP: f64 = ui::tokens::Spacing::BASE.s2 as f64;
/// Between the name and the faces inside a label, and the icon and the text
/// of "New strip".
const INNER_GAP: f64 = ui::tokens::Spacing::BASE.s2 as f64;
/// Widest a label grows before its name is elided.
const LABEL_MAX_W: f64 = ui::tokens::Layout::BASE.strip_card_width as f64 - 32.0;
/// The name field's width while a strip is named.
const FIELD_W: f64 = ui::tokens::Layout::BASE.strip_card_width as f64 - 72.0;
/// "New strip" sits this far under the last strip's pointer row.
const NEW_GAP: f64 = ui::tokens::Spacing::BASE.s1 as f64;
/// A pointer this far outside the labels still counts as on them, so a hand
/// that overshoots a label's edge by a few pixels does not close them.
const GRACE: f64 = ui::tokens::Spacing::BASE.s2 as f64;
/// A face inside a label.
const AVATAR: f64 = ui::tokens::Layout::BASE.avatar as f64;
const AVATAR_STEP: f64 = ui::tokens::Layout::BASE.compact_avatar_step as f64;
/// More people than this on a strip collapse into "+N".
const AVATARS_SHOWN: usize = 3;
/// A face beside a ribbon mark at rest: what fits the gutter beside it.
const REST_FACE: f64 = ui::tokens::Spacing::BASE.s4 as f64;
/// Between a face at rest and its mark.
const REST_FACE_GAP: f64 = ui::tokens::Spacing::BASE.s1 as f64;
/// How far inside its square a face at rest's disc sits, and how far past
/// the square its ring (2 px, with a pixel of ground either side) reaches.
const REST_FACE_INSET: f64 = ui::presence::Face::GROUND_GAP;
const REST_FACE_RING: f64 = ui::presence::Face::REACH - REST_FACE_INSET;
/// How far left of the marks the faces at rest reach.
pub(crate) const REST_FACES_REACH: f64 = REST_FACE + REST_FACE_GAP + REST_FACE_RING;
/// The icon in "New strip".
const ICON: f64 = ui::tokens::Metrics::BASE.icon_sm as f64;
/// One wheel notch, in the pixels a touchpad reports: a strip per notch, and
/// per a bar's height of two-finger travel.
const SCROLL_STEP_PX: f64 = ui::tokens::Metrics::BASE.bar_height as f64;
/// A scroll that pauses this long starts counting afresh.
const SCROLL_IDLE: Duration = Duration::from_millis(400);

/// Someone other than this person, on a strip.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Person {
    pub id: String,
    pub label: String,
    pub color: Color,
    /// Software, not a person: the agent mark instead of an initial.
    pub is_agent: bool,
}

/// One strip of the active workspace, as the switcher lists it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StripEntry {
    pub id: StripId,
    pub name: String,
    pub people: Vec<Person>,
    /// This machine writes the strip's name ([`crate::shell::state::WorkspaceTree::may_rename_strip`]).
    pub renamable: bool,
}

/// What the switcher shows: the active workspace's strips.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SwitcherModel {
    pub strips: Vec<StripEntry>,
    /// Index into `strips` of the strip with focus.
    pub focused: usize,
}

impl SwitcherModel {
    /// The switcher's model for `desktop` as the person `local_user`
    /// (`own_id` on this device) sees it: the strips of the active workspace
    /// with the others on each. `may_rename` says which strips this machine
    /// names.
    pub(crate) fn of(
        desktop: &DesktopTree,
        local_user: &str,
        own_id: &str,
        may_rename: impl Fn(StripId) -> bool,
    ) -> Self {
        let Some(workspace) = desktop.active_workspace() else {
            return Self::default();
        };
        let people: Vec<(StripId, Person)> =
            tilo_desktop::collaborators::collaborator_roster_rows(desktop, local_user)
                .into_iter()
                .filter(|row| !row.invited && row.id != own_id && row.id != local_user)
                .filter_map(|row| {
                    let strip = strip_of(desktop, &row.id)?;
                    Some((
                        strip,
                        Person {
                            id: row.id,
                            label: row.label,
                            color: row.color,
                            is_agent: row.is_agent,
                        },
                    ))
                })
                .collect();
        let strips: Vec<StripEntry> = workspace
            .strips
            .iter()
            .map(|strip| StripEntry {
                id: strip.id,
                name: strip.name.clone(),
                people: people
                    .iter()
                    .filter(|(on, _)| *on == strip.id)
                    .map(|(_, person)| person.clone())
                    .collect(),
                renamable: may_rename(strip.id),
            })
            .collect();
        let focused = desktop
            .active_strip
            .get(&workspace.id)
            .and_then(|id| strips.iter().position(|strip| strip.id == *id))
            .unwrap_or(0);
        Self { strips, focused }
    }

    /// The name a strip shows: its own, or "Strip N" while it has none.
    fn shown_name(&self, i: usize) -> String {
        match self.strips.get(i) {
            Some(strip) if !strip.name.trim().is_empty() => strip.name.clone(),
            _ => default_strip_name(i),
        }
    }
}

/// "Strip N", for the strip at index `i`.
fn default_strip_name(i: usize) -> String {
    t_args(
        "strip-switcher-unnamed",
        &[("number", i.saturating_add(1).to_string().into())],
    )
}

/// The strip of the active workspace that `participant_id` has open, from
/// the tab their presence names.
fn strip_of(desktop: &DesktopTree, participant_id: &str) -> Option<StripId> {
    let workspace = desktop.active_workspace()?;
    let tab = workspace
        .collaborators
        .iter()
        .chain(&desktop.collaborators)
        .find(|participant| participant.id == participant_id)?
        .active_tab?;
    workspace
        .strips
        .iter()
        .find(|strip| {
            strip
                .window_groups
                .iter()
                .flat_map(|group| &group.tabs)
                .any(|t| t.id == tab)
        })
        .map(|strip| strip.id)
}

/// A label of the switcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    Strip(usize),
    NewStrip,
}

/// Something a press lands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Row(Row),
    /// The name field while a name is typed.
    Field,
}

/// What the host does for the switcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Focus(StripId),
    /// Move this many strips down (positive) or up (negative).
    Step(i32),
    /// Make a strip with this name and move to it; the switcher then puts
    /// its name in place for typing.
    Create(String),
    Rename(StripId, String),
}

/// The answer to a press.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Press {
    /// Not the switcher's: the press goes on to whatever is under it.
    Outside,
    /// The switcher's, and nothing for the host to do.
    Consumed,
    Command(Command),
}

/// The answer to a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeyAnswer {
    Ignored,
    Consumed,
    Command(Command),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NameTarget {
    Rename(StripId),
}

struct Naming {
    target: NameTarget,
    text: String,
    selection: Selection,
    input: Option<TextInput>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Opened {
    /// Opened by Super+S: stays until a choice or Escape, and the keyboard
    /// is the switcher's.
    by_keyboard: bool,
}

/// Where everything in the open switcher sits.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Layout {
    /// Every label and the ribbon band: the pointer on this (with
    /// [`GRACE`]) keeps the labels up.
    pub bounds: Rect,
    /// Each label's pointer row: from the label's left edge to the screen
    /// edge, as tall as its ribbon segment's pointer target, so the rows
    /// tile with the marks and the way from a label to its mark never
    /// crosses another row.
    pub rows: Vec<(Row, Rect)>,
    /// Each label as painted.
    pub labels: Vec<(Row, Rect)>,
}

impl Layout {
    /// Lay the labels out beside `ribbon`, each centred on its segment, right
    /// edges aligned a gap from the marks; `width` is each label's painted
    /// width. `focus` is where the ribbon's long segment is drawn while it
    /// slides to another strip; the labels stay level with their segments as
    /// they move, and the pointer rows stay where the segments rest.
    pub(crate) fn beside(
        model: &SwitcherModel,
        ribbon: &StripRibbon,
        focus: Option<f64>,
        width: impl Fn(Row) -> f64,
    ) -> Self {
        let edge = ribbon.content.x1;
        let x1 = edge - RIBBON_SEG_W - LABEL_GAP;
        let mut rows = Vec::new();
        let mut labels = Vec::new();
        let mut bottom = ribbon.content.center().y;
        for i in 0..model.strips.len() {
            let seg = match focus {
                Some(focus) => ribbon.segment_rect_at(i, focus),
                None => ribbon.segment_rect(i),
            };
            let (Some(seg), Some(hit)) = (seg, ribbon.hit_rect(i)) else {
                continue;
            };
            let w = width(Row::Strip(i)).min(LABEL_MAX_W.max(FIELD_W));
            let cy = seg.center().y;
            let label = Rect::new(x1 - w, cy - LABEL_H / 2.0, x1, cy + LABEL_H / 2.0);
            labels.push((Row::Strip(i), label));
            rows.push((Row::Strip(i), Rect::new(label.x0, hit.y0, edge, hit.y1)));
            bottom = hit.y1;
        }
        let w = width(Row::NewStrip).min(LABEL_MAX_W);
        let y0 = bottom + NEW_GAP;
        let label = Rect::new(x1 - w, y0, x1, y0 + LABEL_H);
        labels.push((Row::NewStrip, label));
        rows.push((
            Row::NewStrip,
            Rect::new(label.x0, bottom, edge, label.y1 + NEW_GAP),
        ));
        let bounds = rows
            .iter()
            .map(|(_, r)| *r)
            .chain(labels.iter().map(|(_, r)| *r))
            .reduce(|a, b| a.union(b))
            .unwrap_or_default();
        Self {
            bounds,
            rows,
            labels,
        }
    }

    #[cfg(test)]
    pub(crate) fn row_rect(&self, row: Row) -> Option<Rect> {
        self.rows.iter().find(|(k, _)| *k == row).map(|(_, r)| *r)
    }

    pub(crate) fn label_rect(&self, row: Row) -> Option<Rect> {
        self.labels.iter().find(|(k, _)| *k == row).map(|(_, r)| *r)
    }
}

/// A label's width before its text has been measured: a fair guess from its
/// length, replaced by the measured width at the first paint.
fn estimated_width(text: &str, faces: usize) -> f64 {
    let chars = ui::num::f64_of_usize(text.chars().count());
    LABEL_PAD_X * 2.0 + chars * 7.0 + faces_width(faces).map_or(0.0, |w| w + INNER_GAP)
}

/// The faces on a strip: the same as everywhere people are shown, ringed
/// in their colour, then a drawn count.
const STRIP_FACES: ui::presence::FaceStack = ui::presence::FaceStack {
    side: AVATAR,
    overlap: AVATAR - AVATAR_STEP,
    max: AVATARS_SHOWN + 1,
};

/// The width of a face stack for `people` people, `None` for nobody.
fn faces_width(people: usize) -> Option<f64> {
    if people == 0 {
        return None;
    }
    let (shown, more) = STRIP_FACES.split(people);
    Some(STRIP_FACES.width_of(shown, more))
}

/// The switcher's state between frames.
#[derive(Default)]
pub(crate) struct StripSwitcher {
    model: SwitcherModel,
    open: Option<Opened>,
    aim: EdgeAim,
    /// The last pointer position seen.
    pointer: Option<Point>,
    /// Closed with the pointer on the ribbon: it has to leave before resting
    /// there opens the labels again.
    disarmed: bool,
    /// What the pointer is on, once it has settled there.
    hover: Option<Target>,
    /// The keyboard's label.
    selected: Option<Row>,
    naming: Option<Naming>,
    /// A strip made from "New strip" whose name goes in place for typing as
    /// soon as the model lists it.
    name_when_listed: bool,
    /// The last press on a label and when, for a double click.
    last_press: Option<(Row, Instant)>,
    /// Scroll travel not yet turned into a step, and when it last moved.
    scroll: (f64, Option<Instant>),
    /// The ribbon the labels were last laid out beside, and where its long
    /// segment was drawn.
    ribbon: Option<StripRibbon>,
    focus: Option<f64>,
    /// Each label's measured width, by row, as of the last paint.
    widths: Vec<(Row, f64)>,
    layout: Layout,
    /// The labels fading in (`ui::motion::Ambient`): nothing in them moves,
    /// so the label under the pointer stays level with its mark.
    fade: ui::motion::Presence,
    /// The motion the person allows, as of the last paint.
    motion: ui::tokens::EffectiveMotion,
}

impl StripSwitcher {
    pub(crate) fn set_model(&mut self, model: SwitcherModel) {
        if self.model == model {
            return;
        }
        let known: Vec<StripId> = self.model.strips.iter().map(|s| s.id).collect();
        self.model = model;
        if let Some(Row::Strip(i)) = self.selected
            && i >= self.model.strips.len()
        {
            self.selected = None;
        }
        if let Some(NameTarget::Rename(id)) = self.naming.as_ref().map(|n| n.target)
            && !self.model.strips.iter().any(|s| s.id == id)
        {
            self.naming = None;
        }
        if std::mem::take(&mut self.name_when_listed)
            && let Some(i) = self
                .model
                .strips
                .iter()
                .position(|s| !known.contains(&s.id))
        {
            self.begin_rename(i);
        }
        self.relayout();
    }

    pub(crate) fn model(&self) -> &SwitcherModel {
        &self.model
    }

    /// Lay the labels out beside `ribbon`; call before hit-testing or
    /// drawing.
    pub(crate) fn place(&mut self, ribbon: &StripRibbon) {
        self.ribbon = Some(*ribbon);
        self.relayout();
    }

    /// [`Self::place`] while the ribbon's long segment is drawn at `focus`,
    /// sliding to the strip that took focus.
    pub(crate) fn place_sliding(&mut self, ribbon: &StripRibbon, focus: Option<f64>) {
        self.focus = focus;
        self.place(ribbon);
    }

    /// Everything the open labels would take beside `ribbon`, "New strip"
    /// under the last included: where nothing else may wait at that edge,
    /// open or not, since the labels open over whatever is there.
    pub(crate) fn extent_beside(&self, ribbon: &StripRibbon) -> Rect {
        Layout::beside(&self.model, ribbon, None, |row| self.label_width(row)).bounds
    }

    fn relayout(&mut self) {
        let Some(ribbon) = self.ribbon else {
            return;
        };
        self.layout = Layout::beside(&self.model, &ribbon, self.focus, |row| {
            self.label_width(row)
        });
    }

    /// Whether strip `i`'s name is being typed.
    fn is_renaming(&self, i: usize) -> bool {
        match (self.naming.as_ref(), self.model.strips.get(i)) {
            (Some(naming), Some(strip)) => naming.target == NameTarget::Rename(strip.id),
            _ => false,
        }
    }

    /// A label's width: the field's while it is named, else as measured at
    /// the last paint, else estimated.
    fn label_width(&self, row: Row) -> f64 {
        if let Row::Strip(i) = row
            && self.is_renaming(i)
        {
            return FIELD_W;
        }
        if let Some((_, w)) = self.widths.iter().find(|(r, _)| *r == row) {
            return *w;
        }
        match row {
            Row::Strip(i) => estimated_width(
                &self.model.shown_name(i),
                self.model.strips.get(i).map_or(0, |s| s.people.len()),
            ),
            Row::NewStrip => estimated_width(&t("strip-switcher-new-strip"), 0) + ICON + INNER_GAP,
        }
    }

    pub(crate) fn layout(&self) -> &Layout {
        &self.layout
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The keyboard is the switcher's: opened by Super+S, or a name is
    /// typed.
    pub(crate) fn takes_keys(&self) -> bool {
        self.naming.is_some() || self.open.is_some_and(|o| o.by_keyboard)
    }

    /// Whether the labels stay without the pointer on them: opened for the
    /// keyboard, or a name is being typed.
    fn pinned(&self) -> bool {
        self.takes_keys()
    }

    #[cfg(test)]
    pub(crate) fn is_naming(&self) -> bool {
        self.naming.is_some()
    }

    #[cfg(test)]
    pub(crate) fn hover(&self) -> Option<Target> {
        self.hover
    }

    #[cfg(test)]
    pub(crate) fn selected(&self) -> Option<Row> {
        self.selected
    }

    /// The text being typed, while a strip is named.
    #[cfg(test)]
    pub(crate) fn naming_text(&self) -> Option<&str> {
        self.naming.as_ref().map(|n| n.text.as_str())
    }

    /// The strip whose mark the pointer or the keyboard is on, so the ribbon
    /// lights that mark too.
    pub(crate) fn lit_strip(&self) -> Option<usize> {
        self.open?;
        match (self.hover, self.selected) {
            (Some(Target::Row(Row::Strip(i))), _) | (_, Some(Row::Strip(i))) => Some(i),
            _ => None,
        }
    }

    fn open_at(&mut self, now: Instant, by_keyboard: bool) {
        if self.open.is_none() {
            self.open = Some(Opened { by_keyboard });
            let source = if by_keyboard {
                ui::tokens::MotionSource::Keyboard
            } else {
                ui::tokens::MotionSource::Pointer
            };
            self.fade = ui::motion::Presence::closed();
            self.fade
                .show(true, ui::motion::Ambient::ASKED, self.motion, source, now);
        } else if let Some(open) = self.open.as_mut() {
            open.by_keyboard |= by_keyboard;
        }
    }

    /// Close the labels, dropping a name being typed.
    pub(crate) fn close(&mut self) {
        if self.open.take().is_some() {
            self.disarmed = true;
        }
        self.naming = None;
        self.name_when_listed = false;
        self.hover = None;
        self.selected = None;
        self.last_press = None;
        self.relayout();
    }

    /// Super+S: open the labels for the keyboard on the focused strip, or
    /// close them when they are open.
    pub(crate) fn toggle_from_keyboard(&mut self, now: Instant) {
        if self.open.is_some() {
            self.close();
            return;
        }
        self.open_for_choosing(now);
    }

    /// Open the labels for choosing, as a menu: on the focused strip, taking
    /// the keyboard, and staying until a choice, Escape or a press
    /// elsewhere. Already open, they take the keyboard as they are.
    pub(crate) fn open_for_choosing(&mut self, now: Instant) {
        let was_open = self.open.is_some();
        self.open_at(now, true);
        if !was_open || self.selected.is_none() {
            self.selected = Some(Row::Strip(self.model.focused));
        }
    }

    /// The pointer moved to `p`; `zone` is the ribbon's pointer band. Returns
    /// whether anything visible changed.
    pub(crate) fn pointer_moved(&mut self, p: Point, now: Instant, zone: Rect) -> bool {
        if !zone.contains(p) {
            self.disarmed = false;
        }
        // The compositor notes every motion, and passes the ones over chrome
        // on as events too: one position is one sample.
        if self.pointer == Some(p) {
            return false;
        }
        let before = (self.open, self.hover);
        self.pointer = Some(p);
        let aimed = self.aim.on_motion(p, now, &[zone]).is_some();
        match self.open {
            None => {
                if aimed && !self.disarmed {
                    self.open_at(now, false);
                }
            }
            Some(_) => {
                if !self.on_switcher(p, zone) && !self.pinned() {
                    self.close();
                    // Left on purpose: the ribbon may open it again at once.
                    self.disarmed = false;
                }
            }
        }
        self.refresh_hover(now);
        before != (self.open, self.hover)
    }

    /// Whether `p` is on the ribbon or the labels, with grace.
    fn on_switcher(&self, p: Point, zone: Rect) -> bool {
        zone.contains(p) || self.layout.bounds.inflate(GRACE, GRACE).contains(p)
    }

    /// The pointer left the display.
    pub(crate) fn pointer_left(&mut self) -> bool {
        self.aim.clear();
        self.pointer = None;
        self.disarmed = false;
        let was = self.hover.take().is_some();
        if self.open.is_some() && !self.pinned() {
            self.close();
            self.disarmed = false;
            return true;
        }
        was
    }

    /// Advance to `now` without input: a pointer that came to rest on the
    /// ribbon opens the labels, one that settled on a label lights it.
    /// Returns whether anything visible changed.
    pub(crate) fn settle(&mut self, now: Instant, zone: Rect) -> bool {
        let before = (self.open, self.hover);
        if self.open.is_none()
            && !self.disarmed
            && self.pointer.is_some_and(|p| zone.contains(p))
            && (self.aim.rested_in(now, &[zone]).is_some() || self.aim.settled(now))
        {
            self.open_at(now, false);
        }
        self.refresh_hover(now);
        before != (self.open, self.hover)
    }

    /// When [`Self::settle`] next changes something with no further input.
    pub(crate) fn deadline(&self, now: Instant, zone: Rect) -> Option<Instant> {
        let waiting = self.pointer.is_some_and(|p| {
            (self.open.is_none() && !self.disarmed && zone.contains(p))
                || (self.open.is_some() && self.target_at(p).is_some() && self.hover.is_none())
        });
        waiting.then(|| self.aim.deadline(now)).flatten()
    }

    fn refresh_hover(&mut self, now: Instant) {
        self.hover = match (self.open, self.pointer) {
            (Some(_), Some(p)) if self.aim.settled(now) => self.target_at(p),
            _ => None,
        };
    }

    /// What in the open switcher is at `p`.
    pub(crate) fn target_at(&self, p: Point) -> Option<Target> {
        self.open?;
        if let Some(naming) = &self.naming
            && self
                .field_rect(naming.target)
                .is_some_and(|r| r.contains(p))
        {
            return Some(Target::Field);
        }
        self.layout
            .rows
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|(row, _)| Target::Row(*row))
    }

    /// The name field for `target`: in place of the strip's label.
    fn field_rect(&self, target: NameTarget) -> Option<Rect> {
        let NameTarget::Rename(id) = target;
        let i = self.model.strips.iter().position(|s| s.id == id)?;
        self.layout.label_rect(Row::Strip(i))
    }

    /// A left press at `p` at `now`.
    pub(crate) fn press(&mut self, p: Point, now: Instant) -> Press {
        let Some(target) = self.target_at(p) else {
            if self.open.is_some() && self.pinned() {
                // A press elsewhere ends naming, and labels opened for the
                // keyboard; the press itself still goes where it landed.
                self.close();
            }
            return Press::Outside;
        };
        let Target::Row(row) = target else {
            return Press::Consumed;
        };
        let double = self.last_press.is_some_and(|(last, at)| {
            last == row && now.saturating_duration_since(at) < ui::gesture::multi_click_interval()
        });
        self.last_press = Some((row, now));
        if self.naming.is_some() {
            // A press on another label ends the name being typed, unsaved,
            // as Escape would.
            self.naming = None;
            self.relayout();
        }
        if double && let Row::Strip(i) = row {
            self.last_press = None;
            self.begin_rename(i);
            return Press::Consumed;
        }
        self.activate(row)
    }

    fn activate(&mut self, row: Row) -> Press {
        match row {
            Row::Strip(i) => self
                .model
                .strips
                .get(i)
                .map_or(Press::Consumed, |s| Press::Command(Command::Focus(s.id))),
            Row::NewStrip => {
                self.name_when_listed = true;
                self.selected = Some(Row::NewStrip);
                Press::Command(Command::Create(default_strip_name(self.model.strips.len())))
            }
        }
    }

    fn begin_rename(&mut self, i: usize) {
        let Some(strip) = self.model.strips.get(i).filter(|s| s.renamable) else {
            return;
        };
        let text = self.model.shown_name(i);
        let end = text.len();
        self.naming = Some(Naming {
            target: NameTarget::Rename(strip.id),
            text,
            selection: Selection {
                anchor: 0,
                cursor: end,
            },
            input: None,
        });
        self.selected = Some(Row::Strip(i));
        self.relayout();
    }

    /// Rename the focused strip from the keyboard or an action that asks
    /// for the field: opens the labels with the name selected in place.
    pub(crate) fn rename_focused(&mut self, now: Instant) {
        self.open_at(now, true);
        self.begin_rename(self.model.focused);
    }

    /// A wheel or two-finger scroll over the switcher at `now`: a strip up
    /// or down per notch, or per [`SCROLL_STEP_PX`] of travel. `None` until
    /// the travel adds up to a step, or when the pointer is not on it.
    pub(crate) fn scroll(
        &mut self,
        p: Point,
        delta: MouseScrollDelta,
        now: Instant,
        zone: Rect,
    ) -> Option<Command> {
        let on = zone.contains(p) || (self.open.is_some() && self.target_at(p).is_some());
        if !on || self.naming.is_some() {
            return None;
        }
        // Winit's deltas are positive for a wheel turned away from the
        // person (content moving down): the strip above.
        let notches = match delta {
            MouseScrollDelta::LineDelta(x, y) if y.abs() >= x.abs() => f64::from(y),
            MouseScrollDelta::PixelDelta(d) if d.y.abs() >= d.x.abs() => d.y / SCROLL_STEP_PX,
            _ => return None,
        };
        let (mut travel, last) = self.scroll;
        // A pause, or a turn the other way, starts counting afresh.
        if last.is_none_or(|at| now.saturating_duration_since(at) > SCROLL_IDLE)
            || travel * notches < 0.0
        {
            travel = 0.0;
        }
        travel += notches;
        let whole = travel.trunc();
        self.scroll = (travel - whole, Some(now));
        if whole.abs() < 1.0 {
            return None;
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a handful of notches per event, far inside i32"
        )]
        let steps = (-whole) as i32;
        Some(Command::Step(steps))
    }

    /// Every label in keyboard order.
    fn rows_in_order(&self) -> Vec<Row> {
        (0..self.model.strips.len())
            .map(Row::Strip)
            .chain([Row::NewStrip])
            .collect()
    }

    /// A key while the switcher takes the keyboard.
    pub(crate) fn key(&mut self, key: &Key, mods: ModifiersState) -> KeyAnswer {
        if let Some(naming) = self.naming.as_mut() {
            return match text_entry::apply(
                key,
                Mods::from_state(mods),
                &mut naming.text,
                &mut naming.selection,
            ) {
                Edit::Submit => self.commit_name(),
                Edit::Cancel => {
                    self.naming = None;
                    self.end_pointer_selection();
                    self.relayout();
                    KeyAnswer::Consumed
                }
                Edit::Changed => {
                    if naming.text.chars().count() > STRIP_NAME_MAX_CHARS {
                        naming.text = naming.text.chars().take(STRIP_NAME_MAX_CHARS).collect();
                        naming.selection = Selection::collapsed(naming.text.len());
                    }
                    KeyAnswer::Consumed
                }
                Edit::Ignored | Edit::ConsumedQuiet => KeyAnswer::Consumed,
            };
        }
        if !self.takes_keys() {
            return KeyAnswer::Ignored;
        }
        let rows = self.rows_in_order();
        let at = self
            .selected
            .and_then(|row| rows.iter().position(|r| *r == row));
        match key {
            Key::Named(NamedKey::ArrowDown) => {
                let next = at.map_or(0, |i| i.saturating_add(1).min(rows.len().saturating_sub(1)));
                self.selected = rows.get(next).copied();
                KeyAnswer::Consumed
            }
            Key::Named(NamedKey::ArrowUp) => {
                let next = at.map_or(0, |i| i.saturating_sub(1));
                self.selected = rows.get(next).copied();
                KeyAnswer::Consumed
            }
            Key::Named(NamedKey::Home) => {
                self.selected = rows.first().copied();
                KeyAnswer::Consumed
            }
            Key::Named(NamedKey::End) => {
                self.selected = rows.last().copied();
                KeyAnswer::Consumed
            }
            Key::Named(NamedKey::F2) => {
                if let Some(Row::Strip(i)) = self.selected {
                    self.begin_rename(i);
                }
                KeyAnswer::Consumed
            }
            Key::Named(NamedKey::Escape) => {
                self.close();
                KeyAnswer::Consumed
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                let Some(row) = self.selected else {
                    return KeyAnswer::Consumed;
                };
                match self.activate(row) {
                    // A new strip stays open for its name.
                    Press::Command(command @ Command::Create(_)) => KeyAnswer::Command(command),
                    Press::Command(command) => {
                        self.close();
                        KeyAnswer::Command(command)
                    }
                    Press::Consumed | Press::Outside => KeyAnswer::Consumed,
                }
            }
            // The switcher owns the keyboard while open for it; a stray key
            // must not reach the window behind.
            _ => KeyAnswer::Consumed,
        }
    }

    /// Text an input method committed into the name field.
    pub(crate) fn commit_text(&mut self, text: &str) -> bool {
        let Some(naming) = self.naming.as_mut() else {
            return false;
        };
        text_entry::commit(text, &mut naming.text, &mut naming.selection);
        if naming.text.chars().count() > STRIP_NAME_MAX_CHARS {
            naming.text = naming.text.chars().take(STRIP_NAME_MAX_CHARS).collect();
            naming.selection = Selection::collapsed(naming.text.len());
        }
        true
    }

    fn commit_name(&mut self) -> KeyAnswer {
        let Some(naming) = self.naming.take() else {
            return KeyAnswer::Consumed;
        };
        self.relayout();
        let by_keyboard = self.open.is_some_and(|o| o.by_keyboard);
        let NameTarget::Rename(id) = naming.target;
        let name = valid_strip_name(&naming.text).map(str::to_owned);
        let unchanged = |name: &str| {
            self.model
                .strips
                .iter()
                .any(|s| s.id == id && s.name == name)
        };
        let command = match name {
            Some(name) if !unchanged(&name) => Some(Command::Rename(id, name)),
            _ => None,
        };
        if by_keyboard {
            self.close();
        } else {
            self.end_pointer_selection();
        }
        command.map_or(KeyAnswer::Consumed, KeyAnswer::Command)
    }

    /// A name typed after a pointer asked for it is done: the keyboard ring
    /// the field took goes with it, unless the keyboard opened the labels.
    fn end_pointer_selection(&mut self) {
        if !self.open.is_some_and(|o| o.by_keyboard) {
            self.selected = None;
        }
    }

    /// Opacity of the labels at `now`.
    fn alpha(&self, now: Instant) -> f32 {
        if self.open.is_none() {
            return 0.0;
        }
        ui::motion::unit_opacity(self.fade.progress(now))
    }

    /// Frames are owed only while the labels fade in.
    pub(crate) fn needs_animation_frame(&self, now: Instant) -> bool {
        self.open.is_some() && self.fade.is_moving(now)
    }

    /// Measure every label's text as it will be painted, and lay out again
    /// when a width changed.
    fn measure_labels(&mut self, cx: &mut DrawCx<'_>) {
        let mut widths = Vec::with_capacity(self.model.strips.len().saturating_add(1));
        for i in 0..self.model.strips.len() {
            let active = i == self.model.focused;
            let style = label_style(cx.theme, active);
            let (w, _) = cx
                .text
                .measure_style(&self.model.shown_name(i), None, &style);
            let faces = self
                .model
                .strips
                .get(i)
                .and_then(|s| faces_width(s.people.len()))
                .map_or(0.0, |w| w + INNER_GAP);
            widths.push((
                Row::Strip(i),
                (f64::from(w) + LABEL_PAD_X * 2.0 + faces).min(LABEL_MAX_W),
            ));
        }
        let style = label_style(cx.theme, false);
        let (w, _) = cx
            .text
            .measure_style(&t("strip-switcher-new-strip"), None, &style);
        widths.push((
            Row::NewStrip,
            (f64::from(w) + ICON + INNER_GAP + LABEL_PAD_X * 2.0).min(LABEL_MAX_W),
        ));
        if widths != self.widths {
            self.widths = widths;
            self.relayout();
        }
    }

    /// Paint the open labels. The host paints the ribbon over its band.
    pub(crate) fn draw(&mut self, cx: &mut DrawCx<'_>, now: Instant) {
        self.motion = cx.theme.motion();
        let alpha = self.alpha(now);
        if alpha <= 0.0 {
            return;
        }
        self.measure_labels(cx);
        let fading = alpha < 1.0;
        if fading {
            cx.scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                alpha,
                cx.transform,
                &self.bounds(),
            );
        }
        for (row, rect) in self.layout.labels.clone() {
            self.draw_label(cx, row, rect);
        }
        if let Some(target) = self.naming.as_ref().map(|n| n.target)
            && let Some(field) = self.field_rect(target)
        {
            self.draw_field(cx, field);
        }
        if fading {
            cx.scene.pop_layer();
        }
    }

    /// Everything the open labels paint, shadows included, for damage.
    pub(crate) fn bounds(&self) -> Rect {
        self.layout.bounds.inflate(32.0, 32.0)
    }

    fn row_state(&self, row: Row) -> (bool, bool) {
        let active = matches!(row, Row::Strip(i) if i == self.model.focused);
        let hover = self.hover == Some(Target::Row(row)) || self.selected == Some(row);
        (active, hover)
    }

    /// One label: its floating ground, lifted when it is the focused strip
    /// or under the pointer, the strip's name and who is on it.
    fn draw_label(&self, cx: &mut DrawCx<'_>, row: Row, rect: Rect) {
        if let Row::Strip(i) = row
            && self.is_renaming(i)
        {
            // The field takes the label's place.
            return;
        }
        let (active, hover) = self.row_state(row);
        let radius = ui::chrome::ChromeRole::Control.radius(cx.theme, rect.height());
        let ground = ui::widget::floating_surface::card_ground(cx.theme);
        ui::widget::floating_surface::paint_rounded(cx.scene, cx.transform, rect, radius, cx.theme);
        let shape = RoundedRect::from_rect(rect, radius);
        let tier = ui::chrome::selection_tier_on(
            cx.theme,
            ui::chrome::SelectionState::new(active, hover, false),
            ground,
        );
        if tier.fill.components[3] > 0.0 {
            cx.scene
                .fill(Fill::NonZero, cx.transform, tier.fill, None, &shape);
        }
        if self.selected == Some(row) {
            ui::widget::paint_focus_ring(cx.scene, cx.transform, rect, radius, cx.theme);
        }
        let colors = &cx.theme.tokens.colors;
        let ink = if active || hover {
            colors.text_primary
        } else {
            colors.text_dim
        };
        let inner = Rect::new(
            rect.x0 + LABEL_PAD_X,
            rect.y0,
            rect.x1 - LABEL_PAD_X,
            rect.y1,
        );
        match row {
            Row::Strip(i) => {
                let Some(strip) = self.model.strips.get(i) else {
                    return;
                };
                let x_end = draw_people(cx, &strip.people, inner.x1, rect.center().y);
                let text_x1 = if strip.people.is_empty() {
                    x_end
                } else {
                    x_end - INNER_GAP
                };
                let style = label_style(cx.theme, active);
                draw_line(
                    cx,
                    &self.model.shown_name(i),
                    Rect::new(inner.x0, rect.y0, text_x1, rect.y1),
                    ink,
                    &style,
                );
            }
            Row::NewStrip => {
                let icon = Rect::from_center_size(
                    Point::new(inner.x0 + ICON / 2.0, rect.center().y),
                    (ICON, ICON),
                );
                ui::icons::draw_icon(
                    cx.scene,
                    cx.transform,
                    icon,
                    ui::icons::Icon::Plus,
                    ink,
                    ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
                draw_line(
                    cx,
                    &t("strip-switcher-new-strip"),
                    Rect::new(icon.x1 + INNER_GAP, rect.y0, inner.x1, rect.y1),
                    ink,
                    &label_style(cx.theme, false),
                );
            }
        }
    }

    fn draw_field(&mut self, cx: &mut DrawCx<'_>, field: Rect) {
        let Some(naming) = self.naming.as_mut() else {
            return;
        };
        let radius = ui::chrome::ChromeRole::Control.radius(cx.theme, field.height());
        ui::widget::floating_surface::paint_rounded(
            cx.scene,
            cx.transform,
            field,
            radius,
            cx.theme,
        );
        let placeholder = t("strip-switcher-name-placeholder");
        let face = FieldFace::new(field, &naming.text)
            .placeholder(&placeholder)
            .text_role(TextRole::Label)
            .focused(true);
        if naming.text.is_empty() {
            face.paint(cx.scene, cx.transform, cx.text, cx.theme);
        } else {
            face.paint_chrome(cx.scene, cx.transform, cx.theme);
        }
        let input = naming.input.get_or_insert_with(|| {
            TextInput::new(Vec::new())
                .with_plain_chrome()
                .with_spellcheck(false)
                .with_autocorrect(false)
                .with_text_role(TextRole::Label)
        });
        input.set_text(&naming.text);
        input.set_selection(naming.selection.anchor, naming.selection.cursor);
        input.set_focus(true);
        input.apply_layout_external(face.text_rect(cx.theme));
        let input_theme = cx
            .theme
            .clone()
            .with_text_scale(cx.theme.text_scale * cx.theme.ui_scale);
        input.draw(
            cx.scene,
            cx.transform,
            cx.text,
            &input_theme,
            &PresenceCtx::default(),
        );
    }

    /// The faces of the others on each strip, beside its mark, while the
    /// labels are closed: who is where shows at a glance, as faces.
    pub(crate) fn draw_rest_faces(
        &self,
        cx: &mut DrawCx<'_>,
        (ribbon, focus): (&StripRibbon, Option<f64>),
        alpha: f32,
    ) {
        if self.open.is_some() || alpha <= 0.0 {
            return;
        }
        let ground = ui::widget::SurfaceLevel::Ground.color(cx.theme);
        let mut paint = ui::paint::PaintCtx::new(cx.scene, cx.transform, cx.text, cx.theme);
        for (i, strip) in self.model.strips.iter().enumerate() {
            let seg = match focus {
                Some(focus) => ribbon.segment_rect_at(i, focus),
                None => ribbon.segment_rect(i),
            };
            let (Some(person), Some(seg)) = (strip.people.first(), seg) else {
                continue;
            };
            let rect = Rect::from_center_size(
                Point::new(seg.x0 - REST_FACE_GAP - REST_FACE / 2.0, seg.center().y),
                (REST_FACE, REST_FACE),
            );
            paint.scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                alpha,
                paint.transform,
                &rect.inflate(REST_FACE_RING, REST_FACE_RING),
            );
            // One face per strip fits the gutter; the label lists them all.
            // The one face people have everywhere: one initial (or the agent
            // mark) in a ring of their colour, the ring reaching just past
            // the gutter's square.
            ui::presence::Face {
                is_agent: person.is_agent,
                ..ui::presence::Face::new(&person.label, &person.id, person.color)
            }
            .paint(&mut paint, rect.inset(-REST_FACE_INSET), ground);
            paint.scene.pop_layer();
        }
    }

    /// Named targets for harnesses: the ribbon, and each label when open.
    pub(crate) fn test_rects(&self, zone: Rect) -> Vec<(String, Rect)> {
        if self.open.is_none() {
            return vec![("strip_switcher".into(), zone)];
        }
        let mut out = vec![("strip_switcher".into(), self.layout.bounds)];
        for (row, rect) in &self.layout.labels {
            let name = match row {
                Row::Strip(i) => format!("strip_switcher.strip.{}", i.saturating_add(1)),
                Row::NewStrip => "strip_switcher.new_strip".into(),
            };
            out.push((name, *rect));
        }
        if let Some(field) = self.naming.as_ref().and_then(|n| self.field_rect(n.target)) {
            out.push(("strip_switcher.name_field".into(), field));
        }
        out
    }

    /// The semantic target of a label, for assistive technology and agents.
    pub(crate) fn target_name(target: Target) -> String {
        match target {
            Target::Row(Row::Strip(i)) => format!("{TARGET_PREFIX}strip:{i}"),
            Target::Row(Row::NewStrip) => format!("{TARGET_PREFIX}new-strip"),
            Target::Field => format!("{TARGET_PREFIX}field"),
        }
    }

    /// The semantic target that renames strip `i`.
    fn rename_target_name(i: usize) -> String {
        format!("{TARGET_PREFIX}rename:{i}")
    }

    /// Activate the target named `target` as a press on it would.
    pub(crate) fn activate_target(&mut self, target: &str) -> Option<Press> {
        let rest = target.strip_prefix(TARGET_PREFIX)?;
        let index = |s: &str| s.parse::<usize>().ok();
        match rest.split_once(':') {
            Some(("strip", i)) => {
                let i = index(i)?;
                self.naming = None;
                Some(self.activate(Row::Strip(i)))
            }
            Some(("rename", i)) => {
                self.begin_rename(index(i)?);
                Some(Press::Consumed)
            }
            _ => match rest {
                "new-strip" => {
                    self.naming = None;
                    Some(self.activate(Row::NewStrip))
                }
                "field" => Some(Press::Consumed),
                _ => None,
            },
        }
    }

    /// The open labels for assistive technology: a group of buttons, with
    /// a rename for each strip this machine names.
    pub(crate) fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        self.open?;
        let mut group = AccessNode::new(Role::Group)
            .with_name(t("strip-switcher-a11y"))
            .with_bounds(self.layout.bounds);
        for (row, rect) in &self.layout.labels {
            let name = match row {
                Row::Strip(i) => self.model.shown_name(*i),
                Row::NewStrip => t("strip-switcher-new-strip"),
            };
            let (active, _) = self.row_state(*row);
            group.children.push(
                AccessNode::new(Role::Button)
                    .with_name(name)
                    .with_bounds(*rect)
                    .with_action_target(Self::target_name(Target::Row(*row)))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(active)
                            .focused(self.selected == Some(*row)),
                    ),
            );
            if let Row::Strip(i) = row
                && self.model.strips.get(*i).is_some_and(|s| s.renamable)
            {
                group.children.push(
                    AccessNode::new(Role::Button)
                        .with_name(t_args(
                            "strip-switcher-rename-named",
                            &[("name", self.model.shown_name(*i).into())],
                        ))
                        .with_bounds(*rect)
                        .with_action_target(Self::rename_target_name(*i)),
                );
            }
        }
        if let Some(naming) = &self.naming
            && let Some(field) = self.field_rect(naming.target)
        {
            group.children.push(
                AccessNode::new(Role::TextInput)
                    .with_name(t("strip-switcher-name-placeholder"))
                    .with_optional_value(Some(naming.text.clone()))
                    .with_text_selection(naming.selection.anchor, naming.selection.cursor)
                    .with_bounds(field)
                    .with_state(AccessState::new().focusable(true).focused(true)),
            );
        }
        Some(group)
    }
}

/// Semantic targets of the switcher start with this.
pub(crate) const TARGET_PREFIX: &str = "strip-switcher:";

/// What painting needs.
pub(crate) struct DrawCx<'a> {
    pub scene: &'a mut Scene,
    pub transform: Affine,
    pub text: &'a mut TextRenderer,
    pub theme: &'a Theme,
}

/// A label's text: the focused strip's in medium weight.
fn label_style(theme: &Theme, active: bool) -> ui::text_render::TextStyle {
    let style = theme.text(TextRole::Label);
    if active {
        style.with_weight(TextWeight::Medium)
    } else {
        style
    }
}

/// The others on a strip, right-aligned to end at `x_end`; returns where
/// they begin.
fn draw_people(cx: &mut DrawCx<'_>, people: &[Person], x_end: f64, cy: f64) -> f64 {
    let Some(width) = faces_width(people.len()) else {
        return x_end;
    };
    let (shown, more) = STRIP_FACES.split(people.len());
    let faces: Vec<ui::presence::Face<'_>> = people
        .iter()
        .take(shown)
        .map(|person| ui::presence::Face {
            is_agent: person.is_agent,
            ..ui::presence::Face::new(&person.label, &person.id, person.color)
        })
        .collect();
    let ground = ui::widget::floating_surface::card_ground(cx.theme);
    let mut paint = ui::paint::PaintCtx::new(cx.scene, cx.transform, cx.text, cx.theme);
    STRIP_FACES.paint(&mut paint, (x_end, cy), (&faces, more), ground);
    x_end - width
}

/// One line of text, elided to `rect`'s width and centred in its height.
fn draw_line(
    cx: &mut DrawCx<'_>,
    text: &str,
    rect: Rect,
    color: Color,
    style: &ui::text_render::TextStyle,
) {
    let width = ui::num::f32_of_f64(rect.width().max(1.0));
    let line = cx.text.elide_to_width_style(text, width, style);
    let (_, h) = cx.text.measure_style(&line, None, style);
    cx.text.draw_style(
        cx.scene,
        &line,
        color,
        cx.transform
            .pre_translate((rect.x0, rect.center().y - f64::from(h) / 2.0).into()),
        None,
        style,
    );
}

/// The ribbon's weight while nothing moves and the labels are closed: quiet,
/// but there, so the switcher can be found, once there is more than one
/// strip to switch between.
pub(crate) const RIBBON_REST_ALPHA: f32 = 0.35;

/// The faces beside the marks keep this much while the marks rest: who is
/// here is worth a glance even when nothing moves.
const PEOPLE_REST_ALPHA: f32 = 0.85;

impl super::ShellOverlayHost {
    /// Whether the switcher is offered: a desktop with strips, outside the
    /// overview, Quick Settings and a fullscreen window.
    fn strip_switcher_offered(&self) -> bool {
        let flags = self.overlay_flags();
        !flags.overview
            && !flags.quick_settings
            && self.strip_ribbon().count > 0
            && !self
                .chrome
                .as_ref()
                .is_some_and(|chrome| chrome.borrow().is_fullscreen())
    }

    /// The ribbon's pointer band: what the pointer comes to rest on to open
    /// the labels.
    pub(super) fn strip_switcher_zone(&self) -> Rect {
        let ribbon = self.strip_ribbon();
        match (
            ribbon.hit_rect(0),
            ribbon.hit_rect(ribbon.count.saturating_sub(1)),
        ) {
            (Some(first), Some(last)) if self.strip_switcher_offered() => first.union(last),
            _ => Rect::ZERO,
        }
    }

    /// The open labels' extent, for damage.
    pub(super) fn strip_switcher_bounds(&self) -> Rect {
        self.strip_switcher.bounds()
    }

    /// The ribbon mark to light: the one whose label the pointer or the
    /// keyboard is on while the labels are open, else the one under the
    /// pointer.
    pub(super) fn strip_ribbon_lit(&self) -> Option<usize> {
        self.strip_switcher.lit_strip().or(self.strip_ribbon_hover)
    }

    /// The faces beside the ribbon's marks while the labels are closed.
    pub(super) fn draw_strip_ribbon_faces(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        alpha: f32,
    ) {
        let ribbon = self.strip_ribbon();
        let focus = self.strip_ribbon_focus_at(Instant::now());
        self.strip_switcher.draw_rest_faces(
            &mut DrawCx {
                scene,
                transform,
                text,
                theme,
            },
            (&ribbon, focus),
            // People are worth seeing while the marks rest quietly.
            alpha.max(PEOPLE_REST_ALPHA),
        );
    }

    /// The ribbon's opacity at `now`: full while the labels are open, the
    /// navigation fade while moving, and the quiet resting mark otherwise.
    pub(super) fn strip_ribbon_alpha(&self, now: Instant, reduce_motion: bool) -> f32 {
        if !self.strip_switcher_offered() {
            return 0.0;
        }
        if self.strip_switcher.is_open() {
            return 1.0;
        }
        // One strip is no news: a lone mark at mid-height said nothing and
        // read as a stray sliver at the screen's edge. It shows while the
        // pointer reaches for it (and while moving), as a second strip does
        // at rest.
        let rest = if self.strip_ribbon().count > 1 {
            RIBBON_REST_ALPHA
        } else {
            0.0
        };
        self.strip_indicators_alpha(now, reduce_motion).max(rest)
    }

    /// Let the pointer timers settle at paint time too: a frame is what the
    /// wake deadline asks for, and a host that paints without updating
    /// first must still see the labels open under a resting pointer.
    pub(super) fn settle_strip_switcher(&mut self, now: Instant) {
        let zone = self.strip_switcher_zone();
        if self.strip_switcher.settle(now, zone) {
            self.place_strip_switcher();
            self.publish_strip_switcher_open();
        }
    }

    /// Rebuild the switcher's model when the chrome changed, and let pointer
    /// timers settle. Returns whether the labels need a repaint.
    pub(super) fn refresh_strip_switcher_model(&mut self) -> bool {
        let now = Instant::now();
        let mut dirty = false;
        if let Some(chrome) = self.chrome.clone() {
            let state = chrome.borrow();
            if self.strip_switcher_revision != Some(state.revision) {
                self.strip_switcher_revision = Some(state.revision);
                let local_user = state.active_local_user();
                let own = state.local_user_id.clone();
                let tree = state.tree();
                let model = SwitcherModel::of(&tree.desktop, &local_user, &own, |strip| {
                    tree.may_rename_strip(strip)
                });
                drop(tree);
                drop(state);
                dirty |= self.strip_switcher.is_open() && self.strip_switcher.model() != &model;
                self.strip_switcher.set_model(model);
            }
        }
        let rename = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.borrow_mut().take_strip_rename_request());
        if rename {
            self.rename_focused_strip_in_switcher();
            dirty = true;
        }
        let show = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.borrow_mut().take_strip_switcher_request());
        if show && self.strip_switcher_offered() {
            self.strip_switcher.open_for_choosing(now);
            dirty = true;
        }
        if !self.strip_switcher_offered() && self.strip_switcher.is_open() {
            self.strip_switcher.close();
            dirty = true;
        }
        self.place_strip_switcher();
        let zone = self.strip_switcher_zone();
        dirty |= self.strip_switcher.settle(now, zone);
        if dirty {
            self.publish_strip_switcher_open();
        }
        dirty
    }

    fn place_strip_switcher(&mut self) {
        let ribbon = self.strip_ribbon();
        self.strip_switcher.place(&ribbon);
    }

    /// Mirror the switcher's open state into the chrome, which repaints.
    pub(super) fn publish_strip_switcher_open(&self) {
        if let Some(chrome) = self.chrome.as_ref() {
            let open = self.strip_switcher.is_open();
            let mut state = chrome.borrow_mut();
            state.set_strip_switcher_open(open);
            state.bump();
        }
    }

    /// When the switcher's pointer timers next owe a frame.
    pub(super) fn strip_switcher_wake_deadline(&self, now: Instant) -> Option<Instant> {
        self.strip_switcher
            .deadline(now, self.strip_switcher_zone())
    }

    /// Super+S.
    pub(super) fn toggle_strip_switcher_from_keyboard(&mut self) {
        if !self.strip_switcher_offered() {
            return;
        }
        self.strip_switcher_revision = None;
        self.refresh_strip_switcher_model();
        self.strip_switcher.toggle_from_keyboard(Instant::now());
        self.place_strip_switcher();
        self.publish_strip_switcher_open();
    }

    /// The `strip.rename` action without a name: the labels open with the
    /// focused strip's name selected in place.
    fn rename_focused_strip_in_switcher(&mut self) {
        if !self.strip_switcher_offered() {
            return;
        }
        self.place_strip_switcher();
        self.strip_switcher.rename_focused(Instant::now());
        self.place_strip_switcher();
        self.publish_strip_switcher_open();
    }

    /// A key while the switcher takes the keyboard.
    pub(super) fn strip_switcher_key(&mut self, key: &Key, mods: ModifiersState) {
        match self.strip_switcher.key(key, mods) {
            KeyAnswer::Command(command) => self.apply_strip_switcher_command(command),
            KeyAnswer::Consumed | KeyAnswer::Ignored => {}
        }
        self.publish_strip_switcher_open();
    }

    /// The pointer moved to `p` (layout pixels).
    pub(super) fn strip_switcher_pointer_moved(&mut self, p: Point) {
        let zone = self.strip_switcher_zone();
        if zone.area() <= 0.0 && !self.strip_switcher.is_open() {
            return;
        }
        if self.strip_switcher.pointer_moved(p, Instant::now(), zone) {
            self.place_strip_switcher();
            self.publish_strip_switcher_open();
        }
    }

    /// The pointer left the display.
    pub(super) fn strip_switcher_pointer_left(&mut self) {
        if self.strip_switcher.pointer_left() {
            self.publish_strip_switcher_open();
        }
    }

    /// A press or release for the open labels. Returns whether it was the
    /// switcher's.
    pub(super) fn route_strip_switcher_pointer(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::MouseInput { state, button, .. } = event else {
            return false;
        };
        if *button != MouseButton::Left || !self.strip_switcher.is_open() {
            return false;
        }
        let Some(p) = self.last_cursor else {
            return false;
        };
        if *state == ElementState::Released {
            return std::mem::take(&mut self.strip_switcher_owns_release);
        }
        let answer = self.strip_switcher.press(p, Instant::now());
        let ours = !matches!(answer, Press::Outside);
        if let Press::Command(command) = answer {
            self.apply_strip_switcher_command(command);
        }
        self.strip_switcher_owns_release = ours;
        self.publish_strip_switcher_open();
        ours
    }

    /// A wheel or two-finger scroll: over the ribbon or its labels it moves
    /// a strip up or down. Returns whether it was the switcher's.
    pub(super) fn route_strip_switcher_wheel(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::MouseWheel { delta, .. } = event else {
            return false;
        };
        let Some(p) = self.last_cursor else {
            return false;
        };
        if !self.strip_switcher_offered() {
            return false;
        }
        let zone = self.strip_switcher_zone();
        let on = zone.contains(p) || self.strip_switcher.target_at(p).is_some();
        if let Some(command) = self.strip_switcher.scroll(p, *delta, Instant::now(), zone) {
            self.apply_strip_switcher_command(command);
            self.place_strip_switcher();
            self.publish_strip_switcher_open();
        }
        on
    }

    /// A semantic target of the switcher, activated as a press would.
    pub(super) fn activate_strip_switcher_target(&mut self, target: &str) -> Option<bool> {
        let answer = self.strip_switcher.activate_target(target)?;
        if let Press::Command(command) = answer {
            self.apply_strip_switcher_command(command);
        }
        self.publish_strip_switcher_open();
        Some(true)
    }

    fn apply_strip_switcher_command(&mut self, command: Command) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let created = matches!(command, Command::Create(_));
        let refused = {
            let mut state = chrome.borrow_mut();
            let refused = match command {
                Command::Focus(strip) => {
                    state.focus_strip(strip);
                    None
                }
                Command::Step(steps) => {
                    for _ in 0..steps.unsigned_abs() {
                        if !state.switch_strip(steps.signum()) {
                            break;
                        }
                    }
                    None
                }
                Command::Create(name) => state.create_strip(&name).err(),
                Command::Rename(strip, name) => state.rename_strip(strip, &name).err(),
            };
            if let Some(message) = &refused {
                state.push_warning_notification(t("strip-switcher-refused"), message);
            }
            refused
        };
        if created && refused.is_none() {
            // The new strip is listed now, and its name goes in place.
            self.strip_switcher_revision = None;
            self.refresh_strip_switcher_model();
        }
    }

    /// Paint the labels when they are open.
    pub(super) fn draw_strip_switcher_card(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        now: Instant,
    ) {
        if !self.strip_switcher.is_open() {
            return;
        }
        let ribbon = self.strip_ribbon();
        let focus = self.strip_ribbon_focus_at(now);
        self.strip_switcher.place_sliding(&ribbon, focus);
        self.strip_switcher.draw(
            &mut DrawCx {
                scene,
                transform,
                text,
                theme,
            },
            now,
        );
    }

    /// The switcher's named targets, for harnesses.
    pub(super) fn strip_switcher_test_rects(&self) -> Vec<(String, Rect)> {
        self.strip_switcher.test_rects(self.strip_switcher_zone())
    }

    /// The open labels for assistive technology.
    pub(super) fn strip_switcher_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.strip_switcher.accessibility()
    }

    /// Test access to the switcher's state.
    #[cfg(test)]
    pub(crate) fn strip_switcher_for_tests(&mut self) -> &mut StripSwitcher {
        self.place_strip_switcher();
        &mut self.strip_switcher
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use winit::dpi::PhysicalPosition;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn model(strips: &[&str]) -> SwitcherModel {
        SwitcherModel {
            strips: strips
                .iter()
                .enumerate()
                .map(|(i, name)| StripEntry {
                    id: StripId(u64::try_from(i).map_or(100, |i| i.saturating_add(100))),
                    name: (*name).into(),
                    people: Vec::new(),
                    renamable: true,
                })
                .collect(),
            focused: 0,
        }
    }

    const CONTENT: Rect = Rect::new(0.0, 24.0, 1600.0, 900.0);

    fn ribbon(count: usize, focused: usize) -> StripRibbon {
        StripRibbon {
            content: CONTENT,
            count,
            focused,
        }
    }

    fn switcher(strips: &[&str]) -> (StripSwitcher, Rect) {
        let mut s = StripSwitcher::default();
        s.set_model(model(strips));
        let r = ribbon(strips.len(), 0);
        s.place(&r);
        let zone = zone(&r);
        (s, zone)
    }

    fn zone(r: &StripRibbon) -> Rect {
        let first = r.hit_rect(0).unwrap();
        let last = r.hit_rect(r.count.saturating_sub(1)).unwrap();
        first.union(last)
    }

    fn typed(s: &mut StripSwitcher, text: &str) {
        for c in text.chars() {
            s.key(
                &Key::Character(c.to_string().into()),
                ModifiersState::empty(),
            );
        }
    }

    fn enter(s: &mut StripSwitcher) -> KeyAnswer {
        s.key(&Key::Named(NamedKey::Enter), ModifiersState::empty())
    }

    /// Glide from `from` to `to` in samples 8 ms apart, slowing to a stop at
    /// the end; returns the instant of the last sample.
    fn glide(s: &mut StripSwitcher, zone: Rect, from: Point, to: Point, t0: Instant) -> Instant {
        let steps = 12;
        let mut t = t0;
        for k in 1..=steps {
            let f = f64::from(k) / f64::from(steps);
            let eased = 1.0 - (1.0 - f) * (1.0 - f);
            t = t.checked_add(ms(8)).unwrap();
            s.pointer_moved(from.lerp(to, eased), t, zone);
        }
        t
    }

    #[test]
    fn a_pointer_coming_to_rest_on_the_ribbon_opens_the_labels() {
        let (mut s, zone) = switcher(&["Lounge", "Research", "Design"]);
        let t0 = Instant::now();
        let end = glide(
            &mut s,
            zone,
            Point::new(1200.0, 460.0),
            Point::new(1590.0, zone.center().y),
            t0,
        );
        let opened = s.is_open() || s.settle(end + ms(60), zone) && s.is_open();
        assert!(opened, "aiming at the ribbon and stopping there opens it");
    }

    #[test]
    fn a_pointer_passing_over_the_ribbon_does_not_open_it() {
        let (mut s, zone) = switcher(&["Lounge", "Research", "Design"]);
        let mut t = Instant::now();
        // A fast vertical sweep along the edge, through the band and out.
        for k in 0..30 {
            t += ms(8);
            s.pointer_moved(Point::new(1590.0, 200.0 + 25.0 * f64::from(k)), t, zone);
            assert!(!s.is_open(), "transit is not aim (sample {k})");
        }
        // It ends far below the band, so resting there is not resting on it.
        assert!(!s.settle(t + ms(200), zone));
        assert!(!s.is_open());
    }

    #[test]
    fn each_label_sits_level_with_its_mark_and_new_strip_under_the_last() {
        let (s, _) = switcher(&["Lounge", "Research", "Design"]);
        let r = ribbon(3, 0);
        let mut x1 = None;
        for i in 0..3 {
            let label = s.layout().label_rect(Row::Strip(i)).unwrap();
            let row = s.layout().row_rect(Row::Strip(i)).unwrap();
            let seg = r.segment_rect(i).unwrap();
            let hit = r.hit_rect(i).unwrap();
            assert!(
                (label.center().y - seg.center().y).abs() < 0.5,
                "label {i} is level with its mark"
            );
            assert!(label.x1 < seg.x0, "label {i} stands beside its mark");
            assert_eq!(
                (row.y0, row.y1, row.x1),
                (hit.y0, hit.y1, CONTENT.x1),
                "label {i}'s pointer row tiles with the marks"
            );
            assert!(x1.is_none_or(|x| x == label.x1), "right edges aligned");
            x1 = Some(label.x1);
        }
        let last = s.layout().label_rect(Row::Strip(2)).unwrap();
        let new = s.layout().label_rect(Row::NewStrip).unwrap();
        assert!(new.y0 >= last.y1, "New strip is under the last strip");
    }

    /// The switcher has one job: it lists this workspace's strips and makes
    /// new ones, and nothing about workspaces.
    #[test]
    fn the_labels_are_this_workspaces_strips_and_new_strip_only() {
        let (mut s, _) = switcher(&["Lounge", "Research"]);
        s.open_at(Instant::now(), false);
        let rows: Vec<Row> = s.layout().labels.iter().map(|(row, _)| *row).collect();
        assert_eq!(rows, [Row::Strip(0), Row::Strip(1), Row::NewStrip]);
        let a11y = s.accessibility().expect("open labels are published");
        let names: Vec<String> = a11y
            .children
            .iter()
            .filter_map(|n| n.name.clone())
            .collect();
        assert!(names.iter().any(|n| n == "Lounge"));
        assert!(names.iter().any(|n| n == "New strip"));
        assert!(
            !names.iter().any(|n| n.contains("orkspace")),
            "no workspaces here: {names:?}"
        );
    }

    #[test]
    fn leaving_the_labels_closes_them() {
        let (mut s, zone) = switcher(&["Lounge", "Research"]);
        let now = Instant::now();
        s.open_at(now, false);
        let label = s.layout().label_rect(Row::Strip(0)).unwrap();
        s.pointer_moved(label.center(), now + ms(8), zone);
        assert!(s.is_open());
        s.pointer_moved(
            Point::new(label.x0 - 120.0, label.center().y),
            now + ms(16),
            zone,
        );
        assert!(!s.is_open(), "the pointer moved away");
    }

    #[test]
    fn a_click_on_a_label_switches_to_its_strip() {
        let (mut s, _) = switcher(&["Lounge", "Research"]);
        let now = Instant::now();
        s.open_at(now, false);
        let p = s.layout().label_rect(Row::Strip(1)).unwrap().center();
        assert_eq!(
            s.press(p, now),
            Press::Command(Command::Focus(StripId(101)))
        );
        // Between two short labels, left of them, is not a label.
        let label = s.layout().label_rect(Row::Strip(1)).unwrap();
        let left = Point::new(label.x0 - 40.0, label.center().y);
        assert_eq!(s.press(left, now + ms(800)), Press::Outside);
    }

    /// No text field in a surface that hover alone keeps up: naming pins the
    /// labels, so the pointer wandering off does not drop the name.
    #[test]
    fn new_strip_makes_a_strip_and_puts_its_name_in_place_and_pins_the_labels() {
        let (mut s, zone) = switcher(&["Lounge"]);
        let now = Instant::now();
        s.open_at(now, false);
        let new = s.layout().label_rect(Row::NewStrip).unwrap().center();
        assert_eq!(
            s.press(new, now),
            Press::Command(Command::Create("Strip 2".into())),
            "a click makes the strip at once"
        );
        // The host makes it and moves to it; the model lists it.
        let mut m = model(&["Lounge", "Strip 2"]);
        m.focused = 1;
        s.set_model(m);
        s.place(&ribbon(2, 1));
        assert!(s.is_naming() && s.takes_keys(), "its name is in place");
        assert_eq!(s.naming_text(), Some("Strip 2"), "selected, to type over");
        let field = s.layout().label_rect(Row::Strip(1)).unwrap();
        assert_eq!(field.width(), FIELD_W, "the label grows into the field");
        // The pointer leaves: the field stays.
        s.pointer_moved(Point::new(200.0, 400.0), now + ms(30), zone);
        assert!(s.is_open() && s.is_naming(), "naming pins the labels");
        typed(&mut s, "Games");
        assert_eq!(
            enter(&mut s),
            KeyAnswer::Command(Command::Rename(StripId(101), "Games".into()))
        );
        assert!(!s.is_naming());
        // With the name done, hover rules again.
        s.pointer_moved(Point::new(210.0, 400.0), now + ms(60), zone);
        assert!(!s.is_open());
    }

    #[test]
    fn a_double_click_renames_and_an_empty_or_unchanged_name_does_nothing() {
        let (mut s, _) = switcher(&["main", "Research"]);
        let t0 = Instant::now();
        s.open_at(t0, false);
        let label = s.layout().label_rect(Row::Strip(0)).unwrap().center();
        assert_eq!(
            s.press(label, t0),
            Press::Command(Command::Focus(StripId(100)))
        );
        assert_eq!(s.press(label, t0 + ms(150)), Press::Consumed);
        assert_eq!(s.naming_text(), Some("main"), "the old name, selected");
        typed(&mut s, "Lounge");
        assert_eq!(
            enter(&mut s),
            KeyAnswer::Command(Command::Rename(StripId(100), "Lounge".into()))
        );
        // Unchanged.
        let other = s.layout().label_rect(Row::Strip(1)).unwrap().center();
        s.press(other, t0 + ms(1000));
        s.press(other, t0 + ms(1100));
        assert_eq!(enter(&mut s), KeyAnswer::Consumed);
        // Emptied.
        s.press(other, t0 + ms(2000));
        s.press(other, t0 + ms(2100));
        s.key(&Key::Named(NamedKey::Backspace), ModifiersState::empty());
        assert_eq!(s.naming_text(), Some(""));
        assert_eq!(enter(&mut s), KeyAnswer::Consumed);
        // Two presses far apart are two switches, not a rename.
        s.press(other, t0 + ms(3000));
        s.press(other, t0 + ms(4000));
        assert!(!s.is_naming());
        // A strip another device writes is not renamed here.
        let mut m = model(&["Theirs"]);
        m.strips[0].renamable = false;
        s.set_model(m);
        s.place(&ribbon(1, 0));
        let theirs = s.layout().label_rect(Row::Strip(0)).unwrap().center();
        s.press(theirs, t0 + ms(5000));
        s.press(theirs, t0 + ms(5100));
        assert!(!s.is_naming());
    }

    #[test]
    fn the_wheel_over_the_ribbon_or_its_labels_moves_a_strip_per_notch() {
        let (mut s, zone) = switcher(&["Lounge", "Research", "Design"]);
        let t0 = Instant::now();
        let on = zone.center();
        assert_eq!(
            s.scroll(on, MouseScrollDelta::LineDelta(0.0, -1.0), t0, zone),
            Some(Command::Step(1)),
            "a notch toward the person: the strip below"
        );
        assert_eq!(
            s.scroll(on, MouseScrollDelta::LineDelta(0.0, 1.0), t0 + ms(50), zone),
            Some(Command::Step(-1)),
            "away: the strip above"
        );
        // Two-finger travel adds up to a step.
        let px = |y: f64| MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, y));
        let t1 = t0 + ms(1000);
        assert_eq!(s.scroll(on, px(-15.0), t1, zone), None);
        assert_eq!(s.scroll(on, px(-15.0), t1 + ms(10), zone), None);
        assert_eq!(
            s.scroll(on, px(-15.0), t1 + ms(20), zone),
            Some(Command::Step(1))
        );
        // Over a window, the wheel is the window's.
        assert_eq!(
            s.scroll(
                Point::new(400.0, 400.0),
                MouseScrollDelta::LineDelta(0.0, -1.0),
                t1 + ms(900),
                zone
            ),
            None
        );
        // Over an open label it moves strips too.
        s.open_at(t1, false);
        let label = s.layout().label_rect(Row::Strip(0)).unwrap();
        assert_eq!(
            s.scroll(
                label.center(),
                MouseScrollDelta::LineDelta(0.0, -1.0),
                t1 + ms(2000),
                zone
            ),
            Some(Command::Step(1))
        );
    }

    #[test]
    fn the_keyboard_walks_the_labels_and_takes_one() {
        let (mut s, _) = switcher(&["Lounge", "Research"]);
        s.toggle_from_keyboard(Instant::now());
        assert!(s.is_open() && s.takes_keys());
        assert_eq!(s.selected(), Some(Row::Strip(0)));
        let none = ModifiersState::empty();
        s.key(&Key::Named(NamedKey::ArrowDown), none);
        assert_eq!(s.selected(), Some(Row::Strip(1)));
        assert_eq!(
            enter(&mut s),
            KeyAnswer::Command(Command::Focus(StripId(101)))
        );
        assert!(
            !s.is_open(),
            "a choice closes labels opened by the keyboard"
        );

        s.toggle_from_keyboard(Instant::now());
        s.key(&Key::Named(NamedKey::F2), none);
        assert!(s.is_naming());
        assert_eq!(
            s.key(&Key::Named(NamedKey::Escape), none),
            KeyAnswer::Consumed
        );
        assert!(
            !s.is_naming() && s.is_open(),
            "Escape leaves the name first"
        );
        s.key(&Key::Named(NamedKey::End), none);
        assert_eq!(s.selected(), Some(Row::NewStrip));
        assert_eq!(
            enter(&mut s),
            KeyAnswer::Command(Command::Create("Strip 3".into()))
        );
        assert!(s.is_open(), "the new strip's name comes next");
        assert_eq!(
            s.key(&Key::Named(NamedKey::Escape), none),
            KeyAnswer::Consumed
        );
        assert!(!s.is_open());
    }

    #[test]
    fn labels_light_only_once_the_pointer_settles() {
        let (mut s, zone) = switcher(&["Lounge", "Research", "Design"]);
        let t0 = Instant::now();
        s.open_at(t0, false);
        let r0 = s.layout().label_rect(Row::Strip(0)).unwrap();
        let r2 = s.layout().label_rect(Row::Strip(2)).unwrap();
        // Crossing label 0 quickly on the way to label 2.
        s.pointer_moved(Point::new(r0.center().x, r0.y0 - 4.0), t0 + ms(8), zone);
        s.pointer_moved(r0.center(), t0 + ms(16), zone);
        assert_eq!(s.hover(), None, "in transit over label 0");
        s.pointer_moved(r2.center(), t0 + ms(24), zone);
        assert!(s.deadline(t0 + ms(24), zone).is_some(), "wakes to light it");
        s.settle(t0 + ms(100), zone);
        assert_eq!(s.hover(), Some(Target::Row(Row::Strip(2))));
        assert_eq!(s.lit_strip(), Some(2), "and its mark lights with it");
    }

    #[test]
    fn semantic_targets_activate_like_presses() {
        let (mut s, _) = switcher(&["Lounge", "Research"]);
        s.open_at(Instant::now(), false);
        let name = StripSwitcher::target_name(Target::Row(Row::Strip(1)));
        assert_eq!(
            s.activate_target(&name),
            Some(Press::Command(Command::Focus(StripId(101))))
        );
        assert_eq!(
            s.activate_target(&StripSwitcher::rename_target_name(0)),
            Some(Press::Consumed)
        );
        assert!(s.is_naming());
        assert_eq!(s.activate_target("palette:query"), None);
        let a11y = s.accessibility().expect("open labels are published");
        assert!(a11y.children.len() >= 5);
    }
}
