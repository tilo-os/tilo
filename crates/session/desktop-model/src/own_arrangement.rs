//! The person's own arrangement: the shell's typed record of what is open
//! where (shell.md §2, workspace-model.md §9).
//!
//! The strips shell's arrangement is a list of strips, each a row's column
//! of places with a width, each place showing a resource through a view. It
//! is the person's, not the workspace's: it lives in their own record
//! ([`record_id`], `records: .own`), syncs to their devices that use the
//! same shell, and has history and undo. No other member reads it.
//!
//! The record keeps a row, a strip and a place each as its own entry under
//! its own id, so two devices changing different entries both keep their
//! change and a merge writes only what moved. Every entry names the shell
//! that made it: a shell that does not know an entry another shell made
//! keeps it ([`Strips::foreign`]) and never changes or removes it.
//!
//! Today's arrangement in [`DesktopTree`] is migrated into it by
//! [`Strips::from_tree`], which keeps every workspace's strips, every
//! window group and every place — and every id — as it is.

use std::collections::BTreeMap;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use tilo_automerge::ApplyIncoming as _;

use crate::apps::canonical_app_id;
use crate::arrangement::{ArrangementKind, CanvasPlace};
use crate::ids::{GroupId, StripId, TabId, WorkspaceSlot};
use crate::layout::{Tab, TileContent, WindowGroup};
use crate::tree::DesktopTree;
use crate::width::WindowWidth;

/// The shell this build of the record is the strips shell's.
pub const SHELL: &str = "so.tilo.shell.strips";

/// Another default shell, the conversation's: its places are its own and
/// this shell leaves them alone.
pub const CONVERSATION_SHELL: &str = "so.tilo.shell.conversation";

/// The record kind of a person's arrangement, in their own record.
pub const KIND: &str = "tilolang.room";

/// The id of `person`'s own record of their arrangement: only their devices
/// read and change it ([`tilo_action_policy::member_record`]), the workspace's
/// owner included. `person` is the lowercase hex of their user root key.
#[must_use]
pub fn record_id(person: &str) -> String {
    tilo_action_policy::member_record_id(&person.to_ascii_lowercase(), KIND)
}

/// A row of the desk: a strip across the desk's width, holding columns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub id: StripId,
    pub workspace: WorkspaceSlot,
    pub name: String,
    /// The shell that made it.
    #[serde(default)]
    pub shell: String,
}

impl Row {
    /// Whether this build's shell shows it.
    #[must_use]
    pub fn is_ours(&self) -> bool {
        self.shell.is_empty() || self.shell == SHELL
    }
}

/// A place: one window, showing a resource through a view.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub id: TabId,
    /// The app showing the resource, by its canonical id (`so.tilo.terminal`).
    pub shows: String,
    /// The view it shows the resource through; empty for the app's default.
    #[serde(default)]
    pub view: String,
    /// The record the place shows, when it shows one.
    #[serde(default)]
    pub record: String,
    /// The window's state as the desktop model keeps it (its tile), so a
    /// place round-trips through the record without losing anything.
    #[serde(default)]
    pub state: serde_json::Value,
    /// The title the place is shown under.
    #[serde(default)]
    pub title: String,
    /// Whether the person pinned it: an agent's arrangement leaves a
    /// pinned place alone (shell.md §5).
    #[serde(default)]
    pub pinned: bool,
    /// The shell that made it.
    #[serde(default)]
    pub shell: String,
}

impl Place {
    /// Whether this build's shell shows it: an empty shell (an older
    /// record's) reads as this shell's.
    #[must_use]
    pub fn is_ours(&self) -> bool {
        self.shell.is_empty() || self.shell == SHELL
    }

    /// Whether the window state the record kept reads back here: one this
    /// build cannot read opens nothing, so its tab says why instead of the
    /// place being dropped from the person's record.
    #[must_use]
    pub fn reads_back(&self) -> bool {
        tile_of(self).is_some()
    }
}

/// A strip: a column of places, with a width, in a row of a workspace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Strip {
    pub id: GroupId,
    pub workspace: WorkspaceSlot,
    pub row: StripId,
    pub width: WindowWidth,
    #[serde(default)]
    pub maximized: bool,
    /// The place in front, when the strip has any.
    #[serde(default)]
    pub active: Option<TabId>,
    /// Where the strip stands on its workspace's canvas.
    #[serde(default)]
    pub canvas: Option<CanvasPlace>,
    #[serde(default)]
    pub shell: String,
    pub places: Vec<Place>,
}

impl Strip {
    /// Whether this build's shell shows it.
    #[must_use]
    pub fn is_ours(&self) -> bool {
        self.shell.is_empty() || self.shell == SHELL
    }
}

