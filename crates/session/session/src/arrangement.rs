//! The person's arrangement as the desk they share with the workspace
//! (workspace.md §9): what is open where, kept in the desk's own record
//! ([`tilo_desktop_model::shared_record`], through the desk this session holds)
//! with history and undo. A workspace's history — a strip made, a width
//! dragged — is the desk's, and every member of the workspace reads the same
//! one. What only one device decides — which window is in front, the width a
//! strip has on its screen — stays device-local, as the desk keeps it.
//!
//! A place also has two things the desk's windows do not carry: the view it
//! shows its resource through, and whether the person pinned it. The desk's
//! document keeps both beside the desk ([`Marks`]), so one record answers what
//! the arrangement is, on this device and across a restart.
//!
//! Each arranging action ([`ArrangementAction`]) is one change of the desk,
//! attributed to whoever asked for it — the person's own input included — and
//! undone as that one step.
//!
//! One rule is fixed (shell.md §1): nothing moves or takes focus under an
//! ongoing gesture or while the person types. An arranging action that
//! arrives then waits here and runs when the gesture or the typing ends; it
//! is never dropped.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use collab_types::presence::UserId;
use serde_json::Value as Json;
use tilo_actions::{
    ActionArgs, ActionContract, ActionParam, ActionSpec, EncodedAction, ExecutionMode,
    OperationKind, ParamType, ResourceSpec, ResultSpec,
};
use tilo_desktop_model::own_arrangement::{self, Place, SHELL, Strips};
use tilo_desktop_model::{
    CanvasPlace, DesktopTree, GroupId, StripId, Tab, TabId, TileContent, WindowWidth, WorkspaceSlot,
};
use tilo_desktop_surface::DesktopHandle;
use tilo_surface_host::Published;
use ui::automerge::{AutoCommit, ChangeHash};
use ui::state::UndoLog;

use crate::i18n::t;

/// The size a place made from a resource takes on the canvas when the
/// caller names none: the framed canvas's window, in canvas pixels.
const DEFAULT_WIDTH: i64 = 720;
/// See [`DEFAULT_WIDTH`].
const DEFAULT_HEIGHT: i64 = 520;

/// The operation type every arranging action carries.
pub const OPERATION: &str = "tilo.shell.arranging";
/// The resource the person's arrangement is published as.
pub const RESOURCE: &str = "shell:arrangement";
/// The resource its history is published as: its undo and redo.
pub const RECORD_RESOURCE: &str = "shell:arrangement-record";
/// The kind of a place's own resource.
pub const PLACE_KIND: &str = "arrangement.place";
/// The kind of a strip's own resource.
pub const STRIP_KIND: &str = "arrangement.strip";
/// The kind of a row's own resource.
pub const ROW_KIND: &str = "arrangement.row";

/// What a person chose for places the desk's own windows do not hold: the view
/// each place shows its resource through, and the places they pinned. The
/// desk's document keeps them beside the desk (`arrangement_marks_v1`), so one
/// record answers what the arrangement is, on this device and across a restart.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Marks {
    /// The view a place shows, by the place's id; a place without one shows
    /// its app's default.
    #[serde(default)]
    pub views: BTreeMap<u64, String>,
    /// The places the person pinned, so an agent's arrangement leaves them
    /// alone (shell.md §5).
    #[serde(default)]
    pub pins: std::collections::BTreeSet<u64>,
}

/// What a place, a strip or a row of the arrangement is published as a resource of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Place,
    Strip,
    Row,
}

impl Item {
    /// The id each of its resources begins with: its id is the prefix and the record's key, so a
    /// resource is named by what it is and a key never has to be read as a number.
    const fn prefix(self) -> &'static str {
        match self {
            Self::Place => "shell:place:",
            Self::Strip => "shell:strip:",
            Self::Row => "shell:row:",
        }
    }

    /// The resource the arrangement publishes the thing `key` names as.
    #[must_use]
    pub fn resource(self, key: &str) -> String {
        format!("{}{key}", self.prefix())
    }

    /// The record's key of the thing a resource id names, `None` for another resource's id.
    #[must_use]
    pub fn key(self, id: &str) -> Option<&str> {
        id.strip_prefix(self.prefix()).filter(|key| !key.is_empty())
    }
}

/// The person's arrangement as a shell's views read it, by key: what the session publishes for
/// the shell to draw (`tilo_surface_host::shell_part`).
pub type Reading = tilo_surface_host::shell_part::ArrangementReading;
/// A workspace of [`Reading`].
pub type WorkspaceReading = tilo_surface_host::shell_part::ArrangementWorkspaceReading;
/// A row of [`Reading`].
pub type RowReading = tilo_surface_host::shell_part::ArrangementRowReading;
/// A strip of [`Reading`].
pub type StripReading = tilo_surface_host::shell_part::ArrangementStripReading;
/// A place of [`Reading`].
pub type PlaceReading = tilo_surface_host::shell_part::ArrangementPlaceReading;

/// An arranging action: one change of the person's arrangement.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "do", rename_all = "kebab-case")]
pub enum ArrangementAction {
    /// Open `shows` (an app's id) into the strip `strip`, after the place
    /// `after` or last, under the new place's id `place`.
    OpenInto {
        strip: u64,
        #[serde(default)]
        after: Option<u64>,
        place: u64,
        shows: String,
        #[serde(default)]
        view: String,
        #[serde(default)]
        record: String,
        /// The window's state, as the desktop model keeps it; absent for a
        /// place this build opens by itself.
        #[serde(default)]
        state: Json,
    },
    /// Move the place `place` into the row `row`, before the place `before`
    /// or last.
    Move {
        place: u64,
        row: u64,
        #[serde(default)]
        before: Option<u64>,
    },
    /// Resize the strip `strip` to `permille` of its row's width.
    Resize { strip: u64, permille: u32 },
    /// Maximize the strip `strip` (`maximized`), so it stands alone as the window the desk is,
    /// or restore it beside its row's other windows.
    Maximize { strip: u64, maximized: bool },
    /// Close the place `place`; its window goes with it.
    Close { place: u64 },
    /// Bring the place `place` in front of the strip it stands in, as choosing its tab does.
    Activate { place: u64 },
    /// Pin the place `place` (`pinned`), so an agent's arrangement leaves it
    /// alone (shell.md §5), or unpin it.
    Pin { place: u64, pinned: bool },
    /// Stand the resource `resource` on the canvas as its view `view`
    /// (empty for its primary published view), at `(x, y)` canvas pixels.
    /// The place is the new entry `place`, alone in its own strip `strip`,
    /// in the row `row`; `shows` is the resource's app, resolved here and
    /// never passed in (shell.md §8: showing something is an action on the
    /// arrangement, not on the resource).
    Place {
        workspace: u64,
        row: u64,
        strip: u64,
        place: u64,
        resource: String,
        #[serde(default)]
        view: String,
        x: i64,
        y: i64,
        width: i64,
        height: i64,
    },
    /// Text typed on the canvas: the top result of the system's `find` stands
    /// there as its primary view (or `view`). The session resolves it into a
    /// [`Place`] when the action is invoked, so the record only ever holds
    /// what was placed, never the words it was found by.
    PlaceFound {
        text: String,
        #[serde(default)]
        view: String,
        x: i64,
        y: i64,
    },
}

/// Where a place made on the desk now goes: the workspace in front, its
/// row, and the ids the place's own strip and the place take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Surface {
    pub workspace: u64,
    pub row: u64,
    pub strip: u64,
    pub place: u64,
}

/// The system's search, as the arrangement reaches it: the same `find` the
/// palette and an agent's `tilo` tool run. The session sets it, so text typed
/// on the canvas finds a resource through the one index and the one ranking,
/// never a search of its own (shell.md §8, "Finding what to place").
pub type Find = Rc<dyn Fn(&str) -> Vec<tilo_desktop_search::Hit>>;

/// Whether `hit` names a resource the arrangement can place: an app's own
/// search hit, or an action hit that acts on one. This is the one test a
/// text typed on the canvas and an agent's `find` share, so both lead with
/// the same result.
#[must_use]
pub fn names_a_resource(hit: &tilo_desktop_search::Hit) -> bool {
    match &hit.target {
        tilo_desktop_search::Target::Invoke(invoke) => {
            invoke.address.action == tilo_desktop_search::OPEN_ACTION
        }
        tilo_desktop_search::Target::Action(_) => true,
        _ => false,
    }
}

/// The first resource `text` finds that a place can show, as the arrangement
/// places it: the top hit that names a resource (an app's own search hit) and
/// that `shows` accepts, else the resource such an action hit acts on, else
/// `None` for a query no result answers with something placeable.
#[must_use]
pub fn found(
    text: &str,
    find: &dyn Fn(&str) -> Vec<tilo_desktop_search::Hit>,
    shows: impl Fn(&str) -> bool,
) -> Option<String> {
    let hits = find(text);
    // What a hit opens is the resource it names: an annotated entry's own
    // anchor, or the one an action hit acts on. The best such hit the place
    // can show wins, in the search's order: a hit the arrangement cannot
    // place (a settings section) does not stand in the way of one it can.
    let named: Vec<(&str, &str)> = hits
        .iter()
        .filter_map(|hit| match &hit.target {
            tilo_desktop_search::Target::Invoke(invoke)
                if invoke.address.action == tilo_desktop_search::OPEN_ACTION =>
            {
                let named = invoke.address.resource.as_str();
                let label = invoke
                    .descriptor
                    .as_ref()
                    .map_or("", |descriptor| descriptor.resource_label.as_str());
                Some((named, label))
            }
            tilo_desktop_search::Target::Action(action) => {
                Some((action.resource.as_str(), action.resource_label.as_str()))
            }
            _ => None,
        })
        .filter(|(resource, _)| shows(resource))
        .collect();
    most_specific(&named, text)
}

