//! The strip switcher: the strip ribbon grown into a list when the pointer
//! means it.
//!
//! At rest the switcher is the ribbon on the content's right edge: one small
//! mark per strip, the focused one long. Reaching for it opens a card beside
//! it, each row level with its ribbon segment so nothing under the pointer
//! moves: the strip's name, who is on it, a way to rename it, a row that adds
//! a strip, and below, the workspaces, which are switched less often but
//! must stay one click away.
//!
//! Opening follows aim, not contact ([`EdgeAim`]): a pointer that comes to
//! rest on the ribbon opens the card, one sweeping past the screen edge on
//! its way somewhere else does not, and a row lights under the pointer only
//! once it settles there. The card closes the moment the pointer leaves it,
//! unless a name is being typed. Super+S opens it for the keyboard: arrows
//! choose a row, Enter takes it, F2 renames a strip, Escape closes.
//!
//! The state is pure: every method takes the instant it reasons about, and
//! the host turns a returned [`Command`] into the change on the desktop.

use std::time::{Duration, Instant};

use tilo_desktop::text_entry::{self, Edit, Mods};
use ui::TextRole;
use ui::chrome::shell::{RIBBON_HIT_W, StripRibbon};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::text_selection::Selection;
use ui::theme::Theme;
use ui::widget::{EdgeAim, FieldFace, TextInput, Widget as _};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, RoundedRectRadii, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::shell::i18n::t;
use crate::shell::search::providers::actions::{STRIP_NAME_MAX_CHARS, valid_strip_name};
use crate::shell::state::{StripId, WorkspaceId};
use tilo_desktop_model::DesktopTree;

/// The card's width, ribbon band included.
pub(crate) const CARD_W: f64 = 272.0;
/// A row that is not level with a ribbon segment: the minimum pointer target.
const ROW_H: f64 = 32.0;
/// The workspace's name above the strips.
const HEADER_H: f64 = 28.0;
/// "Workspaces" above the workspace rows.
const SECTION_H: f64 = 26.0;
/// The hairline between the strips and the workspaces, with its air.
const DIVIDER_H: f64 = 9.0;
/// Inner padding of the card.
const PAD: f64 = 6.0;
/// The card keeps this far from the content's top and bottom.
const MARGIN: f64 = 8.0;
/// A pointer this far outside the card still counts as on it, so a hand that
/// overshoots a row's edge by a few pixels does not close it.
const GRACE: f64 = 8.0;
const AVATAR: f64 = 18.0;
const AVATAR_STEP: f64 = 13.0;
/// More people than this on a strip collapse into "+N".
const AVATARS_SHOWN: usize = 3;
/// The rename control at a strip row's end.
const RENAME_W: f64 = 28.0;
/// The card fades in over this long; reduced motion shows it at once.
const OPEN_FADE: Duration = Duration::from_millis(120);

/// Someone other than this person, on a strip.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Person {
    pub id: String,
    pub label: String,
    pub color: Color,
}

/// One strip of the active workspace, as the card lists it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StripEntry {
    pub id: StripId,
    pub name: String,
    pub people: Vec<Person>,
    /// This machine writes the strip's name ([`crate::shell::state::WorkspaceTree::may_rename_strip`]).
    pub renamable: bool,
}

/// One workspace, as the card lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceEntry {
    pub id: WorkspaceId,
    pub name: String,
}

/// What the card shows: the active workspace's strips and every workspace.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SwitcherModel {
    pub workspace_name: String,
    pub strips: Vec<StripEntry>,
    /// Index into `strips` of the strip with focus.
    pub focused: usize,
    pub workspaces: Vec<WorkspaceEntry>,
    /// Index into `workspaces` of the one shown.
    pub active_workspace: usize,
}

impl SwitcherModel {
    /// The card's model for `desktop` as the person `local_user` (`own_id`
    /// on this device) sees it: the strips of the active workspace with the
    /// others on each, and every workspace. `may_rename` says which strips
    /// this machine names.
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
        let workspaces: Vec<WorkspaceEntry> = desktop
            .workspaces
            .iter()
            .map(|w| WorkspaceEntry {
                id: w.id,
                name: w.name.clone(),
            })
            .collect();
        let active_workspace = workspaces
            .iter()
            .position(|w| w.id == workspace.id)
            .unwrap_or(0);
        Self {
            workspace_name: workspace.name.clone(),
            strips,
            focused,
            workspaces,
            active_workspace,
        }
    }
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

/// A row of the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    Strip(usize),
    NewStrip,
    Workspace(usize),
    NewWorkspace,
}

/// Something in the card a press lands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Row(Row),
    /// The rename control of strip `n`.
    Rename(usize),
    /// The name field while a name is typed.
    Field,
}

/// What the host does for the switcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    FocusStrip(StripId),
    CreateStrip(String),
    RenameStrip(StripId, String),
    SelectWorkspace(WorkspaceId),
    NewWorkspace,
}

/// The answer to a press.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Press {
    /// Not the card's: the press goes on to whatever is under it.
    Outside,
    /// The card's, and nothing for the host to do.
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
    New,
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
    at: Instant,
    /// Opened by Super+S: stays until a choice or Escape, and the keyboard
    /// is the card's.
    by_keyboard: bool,
}

/// Where everything in the open card sits.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Layout {
    pub card: Rect,
    pub header: Rect,
    pub section: Rect,
    pub divider_y: f64,
    pub rows: Vec<(Row, Rect)>,
}