/// The person's arrangement as the strips shell reads and writes it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Strips {
    /// Every workspace the person's desk shows, and its name.
    #[serde(default)]
    pub workspaces: Vec<PlacedWorkspace>,
    pub rows: Vec<Row>,
    pub strips: Vec<Strip>,
    /// Places of another shell, and places whose strip another shell made:
    /// read out of the record, kept, and never changed or removed by this
    /// shell.
    #[serde(default)]
    pub foreign: Vec<Place>,
}

/// A workspace of the person's arrangement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacedWorkspace {
    pub id: WorkspaceSlot,
    pub name: String,
}

impl Strips {
    /// The arrangement `tree` holds, as the person's own: every workspace's
    /// rows, strips and places, with their ids and canvas places, and
    /// `records` naming the record each tab shows.
    #[must_use]
    pub fn from_tree(tree: &DesktopTree, records: &BTreeMap<TabId, String>) -> Self {
        let mut arrangement = Self::default();
        for workspace in &tree.workspaces {
            arrangement.workspaces.push(PlacedWorkspace {
                id: workspace.slot,
                name: workspace.name.clone(),
            });
            for row in &workspace.strips {
                arrangement.rows.push(Row {
                    id: row.id,
                    workspace: workspace.slot,
                    name: row.name.clone(),
                    shell: SHELL.to_owned(),
                });
                for group in &row.window_groups {
                    let places: Vec<Place> = group
                        .tabs
                        .iter()
                        .map(|tab| place_of(tab, records))
                        .collect();
                    arrangement.strips.push(Strip {
                        id: group.id,
                        workspace: workspace.slot,
                        row: row.id,
                        width: group.width,
                        maximized: group.maximized,
                        active: Some(group.active_tab)
                            .filter(|tab| group.tabs.iter().any(|held| held.id == *tab)),
                        canvas: workspace.arrangement.place_of(group.id),
                        shell: SHELL.to_owned(),
                        places,
                    });
                }
            }
        }
        arrangement
    }

    /// The desk `self` describes, with `workspace_name` naming a workspace
    /// it holds. A place whose tile the record kept shows it; one it did not
    /// opens its app again ([`TileContent::LocalApp`]).
    #[must_use]
    pub fn to_tree(&self, workspace_name: impl Fn(WorkspaceSlot) -> Option<String>) -> DesktopTree {
        let mut tree = DesktopTree::default();
        for held in self.workspaces() {
            let slot = held.id;
            let mut workspace = crate::workspace::Workspace::new(
                slot,
                workspace_name(slot).unwrap_or_else(|| held.name.clone()),
            );
            for row in self
                .rows
                .iter()
                .filter(|row| row.workspace == slot && row.is_ours())
            {
                workspace
                    .strips
                    .push(crate::layout::Strip::new(row.id, row.name.clone()));
            }
            for strip in self.strips.iter().filter(|strip| {
                strip.workspace == slot && strip.is_ours() && !strip.places.is_empty()
            }) {
                let Some(row) = workspace.strips.iter_mut().find(|row| row.id == strip.row) else {
                    continue;
                };
                let tabs: Vec<Tab> = strip
                    .places
                    .iter()
                    .filter(|place| place.is_ours())
                    .map(tab_of)
                    .collect();
                let Some(first) = tabs.first().map(|tab| tab.id) else {
                    continue;
                };
                row.window_groups.push(WindowGroup {
                    id: strip.id,
                    active_tab: strip
                        .active
                        .filter(|tab| tabs.iter().any(|held| held.id == *tab))
                        .unwrap_or(first),
                    tabs,
                    width: strip.width,
                    maximized: strip.maximized,
                    focus_history: Vec::new(),
                });
                if let Some(canvas) = strip.canvas {
                    workspace.arrangement.places.insert(strip.id, canvas);
                }
            }
            if !workspace.arrangement.places.is_empty() {
                workspace.arrangement.kind = ArrangementKind::Canvas;
            }
            tree.workspaces.push(workspace);
        }
        if let Some(first) = tree.workspaces.first() {
            tree.active = first.slot;
            if let Some(row) = first.strips.first() {
                tree.active_strip.insert(first.slot, row.id);
            }
        }
        tree
    }

    /// Every workspace the arrangement names, in the order the record lists
    /// them.
    fn workspaces(&self) -> Vec<&PlacedWorkspace> {
        self.workspaces.iter().collect()
    }

    /// The place `id` this shell shows, without looking at another's.
    #[must_use]
    pub fn place(&self, id: TabId) -> Option<&Place> {
        self.strips
            .iter()
            .flat_map(|strip| &strip.places)
            .find(|place| place.id == id)
    }