/// The app a window shows, by its id, when its tab holds a window at all.
pub(crate) fn tab_app(tile: &tilo_desktop_model::TileContent) -> Option<String> {
    use tilo_desktop_model::TileContent;
    match tile {
        TileContent::Foreign { app_id, .. }
        | TileContent::LocalApp { app_id, .. }
        | TileContent::PeerWindow { app_id, .. }
        | TileContent::HostedWindow { app_id, .. } => Some(app_id.clone()),
        _ => None,
    }
}

/// The resource `text` names among the hits a place can show, each with the
/// name its resource goes by: the top of them, or the one inside it that
/// carries the same name when the text is that name — a note titled like the
/// notebook it sits in is the note, while a resource the text only describes
/// stays itself.
fn most_specific(named: &[(&str, &str)], text: &str) -> Option<String> {
    let (resource, label) = named.first()?;
    if !text.trim().eq_ignore_ascii_case(label) {
        return Some((*resource).to_owned());
    }
    let deeper = named.iter().find(|(other, other_label)| {
        other != resource && other_label == label && inside(other, resource)
    });
    Some(deeper.map_or(*resource, |(other, _)| *other).to_owned())
}

/// Whether `resource` is a level under `other` (`inbox/m1` is under `inbox`).
fn inside(resource: &str, other: &str) -> bool {
    resource != other
        && resource
            .strip_prefix(other)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// A resource as the arrangement makes a place of it: the app publishing
/// it, its views (the primary first when it declares one), and what it is
/// called. What `place` resolves a resource id against, from what the
/// session publishes now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Target {
    /// The app that publishes the resource, by its canonical id.
    pub app: String,
    /// The resource's published views (`ResourceSpec::views`), primary
    /// first.
    pub views: Vec<String>,
    /// The resource's label, as the place is shown under.
    pub title: String,
}

impl Target {
    /// The view a place shows the resource through: `view` when the
    /// resource publishes it, else its primary view, else its first, else
    /// empty for the app's own default.
    #[must_use]
    pub fn chosen_view(&self, view: &str) -> String {
        let view = view.trim();
        if !view.is_empty() && self.views.iter().any(|held| held == view) {
            return view.to_owned();
        }
        self.views.first().cloned().unwrap_or_default()
    }
}

/// The resources the session publishes now, by id: what a place made from a
/// resource takes its app and view from. The session hands it the door's
/// tree each time it changes, and never reads it inside an invocation.
#[derive(Default)]
pub struct Catalog {
    by_id: BTreeMap<String, Target>,
}

impl Catalog {
    /// Take in `resources` (a door's whole tree), recursing into children.
    pub fn take(&mut self, resources: &[ResourceSpec]) {
        let mut by_id = BTreeMap::new();
        for resource in resources {
            collect_targets(resource, &mut by_id);
        }
        self.by_id = by_id;
    }

    /// The resource `id` as it is published, if it is.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Target> {
        self.by_id.get(id)
    }

    /// Put `target` under `id`, as the resource is published: a place made from a resource the
    /// session has just published but the door's tree does not hold yet.
    pub fn remember(&mut self, id: &str, target: Target) {
        self.by_id.insert(id.to_owned(), target);
    }
}

/// Every resource of `resource` and its children, by id, with the views it
/// publishes primary first.
fn collect_targets(resource: &ResourceSpec, out: &mut BTreeMap<String, Target>) {
    let mut views: Vec<&tilo_actions::ResourceViewSpec> = resource.views.iter().collect();
    views.sort_by_key(|view| std::cmp::Reverse(view.primary));
    if let Some(app) = resource.app() {
        out.insert(
            resource.id.as_str().to_owned(),
            Target {
                app: app.to_owned(),
                views: views.into_iter().map(|view| view.id.clone()).collect(),
                title: resource.label.clone(),
            },
        );
    }
    for child in &resource.children {
        collect_targets(child, out);
    }
}

impl ArrangementAction {
    /// The action as an operation's payload decodes, when it is one.
    #[must_use]
    pub fn decode(type_id: &str, payload: &[u8]) -> Option<Self> {
        (type_id == OPERATION)
            // ignored-failure: an operation of another type, or one that does not read back, is not this action
            .then(|| serde_json::from_slice(payload).ok())
            .flatten()
    }
}

/// What a tab says when the person's record kept a window state this build
/// cannot read: the window is not here, and closing the tab is the end of it.
fn not_read() -> String {
    t("desk-place-not-read")
}

/// What the person is in the middle of, as shell.md §1's one fixed rule
/// reads it: nothing moves under a gesture or while they type.
pub use tilo_compositor::Underway as Busy;

/// The person's arrangement, and the changes waiting for the person to
/// finish what they are doing.
pub struct Arrangement {
    /// The person whose record it is.
    person: String,
    /// The record as [`own_arrangement`] keeps it.
    doc: AutoCommit,
    /// The record's history: each arranging action is one undo step of
    /// whoever made it.
    undo: UndoLog,
    /// The desk whose windows the arrangement places.
    handle: DesktopHandle,
    /// The resources the session publishes now, by id: what a place made
    /// from a resource reads its app and views from ([`Catalog`]).
    catalog: Rc<RefCell<Catalog>>,
    /// Actions that arrived under a gesture or while the person typed: they
    /// run in order once it ends.
    waiting: RefCell<VecDeque<(String, ArrangementAction)>>,
    /// What the desk last had, as the arrangement read it, so a desk that
    /// changed on its own (a window opened, a title set) is taken in once.
    seen: RefCell<Option<Strips>>,
    /// The desk's revision `seen` was read at: a desk that has not moved
    /// since costs nothing to look at again.
    seen_at: Cell<u64>,
    /// The record each of this device's windows shows, as the desk was last
    /// read with it: a window whose app holds a record is named by it in its
    /// place from then on, and the places are read again when one is noted.
    seen_records: RefCell<BTreeMap<u64, String>>,
    /// What each window shows, as the broker knows it
    /// ([`crate::continuations`]): the session that runs them says, and an
    /// arrangement made without it names the records it was made with.
    notes: Option<std::sync::Arc<crate::continuations::Continuations>>,
}

impl Arrangement {
    /// `person`'s arrangement in `doc`: their desk as `keep` shows it,
    /// migrated into the record when the record holds none yet, with
    /// `records` naming the record each place shows.
    #[must_use]
    pub fn new(
        person: &str,
        mut doc: AutoCommit,
        handle: DesktopHandle,
        keep: &DesktopTree,
        records: &BTreeMap<TabId, String>,
        marks: &Marks,
    ) -> Self {
        // Nothing kept: the desk is the arrangement, and the choices the
        // desk's windows do not carry (`marks`) go with it.
        let mut kept = own_arrangement::read(&doc);
        if kept.is_empty() {
            kept = Strips::from_tree(keep, records);
            for strip in &mut kept.strips {
                for place in &mut strip.places {
                    if let Some(view) = marks.views.get(&place.id) {
                        place.view.clone_from(view);
                    }
                    place.pinned = marks.pins.contains(&place.id);
                }
            }
            if let Err(error) = own_arrangement::write(&mut doc, &kept) {
                tracing::warn!(%error, "the desk could not be read as the arrangement");
            }
        }
        // A place the record kept as the live window this device showed at
        // the last write is one its app opens again: nothing of a window
        // outlives the session that mapped it, so the record keeps what opens
        // it, as the desk keeps a window it must open again
        // ([`tilo_desktop_model::Tab::relaunchable`]). Keeping the live window
        // instead would leave the place opening nothing, and the desk loses
        // the tab it was in. The rows, strips, widths and canvas places stay,
        // and so does every place that is not a live window; a live window
        // that names no app opens nothing, so its place goes as it did.
        let mut revived = kept.clone();
        let mut changed = false;
        for strip in &mut revived.strips {
            let places = std::mem::take(&mut strip.places);
            for mut place in places {
                if place.state.get("Foreign").is_none() {
                    strip.places.push(place);
                    continue;
                }
                let tile = match serde_json::from_value::<TileContent>(place.state.clone()) {
                    Ok(tile) => tile,
                    // A live window this build cannot read opens nothing, and
                    // the place stays exactly as the record keeps it: its tab
                    // says why ([`Self::place`]), the person closes it when
                    // they are done, and a build that reads the state opens
                    // the window it kept. Removing it here would take the
                    // place off their other devices too.
                    Err(error) => {
                        tracing::warn!(%error, "a saved place does not read back; its window is not restored");
                        strip.places.push(place);
                        continue;
                    }
                };
                let relaunchable = Tab {
                    id: place.id,
                    title: place.title.clone(),
                    tile,
                    app_title: None,
                }
                .relaunchable();
                if matches!(&relaunchable.tile, TileContent::LocalApp { app_id, .. }
                    if app_id.trim().is_empty())
                {
                    changed = true;
                    continue;
                }
                if let Ok(state) = serde_json::to_value(&relaunchable.tile)
                    && state != place.state
                {
                    place.state = state;
                    changed = true;
                }
                strip.places.push(place);
            }
        }
        if changed && let Err(error) = own_arrangement::write(&mut doc, &revived) {
            tracing::warn!(%error, "the windows to open again could not be kept");
        }
        let undo = UndoLog::new(&mut doc);
        Self {
            person: person.to_owned(),
            doc,
            undo,
            handle,
            catalog: Rc::new(RefCell::new(Catalog::default())),
            waiting: RefCell::new(VecDeque::new()),
            seen: RefCell::new(None),
            seen_at: Cell::new(0),
            seen_records: RefCell::new(records.clone()),
            notes: None,
        }
    }