impl Layout {
    /// Lay the card out beside `ribbon`, inside `content`: each strip row
    /// level with its segment's pointer target, the add row under the last,
    /// then the workspaces. Moved as a whole only when it would leave
    /// `content`.
    pub(crate) fn beside(model: &SwitcherModel, ribbon: &StripRibbon, content: Rect) -> Self {
        let x1 = content.x1;
        let x0 = x1 - CARD_W;
        let row = |y0: f64, h: f64| Rect::new(x0 + PAD, y0, x1 - PAD, y0 + h);
        let mut rows = Vec::new();
        let first_y = ribbon
            .hit_rect(0)
            .map_or_else(|| content.center().y - ROW_H / 2.0, |r| r.y0);
        let header = row(first_y - HEADER_H, HEADER_H);
        let mut y = first_y;
        for i in 0..model.strips.len() {
            let r = ribbon
                .hit_rect(i)
                .map_or_else(|| row(y, ROW_H), |seg| row(seg.y0, seg.height()));
            y = r.y1;
            rows.push((Row::Strip(i), r));
        }
        rows.push((Row::NewStrip, row(y, ROW_H)));
        y += ROW_H;
        let divider_y = y + DIVIDER_H / 2.0;
        y += DIVIDER_H;
        let section = row(y, SECTION_H);
        y += SECTION_H;
        for i in 0..model.workspaces.len() {
            rows.push((Row::Workspace(i), row(y, ROW_H)));
            y += ROW_H;
        }
        rows.push((Row::NewWorkspace, row(y, ROW_H)));
        y += ROW_H;
        let card = Rect::new(x0, header.y0 - PAD, x1, y + PAD);
        let low = content.y1 - MARGIN - card.y1;
        let high = content.y0 + MARGIN - card.y0;
        let dy = if low < 0.0 {
            low.max(high)
        } else if high > 0.0 {
            high
        } else {
            0.0
        };
        let shift = |r: Rect| Affine::translate((0.0, dy)).transform_rect_bbox(r);
        Self {
            card: shift(card),
            header: shift(header),
            section: shift(section),
            divider_y: divider_y + dy,
            rows: rows.into_iter().map(|(k, r)| (k, shift(r))).collect(),
        }
    }

    pub(crate) fn row_rect(&self, row: Row) -> Option<Rect> {
        self.rows.iter().find(|(k, _)| *k == row).map(|(_, r)| *r)
    }

    /// Where a row's own content ends: before the ribbon band, so a strip
    /// row's segment stays its marker.
    fn content_x1(&self) -> f64 {
        self.card.x1 - RIBBON_HIT_W
    }

    /// The rename control of a strip row.
    pub(crate) fn rename_rect(&self, strip: usize) -> Option<Rect> {
        let r = self.row_rect(Row::Strip(strip))?;
        let x1 = self.content_x1() - 2.0;
        Some(Rect::new(
            x1 - RENAME_W,
            r.center().y - RENAME_W / 2.0,
            x1,
            r.center().y + RENAME_W / 2.0,
        ))
    }

    /// Where a row's text sits.
    fn text_rect(&self, row: Rect) -> Rect {
        Rect::new(row.x0 + 10.0, row.y0, self.content_x1() - 4.0, row.y1)
    }
}

/// The switcher's state between frames.
#[derive(Default)]
pub(crate) struct StripSwitcher {
    model: SwitcherModel,
    open: Option<Opened>,
    aim: EdgeAim,
    /// The last pointer position seen, and whether it was in the ribbon band.
    pointer: Option<Point>,
    /// Closed with the pointer on the ribbon: it has to leave before resting
    /// there opens the card again.
    disarmed: bool,
    /// What the pointer is on, once it has settled there.
    hover: Option<Target>,
    /// The keyboard's row.
    selected: Option<Row>,
    naming: Option<Naming>,
    layout: Layout,
    /// The theme's reduced-motion setting as of the last paint.
    reduce_motion: bool,
}

impl StripSwitcher {
    pub(crate) fn set_model(&mut self, model: SwitcherModel) {
        if self.model != model {
            self.model = model;
            if let Some(Row::Strip(i) | Row::Workspace(i)) = self.selected {
                let (strips, workspaces) = (self.model.strips.len(), self.model.workspaces.len());
                if matches!(self.selected, Some(Row::Strip(_))) && i >= strips
                    || matches!(self.selected, Some(Row::Workspace(_))) && i >= workspaces
                {
                    self.selected = None;
                }
            }
        }
    }

    pub(crate) fn model(&self) -> &SwitcherModel {
        &self.model
    }

    /// Lay the card out beside `ribbon`; call before hit-testing or drawing.
    pub(crate) fn place(&mut self, ribbon: &StripRibbon, content: Rect) {
        self.layout = Layout::beside(&self.model, ribbon, content);
    }