    /// Every place this shell shows, in strip order.
    pub fn places(&self) -> impl Iterator<Item = &Place> {
        self.strips.iter().flat_map(|strip| &strip.places)
    }

    /// The strip holding the place `id`, when this shell shows it.
    #[must_use]
    pub fn strip_of(&self, id: TabId) -> Option<&Strip> {
        self.strips
            .iter()
            .find(|strip| strip.places.iter().any(|place| place.id == id))
    }

    /// The row named `id` of `workspace`.
    #[must_use]
    pub fn row(&self, workspace: WorkspaceSlot, id: StripId) -> Option<&Row> {
        self.rows
            .iter()
            .find(|row| row.workspace == workspace && row.id == id)
    }

    /// The strip `id`, shown by this shell.
    #[must_use]
    pub fn strip(&self, id: GroupId) -> Option<&Strip> {
        self.strips.iter().find(|strip| strip.id == id)
    }

    /// Move the place `id` into the strips of `row` (of the workspace the
    /// place is in), before the place `before` or last. Returns the strip it
    /// landed in, or `None` when this shell does not hold the place.
    pub fn move_place(
        &mut self,
        id: TabId,
        row: StripId,
        before: Option<TabId>,
        minted: GroupId,
    ) -> Option<GroupId> {
        let workspace = self.strip_of(id)?.workspace;
        self.row(workspace, row)?;
        let mut place = self.take_place(id)?;
        SHELL.clone_into(&mut place.shell);
        let target = self.target_strip(workspace, row, before, minted);
        let strip = self.strips.iter_mut().find(|strip| strip.id == target)?;
        let at = before
            .and_then(|before| strip.places.iter().position(|held| held.id == before))
            .unwrap_or(strip.places.len());
        strip.places.insert(at, place);
        strip.active = Some(id);
        Some(target)
    }

    /// The strip `place` lands in: the one holding `before`, else the row's
    /// last, else a new one `minted` names.
    fn target_strip(
        &mut self,
        workspace: WorkspaceSlot,
        row: StripId,
        before: Option<TabId>,
        minted: GroupId,
    ) -> GroupId {
        let holds = |strip: &Strip| {
            before.is_some_and(|before| strip.places.iter().any(|held| held.id == before))
        };
        if let Some(id) = self
            .strips
            .iter()
            .find(|strip| strip.workspace == workspace && strip.row == row && holds(strip))
            .map(|strip| strip.id)
        {
            return id;
        }
        if let Some(id) = self
            .strips
            .iter()
            .rev()
            .find(|strip| strip.workspace == workspace && strip.row == row)
            .map(|strip| strip.id)
        {
            return id;
        }
        self.strips.push(Strip {
            id: minted,
            workspace,
            row,
            width: WindowWidth::Normal,
            maximized: false,
            active: None,
            canvas: None,
            shell: SHELL.to_owned(),
            places: Vec::new(),
        });
        minted
    }

    /// Take the place `id` out of the arrangement; a strip left empty goes
    /// with it. Returns the place, or `None` when this shell does not hold
    /// it.
    fn take_place(&mut self, id: TabId) -> Option<Place> {
        let strip = self
            .strips
            .iter_mut()
            .find(|strip| strip.places.iter().any(|held| held.id == id))?;
        let at = strip.places.iter().position(|held| held.id == id)?;
        let place = strip.places.remove(at);
        let emptied = strip.places.is_empty();
        let strip_id = strip.id;
        if !emptied && strip.active == Some(id) {
            strip.active = strip.places.first().map(|held| held.id);
        }
        if emptied {
            self.strips.retain(|held| held.id != strip_id);
        }
        Some(place)
    }

    /// Put the place `place` into the row `row` of `workspace` at the canvas
    /// point `canvas`, as a place made from a resource is made (shell.md §8):
    /// one entry of its own, in a strip that holds nothing else. The strip
    /// `strip` is its, and the row is made when the workspace has none.
    /// Returns whether it was placed: `false` when the place's id or the
    /// strip's is taken.
    pub fn place_at_canvas(
        &mut self,
        workspace: WorkspaceSlot,
        row: StripId,
        strip: GroupId,
        place: Place,
        canvas: CanvasPlace,
    ) -> bool {
        if self.place(place.id).is_some() || self.strip(strip).is_some() {
            return false;
        }
        if !self.workspaces.iter().any(|held| held.id == workspace) {
            return false;
        }
        if self.row(workspace, row).is_none() {
            self.rows.push(Row {
                id: row,
                workspace,
                name: String::new(),
                shell: SHELL.to_owned(),
            });
        }
        self.strips.push(Strip {
            id: strip,
            workspace,
            row,
            width: WindowWidth::Normal,
            maximized: false,
            active: Some(place.id),
            canvas: Some(canvas),
            shell: SHELL.to_owned(),
            places: vec![place],
        });
        true
    }