    /// The resources the session publishes now, by id: what a place made
    /// from a resource reads its app and views from. The session keeps it in
    /// step with the door's tree.
    #[must_use]
    pub fn targets(&self) -> Rc<RefCell<Catalog>> {
        Rc::clone(&self.catalog)
    }

    /// The surface the desk shows at `(x, y)` as a place made there stands:
    /// the workspace in front, its row, the id its own strip takes and the
    /// id its place takes. Every id is this device's: a place made here is
    /// this person's, synced by the record like any other.
    ///
    /// # Errors
    /// The desk has no workspace in front to put anything on.
    pub fn surface_at(&self) -> Result<Surface, String> {
        let tree = self.handle.read(Clone::clone);
        let workspace = tree
            .active_workspace()
            .ok_or("the desk has no workspace in front")?;
        let row = tree
            .active_strip
            .get(&workspace.slot)
            .copied()
            .or_else(|| workspace.strips.first().map(|row| row.id))
            .ok_or("the workspace in front has no row")?;
        let strip = self.handle.mint_id();
        let place = self.handle.mint_id();
        Ok(Surface {
            workspace: workspace.slot.0,
            row: row.0,
            strip,
            place,
        })
    }

    /// Place the resource a text typed at `at` finds on the canvas, as `asked`'s:
    /// the top result of the system's `find` (`find`) that a place can show,
    /// resolved against what the session publishes now ([`Catalog`]), standing at
    /// `at` through the arranging action [`ArrangementAction::Place`] — one
    /// attributed, undoable change of the person's record (shell.md §8). `view`
    /// empty shows the resource's primary view.
    ///
    /// Returns the place's id, or `None` when nothing the text finds is a
    /// resource the arrangement can place.
    ///
    /// # Errors
    /// Why the place could not be made, or the text found nothing to place.
    pub fn place_found(
        &mut self,
        asked: &str,
        text: &str,
        view: &str,
        at: (i64, i64),
        find: &dyn Fn(&str) -> Vec<tilo_desktop_search::Hit>,
        busy: Busy,
    ) -> Result<Option<u64>, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("nothing is typed to place".into());
        }
        let Some(resource) = found(text, find, |resource| {
            self.catalog.borrow().get(resource).is_some()
        }) else {
            return Err(format!("nothing matches {text:?}"));
        };
        let surface = self.surface_at()?;
        let action = ArrangementAction::Place {
            workspace: surface.workspace,
            row: surface.row,
            strip: surface.strip,
            place: surface.place,
            resource,
            view: view.to_owned(),
            x: at.0,
            y: at.1,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
        };
        self.ask(asked, action, busy)?;
        Ok(Some(surface.place))
    }

    /// The arrangement as this shell reads it.
    #[must_use]
    pub fn strips(&self) -> Strips {
        own_arrangement::read(&self.doc)
    }

    /// The changes the arrangement is at: a caller that keeps its choices with
    /// the desk writes them when these move, not every frame.
    #[must_use]
    pub fn heads(&mut self) -> Vec<ChangeHash> {
        self.doc.get_heads()
    }

    /// What the person chose for places the desk's windows do not carry: the
    /// view each shows its resource through and the places they pinned, as the
    /// arrangement holds them now. The desk's document keeps these beside the
    /// desk ([`Marks`]).
    #[must_use]
    pub fn marks(&self) -> Marks {
        let mut marks = Marks::default();
        for place in self.strips().places() {
            if !place.view.is_empty() {
                marks.views.insert(place.id, place.view.clone());
            }
            if place.pinned {
                marks.pins.insert(place.id);
            }
        }
        marks
    }

    /// The desk's windows by tab id, as a place shows one: the app it shows, what the window
    /// calls itself, and whether that name is the person's own. Read once for a whole reading,
    /// so naming a place costs no more than walking the desk the reading already describes.
    fn shown_windows(&self) -> BTreeMap<u64, (String, String, bool)> {
        use tilo_desktop_model::TileContent;
        self.handle.read(|tree| {
            let mut shown = BTreeMap::new();
            for workspace in &tree.workspaces {
                for strip in &workspace.strips {
                    for group in &strip.window_groups {
                        for tab in &group.tabs {
                            let app = match &tab.tile {
                                TileContent::Foreign { app_id, .. }
                                | TileContent::LocalApp { app_id, .. }
                                | TileContent::PeerWindow { app_id, .. }
                                | TileContent::HostedWindow { app_id, .. } => app_id.clone(),
                                TileContent::AppPlaceholder { label, .. }
                                | TileContent::Stub { label, .. } => label.clone(),
                            };
                            shown.insert(tab.id, (app, tab.title.clone(), tab.app_title.is_some()));
                        }
                    }
                }
            }
            shown
        })
    }

    /// The arrangement as a shell's views read it: every workspace, row, strip and place, each
    /// by its key, laid out as the shell's arrangement type holds it. This is what the session
    /// publishes on its door for a shell to draw.
    #[must_use]
    pub fn reading(&self) -> Reading {
        let strips = self.strips();
        let shown = self.shown_windows();
        let mut places = Vec::new();
        let mut strips_reading = Vec::new();
        for strip in &strips.strips {
            for place in &strip.places {
                // What the place shows, as the desk has it now: a window the person renamed, or
                // one whose app took another id, is named by what it says today, not by what
                // the record kept when it opened. A place the desk no longer holds keeps the
                // record's own title.
                let held = shown.get(&place.id);
                let record_title = self
                    .catalog
                    .borrow()
                    .get(&place.record)
                    .map(|target| target.title.trim().to_owned())
                    .unwrap_or_default();
                let (app, title) = match held {
                    // A place showing a record is named by that record: its window shows it,
                    // so its title is the window's, unless the person named the window itself.
                    Some((app, title, named)) if !*named && !record_title.is_empty() => {
                        (app.clone(), record_title)
                    }
                    Some((app, title, _)) => (app.clone(), title.clone()),
                    None if !record_title.is_empty() => (place.shows.clone(), record_title),
                    None => (place.shows.clone(), place.title.clone()),
                };
                places.push(PlaceReading {
                    key: place.id.to_string(),
                    strip: strip.id.to_string(),
                    shows: place.shows.clone(),
                    shown_as: place.view.clone(),
                    record: place.record.clone(),
                    title: place.title.clone(),
                    name: crate::app_names::window_name(&app, &title),
                    pinned: place.pinned,
                    shell: place.shell.clone(),
                });
            }
            strips_reading.push(StripReading {
                key: strip.id.to_string(),
                workspace: strip.workspace.0.to_string(),
                row: strip.row.0.to_string(),
                width: i64::from(strip.width.permille()),
                maximized: strip.maximized,
                active: strip
                    .active
                    .filter(|active| strip.places.iter().any(|place| place.id == *active))
                    .map_or_else(String::new, |active| active.to_string()),
                placed: strip.canvas.is_some(),
                canvas_x: strip.canvas.map_or(0, |canvas| i64::from(canvas.x)),
                canvas_y: strip.canvas.map_or(0, |canvas| i64::from(canvas.y)),
                canvas_width: strip.canvas.map_or(0, |canvas| i64::from(canvas.width)),
                canvas_height: strip.canvas.map_or(0, |canvas| i64::from(canvas.height)),
                shell: strip.shell.clone(),
            });
        }
        Reading {
            workspaces: strips
                .workspaces
                .iter()
                .map(|workspace| WorkspaceReading {
                    key: workspace.id.0.to_string(),
                    name: workspace.name.clone(),
                })
                .collect(),
            rows: strips
                .rows
                .iter()
                .map(|row| RowReading {
                    key: row.id.0.to_string(),
                    workspace: row.workspace.0.to_string(),
                    name: row.name.clone(),
                    shell: row.shell.clone(),
                })
                .collect(),
            strips: strips_reading,
            places,
        }
    }

    /// The arrangement as the session publishes it now: what [`resources`] publishes, with a
    /// resource of its own for every row, strip and place, named by its record key. A press
    /// names the thing it presses (`shell:place:<key>`), so a key is never read as a number,
    /// and a place's own actions act on that place.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceSpec> {
        let reading = self.reading();
        let rows = reading
            .rows
            .iter()
            .map(|row| ResourceSpec::new(Item::Row.resource(&row.key), ROW_KIND, row_label(row)));
        let strips = reading.strips.iter().map(|strip| {
            strip_actions(&strip.key).into_iter().fold(
                ResourceSpec::new(
                    Item::Strip.resource(&strip.key),
                    STRIP_KIND,
                    strip_label(strip),
                ),
                ResourceSpec::with_action,
            )
        });
        let places = reading.places.iter().map(|place| {
            place_actions(&place.key).into_iter().fold(
                ResourceSpec::new(
                    Item::Place.resource(&place.key),
                    PLACE_KIND,
                    place_label(place),
                ),
                ResourceSpec::with_action,
            )
        });
        vec![
            arrangement_resource().with_children(rows.chain(strips).chain(places)),
            record_resource(),
        ]
    }

    /// Where what it publishes stands ([`Published::revision`]): what it publishes and the
    /// desk's own revision, so a place, strip or row that came, went or was renamed changes it.
    #[must_use]
    pub fn revision(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::hash::DefaultHasher::new();
        hash_resources(&self.resources(), &mut hasher);
        self.handle.revision().hash(&mut hasher);
        hasher.finish()
    }

    /// The person whose arrangement it is.
    #[must_use]
    pub fn person(&self) -> &str {
        &self.person
    }

    /// Carry out `action` as `asked`'s: one change of the record, attributed
    /// to them and undone as one step. Under an ongoing gesture or while the
    /// person types it waits ([`Self::waiting`]) and runs when they end.
    ///
    /// Returns whether it ran now.
    ///
    /// # Errors
    /// Why the action names something the arrangement does not hold.
    pub fn ask(
        &mut self,
        asked: &str,
        action: ArrangementAction,
        busy: Busy,
    ) -> Result<bool, String> {
        if busy.holds() {
            self.waiting
                .borrow_mut()
                .push_back((asked.to_owned(), action));
            return Ok(false);
        }
        self.write(asked, &action, true)?;
        Ok(true)
    }

    /// Run one of the actions that waited, now the person's gesture or typing
    /// ended. One per call, so a queue never holds a frame.
    ///
    /// # Errors
    /// Why the action could not be made.
    pub fn release(&mut self) -> Result<(), String> {
        let Some((asked, action)) = self.waiting.borrow_mut().pop_front() else {
            return Ok(());
        };
        self.write(&asked, &action, true)
    }

    /// How many actions wait for the person to finish.
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.waiting.borrow().len()
    }

    /// The last step of the record's history, as who made it and what it
    /// did: what a history view names it by.
    #[must_use]
    pub fn last_step(&mut self) -> Option<(String, String)> {
        let entry = self
            .undo
            .next_undo_as(&self.doc, Some(&user(&self.person)))?;
        let step = entry.step?;
        // Who asked for it: the invoker the step records, else the person
        // whose record it is.
        let who = step
            .invoker
            .map_or_else(|| self.person.clone(), |invoker| invoker.actor);
        let what = match step.what {
            Some(tilo_actions::Did::Verb(call)) => call.action.action,
            _ => String::new(),
        };
        Some((who, what))
    }

    /// The person's own change, made as their gesture's end says: their own
    /// input never waits for itself.
    ///
    /// # Errors
    /// Why the action names something the arrangement does not hold.
    pub fn ask_person(&mut self, action: &ArrangementAction) -> Result<(), String> {
        let person = self.person.clone();
        self.write(&person, action, true)
    }

    /// Undo `person`'s last change of the arrangement. Whether there was one.
    pub fn undo(&mut self, person: &str) -> bool {
        let stepped = self
            .undo
            .undo_as(&mut self.doc, Some(&user(person)), None, None)
            .is_some();
        if stepped {
            self.place();
        }
        stepped
    }

    /// Redo `person`'s last undone change of the arrangement.
    pub fn redo(&mut self, person: &str) -> bool {
        let stepped = self
            .undo
            .redo_as(&mut self.doc, Some(&user(person)), None, None)
            .is_some();
        if stepped {
            self.place();
        }
        stepped
    }

    /// Make `action` one change of the record: written, attributed to `asked`
    /// as its undo step, and (with `place`) laid out on the desk.
    fn write(
        &mut self,
        asked: &str,
        action: &ArrangementAction,
        place: bool,
    ) -> Result<(), String> {
        let before: Vec<ChangeHash> = self.doc.get_heads();
        let mut strips = own_arrangement::read(&self.doc);
        // An agent arranges the person's desk like the person does, except
        // where they pinned something: a pinned place is theirs to move
        // (shell.md §5's default, "not what they pinned").
        let by_the_person = asked == self.person;
        let catalog = self.catalog.clone();
        if !change(&mut strips, action, by_the_person, &catalog)? {
            return Ok(());
        }
        own_arrangement::write(&mut self.doc, &strips).map_err(|error| error.to_string())?;
        self.doc.commit_with(
            ui::automerge::transaction::CommitOptions::default()
                .with_message(Self::step_message(asked, action)),
        );
        // The step is the person's to take back, whoever acted for them; who
        // it was is the step's invoker, which history names.
        let person = self.person.clone();
        self.undo.record_committed_action_authored(
            &mut self.doc,
            before,
            None,
            None,
            Some(user(&person)),
        );
        if place {
            self.place();
        }
        Ok(())
    }

    /// The message the step `action` commits under: what it did, and who
    /// asked for it.
    fn step_message(asked: &str, action: &ArrangementAction) -> String {
        let context = tilo_actions::ActionContext {
            invoker: Some(tilo_actions::ActionInvoker::actor(asked.to_owned())),
            what: Some(tilo_actions::Did::Verb(tilo_actions::VerbCall::new(
                tilo_actions::ActionKey::new("so.tilo.session", "arrangement", verb(action)),
                std::iter::empty(),
            ))),
            ..tilo_actions::ActionContext::default()
        };
        context
            .change_message()
            .unwrap_or_else(|| format!("arrange: {}", verb(action)))
    }

    /// Read what each window shows from `notes`, the session's, from now on:
    /// a window whose app holds a record is named by it in its place, and the
    /// places are read again when one is noted.
    pub(crate) fn reads_records(
        &mut self,
        notes: std::sync::Arc<crate::continuations::Continuations>,
    ) {
        *self.seen_records.borrow_mut() = notes.records();
        self.notes = Some(notes);
    }

    /// Take in what the desk did on its own since the last pass — a row or
    /// strip made, a window opened, a width dragged, a tab closed — as
    /// arranging actions of the person's: their own input goes through the
    /// same actions an agent's does, so it is attributed to them and undone
    /// as one step.
    pub fn observe(&mut self) {
        // A desk that has not moved since it was last read is the same desk:
        // there is nothing to take in.
        let at = self.handle.revision();
        let records = self.notes.as_ref().map_or_else(
            || self.seen_records.borrow().clone(),
            |notes| notes.records(),
        );
        let noted = *self.seen_records.borrow() != records;
        if at == self.seen_at.replace(at) && !noted {
            return;
        }
        *self.seen_records.borrow_mut() = records.clone();
        let became = self.handle.read(|tree| Strips::from_tree(tree, &records));
        if self
            .seen
            .borrow()
            .as_ref()
            .is_some_and(|seen| same_places(seen, &became))
        {
            return;
        }
        let was = self.seen.borrow_mut().take();
        let Some(was) = was else {
            *self.seen.borrow_mut() = Some(became);
            return;
        };
        *self.seen.borrow_mut() = Some(became.clone());
        let before: Vec<ChangeHash> = self.doc.get_heads();
        let mut strips = own_arrangement::read(&self.doc);
        let mut changed = adopt_structure(&mut strips, &became);
        for action in actions_between(&was, &became) {
            let catalog = self.catalog.clone();
            match change(&mut strips, &action, true, &catalog) {
                Ok(made) => changed |= made,
                Err(why) => tracing::warn!(why, "the desk's own change could not be kept"),
            }
        }
        if !changed {
            return;
        }
        if let Err(error) = own_arrangement::write(&mut self.doc, &strips) {
            tracing::warn!(%error, "the desk's own change could not be kept");
            return;
        }
        self.doc.commit_with(
            ui::automerge::transaction::CommitOptions::default()
                .with_message("arrange: the desk's own change"),
        );
        let person = self.person.clone();
        self.undo.record_committed_action_authored(
            &mut self.doc,
            before,
            None,
            None,
            Some(user(&person)),
        );
    }

    /// Lay the desk out from the record now, as the session starts: the
    /// arrangement's rows, strips and places become the desk's. What the
    /// desk shows from here on is the record's, so what it does next is
    /// taken in from this point.
    pub fn place_now(&mut self) {
        self.place();
        *self.seen.borrow_mut() = Some(self.strips());
    }

    /// Lay the desk out from the record. The workspaces and their names are
    /// the desk's own, and so is which of them is in front on this device;
    /// the rows, strips and places are the record's.
    fn place(&self) {
        let (held, shown) = self.handle.read(|tree| {
            let held: BTreeMap<u64, tilo_desktop_model::Workspace> = tree
                .workspaces
                .iter()
                .map(|workspace| (workspace.slot.0, workspace.clone()))
                .collect();
            let shown = (
                tree.active,
                tree.active_strip.clone(),
                tree.active_group.clone(),
                tree.outputs.clone(),
            );
            (held, shown)
        });
        let strips = own_arrangement::read(&self.doc);
        let mut tree = strips.to_tree(|id| held.get(&id.0).map(|workspace| workspace.name.clone()));
        // A workspace the desk holds stays the desk's: its id, who may do
        // what in it, its shortcuts and the rest are never the arrangement's.
        // Only its rows, strips and places are the record's.
        for workspace in &mut tree.workspaces {
            if let Some(kept) = held.get(&workspace.slot.0) {
                let mut kept = kept.clone();
                kept.strips = std::mem::take(&mut workspace.strips);
                kept.arrangement = std::mem::take(&mut workspace.arrangement);
                *workspace = kept;
            }
        }
        // Which workspace and strip are in front is this device's, and
        // survives the record's change.
        let (active, active_strip, active_group, outputs) = shown;
        if tree
            .workspaces
            .iter()
            .any(|workspace| workspace.slot == active)
        {
            tree.active = active;
            tree.active_strip = active_strip
                .into_iter()
                .filter(|(_, strip)| {
                    tree.workspaces
                        .iter()
                        .any(|workspace| workspace.strips.iter().any(|row| row.id == *strip))
                })
                .collect();
            tree.active_group = active_group;
            tree.outputs = outputs;
        }
        self.handle.replace_tree(tree);
        // A place whose window state this build cannot read has its tab all
        // the same, saying why its window is not here: the person closes it
        // when they are done, and the place the record keeps is opened by a
        // build that reads it.
        for place in strips.places().filter(|place| !place.reads_back()) {
            self.handle.note_in_tab(place.id, &not_read());
        }
        // What the desk shows now is the record's: it is not the desk doing
        // something of its own.
        *self.seen.borrow_mut() = Some(self.strips());
    }
}