    pub(crate) fn layout(&self) -> &Layout {
        &self.layout
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The keyboard is the card's: opened by Super+S, or a name is typed.
    pub(crate) fn takes_keys(&self) -> bool {
        self.naming.is_some() || self.open.is_some_and(|o| o.by_keyboard)
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

    fn open_at(&mut self, now: Instant, by_keyboard: bool) {
        if self.open.is_none() {
            self.open = Some(Opened {
                at: now,
                by_keyboard,
            });
        } else if let Some(open) = self.open.as_mut() {
            open.by_keyboard |= by_keyboard;
        }
    }

    /// Close the card, dropping a name being typed.
    pub(crate) fn close(&mut self) {
        if self.open.take().is_some() {
            self.disarmed = true;
        }
        self.naming = None;
        self.hover = None;
        self.selected = None;
    }

    /// Super+S: open the card for the keyboard on the focused strip, or close
    /// it when it is open.
    pub(crate) fn toggle_from_keyboard(&mut self, now: Instant) {
        if self.open.is_some() {
            self.close();
            return;
        }
        self.open_at(now, true);
        self.selected = Some(Row::Strip(self.model.focused));
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
            Some(open) => {
                let on_card =
                    self.layout.card.inflate(GRACE, GRACE).contains(p) || zone.contains(p);
                if !on_card && self.naming.is_none() && !open.by_keyboard {
                    self.close();
                    // Left on purpose: the ribbon may open it again at once.
                    self.disarmed = false;
                }
            }
        }
        self.refresh_hover(now);
        before != (self.open, self.hover)
    }

    /// The pointer left the display.
    pub(crate) fn pointer_left(&mut self) -> bool {
        self.aim.clear();
        self.pointer = None;
        self.disarmed = false;
        let was = self.hover.take().is_some();
        if self.open.is_some_and(|o| !o.by_keyboard) && self.naming.is_none() {
            self.close();
            self.disarmed = false;
            return true;
        }
        was
    }

    /// Advance to `now` without input: a pointer that came to rest on the
    /// ribbon opens the card, one that settled on a row lights it. Returns
    /// whether anything visible changed.
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

    /// What in the open card is at `p`.
    pub(crate) fn target_at(&self, p: Point) -> Option<Target> {
        self.open?;
        if !self.layout.card.contains(p) {
            return None;
        }
        if let Some(naming) = &self.naming
            && self
                .field_rect(naming.target)
                .is_some_and(|r| r.contains(p))
        {
            return Some(Target::Field);
        }
        let (row, rect) = self
            .layout
            .rows
            .iter()
            .find(|(_, r)| p.y >= r.y0 && p.y < r.y1)
            .copied()?;
        if let Row::Strip(i) = row
            && self.model.strips.get(i).is_some_and(|s| s.renamable)
            && self.layout.rename_rect(i).is_some_and(|r| r.contains(p))
        {
            return Some(Target::Rename(i));
        }
        (p.x >= rect.x0 - PAD).then_some(Target::Row(row))
    }

    /// The name field for `target`: over the strip's name, or in the add row.
    fn field_rect(&self, target: NameTarget) -> Option<Rect> {
        let row = match target {
            NameTarget::New => self.layout.row_rect(Row::NewStrip)?,
            NameTarget::Rename(id) => {
                let i = self.model.strips.iter().position(|s| s.id == id)?;
                self.layout.row_rect(Row::Strip(i))?
            }
        };
        let text = self.layout.text_rect(row);
        Some(Rect::new(
            text.x0 - 4.0,
            row.center().y - 13.0,
            text.x1,
            row.center().y + 13.0,
        ))
    }

    /// A left press at `p`.
    pub(crate) fn press(&mut self, p: Point) -> Press {
        let Some(target) = self.target_at(p) else {
            if self.open.is_some() && (self.naming.is_some() || self.takes_keys()) {
                // A press elsewhere ends naming, and a card opened for the
                // keyboard; the press itself still goes where it landed.
                self.close();
            }
            return Press::Outside;
        };
        if target == Target::Field {
            return Press::Consumed;
        }
        self.naming = None;
        match target {
            Target::Field => Press::Consumed,
            Target::Rename(i) => {
                self.begin_rename(i);
                Press::Consumed
            }
            Target::Row(row) => self.activate(row),
        }
    }

    fn activate(&mut self, row: Row) -> Press {
        match row {
            Row::Strip(i) => self.model.strips.get(i).map_or(Press::Consumed, |s| {
                Press::Command(Command::FocusStrip(s.id))
            }),
            Row::NewStrip => {
                self.naming = Some(Naming {
                    target: NameTarget::New,
                    text: String::new(),
                    selection: Selection::collapsed(0),
                    input: None,
                });
                self.selected = Some(Row::NewStrip);
                Press::Consumed
            }
            Row::Workspace(i) if i == self.model.active_workspace => Press::Consumed,
            Row::Workspace(i) => self.model.workspaces.get(i).map_or(Press::Consumed, |w| {
                Press::Command(Command::SelectWorkspace(w.id))
            }),
            Row::NewWorkspace => Press::Command(Command::NewWorkspace),
        }
    }

    fn begin_rename(&mut self, i: usize) {
        let Some(strip) = self.model.strips.get(i).filter(|s| s.renamable) else {
            return;
        };
        let text = strip.name.clone();
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
    }

    /// Rename the focused strip from the keyboard or an action that asks
    /// for the field: opens the card with the name selected.
    pub(crate) fn rename_focused(&mut self, now: Instant) {
        self.open_at(now, true);
        self.begin_rename(self.model.focused);
    }

    /// Every row in keyboard order.
    fn rows_in_order(&self) -> Vec<Row> {
        (0..self.model.strips.len())
            .map(Row::Strip)
            .chain([Row::NewStrip])
            .chain((0..self.model.workspaces.len()).map(Row::Workspace))
            .chain([Row::NewWorkspace])
            .collect()
    }

    /// A key while the card takes the keyboard.
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
                    Press::Command(command) => {
                        self.close();
                        KeyAnswer::Command(command)
                    }
                    Press::Consumed | Press::Outside => {
                        if matches!(row, Row::Workspace(_)) {
                            // The workspace already shown: nothing to switch.
                            self.close();
                        }
                        KeyAnswer::Consumed
                    }
                }
            }
            // The card owns the keyboard while open for it; a stray key must
            // not reach the window behind.
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
        let Some(name) = valid_strip_name(&naming.text).map(str::to_owned) else {
            return KeyAnswer::Consumed;
        };
        let by_keyboard = self.open.is_some_and(|o| o.by_keyboard);
        let command = match naming.target {
            NameTarget::New => Command::CreateStrip(name),
            NameTarget::Rename(id) => {
                if self
                    .model
                    .strips
                    .iter()
                    .any(|s| s.id == id && s.name == name)
                {
                    return KeyAnswer::Consumed;
                }
                Command::RenameStrip(id, name)
            }
        };
        if by_keyboard {
            self.close();
        }
        KeyAnswer::Command(command)
    }

    /// Opacity of the card at `now`.
    fn alpha(&self, now: Instant, reduce_motion: bool) -> f32 {
        let Some(open) = self.open else {
            return 0.0;
        };
        if reduce_motion {
            return 1.0;
        }
        let t = now.saturating_duration_since(open.at).as_secs_f32() / OPEN_FADE.as_secs_f32();
        let t = t.clamp(0.0, 1.0);
        1.0 - (1.0 - t) * (1.0 - t)
    }

    /// Frames are owed only while the card fades in.
    pub(crate) fn needs_animation_frame(&self, now: Instant) -> bool {
        self.open.is_some() && self.alpha(now, self.reduce_motion) < 1.0
    }

    /// Paint the open card. The host paints the ribbon over its band.
    pub(crate) fn draw(&mut self, cx: &mut DrawCx<'_>, now: Instant) {
        self.reduce_motion = cx.theme.reduce_motion;
        let alpha = self.alpha(now, self.reduce_motion);
        if alpha <= 0.0 {
            return;
        }
        let card = self.layout.card;
        let fading = alpha < 1.0;
        if fading {
            cx.scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                alpha,
                cx.transform,
                &card.inflate(32.0, 32.0),
            );
        }
        paint_card_ground(cx.scene, cx.transform, cx.theme, card);
        self.draw_header(cx);
        for (row, rect) in self.layout.rows.clone() {
            self.draw_row(cx, row, rect);
        }
        let x_end = self.layout.content_x1();
        cx.scene.stroke(
            &Stroke::new(1.0),
            cx.transform,
            ui::chrome::chrome_hairline(cx.theme),
            None,
            &vello::kurbo::Line::new(
                (card.x0 + 12.0, self.layout.divider_y),
                (x_end, self.layout.divider_y),
            ),
        );
        if let Some(target) = self.naming.as_ref().map(|n| n.target)
            && let Some(field) = self.field_rect(target)
        {
            self.draw_field(cx, field);
        }
        if fading {
            cx.scene.pop_layer();
        }
    }

    fn draw_header(&self, cx: &mut DrawCx<'_>) {
        let style = cx.theme.text(TextRole::Caption);
        let dim = cx.theme.tokens.colors.text_dim;
        for (text, rect) in [
            (self.model.workspace_name.clone(), self.layout.header),
            (t("strip-switcher-workspaces"), self.layout.section),
        ] {
            let r = self.layout.text_rect(rect);
            draw_line(cx, &text, r, dim, &style);
        }
    }

    fn row_state(&self, row: Row) -> (bool, bool) {
        let active = match row {
            Row::Strip(i) => i == self.model.focused,
            Row::Workspace(i) => i == self.model.active_workspace,
            Row::NewStrip | Row::NewWorkspace => false,
        };
        let hover = self.hover == Some(Target::Row(row))
            || matches!((self.hover, row), (Some(Target::Rename(a)), Row::Strip(b)) if a == b)
            || self.selected == Some(row);
        (active, hover)
    }

    /// The row's selection wash and keyboard ring.
    fn draw_wash(&self, cx: &mut DrawCx<'_>, row: Row, rect: Rect, (active, hover): (bool, bool)) {
        let wash = Rect::new(
            rect.x0,
            rect.y0 + 2.0,
            self.layout.content_x1(),
            rect.y1 - 2.0,
        );
        if active || hover {
            let tier = ui::chrome::selection_tier(
                cx.theme,
                ui::chrome::SelectionState::new(active, hover, false),
            );
            let shape = RoundedRect::from_rect(
                wash,
                ui::chrome::ChromeRole::Control.radius(cx.theme, wash.height()),
            );
            cx.scene
                .fill(Fill::NonZero, cx.transform, tier.fill, None, &shape);
            if let Some(edge) = tier.border {
                cx.scene
                    .stroke(&Stroke::new(1.0), cx.transform, edge, None, &shape);
            }
        }
        if self.selected == Some(row) {
            let ring = RoundedRect::from_rect(
                wash.inflate(1.0, 1.0),
                ui::chrome::ChromeRole::Control.radius(cx.theme, wash.height()),
            );
            cx.scene.stroke(
                &Stroke::new(2.0),
                cx.transform,
                cx.theme.tokens.colors.border_focus,
                None,
                &ring,
            );
        }
    }

    /// A strip's row: its name, who is on it, and the rename control once
    /// the pointer settles on it.
    fn draw_strip_row(
        &self,
        cx: &mut DrawCx<'_>,
        i: usize,
        rect: Rect,
        (active, hover): (bool, bool),
    ) {
        let colors = &cx.theme.tokens.colors;
        let text_rect = self.layout.text_rect(rect);
        let ink = if active {
            colors.text_primary
        } else {
            colors.text_dim
        };
        let mut style = cx.theme.text(TextRole::Label);
        if active {
            style = style.with_weight(TextWeight::Semibold);
        }
        let Some(strip) = self.model.strips.get(i) else {
            return;
        };
        let renaming = self
            .naming
            .as_ref()
            .is_some_and(|n| n.target == NameTarget::Rename(strip.id));
        let mut x_end = text_rect.x1;
        let show_rename = strip.renamable && hover && !renaming;
        if show_rename && let Some(r) = self.layout.rename_rect(i) {
            let lit = self.hover == Some(Target::Rename(i));
            draw_glyph(
                cx,
                r,
                ui::icons::Icon::Pencil,
                if lit {
                    colors.text_primary
                } else {
                    colors.text_dim
                },
            );
            x_end = r.x0 - 2.0;
        }
        x_end = Self::draw_people(cx, &strip.people, x_end, rect.center().y);
        if !renaming {
            let name = if strip.name.trim().is_empty() {
                crate::shell::i18n::t_args(
                    "strip-switcher-unnamed",
                    &[("number", i.saturating_add(1).to_string().into())],
                )
            } else {
                strip.name.clone()
            };
            let r = Rect::new(text_rect.x0, rect.y0, x_end - 6.0, rect.y1);
            draw_line(cx, &name, r, ink, &style);
        }
    }

    fn draw_row(&self, cx: &mut DrawCx<'_>, row: Row, rect: Rect) {
        let (active, hover) = self.row_state(row);
        let colors = &cx.theme.tokens.colors;
        self.draw_wash(cx, row, rect, (active, hover));
        let text_rect = self.layout.text_rect(rect);
        let ink = if active {
            colors.text_primary
        } else {
            colors.text_dim
        };
        let mut style = cx.theme.text(TextRole::Label);
        if active {
            style = style.with_weight(TextWeight::Semibold);
        }
        match row {
            Row::Strip(i) => self.draw_strip_row(cx, i, rect, (active, hover)),
            Row::Workspace(i) => {
                let Some(workspace) = self.model.workspaces.get(i) else {
                    return;
                };
                let mut x_end = text_rect.x1;
                if active {
                    let r = Rect::from_center_size(
                        Point::new(text_rect.x1 - 10.0, rect.center().y),
                        (14.0, 14.0),
                    );
                    draw_glyph(cx, r, ui::icons::Icon::Check, colors.text_dim);
                    x_end = r.x0 - 4.0;
                }
                let r = Rect::new(text_rect.x0, rect.y0, x_end, rect.y1);
                draw_line(cx, &workspace.name, r, ink, &style);
            }
            Row::NewStrip | Row::NewWorkspace => {
                if row == Row::NewStrip
                    && self
                        .naming
                        .as_ref()
                        .is_some_and(|n| n.target == NameTarget::New)
                {
                    return;
                }
                let glyph = Rect::from_center_size(
                    Point::new(text_rect.x0 + 7.0, rect.center().y),
                    (14.0, 14.0),
                );
                draw_glyph(cx, glyph, ui::icons::Icon::Plus, colors.text_dim);
                let label = t(if row == Row::NewStrip {
                    "strip-switcher-new-strip"
                } else {
                    "strip-switcher-new-workspace"
                });
                let r = Rect::new(glyph.x1 + 8.0, rect.y0, text_rect.x1, rect.y1);
                draw_line(
                    cx,
                    &label,
                    r,
                    colors.text_dim,
                    &cx.theme.text(TextRole::Label),
                );
            }
        }
    }

    /// The others on a strip, right-aligned to end at `x_end`; returns where
    /// they begin.
    fn draw_people(cx: &mut DrawCx<'_>, people: &[Person], x_end: f64, cy: f64) -> f64 {
        if people.is_empty() {
            return x_end;
        }
        let shown = people.len().min(AVATARS_SHOWN);
        let extra = people.len().saturating_sub(shown);
        let slots = shown.saturating_add(usize::from(extra > 0));
        let width = AVATAR + AVATAR_STEP * ui::num::f64_of_usize(slots.saturating_sub(1));
        let x0 = x_end - width;
        let ground = cx.theme.tokens.colors.surfaces.overlay;
        let mut paint = ui::paint::PaintCtx::new(cx.scene, cx.transform, cx.text, cx.theme);
        for (k, person) in people.iter().take(shown).enumerate() {
            let x = x0 + AVATAR_STEP * ui::num::f64_of_usize(k);
            let rect = Rect::new(x, cy - AVATAR / 2.0, x + AVATAR, cy + AVATAR / 2.0);
            ui::widget::AvatarFace::new(&person.label, &person.id)
                .presence(person.color)
                .ringed_on(ground)
                .paint(&mut paint, rect);
        }
        if extra > 0 {
            let x = x0 + AVATAR_STEP * ui::num::f64_of_usize(shown);
            let rect = Rect::new(x, cy - AVATAR / 2.0, x + AVATAR, cy + AVATAR / 2.0);
            ui::widget::AvatarFace::paint_overflow(&mut paint, rect, extra, Some(ground));
        }
        x0
    }

    fn draw_field(&mut self, cx: &mut DrawCx<'_>, field: Rect) {
        let Some(naming) = self.naming.as_mut() else {
            return;
        };
        let placeholder = match naming.target {
            // The first strip is where a workspace's shared Lounge lives.
            NameTarget::Rename(id) if self.model.strips.first().is_some_and(|s| s.id == id) => {
                t("strip-switcher-lounge-placeholder")
            }
            _ => t("strip-switcher-name-placeholder"),
        };
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

    /// Named targets for harnesses: the card and each row.
    pub(crate) fn test_rects(&self, zone: Rect) -> Vec<(String, Rect)> {
        if self.open.is_none() {
            return vec![("strip_switcher".into(), zone)];
        }
        let mut out = vec![("strip_switcher".into(), self.layout.card)];
        for (row, rect) in &self.layout.rows {
            let name = match row {
                Row::Strip(i) => format!("strip_switcher.strip.{}", i.saturating_add(1)),
                Row::NewStrip => "strip_switcher.new_strip".into(),
                Row::Workspace(i) => format!("strip_switcher.workspace.{}", i.saturating_add(1)),
                Row::NewWorkspace => "strip_switcher.new_workspace".into(),
            };
            out.push((name, *rect));
            if let Row::Strip(i) = row
                && self.model.strips.get(*i).is_some_and(|s| s.renamable)
                && let Some(r) = self.layout.rename_rect(*i)
            {
                out.push((format!("strip_switcher.rename.{}", i.saturating_add(1)), r));
            }
        }
        if let Some(field) = self.naming.as_ref().and_then(|n| self.field_rect(n.target)) {
            out.push(("strip_switcher.name_field".into(), field));
        }
        out
    }

    /// The semantic target of a row, for assistive technology and agents.
    pub(crate) fn target_name(target: Target) -> String {
        match target {
            Target::Row(Row::Strip(i)) => format!("{TARGET_PREFIX}strip:{i}"),
            Target::Row(Row::NewStrip) => format!("{TARGET_PREFIX}new-strip"),
            Target::Row(Row::Workspace(i)) => format!("{TARGET_PREFIX}workspace:{i}"),
            Target::Row(Row::NewWorkspace) => format!("{TARGET_PREFIX}new-workspace"),
            Target::Rename(i) => format!("{TARGET_PREFIX}rename:{i}"),
            Target::Field => format!("{TARGET_PREFIX}field"),
        }
    }

    /// Activate the target named `target` as a press on it would.
    pub(crate) fn activate_target(&mut self, target: &str) -> Option<Press> {
        let rest = target.strip_prefix(TARGET_PREFIX)?;
        let index = |s: &str| s.parse::<usize>().ok();
        let target = match rest.split_once(':') {
            Some(("strip", i)) => Target::Row(Row::Strip(index(i)?)),
            Some(("workspace", i)) => Target::Row(Row::Workspace(index(i)?)),
            Some(("rename", i)) => Target::Rename(index(i)?),
            _ => match rest {
                "new-strip" => Target::Row(Row::NewStrip),
                "new-workspace" => Target::Row(Row::NewWorkspace),
                _ => return None,
            },
        };
        Some(match target {
            Target::Rename(i) => {
                self.begin_rename(i);
                Press::Consumed
            }
            Target::Row(row) => {
                self.naming = None;
                self.activate(row)
            }
            Target::Field => Press::Consumed,
        })
    }

    /// The open card for assistive technology: a group of buttons.
    pub(crate) fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        self.open?;
        let mut card = AccessNode::new(Role::Group)
            .with_name(t("strip-switcher-a11y"))
            .with_bounds(self.layout.card);
        for (row, rect) in &self.layout.rows {
            let name = match row {
                Row::Strip(i) => self.model.strips.get(*i).map(|s| s.name.clone()),
                Row::Workspace(i) => self.model.workspaces.get(*i).map(|w| w.name.clone()),
                Row::NewStrip => Some(t("strip-switcher-new-strip")),
                Row::NewWorkspace => Some(t("strip-switcher-new-workspace")),
            };
            let Some(name) = name else { continue };
            let (active, _) = self.row_state(*row);
            card.children.push(
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
                && let Some(r) = self.layout.rename_rect(*i)
            {
                card.children.push(
                    AccessNode::new(Role::Button)
                        .with_name(t("strip-switcher-rename"))
                        .with_bounds(r)
                        .with_action_target(Self::target_name(Target::Rename(*i))),
                );
            }
        }
        if let Some(naming) = &self.naming
            && let Some(field) = self.field_rect(naming.target)
        {
            card.children.push(
                AccessNode::new(Role::TextInput)
                    .with_name(t("strip-switcher-name-placeholder"))
                    .with_optional_value(Some(naming.text.clone()))
                    .with_text_selection(naming.selection.anchor, naming.selection.cursor)
                    .with_bounds(field)
                    .with_state(AccessState::new().focusable(true).focused(true)),
            );
        }
        Some(card)
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

/// The floating ground, square where it meets the screen's right edge like
/// the ribbon it grows from, rounded inward.
fn paint_card_ground(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
    let radius = ui::chrome::ChromeRole::Panel.radius(theme, rect.height());
    ui::surface::shadow(
        scene,
        transform,
        rect,
        radius,
        &theme.tokens.elevation.floating,
    );
    let shape = RoundedRect::from_rect(rect, RoundedRectRadii::new(radius, 0.0, 0.0, radius));
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.surfaces.overlay,
        None,
        &shape,
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        ui::chrome::panel_border(theme),
        None,
        &shape,
    );
}

fn draw_glyph(cx: &mut DrawCx<'_>, rect: Rect, icon: ui::icons::Icon, color: Color) {
    let glyph = Rect::from_center_size(rect.center(), (14.0, 14.0));
    ui::icons::draw_icon(
        cx.scene,
        cx.transform,
        glyph,
        icon,
        color,
        ui::icons::LUCIDE_DEFAULT_STROKE_PX,
    );
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

/// The ribbon's weight while nothing moves and the card is closed: quiet,
/// but there, so the switcher can be found.
pub(crate) const RIBBON_REST_ALPHA: f32 = 0.35;

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
    /// the card.
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

    /// The open card's extent, for damage.
    pub(super) fn strip_switcher_bounds(&self) -> Rect {
        self.strip_switcher.layout().card.inflate(32.0, 32.0)
    }

    /// The ribbon's opacity at `now`: full while the card is open, the
    /// navigation fade while moving, and the quiet resting mark otherwise.
    pub(super) fn strip_ribbon_alpha(&self, now: Instant, reduce_motion: bool) -> f32 {
        if !self.strip_switcher_offered() {
            return 0.0;
        }
        if self.strip_switcher.is_open() {
            return 1.0;
        }
        self.strip_indicators_alpha(now, reduce_motion)
            .max(RIBBON_REST_ALPHA)
    }

    /// Let the pointer timers settle at paint time too: a frame is what the
    /// wake deadline asks for, and a host that paints without updating
    /// first must still see the card open under a resting pointer.
    pub(super) fn settle_strip_switcher(&mut self, now: Instant) {
        let zone = self.strip_switcher_zone();
        if self.strip_switcher.settle(now, zone) {
            self.place_strip_switcher();
            self.publish_strip_switcher_open();
        }
    }

    /// Rebuild the card's model when the chrome changed, and let pointer
    /// timers settle. Returns whether the card needs a repaint.
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
        self.strip_switcher.place(&ribbon, ribbon.content);
    }

    /// Mirror the card's open state into the chrome, which repaints.
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

    /// The `strip.rename` action without a name: the card opens with the
    /// focused strip's name selected in its field.
    fn rename_focused_strip_in_switcher(&mut self) {
        if !self.strip_switcher_offered() {
            return;
        }
        self.place_strip_switcher();
        self.strip_switcher.rename_focused(Instant::now());
        self.place_strip_switcher();
        self.publish_strip_switcher_open();
    }

    /// A key while the card takes the keyboard.
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

    /// A press or release for the open card. Returns whether it was the
    /// card's.
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
        let answer = self.strip_switcher.press(p);
        let ours = !matches!(answer, Press::Outside);
        if let Press::Command(command) = answer {
            self.apply_strip_switcher_command(command);
        }
        self.strip_switcher_owns_release = ours;
        self.publish_strip_switcher_open();
        ours
    }

    /// A semantic target of the card, activated as a press would.
    pub(super) fn activate_strip_switcher_target(&mut self, target: &str) -> Option<bool> {
        let answer = self.strip_switcher.activate_target(target)?;
        if let Press::Command(command) = answer {
            self.apply_strip_switcher_command(command);
        }
        self.publish_strip_switcher_open();
        Some(true)
    }

    fn apply_strip_switcher_command(&self, command: Command) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut state = chrome.borrow_mut();
        let refused = match command {
            Command::FocusStrip(strip) => {
                state.focus_strip(strip);
                None
            }
            Command::CreateStrip(name) => state.create_strip(&name).err(),
            Command::RenameStrip(strip, name) => state.rename_strip(strip, &name).err(),
            Command::SelectWorkspace(workspace) => {
                state.tree_mut().set_active(workspace);
                state.bump();
                None
            }
            Command::NewWorkspace => {
                state.add_workspace(t("search-action-workspace-create-default-name"));
                None
            }
        };
        if let Some(message) = refused {
            state.push_warning_notification(t("strip-switcher-refused"), &message);
        }
    }

    /// Paint the card when it is open.
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
        self.place_strip_switcher();
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

    /// The card's named targets, for harnesses.
    pub(super) fn strip_switcher_test_rects(&self) -> Vec<(String, Rect)> {
        self.strip_switcher.test_rects(self.strip_switcher_zone())
    }

    /// The open card for assistive technology.
    pub(super) fn strip_switcher_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.strip_switcher.accessibility()
    }

    /// Test access to the card's state.
    #[cfg(test)]
    pub(crate) fn strip_switcher_for_tests(&mut self) -> &mut StripSwitcher {
        self.place_strip_switcher();
        &mut self.strip_switcher
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn model(strips: &[&str]) -> SwitcherModel {
        SwitcherModel {
            workspace_name: "Home".into(),
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
            workspaces: vec![
                WorkspaceEntry {
                    id: WorkspaceId(1),
                    name: "Home".into(),
                },
                WorkspaceEntry {
                    id: WorkspaceId(2),
                    name: "Music".into(),
                },
            ],
            active_workspace: 0,
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
        s.place(&r, CONTENT);
        let zone = zone(&r);
        (s, zone)
    }

    fn zone(r: &StripRibbon) -> Rect {
        let first = r.hit_rect(0).unwrap();
        let last = r.hit_rect(r.count.saturating_sub(1)).unwrap();
        first.union(last)
    }

    /// Glide from `from` to `to` in `steps` samples 8 ms apart, slowing to a
    /// stop at the end; returns the instant of the last sample.
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
    fn a_pointer_coming_to_rest_on_the_ribbon_opens_the_card() {
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
    fn rows_sit_level_with_their_ribbon_segments() {
        let (s, _) = switcher(&["Lounge", "Research", "Design"]);
        let r = ribbon(3, 0);
        for i in 0..3 {
            let row = s.layout().row_rect(Row::Strip(i)).unwrap();
            let seg = r.hit_rect(i).unwrap();
            assert_eq!((row.y0, row.y1), (seg.y0, seg.y1), "row {i}");
            assert!(row.x1 <= CONTENT.x1 && s.layout().card.x1 == CONTENT.x1);
        }
        assert!(s.layout().card.y0 >= CONTENT.y0 && s.layout().card.y1 <= CONTENT.y1);
    }

    #[test]
    fn leaving_the_card_closes_it_and_the_ribbon_waits_for_a_fresh_approach() {
        let (mut s, zone) = switcher(&["Lounge", "Research"]);
        let now = Instant::now();
        s.toggle_from_keyboard(now);
        s.close();
        assert!(!s.is_open());
        // Opened by the pointer, then left.
        s.open_at(now, false);
        let card = s.layout().card;
        s.pointer_moved(
            Point::new(card.x0 + 40.0, card.center().y),
            now + ms(8),
            zone,
        );
        assert!(s.is_open());
        s.pointer_moved(
            Point::new(card.x0 - 120.0, card.center().y),
            now + ms(16),
            zone,
        );
        assert!(!s.is_open(), "the pointer moved away");
    }

    #[test]
    fn clicks_switch_strips_and_workspaces_and_add_ones() {
        let (mut s, zone) = switcher(&["Lounge", "Research"]);
        let now = Instant::now();
        s.open_at(now, false);
        let _ = zone;
        let at = |s: &StripSwitcher, row| {
            let r = s.layout().row_rect(row).unwrap();
            Point::new(r.x0 + 30.0, r.center().y)
        };
        let p = at(&s, Row::Strip(1));
        assert_eq!(
            s.press(p),
            Press::Command(Command::FocusStrip(StripId(101)))
        );
        let p = at(&s, Row::Workspace(1));
        assert_eq!(
            s.press(p),
            Press::Command(Command::SelectWorkspace(WorkspaceId(2)))
        );
        let p = at(&s, Row::Workspace(0));
        assert_eq!(s.press(p), Press::Consumed, "the shown workspace stays");
        let p = at(&s, Row::NewWorkspace);
        assert_eq!(s.press(p), Press::Command(Command::NewWorkspace));
        let p = at(&s, Row::NewStrip);
        assert_eq!(s.press(p), Press::Consumed);
        assert!(s.is_naming() && s.takes_keys());
        for c in "Games".chars() {
            assert_eq!(
                s.key(
                    &Key::Character(c.to_string().into()),
                    ModifiersState::empty()
                ),
                KeyAnswer::Consumed
            );
        }
        assert_eq!(
            s.key(&Key::Named(NamedKey::Enter), ModifiersState::empty()),
            KeyAnswer::Command(Command::CreateStrip("Games".into()))
        );
        assert!(!s.is_naming());
    }

    #[test]
    fn the_pencil_renames_a_strip_and_an_empty_or_unchanged_name_does_nothing() {
        let (mut s, _) = switcher(&["main", "Research"]);
        s.open_at(Instant::now(), false);
        let pencil = s.layout().rename_rect(0).unwrap().center();
        assert_eq!(s.target_at(pencil), Some(Target::Rename(0)));
        assert_eq!(s.press(pencil), Press::Consumed);
        assert_eq!(s.naming_text(), Some("main"), "the old name, selected");
        // Typing replaces the selection.
        for c in "Lounge".chars() {
            s.key(
                &Key::Character(c.to_string().into()),
                ModifiersState::empty(),
            );
        }
        assert_eq!(
            s.key(&Key::Named(NamedKey::Enter), ModifiersState::empty()),
            KeyAnswer::Command(Command::RenameStrip(StripId(100), "Lounge".into()))
        );
        // Unchanged.
        s.press(s.layout().rename_rect(1).unwrap().center());
        assert_eq!(
            s.key(&Key::Named(NamedKey::Enter), ModifiersState::empty()),
            KeyAnswer::Consumed
        );
        // Emptied.
        s.press(s.layout().rename_rect(1).unwrap().center());
        s.key(&Key::Named(NamedKey::Backspace), ModifiersState::empty());
        assert_eq!(s.naming_text(), Some(""));
        assert_eq!(
            s.key(&Key::Named(NamedKey::Enter), ModifiersState::empty()),
            KeyAnswer::Consumed
        );
        // A strip another device writes has no pencil.
        let mut m = model(&["Theirs"]);
        m.strips[0].renamable = false;
        s.set_model(m);
        s.place(&ribbon(1, 0), CONTENT);
        let spot = s.layout().rename_rect(0).unwrap().center();
        assert_eq!(s.target_at(spot), Some(Target::Row(Row::Strip(0))));
    }

    #[test]
    fn the_keyboard_walks_the_rows_and_takes_one() {
        let (mut s, _) = switcher(&["Lounge", "Research"]);
        s.toggle_from_keyboard(Instant::now());
        assert!(s.is_open() && s.takes_keys());
        assert_eq!(s.selected(), Some(Row::Strip(0)));
        let none = ModifiersState::empty();
        s.key(&Key::Named(NamedKey::ArrowDown), none);
        assert_eq!(s.selected(), Some(Row::Strip(1)));
        assert_eq!(
            s.key(&Key::Named(NamedKey::Enter), none),
            KeyAnswer::Command(Command::FocusStrip(StripId(101)))
        );
        assert!(!s.is_open(), "a choice closes a keyboard card");

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
        assert_eq!(s.selected(), Some(Row::NewWorkspace));
        s.key(&Key::Named(NamedKey::ArrowUp), none);
        assert_eq!(s.selected(), Some(Row::Workspace(1)));
        assert_eq!(
            s.key(&Key::Named(NamedKey::Enter), none),
            KeyAnswer::Command(Command::SelectWorkspace(WorkspaceId(2)))
        );
        s.toggle_from_keyboard(Instant::now());
        assert_eq!(
            s.key(&Key::Named(NamedKey::Escape), none),
            KeyAnswer::Consumed
        );
        assert!(!s.is_open());
    }

    #[test]
    fn rows_light_only_once_the_pointer_settles() {
        let (mut s, zone) = switcher(&["Lounge", "Research", "Design"]);
        let t0 = Instant::now();
        s.open_at(t0, false);
        let r0 = s.layout().row_rect(Row::Strip(0)).unwrap();
        let r2 = s.layout().row_rect(Row::Strip(2)).unwrap();
        // Crossing row 0 quickly on the way to row 2.
        s.pointer_moved(Point::new(r0.x0 + 60.0, r0.y0 - 30.0), t0 + ms(8), zone);
        s.pointer_moved(Point::new(r0.x0 + 60.0, r0.center().y), t0 + ms(16), zone);
        assert_eq!(s.hover(), None, "in transit over row 0");
        s.pointer_moved(Point::new(r2.x0 + 60.0, r2.center().y), t0 + ms(24), zone);
        assert!(s.deadline(t0 + ms(24), zone).is_some(), "wakes to light it");
        s.settle(t0 + ms(100), zone);
        assert_eq!(s.hover(), Some(Target::Row(Row::Strip(2))));
    }

    #[test]
    fn semantic_targets_activate_like_presses() {
        let (mut s, _) = switcher(&["Lounge", "Research"]);
        s.open_at(Instant::now(), false);
        let name = StripSwitcher::target_name(Target::Row(Row::Strip(1)));
        assert_eq!(
            s.activate_target(&name),
            Some(Press::Command(Command::FocusStrip(StripId(101))))
        );
        let rename = StripSwitcher::target_name(Target::Rename(0));
        assert_eq!(s.activate_target(&rename), Some(Press::Consumed));
        assert!(s.is_naming());
        assert_eq!(s.activate_target("palette:query"), None);
        let a11y = s.accessibility().expect("open card is published");
        assert!(a11y.children.len() >= 6);
    }
}