    /// Put `place` into the strip `strip`, after the place `after` or last,
    /// as opening a resource into it does. `false` when this shell shows no
    /// such strip, or the place's id is taken.
    pub fn open_into(&mut self, strip: GroupId, place: Place, after: Option<TabId>) -> bool {
        if self.place(place.id).is_some() {
            return false;
        }
        let Some(target) = self.strips.iter_mut().find(|held| held.id == strip) else {
            return false;
        };
        let at = after
            .and_then(|after| {
                target
                    .places
                    .iter()
                    .position(|held| held.id == after)
                    .map(|at| at.saturating_add(1))
            })
            .unwrap_or(target.places.len());
        target.places.insert(at, place);
        target.active = target.places.get(at).map(|held| held.id);
        true
    }

    /// Set the width of the strip `id`, as the person resized it.
    pub fn resize(&mut self, id: GroupId, width: WindowWidth) -> bool {
        match self.strips.iter_mut().find(|strip| strip.id == id) {
            Some(strip) => {
                let changed = strip.width != width;
                strip.width = width;
                changed
            }
            None => false,
        }
    }

    /// Maximize the strip `id` (`maximized`), so it stands alone as the window the desk is,
    /// or restore it beside its row's other windows.
    pub fn maximize(&mut self, id: GroupId, maximized: bool) -> bool {
        match self.strips.iter_mut().find(|strip| strip.id == id) {
            Some(strip) => {
                let changed = strip.maximized != maximized;
                strip.maximized = maximized;
                changed
            }
            None => false,
        }
    }

    /// Take the place `id` out of the arrangement, as closing its window
    /// does.
    pub fn close_place(&mut self, id: TabId) -> bool {
        self.take_place(id).is_some()
    }
    /// Pin the place `id` (`true`) or unpin it.
    pub fn pin(&mut self, id: TabId, pinned: bool) -> bool {
        let Some(place) = self
            .strips
            .iter_mut()
            .flat_map(|strip| &mut strip.places)
            .find(|place| place.id == id)
        else {
            return false;
        };
        let changed = place.pinned != pinned;
        place.pinned = pinned;
        changed
    }

    /// Whether this shell shows no place at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strips.iter().all(|strip| strip.places.is_empty())
    }

    /// Bring the place `id` in front of the strip it stands in, as choosing its tab does.
    /// `false` when no strip of this shell holds it, or it is in front already.
    pub fn activate(&mut self, id: TabId) -> bool {
        let Some(strip) = self
            .strips
            .iter_mut()
            .find(|strip| strip.places.iter().any(|place| place.id == id))
        else {
            return false;
        };
        let changed = strip.active != Some(id);
        strip.active = Some(id);
        changed
    }
}

/// `tab` as a place of this shell; `records` names the record each tab
/// shows, by tab.
fn place_of(tab: &Tab, records: &BTreeMap<TabId, String>) -> Place {
    Place {
        id: tab.id,
        shows: canonical_app_id(&tab.tile).unwrap_or_default(),
        view: String::new(),
        record: records.get(&tab.id).cloned().unwrap_or_default(),
        state: serde_json::to_value(&tab.tile).unwrap_or(serde_json::Value::Null),
        title: tab.title.clone(),
        pinned: false,
        shell: SHELL.to_owned(),
    }
}

/// The tab `place` shows.
fn tab_of(place: &Place) -> Tab {
    let tile = tile_of(place).unwrap_or_else(|| TileContent::LocalApp {
        app_id: place.shows.clone(),
        label: place.title.clone(),
        note: String::new(),
    });
    Tab {
        id: place.id,
        title: place.title.clone(),
        tile,
        app_title: None,
    }
}

/// The window state the record kept for `place`, when this build reads it.
fn tile_of(place: &Place) -> Option<TileContent> {
    serde_json::from_value::<TileContent>(place.state.clone()).ok()
}

// ---------------------------------------------------------------------------
// The record: rows, strips and places entry by entry in an Automerge
// document, so two devices' changes to different entries merge.

const WORKSPACES: &str = "workspaces";
const ROWS: &str = "rows";
const STRIPS: &str = "strips";
const PLACES: &str = "places";
const WORKSPACE: &str = "workspace";
const ORDER: &str = "order";
const NAME: &str = "name";
const ROW: &str = "row";
const WIDTH: &str = "width";
const MAXIMIZED: &str = "maximized";
const ACTIVE: &str = "active";
const CANVAS: &str = "canvas";
const STRIP: &str = "strip";
const SHOWS: &str = "shows";
const VIEW: &str = "view";
const RECORD: &str = "record";
const STATE: &str = "state";
const TITLE: &str = "title";
const PINNED: &str = "pinned";
const SHELL_KEY: &str = "shell";