/// One change of `strips`, as `action` names it. `by_the_person` is whether
/// the person's own input asked for it: an agent's move leaves a place the
/// person pinned where it is. Whether anything changed.
fn change(
    strips: &mut Strips,
    action: &ArrangementAction,
    by_the_person: bool,
    catalog: &Rc<RefCell<Catalog>>,
) -> Result<bool, String> {
    match action {
        ArrangementAction::OpenInto {
            strip,
            after,
            place,
            shows,
            view,
            record,
            state,
        } => {
            let opened = Place {
                id: *place,
                shows: shows.clone(),
                view: view.clone(),
                record: record.clone(),
                state: state.clone(),
                title: String::new(),
                pinned: false,
                shell: SHELL.to_owned(),
            };
            open_into(strips, *strip, *after, opened)
        }
        ArrangementAction::Move { place, row, before } => {
            if !by_the_person && strips.place(*place).is_some_and(|held| held.pinned) {
                return Err("the person pinned that place: it stays where it is".into());
            }
            if strips.rows.iter().all(|held| held.id.0 != *row) {
                return Err("that row is not in the arrangement".into());
            }
            // A strip the row has none of yet is minted from the place, so
            // two devices making one at once agree.
            let minted: GroupId = row.wrapping_mul(31).wrapping_add(1) & PEER_MASK;
            strips
                .move_place(*place, StripId(*row), *before, minted)
                .map(|_| true)
                .ok_or_else(|| "that place is not in the arrangement".to_owned())
        }
        ArrangementAction::Resize { strip, permille } => {
            Ok(strips.resize(*strip, WindowWidth::Fraction(*permille)))
        }
        ArrangementAction::Maximize { strip, maximized } => {
            if strips.strip(*strip).is_none() {
                return Err("that strip is not in the arrangement".into());
            }
            Ok(strips.maximize(*strip, *maximized))
        }
        ArrangementAction::Close { place } => {
            if strips.close_place(*place) {
                Ok(true)
            } else {
                Err("that place is not in the arrangement".into())
            }
        }
        ArrangementAction::Activate { place } => {
            if strips.activate(*place) {
                Ok(true)
            } else if strips.place(*place).is_some() {
                Err("that place is in front already".into())
            } else {
                Err("that place is not in the arrangement".into())
            }
        }
        ArrangementAction::Pin { place, pinned } => {
            if strips.place(*place).is_none() {
                return Err("that place is not in the arrangement".into());
            }
            Ok(strips.pin(*place, *pinned))
        }
        // Resolved into a `Place` at invocation, where the system's search
        // and what is published now are; the record never holds the words.
        ArrangementAction::PlaceFound { .. } => {
            Err("a place found by text is resolved before it reaches the record".into())
        }
        ArrangementAction::Place {
            workspace,
            row,
            strip,
            place,
            resource,
            view,
            x,
            y,
            width,
            height,
        } => place_at(
            strips,
            catalog,
            Placed {
                workspace: *workspace,
                row: *row,
                strip: *strip,
                place: *place,
                resource,
                view,
                x: *x,
                y: *y,
                width: *width,
                height: *height,
            },
        ),
    }
}