/// The arrangement `doc` holds, as this build's shell reads it
/// ([`read_as`], for [`SHELL`]).
#[must_use]
pub fn read(doc: &impl ReadDoc) -> Strips {
    read_as(doc, SHELL)
}

/// Take in `other`, another device's record, entry by entry: what merged in
/// besides what `doc` already held.
///
/// # Errors
/// Why `other`'s changes cannot be taken in; `doc` is unchanged.
pub fn merge_into(
    doc: &mut AutoCommit,
    other: &AutoCommit,
) -> Result<usize, automerge::AutomergeError> {
    // `AutoCommit::merge` takes the other document mutably; a copy is merged
    // so the caller's own reading stands as it was.
    let mut other = other.clone();
    doc.merge_incoming(&mut other).map(|added| added.len())
}

/// The arrangement `doc` holds, as the shell `shell` reads it. Places of
/// another shell, and places whose strip another shell made, are
/// [`Strips::foreign`]: kept, never shown, never changed.
#[must_use]
pub fn read_as(doc: &impl ReadDoc, shell: &str) -> Strips {
    let mut arrangement = Strips::default();
    let mut held_workspaces: Vec<(f64, u64, PlacedWorkspace)> = Vec::new();
    for (id, entry) in entries(doc, WORKSPACES) {
        held_workspaces.push((
            number(doc, &entry, ORDER).unwrap_or(0.0),
            id,
            PlacedWorkspace {
                id: WorkspaceSlot(id),
                name: text(doc, &entry, NAME).unwrap_or_default(),
            },
        ));
    }
    held_workspaces.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    arrangement.workspaces = held_workspaces
        .into_iter()
        .map(|(_, _, held)| held)
        .collect();

    let mut rows: Vec<(f64, u64, Row)> = Vec::new();
    for (id, entry) in entries(doc, ROWS) {
        rows.push((
            number(doc, &entry, ORDER).unwrap_or(0.0),
            id,
            Row {
                id: StripId(id),
                workspace: WorkspaceSlot(parse(doc, &entry, WORKSPACE).unwrap_or_default()),
                name: text(doc, &entry, NAME).unwrap_or_default(),
                shell: text(doc, &entry, SHELL_KEY).unwrap_or_default(),
            },
        ));
    }
    rows.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    arrangement.rows = rows.into_iter().map(|(_, _, row)| row).collect();

    let mut strips: Vec<(f64, u64, Strip)> = Vec::new();
    for (id, entry) in entries(doc, STRIPS) {
        strips.push((
            number(doc, &entry, ORDER).unwrap_or(0.0),
            id,
            Strip {
                id,
                workspace: WorkspaceSlot(parse(doc, &entry, WORKSPACE).unwrap_or_default()),
                row: StripId(parse(doc, &entry, ROW).unwrap_or_default()),
                width: json(doc, &entry, WIDTH)
                    .and_then(|json| serde_json::from_str(&json).ok())
                    .unwrap_or(WindowWidth::Normal),
                maximized: truth(doc, &entry, MAXIMIZED).unwrap_or(false),
                active: parse(doc, &entry, ACTIVE),
                canvas: json(doc, &entry, CANVAS).and_then(|json| serde_json::from_str(&json).ok()),
                shell: text(doc, &entry, SHELL_KEY).unwrap_or_default(),
                places: Vec::new(),
            },
        ));
    }
    strips.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    // Another shell's strips are kept in the record and left out of this
    // shell's arrangement: it neither shows nor writes them.
    arrangement.strips = strips
        .into_iter()
        .map(|(_, _, strip)| strip)
        .filter(|strip| in_shell(&strip.shell, shell))
        .collect();

    let mut places: Vec<(u64, f64, u64, Place)> = Vec::new();
    for (id, entry) in entries(doc, PLACES) {
        places.push((
            parse(doc, &entry, STRIP).unwrap_or_default(),
            number(doc, &entry, ORDER).unwrap_or(0.0),
            id,
            Place {
                id,
                shows: text(doc, &entry, SHOWS).unwrap_or_default(),
                view: text(doc, &entry, VIEW).unwrap_or_default(),
                record: text(doc, &entry, RECORD).unwrap_or_default(),
                state: json(doc, &entry, STATE)
                    .and_then(|json| serde_json::from_str(&json).ok())
                    .unwrap_or(serde_json::Value::Null),
                title: text(doc, &entry, TITLE).unwrap_or_default(),
                pinned: truth(doc, &entry, PINNED).unwrap_or(false),
                shell: text(doc, &entry, SHELL_KEY).unwrap_or_default(),
            },
        ));
    }
    places.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2)));
    for (strip, _, _, place) in places {
        let shown = arrangement.strips.iter_mut().find(|held| held.id == strip);
        match shown {
            Some(held) if in_shell(&held.shell, shell) && in_shell(&place.shell, shell) => {
                held.places.push(place);
            }
            // Another shell's strip, or a place of another shell in ours:
            // kept, never placed nor removed here.
            _ => arrangement.foreign.push(place),
        }
    }
    arrangement
}