/// The place `opened` open in the strip `strip`, after the place `after` or
/// last. Whether the desk changed.
fn open_into(
    strips: &mut Strips,
    strip: u64,
    after: Option<u64>,
    opened: Place,
) -> Result<bool, String> {
    if strips.strip(strip).is_none() {
        return Err("that strip is not in the arrangement".into());
    }
    if strips.open_into(strip, opened, after) {
        Ok(true)
    } else {
        Err("that place is open already".into())
    }
}

/// What [`change`] is asked to place: the resource `resource`, through
/// `view`, as the place `place` in its own strip `strip` of the row `row`
/// of `workspace`, at `(x, y)` canvas pixels, `width` by `height`.
#[derive(Clone, Copy)]
struct Placed<'a> {
    workspace: u64,
    row: u64,
    strip: u64,
    place: u64,
    resource: &'a str,
    view: &'a str,
    x: i64,
    y: i64,
    width: i64,
    height: i64,
}

/// Stand `asked` on the canvas as one of its published views: an entry
/// naming the resource and the view, with `shows` from the resource's app
/// (shell.md §8). Whether the record changed.
fn place_at(
    strips: &mut Strips,
    catalog: &Rc<RefCell<Catalog>>,
    asked: Placed<'_>,
) -> Result<bool, String> {
    let resource = asked.resource.trim();
    if resource.is_empty() {
        return Err("a place names the resource it shows".into());
    }
    let target = catalog
        .borrow()
        .get(resource)
        .cloned()
        .ok_or_else(|| format!("nothing published is `{resource}`"))?;
    let id: TabId = asked.place;
    if strips.place(id).is_some() {
        return Err("that place is open already".into());
    }
    let placed = Place {
        id,
        shows: target.app.clone(),
        view: target.chosen_view(asked.view),
        record: resource.to_owned(),
        state: Json::Null,
        title: target.title,
        pinned: false,
        shell: SHELL.to_owned(),
    };
    let canvas = CanvasPlace {
        x: clamp_coord(asked.x),
        y: clamp_coord(asked.y),
        width: clamp_side(asked.width),
        height: clamp_side(asked.height),
    };
    if strips.place_at_canvas(
        WorkspaceSlot(asked.workspace),
        StripId(asked.row),
        asked.strip,
        placed,
        canvas,
    ) {
        Ok(true)
    } else {
        Err("that workspace is not on this desk, or that place or strip is taken".into())
    }
}

/// A canvas coordinate as a place keeps it.
fn clamp_coord(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
}

/// A canvas side, at least one pixel.
fn clamp_side(value: i64) -> u32 {
    u32::try_from(value.max(1)).unwrap_or(u32::MAX)
}

/// The arrangement a build before this one kept beside the desk under `state`,
/// taken once: what it holds is the desk from here on, so the session lays the
/// desk out from it and the file goes. `None` when this device kept none, or
/// kept one this build cannot read.
#[must_use]
pub fn take_kept(state: &std::path::Path) -> Option<AutoCommit> {
    let path = state.join("tilo").join(LEGACY_FILE);
    let text = std::fs::read(&path).ok()?;
    let doc = match AutoCommit::load(&text) {
        Ok(doc) => doc,
        Err(error) => {
            // ignored-failure: another build kept the arrangement, and the desk
            // this device holds is the one its person sees
            tracing::debug!(%error, "the kept arrangement is not this build's");
            return None;
        }
    };
    if let Err(error) = std::fs::remove_file(&path) {
        tracing::warn!(path = %path.display(), %error, "the kept arrangement is not cleared");
    }
    Some(doc)
}

/// The arrangement file a build before this one kept beside the desk, brought
/// into the desk's document once.
const LEGACY_FILE: &str = "arrangement.automerge";

/// The id space's counter mask, for an id this build derives rather than
/// mints.
const PEER_MASK: u64 = tilo_desktop_model::PEER_ID_COUNTER_MASK;

/// The verb `action` is, as its step names it.
fn verb(action: &ArrangementAction) -> &'static str {
    match action {
        ArrangementAction::OpenInto { .. } => "open",
        ArrangementAction::Move { .. } => "move",
        ArrangementAction::Resize { .. } => "resize",
        ArrangementAction::Maximize { .. } => "maximize-strip",
        ArrangementAction::Close { .. } => "close",
        ArrangementAction::Activate { .. } => "activate",
        ArrangementAction::Pin { .. } => "pin",
        ArrangementAction::Place { .. } | ArrangementAction::PlaceFound { .. } => "place",
    }
}

/// The person `person`, as the history names them.
fn user(person: &str) -> UserId {
    UserId(person.to_owned())
}

/// The person's arranging actions, published as every other action is: a
/// person's own drag, the palette, a script and an agent all arrange through
/// them, and the catalogue lists them. What is already arranged is published
/// per place, strip and row by [`Arrangement::resources`].
#[must_use]
pub fn resources() -> Vec<ResourceSpec> {
    vec![arrangement_resource(), record_resource()]
}

/// What puts a resource on the canvas and steps the arrangement's history.
fn arrangement_resource() -> ResourceSpec {
    placing_actions()
        .into_iter()
        .fold(
            ResourceSpec::new(RESOURCE, "shell", "The person's arrangement"),
            ResourceSpec::with_action,
        )
        .with_action(step_action("undo", "Undo the last arranging step"))
        .with_action(step_action("redo", "Redo the last undone arranging step"))
        // The conversation's one input (shell.md §8.1): a program or a sentence.
        .with_action(crate::conversation::action())
}

/// The record itself, whose actions step its history.
fn record_resource() -> ResourceSpec {
    ResourceSpec::new(RECORD_RESOURCE, "shell", "The person's arrangement record")
        .with_action(step_action("undo", "Undo the last arranging step"))
        .with_action(step_action("redo", "Redo the last undone arranging step"))
}

/// The resource a place's own actions are published on, for the package the session describes:
/// no key, so it stands for every place. A place's resource is named by the place's key
/// ([`Arrangement::resources`]).
#[must_use]
pub fn place_kind_resource() -> ResourceSpec {
    place_actions("").into_iter().fold(
        ResourceSpec::new(String::new(), PLACE_KIND, String::new()),
        ResourceSpec::with_action,
    )
}

/// The resource a strip's own actions are published on: [`place_kind_resource`] for a strip.
#[must_use]
pub fn strip_kind_resource() -> ResourceSpec {
    strip_actions("").into_iter().fold(
        ResourceSpec::new(String::new(), STRIP_KIND, String::new()),
        ResourceSpec::with_action,
    )
}

/// The resource a row is published as: [`place_kind_resource`] for a row. A row offers no action
/// of its own: it is what a place moves into, named by the place's `move`.
#[must_use]
pub fn row_kind_resource() -> ResourceSpec {
    ResourceSpec::new(String::new(), ROW_KIND, String::new())
}

/// The actions a place's own resource offers, its key bound into each: the place is the resource
/// they are pressed on, so none of them names it and its key is never read as a number. What a
/// press names instead is a row, by that row's resource.
fn place_actions(key: &str) -> Vec<ActionSpec> {
    let pinned = key.to_owned();
    let closed = key.to_owned();
    let moved = key.to_owned();
    let in_front = key.to_owned();
    vec![
        action(
            "pin",
            "Pin a place, so an agent's arrangement leaves it alone",
            vec![optional(
                "pinned",
                ParamType::Boolean,
                "Whether it is pinned",
            )],
            move |args| ArrangementAction::Pin {
                place: count_of(&pinned),
                pinned: args.boolean("pinned").unwrap_or(true),
            },
        ),
        action("close", "Close a place", Vec::new(), move |_| {
            ArrangementAction::Close {
                place: count_of(&closed),
            }
        }),
        action(
            "activate",
            "Bring a place in front of its strip",
            Vec::new(),
            move |_| ArrangementAction::Activate {
                place: count_of(&in_front),
            },
        ),
        action(
            "move",
            "Move a place into a row",
            vec![
                required("row", ParamType::Resource, "The row it goes into")
                    .of_resource_kind(ROW_KIND),
            ],
            move |args| ArrangementAction::Move {
                place: count_of(&moved),
                row: named(args, "row", Item::Row).unwrap_or_default(),
                before: None,
            },
        ),
    ]
}

/// The actions a strip's own resource offers, its key bound into each: the strip is the resource
/// they are pressed on.
fn strip_actions(key: &str) -> Vec<ActionSpec> {
    let resized = key.to_owned();
    let maximized = key.to_owned();
    vec![
        action(
            "resize",
            "Resize a strip",
            vec![required(
                "permille",
                ParamType::Integer,
                "Its width in per-mille of the row",
            )],
            move |args| ArrangementAction::Resize {
                strip: count_of(&resized),
                permille: u32::try_from(args.integer("permille").unwrap_or(1)).unwrap_or(1),
            },
        ),
        action(
            "maximize-strip",
            "Maximize a strip, so it stands alone as the window the desk is",
            vec![optional(
                "on",
                ParamType::Boolean,
                "Whether it fills the desk",
            )],
            move |args| ArrangementAction::Maximize {
                strip: count_of(&maximized),
                maximized: args.boolean("on").unwrap_or(true),
            },
        ),
    ]
}

/// The row `arguments` names as `name`, by the id of the row's resource. `None` for an id of
/// another resource, which the action then refuses as naming no row.
fn named(args: &ActionArgs, name: &str, item: Item) -> Option<u64> {
    args.text(name)
        .and_then(|id| item.key(id.trim()))
        // ignored-failure: an id that is not a number names no row, which the action refuses
        .and_then(|key| key.parse().ok())
}

/// The record key of a place, a strip or a row, as its actions take it. `0` for the keyless
/// resource a package describes the kind with, which no door publishes.
fn count_of(key: &str) -> u64 {
    // ignored-failure: a keyless resource has no count, and 0 is its count
    key.parse().unwrap_or_default()
}

/// What a row is called where its own actions are listed: its name, or what it is when it has
/// none.
fn row_label(row: &RowReading) -> String {
    if row.name.is_empty() {
        "Row".to_owned()
    } else {
        row.name.clone()
    }
}

/// What a strip is called where its own actions are listed: how wide it is.
fn strip_label(strip: &StripReading) -> String {
    format!("Strip, {}% wide", strip.width / 10)
}

/// What a place is called where its own actions are listed: the window it holds.
fn place_label(place: &PlaceReading) -> String {
    if place.name.is_empty() {
        "Place".to_owned()
    } else {
        place.name.clone()
    }
}

/// Feed what `specs` publish into `hasher`: each resource's id, kind and label, and its
/// children's, so two revisions are equal exactly when the door publishes the same.
fn hash_resources(specs: &[ResourceSpec], hasher: &mut std::hash::DefaultHasher) {
    use std::hash::{Hash as _, Hasher as _};
    for spec in specs {
        spec.id.0.hash(hasher);
        spec.kind.hash(hasher);
        spec.label.hash(hasher);
        hash_resources(&spec.children, hasher);
    }
    hasher.write_u8(0);
}

/// The actions that put a resource on the canvas: by naming it and one of
/// its views, or by what a text finds (shell.md §8).
fn placing_actions() -> Vec<ActionSpec> {
    vec![
        action(
            "place",
            "Put a resource on the canvas as one of its views",
            vec![
                // A workspace's id is its own, across all of u64: text, as
                // the arrangement record names it, where an integer stops at
                // i64. Each left out is the surface in front's: its workspace
                // and row, and a strip and place id of its own.
                optional(
                    "workspace",
                    ParamType::Text,
                    "The workspace whose canvas it stands on, by its id; the one in front when left out",
                ),
                optional(
                    "row",
                    ParamType::Integer,
                    "The row it stands in; the row in front when left out",
                ),
                optional(
                    "strip",
                    ParamType::Integer,
                    "The strip it stands in; a new one when left out",
                ),
                optional(
                    "place",
                    ParamType::Integer,
                    "The id the new place takes; a new one when left out",
                ),
                required("resource", ParamType::Resource, "The resource it shows"),
                optional("view", ParamType::Text, "The view it shows it through"),
                optional("x", ParamType::Integer, "Its canvas x"),
                optional("y", ParamType::Integer, "Its canvas y"),
                optional("width", ParamType::Integer, "Its width in canvas pixels"),
                optional("height", ParamType::Integer, "Its height in canvas pixels"),
            ],
            |args| ArrangementAction::Place {
                workspace: id_text(args, "workspace"),
                row: count(args, "row"),
                strip: count(args, "strip"),
                place: count(args, "place"),
                resource: text(args, "resource"),
                view: args.text("view").unwrap_or_default().to_owned(),
                x: args.integer("x").unwrap_or_default(),
                y: args.integer("y").unwrap_or_default(),
                width: args.integer("width").unwrap_or(DEFAULT_WIDTH),
                height: args.integer("height").unwrap_or(DEFAULT_HEIGHT),
            },
        ),
        action(
            "place-found",
            "Put what a text finds on the canvas",
            vec![
                required("text", ParamType::Text, "What to find and place"),
                optional("view", ParamType::Text, "The view it shows it through"),
                optional("x", ParamType::Integer, "Its canvas x"),
                optional("y", ParamType::Integer, "Its canvas y"),
            ],
            |args| ArrangementAction::PlaceFound {
                text: text(args, "text"),
                view: args.text("view").unwrap_or_default().to_owned(),
                x: args.integer("x").unwrap_or_default(),
                y: args.integer("y").unwrap_or_default(),
            },
        ),
    ]
}

/// An immediate effect action `id` the session answers itself: a step of the
/// arrangement's history.
#[expect(
    clippy::expect_used,
    reason = "the step action always serializes to JSON"
)]
fn step_action(id: &'static str, label: &str) -> ActionSpec {
    let mut spec = ActionSpec::typed_operation(
        id,
        label.to_owned(),
        String::new(),
        OperationKind::Effect,
        OPERATION,
        Vec::new(),
        |_| {
            EncodedAction::encode_operation(OPERATION, &Json::Null)
                .expect("a step action always serializes")
        },
    );
    spec.type_mut().contract = ActionContract {
        execution: ExecutionMode::Immediate,
        result: ResultSpec::None,
        ..ActionContract::default()
    };
    spec
}

/// An immediate effect action `id` that builds an [`ArrangementAction`].
#[expect(
    clippy::expect_used,
    reason = "an arranging action always serializes to JSON"
)]
fn action(
    id: &'static str,
    label: &str,
    params: Vec<ActionParam>,
    build: impl Fn(&ActionArgs) -> ArrangementAction + Send + Sync + 'static,
) -> ActionSpec {
    let mut spec = ActionSpec::typed_operation(
        id,
        label.to_owned(),
        String::new(),
        OperationKind::Effect,
        OPERATION,
        params,
        move |args| {
            EncodedAction::encode_operation(OPERATION, &build(args))
                .expect("an arranging action always serializes")
        },
    );
    spec.type_mut().contract = ActionContract {
        execution: ExecutionMode::Immediate,
        result: ResultSpec::None,
        ..ActionContract::default()
    };
    spec
}

fn required(name: &'static str, ty: ParamType, help: &str) -> ActionParam {
    ActionParam::required_owned(name, ty, help.to_owned())
}

fn optional(name: &'static str, ty: ParamType, help: &str) -> ActionParam {
    ActionParam::optional_owned(name, ty, help.to_owned())
}

fn count(args: &ActionArgs, name: &str) -> u64 {
    args.integer(name)
        .unwrap_or_default()
        .max(0)
        .cast_unsigned()
}

/// An id given as text: one no workspace has (`0`) when it is not a number,
/// which the action then refuses as naming no workspace.
fn id_text(args: &ActionArgs, name: &str) -> u64 {
    args.text(name)
        // ignored-failure: text that is not a number names no workspace, which the action refuses
        .and_then(|id| id.trim().parse().ok())
        .unwrap_or_default()
}