/// Write `arrangement` into `doc`, changing only what differs, as this
/// build's shell's entries ([`write_as`], for [`SHELL`]).
///
/// Returns whether the document changed.
///
/// # Errors
/// The document's own error when a map cannot be made.
pub fn write(
    doc: &mut AutoCommit,
    arrangement: &Strips,
) -> Result<bool, automerge::AutomergeError> {
    write_as(doc, SHELL, arrangement)
}

/// Write `arrangement` into `doc` as `shell`'s own: its rows, its strips,
/// its places. Entries another shell wrote are left exactly as they are, so
/// two shells keep their arrangements in one record. Every entry written is
/// stamped with `shell`, which a shell that does not know it leaves alone.
///
/// Returns whether the document changed.
///
/// # Errors
/// The document's own error when a map cannot be made.
pub fn write_as(
    doc: &mut AutoCommit,
    shell: &str,
    arrangement: &Strips,
) -> Result<bool, automerge::AutomergeError> {
    let mut changed = false;
    changed |= write_workspaces(doc, shell, arrangement)?;
    changed |= write_rows(doc, shell, arrangement)?;
    changed |= write_strips(doc, shell, arrangement)?;
    changed |= write_places(doc, shell, arrangement)?;
    Ok(changed)
}

fn write_workspaces(
    doc: &mut AutoCommit,
    shell: &str,
    arrangement: &Strips,
) -> Result<bool, automerge::AutomergeError> {
    let held0 = entries(doc, WORKSPACES);
    let wanted: Vec<u64> = arrangement
        .workspaces
        .iter()
        .map(|held| held.id.0)
        .collect();
    let container = container(doc, WORKSPACES, !wanted.is_empty())?;
    let mut changed = false;
    if let Some(container) = &container {
        for (id, entry) in &held0 {
            if !wanted.contains(id) && written_by(doc, entry, shell) {
                doc.delete(container, id.to_string())?;
                changed = true;
            }
        }
    }
    let Some(container) = container else {
        return Ok(changed);
    };
    for (order, held) in arrangement.workspaces.iter().enumerate() {
        let entry = at(doc, &container, held.id.0, &held0)?;
        let order = u32::try_from(order).map_or(f64::MAX, f64::from);
        changed |= put_number(doc, &entry, ORDER, order)?;
        changed |= put_text(doc, &entry, NAME, &held.name)?;
        changed |= put_text(doc, &entry, SHELL_KEY, shell)?;
    }
    Ok(changed)
}

fn write_rows(
    doc: &mut AutoCommit,
    shell: &str,
    arrangement: &Strips,
) -> Result<bool, automerge::AutomergeError> {
    let held = entries(doc, ROWS);
    let shown: Vec<&Row> = arrangement
        .rows
        .iter()
        .filter(|row| in_shell(&row.shell, shell))
        .collect();
    let wanted: Vec<u64> = shown.iter().map(|row| row.id.0).collect();
    let container = container(doc, ROWS, !wanted.is_empty())?;
    let mut changed = false;
    if let Some(container) = &container {
        for (id, entry) in &held {
            if !wanted.contains(id) && written_by(doc, entry, shell) {
                doc.delete(container, id.to_string())?;
                changed = true;
            }
        }
    }
    let Some(container) = container else {
        return Ok(changed);
    };
    for (order, row) in shown.into_iter().enumerate() {
        let entry = at(doc, &container, row.id.0, &held)?;
        let order = u32::try_from(order).map_or(f64::MAX, f64::from);
        changed |= put_number(doc, &entry, ORDER, order)?;
        changed |= put_text(doc, &entry, NAME, &row.name)?;
        changed |= put_text(doc, &entry, WORKSPACE, &row.workspace.0.to_string())?;
        changed |= put_text(doc, &entry, SHELL_KEY, shell)?;
    }
    Ok(changed)
}

fn write_strips(
    doc: &mut AutoCommit,
    shell: &str,
    arrangement: &Strips,
) -> Result<bool, automerge::AutomergeError> {
    let held = entries(doc, STRIPS);
    let shown: Vec<&Strip> = arrangement
        .strips
        .iter()
        .filter(|strip| in_shell(&strip.shell, shell))
        .collect();
    let wanted: Vec<u64> = shown.iter().map(|strip| strip.id).collect();
    let container = container(doc, STRIPS, !wanted.is_empty())?;
    let mut changed = false;
    if let Some(container) = &container {
        for (id, entry) in &held {
            if !wanted.contains(id) && written_by(doc, entry, shell) {
                doc.delete(container, id.to_string())?;
                changed = true;
            }
        }
    }
    let Some(container) = container else {
        return Ok(changed);
    };
    for (order, strip) in shown.into_iter().enumerate() {
        let entry = at(doc, &container, strip.id, &held)?;
        let order = u32::try_from(order).map_or(f64::MAX, f64::from);
        changed |= put_number(doc, &entry, ORDER, order)?;
        changed |= put_text(doc, &entry, WORKSPACE, &strip.workspace.0.to_string())?;
        changed |= put_text(doc, &entry, ROW, &strip.row.0.to_string())?;
        let width = serde_json::to_string(&strip.width).unwrap_or_default();
        changed |= put_text(doc, &entry, WIDTH, &width)?;
        changed |= put_flag(doc, &entry, MAXIMIZED, strip.maximized)?;
        match strip.active {
            Some(active) => changed |= put_text(doc, &entry, ACTIVE, &active.to_string())?,
            None => changed |= remove(doc, &entry, ACTIVE)?,
        }
        match strip.canvas {
            Some(canvas) => {
                let json = serde_json::to_string(&canvas).unwrap_or_default();
                changed |= put_text(doc, &entry, CANVAS, &json)?;
            }
            None => changed |= remove(doc, &entry, CANVAS)?,
        }
        changed |= put_text(doc, &entry, SHELL_KEY, shell)?;
    }
    Ok(changed)
}

fn write_places(
    doc: &mut AutoCommit,
    shell: &str,
    arrangement: &Strips,
) -> Result<bool, automerge::AutomergeError> {
    let held = entries(doc, PLACES);
    // Each place `shell` shows, with the strip it stands in.
    let mut wanted: BTreeMap<u64, (u64, f64, &Place)> = BTreeMap::new();
    for strip in arrangement
        .strips
        .iter()
        .filter(|strip| in_shell(&strip.shell, shell))
    {
        for (order, place) in strip
            .places
            .iter()
            .filter(|place| in_shell(&place.shell, shell))
            .enumerate()
        {
            let order = u32::try_from(order).map_or(f64::MAX, f64::from);
            wanted.insert(place.id, (strip.id, order, place));
        }
    }
    let container = container(doc, PLACES, !wanted.is_empty())?;
    let mut changed = false;
    if let Some(container) = &container {
        // Remove only `shell`'s places the arrangement no longer holds;
        // another shell's stay, whoever writes.
        for (id, entry) in &held {
            if !wanted.contains_key(id) && written_by(doc, entry, shell) {
                doc.delete(container, id.to_string())?;
                changed = true;
            }
        }
    }
    let Some(container) = container else {
        return Ok(changed);
    };
    for (id, (strip, order, place)) in wanted {
        let entry = at(doc, &container, id, &held)?;
        changed |= put_text(doc, &entry, STRIP, &strip.to_string())?;
        changed |= put_number(doc, &entry, ORDER, order)?;
        changed |= put_text(doc, &entry, SHOWS, &place.shows)?;
        changed |= put_text(doc, &entry, VIEW, &place.view)?;
        changed |= put_text(doc, &entry, RECORD, &place.record)?;
        let state = serde_json::to_string(&place.state).unwrap_or_default();
        changed |= put_text(doc, &entry, STATE, &state)?;
        changed |= put_text(doc, &entry, TITLE, &place.title)?;
        changed |= put_flag(doc, &entry, PINNED, place.pinned)?;
        changed |= put_text(doc, &entry, SHELL_KEY, shell)?;
    }
    Ok(changed)
}

/// The map at `ROOT[key]`, when the document has one.
fn map(doc: &impl ReadDoc, key: &str) -> Option<ObjId> {
    match doc.get(&ROOT, key).ok().flatten() {
        Some((Value::Object(ObjType::Map), id)) => Some(id),
        _ => None,
    }
}

/// The map at `ROOT[key]`, made when `needed` and absent.
fn container(
    doc: &mut AutoCommit,
    key: &str,
    needed: bool,
) -> Result<Option<ObjId>, automerge::AutomergeError> {
    match map(doc, key) {
        Some(id) => Ok(Some(id)),
        None if needed => Ok(Some(doc.put_object(&ROOT, key, ObjType::Map)?)),
        None => Ok(None),
    }
}