fn text(args: &ActionArgs, name: &str) -> String {
    args.text(name).unwrap_or_default().to_owned()
}

/// The session's side of the arrangement's published actions: the one door
/// the palette, a script and an agent reach them through, beside the
/// desktop's own.
pub struct ArrangementDoor {
    arrangement: std::rc::Rc<RefCell<Arrangement>>,
    /// How the session says whether the person is mid-gesture or typing, as
    /// the compositor last saw it.
    busy: std::rc::Rc<dyn Fn() -> Busy>,
    /// Who asks, for an action that names no one (see [`Self::named`]).
    actor: RefCell<String>,
    /// The system's `find`, as the palette and an agent's `tilo` tool run it:
    /// what text typed on the canvas places (shell.md §8).
    find: Find,
    /// The conversation's runs (shell.md §8.1), when this door has them: a
    /// line typed in a run's conversation is a program it runs as the
    /// person, or a sentence it takes in. `None` for a door that has none (a
    /// session with no runs of its own).
    conversation: Option<std::sync::Arc<crate::conversation::Conversation>>,
}

impl ArrangementDoor {
    /// The door over `arrangement`, whose changes wait while `busy` says the
    /// person is arranging or typing.
    #[must_use]
    pub fn new(
        arrangement: std::rc::Rc<RefCell<Arrangement>>,
        busy: std::rc::Rc<dyn Fn() -> Busy>,
        find: Find,
    ) -> Self {
        Self {
            arrangement,
            busy,
            actor: RefCell::new(String::new()),
            find,
            conversation: None,
        }
    }

    /// Answer the conversation's lines with `conversation` (shell.md §8.1).
    #[must_use]
    pub fn with_conversation(
        mut self,
        conversation: std::sync::Arc<crate::conversation::Conversation>,
    ) -> Self {
        self.conversation = Some(conversation);
        self
    }

    /// Arrange as `actor` from now on: what the person's own input does, so
    /// their drag, their resize and their close are attributed to them.
    pub fn arrange_as(&self, actor: &str) {
        actor.clone_into(&mut self.actor.borrow_mut());
    }
}

impl Published for ArrangementDoor {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        self.arrangement.borrow().resources()
    }

    fn revision(&self) -> Option<u64> {
        Some(self.arrangement.borrow().revision())
    }

    fn invoke(
        &mut self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, Json>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        // The arrangement publishes its record's undo and redo beside its
        // arranging actions: both step the record, never decode as an
        // arranging action.
        if address.resource == RECORD_RESOURCE
            || (address.resource == RESOURCE && matches!(address.action.as_str(), "undo" | "redo"))
        {
            return self.step(address);
        }
        // The conversation's one input (shell.md §8.1): a line is a program, run as the person
        // in the run's door session, or a sentence, taken in by the run. It reaches the runs
        // and the search the same way, so it answers here and never as an arranging action.
        if address.resource == RESOURCE && address.action == crate::conversation::ACTION {
            return self.converse(arguments);
        }
        let published = self.arrangement.borrow().resources();
        let built = tilo_actions::door::spec_of(&published, &address.resource, &address.action)
            .ok_or_else(|| tilo_actions::DoorError::Unknown(address.clone()))?;
        let encoded = built
            .build_json(arguments)
            .map_err(|error| tilo_actions::DoorError::InvalidArguments(error.to_string()))?;
        let mut action =
            ArrangementAction::decode(&encoded.type_id, &encoded.payload).ok_or_else(|| {
                tilo_actions::DoorError::UnregisteredType("tilo.shell.arranging".to_owned())
            })?;
        if address.action == "place" {
            self.place_at_surface(arguments, &mut action)?;
        }
        let mut asked = self.actor.borrow().clone();
        // An action that names no one is the person's own input: theirs to
        // take back, and never made to wait for themselves.
        if asked.is_empty() {
            asked = self.person();
        }
        // The person's own change is what their gesture is making; only
        // anyone else's waits for them to finish.
        let busy = if asked == self.person() {
            Busy::default()
        } else {
            (self.busy)()
        };
        // A place found by text is resolved here, where the search and what
        // is published now are: the record only ever holds what was placed.
        if let ArrangementAction::PlaceFound { text, view, x, y } = &action {
            let spot = (*x, *y);
            let find = Rc::clone(&self.find);
            let placed = self
                .arrangement
                .borrow_mut()
                .place_found(&asked, text, view, spot, &*find, busy);
            placed.map_err(|why| tilo_actions::DoorError::Unavailable {
                code: "nothing_to_place".to_owned(),
                reason: why,
            })?;
            return Ok(ui::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ));
        }
        self.arrangement
            .borrow_mut()
            .ask(&asked, action, busy)
            .map(drop)
            .map_err(tilo_actions::DoorError::Failed)?;
        Ok(ui::host_operation::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        ))
    }

    fn invoke_as(
        &mut self,
        actor: &str,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, Json>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        actor.clone_into(&mut self.actor.borrow_mut());
        self.invoke(address, arguments)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        Vec::new()
    }
}

impl ArrangementDoor {
    /// The conversation's one input (shell.md §8.1): a program, run as the person in the run's
    /// door session, or a sentence to the agent. A sentence with no run starts one, and its
    /// conversation stands where the person typed, as one more arranging step of theirs.
    fn converse(
        &self,
        arguments: &serde_json::Map<String, Json>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        use tilo_actions::DoorError;
        let Some(conversation) = self.conversation.clone() else {
            return Err(DoorError::Unavailable {
                code: "no-conversation".to_owned(),
                reason: "this arrangement holds no agent's conversation".into(),
            });
        };
        let param = |name: &'static str| {
            arguments
                .get(name)
                .and_then(Json::as_str)
                .map(str::to_owned)
        };
        let number = |name: &'static str| {
            arguments
                .get(name)
                .and_then(Json::as_i64)
                .unwrap_or_default()
        };
        let line = param(crate::conversation::LINE)
            .map(|line| line.trim().to_owned())
            .unwrap_or_default();
        let run = param(crate::conversation::RUN).unwrap_or_default();
        let at = (
            number(crate::conversation::X),
            number(crate::conversation::Y),
        );
        if line.is_empty() {
            return Err(DoorError::InvalidArguments(
                "a line typed in the conversation says something".into(),
            ));
        }
        let mut asked = self.actor.borrow().clone();
        if asked.is_empty() {
            asked = self.person();
        }
        if crate::conversation::is_program(&line, &run) {
            conversation
                .say(&asked, &run, &line)
                .map_err(DoorError::Failed)?;
            return Ok(ui::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ));
        }
        if !run.is_empty() {
            conversation.tell(&run, &line).map_err(DoorError::Failed)?;
            return Ok(ui::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ));
        }
        // No run yet: what the person typed is first a search, as the canvas's text always is
        // (shell.md §8, "Finding what to place"): the top result stands at the caret when the
        // search names something, and the text is a question to the agent when it does not.
        if self.place_found_beside(&asked, &line, at).is_ok() {
            return Ok(ui::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ));
        }
        // A question: it starts a run, and the run's conversation stands where the person
        // typed. Both are theirs: the ask is recorded as theirs, and the place is one step of
        // their arrangement, as any other place is.
        let started = conversation.ask(&line).map_err(DoorError::Failed)?;
        let place = self.place_run(&asked, &started, at);
        if let Err(why) = place {
            return Err(DoorError::Unavailable {
                code: "not_placed".to_owned(),
                reason: format!("the run started, but its conversation could not be placed: {why}"),
            });
        }
        Ok(ui::host_operation::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        ))
    }

    /// Put run `resource` (`agent-run:<id>`) in what the arrangement resolves a place's app and
    /// view from, as the runs service publishes it: the shell, through its conversation.
    fn remember_run(&self, resource: &str) {
        let Some(key) = resource.strip_prefix(tilo_agent_runs::wire::RUN_PREFIX) else {
            return;
        };
        let title = key.to_owned();
        self.arrangement.borrow().targets().borrow_mut().remember(
            resource,
            Target {
                app: tilo_agent_runs::SHELL_APP.to_owned(),
                views: vec![tilo_agent_runs::CONVERSATION_VIEW.to_owned()],
                title,
            },
        );
    }

    /// Place what the text `line` finds at `(x, y)` on the canvas, as `asked`'s: the canvas's
    /// one search (shell.md §8), the same `find` the palette runs.
    ///
    /// # Errors
    /// Nothing the text finds is a resource the arrangement places.
    fn place_found_beside(&self, asked: &str, line: &str, at: (i64, i64)) -> Result<(), String> {
        let find = Rc::clone(&self.find);
        let busy = if asked == self.person() {
            Busy::default()
        } else {
            (self.busy)()
        };
        self.arrangement
            .borrow_mut()
            .place_found(asked, line, "", at, &*find, busy)
            .map(drop)
    }

    /// Stand run `run`'s conversation at `(x, y)` on the canvas, as `asked`'s: one arranging
    /// step, as any other place is (shell.md §3, §8.1).
    ///
    /// # Errors
    /// The arrangement does not publish the run, or the place could not be made.
    fn place_run(&self, asked: &str, run: &str, at: (i64, i64)) -> Result<(), String> {
        let resource = crate::conversation::run_resource(run);
        // The run was just asked for, and the session publishes it on its door's tree the next
        // time the doors' offices are read. A place names the resource it shows as its app and
        // its view, so the run is put in that tree now, as the runs service publishes it: the
        // arrangement does not wait a pass to show a conversation the person just started.
        self.remember_run(&resource);
        let action = ArrangementAction::Place {
            workspace: 0,
            row: 0,
            strip: 0,
            place: 0,
            resource,
            view: String::new(),
            x: at.0,
            y: at.1,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
        };
        let (workspace, row, strip, place) = {
            let surface = self.arrangement.borrow().surface_at()?;
            (surface.workspace, surface.row, surface.strip, surface.place)
        };
        let action = match action {
            ArrangementAction::Place {
                resource,
                view,
                x,
                y,
                width,
                height,
                ..
            } => ArrangementAction::Place {
                workspace,
                row,
                strip,
                place,
                resource,
                view,
                x,
                y,
                width,
                height,
            },
            other => other,
        };
        self.arrangement
            .borrow_mut()
            .ask(asked, action, Busy::default())
            .map(drop)
    }

    /// A `place` that names no workspace, row, strip or id of its own stands
    /// on the surface in front, in a strip of its own, under ids minted here:
    /// what a reference activated in a view asks for (shell.md §3), which
    /// knows the point it wants and none of the desk's ids. `action`, decoded
    /// from `arguments`, takes the surface in front's workspace and row and
    /// fresh strip and place ids for whichever of them `arguments` leaves
    /// out. They go into the action itself, never back through the
    /// arguments: a desk's ids span all of `u64`, which an integer argument
    /// does not. The record holds the ids it was given, as for any other
    /// place.
    fn place_at_surface(
        &self,
        arguments: &serde_json::Map<String, Json>,
        action: &mut ArrangementAction,
    ) -> Result<(), tilo_actions::DoorError> {
        let ArrangementAction::Place {
            workspace,
            row,
            strip,
            place,
            ..
        } = action
        else {
            return Ok(());
        };
        let named = ["workspace", "row", "strip", "place"]
            .iter()
            .all(|id| arguments.contains_key(*id));
        if named {
            return Ok(());
        }
        // A row is a row of one workspace: the two come named together, or
        // both from the surface in front.
        if arguments.contains_key("workspace") != arguments.contains_key("row") {
            return Err(tilo_actions::DoorError::InvalidArguments(
                "a place names its workspace and its row together, or neither".to_owned(),
            ));
        }
        let surface = self.arrangement.borrow().surface_at().map_err(|why| {
            tilo_actions::DoorError::Unavailable {
                code: "nowhere_to_place".to_owned(),
                reason: why,
            }
        })?;
        if !arguments.contains_key("workspace") {
            *workspace = surface.workspace;
            *row = surface.row;
        }
        if !arguments.contains_key("strip") {
            *strip = surface.strip;
        }
        if !arguments.contains_key("place") {
            *place = surface.place;
        }
        Ok(())
    }

    /// Undo or redo the session's last change of the record.
    fn step(
        &self,
        address: &tilo_actions::ActionAddress,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        let person = self.person();
        let stepped = match address.action.as_str() {
            "undo" => self.arrangement.borrow_mut().undo(&person),
            "redo" => self.arrangement.borrow_mut().redo(&person),
            _ => return Err(tilo_actions::DoorError::Unknown(address.clone())),
        };
        if !stepped {
            return Err(tilo_actions::DoorError::Unavailable {
                code: "nothing_to_undo".to_owned(),
                reason: "the arrangement has no step left".to_owned(),
            });
        }
        Ok(ui::host_operation::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        ))
    }

    /// The person whose arrangement it is.
    fn person(&self) -> String {
        self.arrangement.borrow().person().to_owned()
    }
}