/// Every entry of the map at `ROOT[key]`, by id.
fn entries(doc: &impl ReadDoc, key: &str) -> Vec<(u64, ObjId)> {
    let Some(container) = map(doc, key) else {
        return Vec::new();
    };
    let mut held = Vec::new();
    for key in doc.keys(&container) {
        let Ok(id) = key.parse::<u64>() else { continue };
        if let Some((Value::Object(ObjType::Map), entry)) = doc.get(&container, &key).ok().flatten()
        {
            held.push((id, entry));
        }
    }
    held
}

/// The entry of `id` in `container`, made when absent.
fn at(
    doc: &mut AutoCommit,
    container: &ObjId,
    id: u64,
    held: &[(u64, ObjId)],
) -> Result<ObjId, automerge::AutomergeError> {
    match held.iter().find(|(held, _)| *held == id) {
        Some((_, entry)) => Ok(entry.clone()),
        None => Ok(doc.put_object(container, id.to_string(), ObjType::Map)?),
    }
}

/// Whether a place or strip `shell` wrote is this shell's to show: an entry
/// an older record wrote with no shell at all is.
fn in_shell(entry: &str, shell: &str) -> bool {
    entry.is_empty() || entry == shell
}

/// Whether `shell` wrote the entry: an entry an older record wrote with no
/// shell at all is whoever writes it next.
fn written_by(doc: &impl ReadDoc, entry: &ObjId, shell: &str) -> bool {
    match text(doc, entry, SHELL_KEY) {
        Some(held) => held.is_empty() || held == shell,
        None => true,
    }
}

fn put(
    doc: &mut AutoCommit,
    obj: &ObjId,
    key: &str,
    value: &str,
) -> Result<bool, automerge::AutomergeError> {
    if text(doc, obj, key).as_deref() == Some(value) {
        return Ok(false);
    }
    doc.put(obj, key, value)?;
    Ok(true)
}

fn put_number(
    doc: &mut AutoCommit,
    obj: &ObjId,
    key: &str,
    value: f64,
) -> Result<bool, automerge::AutomergeError> {
    if number(doc, obj, key) == Some(value) {
        return Ok(false);
    }
    doc.put(obj, key, value)?;
    Ok(true)
}

fn put_text(
    doc: &mut AutoCommit,
    obj: &ObjId,
    key: &str,
    value: &str,
) -> Result<bool, automerge::AutomergeError> {
    put(doc, obj, key, value)
}

fn put_flag(
    doc: &mut AutoCommit,
    obj: &ObjId,
    key: &str,
    value: bool,
) -> Result<bool, automerge::AutomergeError> {
    if truth(doc, obj, key) == Some(value) {
        return Ok(false);
    }
    doc.put(obj, key, value)?;
    Ok(true)
}

fn remove(doc: &mut AutoCommit, obj: &ObjId, key: &str) -> Result<bool, automerge::AutomergeError> {
    if doc.get(obj, key).ok().flatten().is_none() {
        return Ok(false);
    }
    doc.delete(obj, key)?;
    Ok(true)
}

fn text(doc: &impl ReadDoc, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key).ok().flatten()? {
        (Value::Scalar(scalar), _) => match scalar.as_ref() {
            ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn json(doc: &impl ReadDoc, obj: &ObjId, key: &str) -> Option<String> {
    text(doc, obj, key)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "order and width come from a document's numbers and are exact in f64 as read"
)]
fn number(doc: &impl ReadDoc, obj: &ObjId, key: &str) -> Option<f64> {
    match doc.get(obj, key).ok().flatten()? {
        (Value::Scalar(scalar), _) => match scalar.as_ref() {
            ScalarValue::F64(value) => Some(*value),
            ScalarValue::Int(value) => Some(*value as f64),
            ScalarValue::Uint(value) => Some(*value as f64),
            _ => None,
        },
        _ => None,
    }
}

/// A numeric id held as text: the whole `u64`, not a float that loses its
/// low bits past 2^53.
fn parse(doc: &impl ReadDoc, obj: &ObjId, key: &str) -> Option<u64> {
    text(doc, obj, key)?.parse().ok()
}

fn truth(doc: &impl ReadDoc, obj: &ObjId, key: &str) -> Option<bool> {
    match doc.get(obj, key).ok().flatten()? {
        (Value::Scalar(scalar), _) => match scalar.as_ref() {
            ScalarValue::Boolean(value) => Some(*value),
            _ => None,
        },
        _ => None,
    }
}