/// Whether two readings hold the same rows, strips and places: what the desk
/// shows, ignoring the shell stamps and orders.
fn same_places(one: &Strips, other: &Strips) -> bool {
    one.rows.len() == other.rows.len()
        && one
            .rows
            .iter()
            .zip(&other.rows)
            .all(|(a, b)| a.id == b.id && a.name == b.name)
        && one.strips.len() == other.strips.len()
        && one.strips.iter().zip(&other.strips).all(|(a, b)| {
            a.id == b.id
                && a.places.len() == b.places.len()
                && a.places
                    .iter()
                    .zip(&b.places)
                    .all(|(a, b)| a.id == b.id && a.record == b.record)
        })
}

/// Take in the rows and strips the desk made, renamed or removed, so the places the
/// same pass opened have a strip to be in. Whether anything changed.
fn adopt_structure(strips: &mut Strips, became: &Strips) -> bool {
    let was = strips.clone();
    for workspace in &became.workspaces {
        if !strips.workspaces.iter().any(|held| held.id == workspace.id) {
            strips.workspaces.push(workspace.clone());
        }
    }
    for row in &became.rows {
        match strips
            .rows
            .iter_mut()
            .find(|held| held.workspace == row.workspace && held.id == row.id)
        {
            // A row the person renamed on the desk is renamed in the record.
            Some(held) => held.name.clone_from(&row.name),
            None => strips.rows.push(row.clone()),
        }
    }
    // A row the desk no longer has goes, with its strips.
    strips.rows.retain(|row| {
        became
            .rows
            .iter()
            .any(|held| held.workspace == row.workspace && held.id == row.id)
    });
    for strip in &became.strips {
        if !strips.strips.iter().any(|held| held.id == strip.id) {
            strips.strips.push(strip.clone());
            continue;
        }
        let Some(held) = strips.strips.iter_mut().find(|held| held.id == strip.id) else {
            continue;
        };
        // The record each of its windows shows, as the window's app holds it
        // now: what a window shows is noted beside it, not arranged, so it is
        // the desk's own change like a row's new name.
        for place in &mut held.places {
            if let Some(shown) = strip.places.iter().find(|shown| shown.id == place.id)
                && !shown.record.is_empty()
            {
                place.record.clone_from(&shown.record);
            }
        }
    }
    let live: std::collections::BTreeSet<u64> =
        became.strips.iter().map(|strip| strip.id).collect();
    strips.strips.retain(|strip| live.contains(&strip.id));
    *strips != was
}

/// The arranging actions that take `was` to `became`: what the desk opened,
/// closed, moved and resized since the last pass.
fn actions_between(was: &Strips, became: &Strips) -> Vec<ArrangementAction> {
    let mut actions = Vec::new();
    let held: BTreeMap<TabId, (GroupId, StripId)> = was
        .strips
        .iter()
        .flat_map(|strip| {
            strip
                .places
                .iter()
                .map(move |place| (place.id, (strip.id, strip.row)))
        })
        .collect();
    for strip in &became.strips {
        for place in &strip.places {
            match held.get(&place.id) {
                // Opened on the desk: a window the person started.
                None => actions.push(ArrangementAction::OpenInto {
                    strip: strip.id,
                    after: None,
                    place: place.id,
                    shows: place.shows.clone(),
                    view: place.view.clone(),
                    record: place.record.clone(),
                    state: place.state.clone(),
                }),
                // Moved to another row by the person.
                Some((_, row)) if *row != strip.row => {
                    actions.push(ArrangementAction::Move {
                        place: place.id,
                        row: strip.row.0,
                        before: None,
                    });
                }
                Some(_) => {}
            }
        }
    }
    let now: std::collections::BTreeSet<TabId> = became.places().map(|place| place.id).collect();
    for place in was.places() {
        if !now.contains(&place.id) {
            actions.push(ArrangementAction::Close { place: place.id });
        }
    }
    // A width the person dragged, to where the desk has it.
    for strip in &became.strips {
        let changed = was
            .strip(strip.id)
            .is_some_and(|before| before.width != strip.width);
        if changed {
            actions.push(ArrangementAction::Resize {
                strip: strip.id,
                permille: strip.width.permille(),
            });
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::{found, most_specific};
    use tilo_desktop_search::{Hit, InvokeTarget, ResourceKind, Target};

    /// A row text typed on the canvas would stand for: what it opens, and what it is.
    fn opening(resource: &str, title: &str, kind: ResourceKind) -> Hit {
        Hit {
            row: None,
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind,
            provider: "test".into(),
            id: resource.into(),
            title: title.into(),
            subtitle: None,
            facts: Vec::new(),
            score: 0,
            icon: None,
            target: Target::Invoke(Box::new(InvokeTarget::open("test", "", resource))),
        }
    }

    #[test]
    fn the_text_places_the_top_result_of_the_hits_a_place_can_show() {
        let hits = vec![
            opening(
                "settings.section:agent_models",
                "Chat models",
                ResourceKind::Record,
            ),
            opening("tl:so.tilo.lang.chat", "Chat", ResourceKind::Application),
        ];
        let showable = |resource: &str| resource == "tl:so.tilo.lang.chat";
        assert_eq!(
            found("Chat", &|_| hits.clone(), showable).as_deref(),
            Some("tl:so.tilo.lang.chat"),
            "a hit the arrangement cannot place does not stand in the way of one it can"
        );
        assert_eq!(
            found("Chat", &|_| hits.clone(), |_| false),
            None,
            "and with nothing placeable, nothing is placed"
        );
    }

    #[test]
    fn the_text_places_what_it_names_inside_the_resource_it_sits_in() {
        let named = [
            ("notes", "Groceries"),
            ("notes/i1", "Groceries"),
            ("notes/i2", "Groceries"),
        ];
        assert_eq!(
            most_specific(&named, "Groceries").as_deref(),
            Some("notes/i1"),
            "a note titled like its notebook is the note, and the top of them"
        );
        assert_eq!(
            most_specific(&named, "my notes").as_deref(),
            Some("notes"),
            "a resource the text only describes stays itself"
        );
        assert_eq!(
            most_specific(&[("inbox", "Inbox")], "inbox").as_deref(),
            Some("inbox"),
            "and the top stays when nothing inside it carries the same name"
        );
    }
}
