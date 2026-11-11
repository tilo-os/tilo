//! The person's arrangement as the desk they share with the workspace: what is
//! open where, read off the desk itself, kept in the desk's own record
//! ([`tilo_desktop_model::shared_record`]) with history and undo, so every member
//! of the workspace reads the same one. A place's view and its pin are the
//! window's own ([`tilo_desktop_model::Tab`]).
//! Each arranging action ([`ArrangementAction`]) is one change of the desk,
//! attributed to whoever asked for it and undone as that one step; what the desk
//! changes on its own is a change like any other.
//! One rule is fixed (shell.md §1): nothing moves or takes focus under an ongoing
//! gesture or while the person types. An action that arrives then waits here and
//! runs when they end; it is never dropped.
//! Both hosts run this: the native session and the page the browser shows.
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tilo_actions::remote::session_actions;
use tilo_actions::{
    ActionArgs, ActionContract, ActionParam, ActionSpec, EncodedAction, ExecutionMode,
    OperationKind, ParamType, ResourceSpec, ResultSpec,
};
use tilo_desktop_model::{
    ArrangementKind, DesktopTree, FloatCorner, FloatPlace, GroupId, Strip, StripId, Tab, TabId,
    TileContent, WindowGroup, WindowWidth, WorkspaceSlot,
};
use tilo_desktop_surface::DesktopHandle;

use crate::desk::Desk;

/// The size a place made from a resource takes on the canvas when the caller names none: the framed
/// canvas's window, in canvas pixels.
const DEFAULT_WIDTH: i64 = 720;
/// See [`DEFAULT_WIDTH`].
const DEFAULT_HEIGHT: i64 = 520;

/// Where a place floats when the action names nothing else: the work area's bottom-right corner, a
/// window's inset from its edges, at a floating size smaller than a canvas window.
const FLOAT_INSET: i64 = 24;
const FLOAT_WIDTH: u32 = 640;
const FLOAT_HEIGHT: u32 = 480;

/// The operation type every arranging action carries.
pub const OPERATION: &str = "tilo.shell.arranging";
/// The resource the person's arrangement is published as. Its id is the one every door that names
/// the system's `open` names ([`session_actions::ARRANGEMENT_RESOURCE`]): the broker rewrites a
/// served app's `open` into the arrangement's own, so the two are one address.
pub const RESOURCE: &str = session_actions::ARRANGEMENT_RESOURCE;
/// The resource its history is published as: its undo and redo.
pub const RECORD_RESOURCE: &str = "shell:arrangement-record";
/// The kind of a place's own resource.
pub const PLACE_KIND: &str = "arrangement.place";
/// The kind of a strip's own resource.
pub const STRIP_KIND: &str = "arrangement.strip";
/// The kind of a row's own resource.
pub const ROW_KIND: &str = "arrangement.row";

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

/// A workspace the person's desk shows, as their arrangement names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangementWorkspaceReading {
    pub key: String,
    pub name: String,
    /// Whether it is the workspace in front: a desk holds several and shows one, so a shell draws
    /// the rows of this one and no other.
    pub current: bool,
}

/// A row of the desk, as the person's arrangement names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangementRowReading {
    pub key: String,
    pub workspace: String,
    pub name: String,
    pub shell: String,
}

/// A strip of the desk, as the person's arrangement names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangementStripReading {
    pub key: String,
    pub workspace: String,
    pub row: String,
    /// Its width in per-mille of its row's usable width.
    pub width: i64,
    pub maximized: bool,
    pub active: String,
    pub placed: bool,
    pub canvas_x: i64,
    pub canvas_y: i64,
    pub canvas_width: i64,
    pub canvas_height: i64,
    /// Whether the strip's window floats above the workspace's windows,
    /// rather than standing among them in its row.
    #[serde(default)]
    pub floating: bool,
    /// The corner of the work area a floating window keeps its distance from:
    /// `top-left`, `top-right`, `bottom-right` or `bottom-left`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub float_corner: String,
    /// Its distance from that corner's side edge, in canvas pixels.
    #[serde(default)]
    pub float_dx: i64,
    /// Its distance from that corner's top or bottom edge, in canvas pixels.
    #[serde(default)]
    pub float_dy: i64,
    /// Its floating width and height in canvas pixels.
    #[serde(default)]
    pub float_width: i64,
    #[serde(default)]
    pub float_height: i64,
    pub shell: String,
}

/// A place of the desk, as the person's arrangement names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangementPlaceReading {
    pub key: String,
    pub strip: String,
    pub shows: String,
    pub shown_as: String,
    pub record: String,
    /// The title the place recorded when it was made, kept for the record's own bookkeeping:
    /// what a surface shows a window by is [`Self::name`].
    pub title: String,
    /// What every surface calls this place: its window's title and the app's own name
    /// (`Groceries — Notes`), or the app's name alone when the window has no title. Never an
    /// app id.
    pub name: String,
    pub pinned: bool,
    pub shell: String,
}

/// The person's arrangement as a shell's views read it, by key: every workspace, its rows, every
/// window group and every window, each as the desk has it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangementReading {
    pub workspaces: Vec<ArrangementWorkspaceReading>,
    pub rows: Vec<ArrangementRowReading>,
    pub strips: Vec<ArrangementStripReading>,
    pub places: Vec<ArrangementPlaceReading>,
}

/// The person's arrangement as a shell's views read it: what [`Arrangement::reading`] answers and
/// [`Arrangement::resources`] publishes a resource of.
pub type Reading = ArrangementReading;
/// A workspace of [`Reading`].
pub type WorkspaceReading = ArrangementWorkspaceReading;
/// A row of [`Reading`].
pub type RowReading = ArrangementRowReading;
/// A strip of [`Reading`].
pub type StripReading = ArrangementStripReading;
/// A place of [`Reading`].
pub type PlaceReading = ArrangementPlaceReading;

/// An arranging action: one change of the person's arrangement.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "do", rename_all = "kebab-case")]
pub enum ArrangementAction {
    /// Retired: a window of an app stood in a strip, before a place was made from the resource it
    /// shows ([`Self::Place`]). Kept in place because an action already written names a variant by
    /// its position; one that sends it is refused.
    RetiredOpenInto {
        strip: u64,
        #[serde(default)]
        after: Option<u64>,
        place: u64,
        shows: String,
        #[serde(default)]
        view: String,
        #[serde(default)]
        record: String,
        /// The window's state, as the desktop model keeps it.
        #[serde(default)]
        state: serde_json::Value,
    },
    /// Move the place `place` into the row `row` , before the place `before` or last. A `strip`
    /// names a strip it becomes a tab of, in front, instead of the place's own strip: how a person,
    /// an agent and the REPL open something beside what they are in, and how a drag from a tab or a
    /// window bar lands on another strip (shell.md §2). A strip already the place's own is nothing
    /// to move.
    Move {
        place: u64,
        row: u64,
        #[serde(default)]
        before: Option<u64>,
        /// The strip it becomes a tab of; its own when left out.
        #[serde(default)]
        strip: Option<u64>,
    },
    /// Resize the strip `strip` to `permille` of its row's width.
    Resize { strip: u64, permille: u32 },
    /// Maximize the strip `strip` (`maximized`), so it stands alone as the window the desk is,
    /// or restore it beside its row's other windows.
    Maximize { strip: u64, maximized: bool },
    /// Close the place `place` ; its window goes with it.
    Close { place: u64 },
    /// Bring the place `place` in front of the strip it stands in, as choosing its tab does.
    Activate { place: u64 },
    /// Pin the place `place` (`pinned`), so an agent's arrangement leaves it alone (shell.md §5),
    /// or unpin it.
    Pin { place: u64, pinned: bool },
    /// Stand the place `place` at `(x, y)` canvas pixels, keeping every other place where it is:
    /// what a drag that let a place go on the canvas runs (`canvas.md` §6, "moving a window is one
    /// arranging action", shell.md §2 `move`). The place's strip takes the point; nothing else
    /// changes. Appended last: existing encodings are unchanged.
    Position { place: u64, x: i64, y: i64 },
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
        /// Float the place above the workspace's windows at `float` , instead of standing it on the
        /// canvas: how a conversation popped out of its app opens, its draft and scroll kept in the
        /// window it already was. Left out, the place stands on the canvas as it always did.
        #[serde(default)]
        float: Option<FloatPlace>,
    },
    /// Text typed on the canvas: the top result of the system's `find` stands there as its primary
    /// view (or `view` ). The session resolves it into a [`Place`] when the action is invoked, so
    /// the record only ever holds what was placed, never the words it was found by. A `strip` names
    /// the strip the found resource joins as a tab, in front, instead of a strip of its own: how a
    /// person opens a found thing beside what they are in (shell.md §2, §3), as pressing a
    /// reference does.
    PlaceFound {
        text: String,
        #[serde(default)]
        view: String,
        #[serde(default)]
        strip: Option<u64>,
        #[serde(default)]
        x: i64,
        #[serde(default)]
        y: i64,
    },
    /// Float the place `place` above its workspace's windows at `float`:
    /// every member sees it there, and the strip it stands in is untouched,
    /// so snapping it back puts it exactly where it was. Appended last: an
    /// action already written names a variant by its own name, and none of
    /// them changed.
    Float { place: u64, float: FloatPlace },
    /// Snap the floating place `place` back among its workspace's windows.
    /// A `row` names the row it goes into, before the place `before` or last,
    /// as a drag into a gap does; left out, it returns to its own strip.
    /// Appended last.
    Snap {
        place: u64,
        #[serde(default)]
        row: Option<u64>,
        #[serde(default)]
        before: Option<u64>,
    },
    /// The system's `open` (`docs/designs/app-language/one-app-model.md` §3): the resource
    /// `resource` placed with its view `view` (empty for its primary one) beside the place showing
    /// `beside` — the conversation of the turn that asked, by its address — or beside the place in
    /// front when it names none. A resource already standing is brought forward instead. The door
    /// resolves it into the `Place` (or `Activate`) it makes before the step is recorded, so the
    /// record holds ids. Appended last.
    Open {
        resource: String,
        #[serde(default)]
        view: String,
        #[serde(default)]
        beside: String,
    },
}

/// Where a place made on the desk now goes: the workspace in front, its row, and the ids the
/// place's own strip and the place take.
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

/// What the canvas does with text that is a question, or that nothing matches (shell.md §8.1,
/// "Asking from the canvas"): the session posts it as the person into their ask conversation with
/// the workspace's agent, and answers the resource that conversation is, so the text typed stands
/// as a place of it. `None` when the session cannot ask — no agent to ask, no workspace to ask in —
/// and the text then fails to place as it always did.
pub type Ask = Rc<dyn Fn(&str) -> Option<String>>;

/// What a text typed on the canvas found, and where it stands: the inputs of
/// [`Arrangement::place_found`] besides the arrangement itself.
#[derive(Clone, Copy)]
pub struct Found<'a> {
    /// The person the place is made as.
    pub asked: &'a str,
    /// The text they typed.
    pub text: &'a str,
    /// The resource's view, empty for its primary one.
    pub view: &'a str,
    /// The strip the place joins as a tab, else one of its own.
    pub strip: Option<u64>,
    /// Where on the canvas it stands.
    pub at: (i64, i64),
    /// The system's search, [`Find`].
    pub find: &'a dyn Fn(&str) -> Vec<tilo_desktop_search::Hit>,
    /// The ask for text that asks, [`Ask`].
    pub ask: &'a dyn Fn(&str) -> Option<String>,
    /// Whether anyone else's change is still in flight.
    pub busy: Busy,
}

/// The resource `hit` names, and the name its resource goes by, when a place can make something of
/// it: an app's own `open` of a resource, or an action that acts on one. Every other row (a
/// settings section, a file, a window's focus, a command) names nothing a place stands.
fn placeable(hit: &tilo_desktop_search::Hit) -> Option<(&str, &str)> {
    match &hit.target {
        tilo_desktop_search::Target::Invoke(invoke)
            if invoke.address.action == tilo_desktop_search::OPEN_ACTION =>
        {
            let label = invoke
                .descriptor
                .as_ref()
                .map_or("", |descriptor| descriptor.resource_label.as_str());
            Some((invoke.address.resource.as_str(), label))
        }
        tilo_desktop_search::Target::Action(action) => {
            Some((action.resource.as_str(), action.resource_label.as_str()))
        }
        _ => None,
    }
}

/// Which of `hits` a text typed on the canvas places: the top one that names a resource `shows`
/// accepts, or the one inside it that carries the same name. This is the one ranking a typed text
/// and an agent's `find` share, so `find` never leads with another row about the same resource. A
/// hit the arrangement cannot place does not stand in the way of one it can.
#[must_use]
pub fn placed(
    hits: &[tilo_desktop_search::Hit],
    text: &str,
    shows: impl Fn(&str) -> bool,
) -> Option<usize> {
    let named: Vec<(usize, &str, &str)> = hits
        .iter()
        .enumerate()
        .filter_map(|(at, hit)| placeable(hit).map(|(resource, label)| (at, resource, label)))
        .filter(|(_, resource, _)| shows(resource))
        .collect();
    most_specific(&named, text)
}

/// The first resource `text` finds that a place can show, as the arrangement places it: the
/// resource of the hit [`placed`] chooses, else `None` for a query no result answers with something
/// placeable.
#[must_use]
pub fn found(
    text: &str,
    find: &dyn Fn(&str) -> Vec<tilo_desktop_search::Hit>,
    shows: impl Fn(&str) -> bool,
) -> Option<String> {
    let hits = find(text);
    let at = placed(&hits, text, shows)?;
    let (resource, _) = placeable(hits.get(at)?)?;
    Some(resource.to_owned())
}

/// The app a window shows, by its id, when its tab holds a window at all.
pub fn tab_app(tile: &tilo_desktop_model::TileContent) -> Option<String> {
    use tilo_desktop_model::TileContent;
    match tile {
        TileContent::Foreign { app_id, .. }
        | TileContent::LocalApp { app_id, .. }
        | TileContent::PeerWindow { app_id, .. }
        | TileContent::HostedWindow { app_id, .. } => Some(app_id.clone()),
        _ => None,
    }
}

/// The hit `text` names among the placeable ones, each with its position in the search's order and
/// the name its resource goes by: the top of them, or the one inside it that carries the same name
/// when the text is that name — a note titled like the notebook it sits in is the note, while a
/// resource the text only describes stays itself.
fn most_specific(named: &[(usize, &str, &str)], text: &str) -> Option<usize> {
    let (at, resource, label) = named.first()?;
    if !text.trim().eq_ignore_ascii_case(label) {
        return Some(*at);
    }
    let deeper = named.iter().find(|(_, other, other_label)| {
        other != resource && other_label == label && inside(other, resource)
    });
    Some(deeper.map_or(*at, |(at, _, _)| *at))
}

/// Whether `resource` is a level under `other` (`inbox/m1` is under `inbox` ).
fn inside(resource: &str, other: &str) -> bool {
    resource != other
        && resource
            .strip_prefix(other)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The root a resource of an app belongs to, when it is one the app publishes a root for:
/// `root-<type>` after the app's own id, so `tl:so.tilo.lang.mail:root-su4h67i5/ie99c2c19-72`
/// has the root `tl:so.tilo.lang.mail:root-su4h67i5`. `None` for a resource with no app id or no
/// `root-` segment, and for a root itself.
fn root_of(resource: &str) -> Option<&str> {
    let rest = resource.strip_prefix("tl:")?;
    let (app, doc) = rest.split_once(':')?;
    if app.is_empty() {
        return None;
    }
    // The root is the `root-<type>` segment at the front of the document's path: everything from
    // the app's id through that segment, and no further. A split of the resource at the segment's
    // end names it whole, so no length is added up.
    let head = doc.split('/').next()?;
    if !head.starts_with("root-") {
        return None;
    }
    let (prefix, _) = resource.split_once(head)?;
    let end = prefix.len().checked_add(head.len())?;
    resource.get(..end)
}

/// A resource as the arrangement makes a place of it: the app publishing it, its views (the primary
/// first when it declares one), and what it is called. What `place` resolves a resource id against,
/// from what the session publishes now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Target {
    /// The app that publishes the resource, by its canonical id.
    pub app: String,
    /// The resource's published views (`ResourceSpec::views`), primary first.
    pub views: Vec<String>,
    /// The resource's label, as the place is shown under.
    pub title: String,
    /// The resource's kind, as its app publishes it (`tube.video`,
    /// `tl.so.tilo.lang.tasks.Tasks`): what a reference to it names.
    pub kind: String,
}

impl Target {
    /// The view a place shows the resource through: `view` when the resource publishes it, else its
    /// primary view, else its first, else empty for the app's own default.
    #[must_use]
    pub fn chosen_view(&self, view: &str) -> String {
        let view = view.trim();
        if !view.is_empty() && self.views.iter().any(|held| held == view) {
            return view.to_owned();
        }
        self.views.first().cloned().unwrap_or_default()
    }
}

/// The resources the session publishes now, by id: what a place made from a resource takes its app
/// and view from. The session hands it the door's tree each time it changes, and never reads it
/// inside an invocation.
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

    /// Put `target` under `id` , as the resource is published: a place made from a resource the
    /// session has just published but the door's tree does not hold yet.
    pub fn remember(&mut self, id: &str, target: Target) {
        self.by_id.insert(id.to_owned(), target);
    }

    /// Remember `resource` as its app publishes it: the app, the views it publishes, and what it
    /// is called — the same target the door's tree yields for it, so a place made from a resource
    /// the session has just published shows it through its app's own view, never as the app's
    /// window.
    pub fn remember_resource(&mut self, resource: &ResourceSpec) {
        collect_targets(resource, &mut self.by_id);
    }
}

/// Every resource of `resource` and its children, by id, with the views it publishes primary first.
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
                kind: resource.kind.clone(),
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

/// What the person is in the middle of, as shell.md §1's one fixed rule reads it: nothing moves
/// under a gesture or while they type. The host says what it knows: the session from its
/// compositor, the page from what it can see of its own input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Busy {
    /// A pointer button is down: a drag or a resize is in flight.
    pub gesture: bool,
    /// The person typed within the typing pause.
    pub typing: bool,
}

impl Busy {
    /// Whether an arrangement must wait.
    #[must_use]
    pub const fn holds(self) -> bool {
        self.gesture || self.typing
    }
}

/// What a window showing `app_id` and calling itself `title` is called to the person
/// ("Groceries — Notes"), as the host names it: the session's app lists and desktop entries on a
/// device, the registry's names on the page.
pub type Names = Rc<dyn Fn(&str, &str) -> String>;

/// The person's arrangement, and the changes waiting for the person to finish what they are doing.
pub struct Arrangement {
    /// The person whose record it is.
    person: String,
    /// The desk itself: what the arrangement reads, and whose change every arranging action is.
    handle: DesktopHandle,
    /// The document that keeps the desk: the record the workspace shares,
    /// whose own changes are its history.
    desk: Rc<Desk>,
    /// What the host calls a window, by [`Names`].
    names: Names,
    /// The resources the session publishes now, by id: what a place made from a resource reads its
    /// app and views from ([`Catalog`]).
    catalog: Rc<RefCell<Catalog>>,
    /// Actions that arrived under a gesture or while the person typed: they run in order once it
    /// ends.
    waiting: RefCell<VecDeque<(String, ArrangementAction)>>,
}

impl Arrangement {
    /// `person` 's arrangement of `handle` 's desk, whose changes `desk` keeps, naming windows
    /// through `names`. A host brings over whatever an older build kept of the arrangement before
    /// it makes one: what it holds is the desk.
    #[must_use]
    pub fn new(person: &str, handle: DesktopHandle, desk: Rc<Desk>, names: Names) -> Self {
        Self {
            person: person.to_owned(),
            handle,
            desk,
            names,
            catalog: Rc::new(RefCell::new(Catalog::default())),
            waiting: RefCell::new(VecDeque::new()),
        }
    }

    /// The resources the session publishes now, by id: what a place made from a resource reads its
    /// app and views from. The session keeps it in step with the door's tree.
    #[must_use]
    pub fn targets(&self) -> Rc<RefCell<Catalog>> {
        Rc::clone(&self.catalog)
    }

    /// The surface the desk shows at `(x, y)` as a place made there stands: the
    /// workspace in front, its row, and the ids its strip and place take. Every id is
    /// this device's. A workspace with no row yet — one just joined, whose first window
    /// makes its row — is given a row here, so a person who joined a workspace can
    /// place something in it.
    /// # Errors
    /// The desk has no workspace in front.
    pub fn surface_at(&self) -> Result<Surface, String> {
        self.surface_at_strip(None)
    }

    /// The surface a place made in `strip` stands on: the strip's own workspace and row
    /// when it names one on the desk, so the place joins that strip as a tab; the
    /// workspace and row in front otherwise. The place's id is minted here.
    /// # Errors
    /// The desk has no workspace in front, or `strip` is not a strip on this desk.
    pub fn surface_at_strip(&self, strip: Option<u64>) -> Result<Surface, String> {
        self.surface_on(None, strip)
    }

    /// The surface a place made in `workspace` (a slot) in `strip` stands on: as
    /// [`Self::surface_at_strip`], for a place the session makes in a workspace of its own
    /// choosing rather than the one in front. `workspace` `None` is the workspace in front.
    ///
    /// # Errors
    /// The desk has no such workspace, no workspace in front, or `strip` is not a strip on
    /// this desk.
    fn surface_on(&self, workspace: Option<u64>, strip: Option<u64>) -> Result<Surface, String> {
        let tree = self.handle.read(Clone::clone);
        if let Some(strip) = strip {
            let held = tree.workspaces.iter().find_map(|workspace| {
                workspace.strips.iter().find_map(|row| {
                    row.window_groups
                        .iter()
                        .any(|group| group.id == strip)
                        .then_some((workspace.slot.0, row.id.0))
                })
            });
            let Some((workspace, row)) = held else {
                return Err("that strip is not in the arrangement".into());
            };
            return Ok(Surface {
                workspace,
                row,
                strip,
                place: self.handle.mint_id(),
            });
        }
        let (workspace, row) = {
            let workspace = match workspace {
                Some(slot) => tree
                    .workspaces
                    .iter()
                    .find(|held| held.slot.0 == slot)
                    .ok_or("that workspace is not on this desk")?,
                None => tree
                    .active_workspace()
                    .ok_or("the desk has no workspace in front")?,
            };
            let row = tree
                .active_strip
                .get(&workspace.slot)
                .copied()
                .or_else(|| workspace.strips.first().map(|row| row.id));
            (workspace.slot, row)
        };
        drop(tree);
        // A workspace that holds no row yet — one just joined, whose first window is what makes the
        // row it goes in — is given one here: a place put on the canvas is that window, and
        // refusing it would leave a person who joined a workspace unable to place anything in it.
        let row = match row {
            Some(row) => row,
            None => self.make_row(workspace)?,
        };
        let strip = self.handle.mint_id();
        let place = self.handle.mint_id();
        Ok(Surface {
            workspace: workspace.0,
            row: row.0,
            strip,
            place,
        })
    }

    /// A row for the workspace `workspace` that holds none: the desk's own first row of it, named
    /// `Main` , which its first window stands in — as a workspace made here starts. Made before the
    /// place, so the record the place is put in is one the desk shows.
    fn make_row(&self, workspace: WorkspaceSlot) -> Result<StripId, String> {
        let id = StripId(self.handle.mint_id());
        let mut tree = self.handle.read(Clone::clone);
        let Some(held) = tree
            .workspaces
            .iter_mut()
            .find(|held| held.slot == workspace)
        else {
            return Err("the workspace in front went while a place was made".to_owned());
        };
        if let Some(row) = held.strips.first() {
            // Another part of the desk made it first: its row stands.
            return Ok(row.id);
        }
        held.strips.push(Strip::new(id, "Main"));
        tree.active_strip.insert(workspace, id);
        self.handle.replace_tree(tree);
        Ok(id)
    }

    /// Place the resource a text typed at `at` finds on the canvas, as `asked`'s:
    /// the top result of the system's `find` (`find`) that a place can show,
    /// resolved against what the session publishes now ([`Catalog`]), standing at
    /// `at` through the arranging action [`ArrangementAction::Place`] — one
    /// attributed, undoable change of the person's record (shell.md §8). `view`
    /// empty shows the resource's primary view. `strip` names the strip the
    /// found resource joins as a tab (a strip id on the desk), else it takes a
    /// strip of its own.
    ///
    /// Returns the place's id, or `None` when nothing the text finds is a
    /// resource the arrangement can place.
    ///
    /// Text that *asks* — a question ([`tilo_desktop_search::answers::question::asks`]),
    /// or text nothing matches — goes to `ask` instead (shell.md §8.1,
    /// "Asking from the canvas"): what it answers is the resource to place,
    /// the person's ask conversation, so the text typed stands as a place of
    /// that conversation, where they typed it, as one arranging step.
    ///
    /// # Errors
    /// Why the place could not be made, or the text found nothing to place
    /// and nothing answered it.
    pub fn place_found(&mut self, inputs: Found<'_>) -> Result<Option<u64>, String> {
        let Found {
            asked,
            text,
            view,
            strip,
            at,
            find,
            ask,
            busy,
        } = inputs;
        let text = text.trim();
        if text.is_empty() {
            return Err("nothing is typed to place".into());
        }
        let found_resource = found(text, find, |resource| {
            self.catalog.borrow().get(resource).is_some()
        });
        // A question is answered even when it happens to match something the search holds: the
        // person asked, so the text they typed is a message to their agent, not a thing to stand
        // (shell.md §8.1).
        let asked_agent =
            tilo_desktop_search::answers::question::asks(text) || found_resource.is_none();
        let resource = if asked_agent {
            ask(text)
        } else {
            found_resource
        };
        let Some(resource) = resource else {
            return Err(format!("nothing matches {text:?}"));
        };
        // A text that asks brings the conversation it is with forward rather than standing a second
        // place of it: the same conversation belongs in one place (shell.md §8.1). Text that finds
        // a resource places it where the person typed, as it always has — a second place of a note
        // is the person's to make.
        if asked_agent && let Some(held) = self.place_of(&resource) {
            // The ask succeeded: its message is posted whether or not the conversation stood in
            // front already, so bringing it forward tolerates that (`bring_forward`).
            self.bring_forward(asked, held, busy)?;
            return Ok(Some(held));
        }
        let surface = self.surface_at_strip(strip)?;
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
            float: None,
        };
        self.ask(asked, action, busy)?;
        Ok(Some(surface.place))
    }

    /// The place already showing `resource` , if one stands: what text typed again brings forward
    /// rather than placing a second time.
    #[must_use]
    pub fn place_of(&self, resource: &str) -> Option<u64> {
        let records = self.records();
        self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| workspace.strips.iter())
                .flat_map(|row| row.window_groups.iter())
                .flat_map(|group| group.tabs.iter())
                .find(|tab| records.get(&tab.id) == Some(&resource.to_owned()))
                .map(|tab| tab.id)
        })
    }

    /// The place already showing `resource` or anything of its root, if one stands: what opening a
    /// mailbox, a conversation, a notebook or one of its entries brings forward rather than standing
    /// a second place of the same app's record (`shell.md` §3, "Activating something already shown
    /// goes to it"). Opening the root and then one view inside it is one thing open, not two.
    #[must_use]
    pub fn place_of_root(&self, resource: &str) -> Option<u64> {
        let records = self.records();
        let root = root_of(resource);
        self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| workspace.strips.iter())
                .flat_map(|row| row.window_groups.iter())
                .flat_map(|group| group.tabs.iter())
                .find(|tab| {
                    records.get(&tab.id).is_some_and(|held| {
                        held == resource || (root.is_some() && root_of(held) == root)
                    })
                })
                .map(|tab| tab.id)
        })
    }

    /// Show the resource `resource` as the app the session itself runs shows it
    /// (`docs/designs/app-language/one-app-model.md` §3): a place of the person's arrangement in
    /// `workspace`, drawn by the shell from what the session publishes, exactly as a place made by
    /// `place` is. There is no window process behind it — the place's frame, bar and tabs are the
    /// shell's.
    ///
    /// A workspace that already holds a place of `resource` brings that place forward rather than
    /// standing a second one: opening an app the person already has open raises what shows it.
    ///
    /// `resource` and `view` are placed as `place` places them: it must be one what the session
    /// publishes now ([`Catalog`]), and `view` empty shows its primary view.
    ///
    /// Returns the place's id, or `None` when the workspace already held one.
    ///
    /// # Errors
    /// The workspace is not on this desk, or `resource` names nothing published.
    pub fn place_root(
        &mut self,
        asked: &str,
        workspace: u64,
        resource: &str,
        view: &str,
    ) -> Result<Option<u64>, String> {
        if let Some(held) = self.place_in(workspace, resource) {
            self.bring_forward(asked, held, Busy::default())?;
            return Ok(None);
        }
        let surface = self.surface_on(Some(workspace), None)?;
        let action = ArrangementAction::Place {
            workspace: surface.workspace,
            row: surface.row,
            strip: surface.strip,
            place: surface.place,
            resource: resource.to_owned(),
            view: view.to_owned(),
            x: 0,
            y: 0,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            float: None,
        };
        self.ask(asked, action, Busy::default())?;
        Ok(Some(surface.place))
    }

    /// Bring the place `place` forward as `asked`'s step, for a caller that is raising what
    /// already stands rather than making it: a place already in front is where it should be, so
    /// the activation that would refuse ("that place is in front already") is no failure — a
    /// text that asks posts its message and then brings the conversation forward, and the ask
    /// has succeeded whether or not that place was in front.
    ///
    /// # Errors
    /// Why the place could not be activated at all.
    fn bring_forward(&mut self, asked: &str, place: u64, busy: Busy) -> Result<(), String> {
        match self.ask(asked, ArrangementAction::Activate { place }, busy) {
            Ok(_) => Ok(()),
            Err(why) if why == "that place is in front already" => Ok(()),
            Err(why) => Err(why),
        }
    }

    /// The system's `open` (`docs/designs/app-language/one-app-model.md` §3): stand `resource`
    /// with its view `view` (empty for its primary one) beside the place that shows `beside`'s
    /// resource — the conversation of the turn that asked — or beside the place in front when
    /// `beside` names nothing. A resource already standing on the desk is brought forward instead,
    /// so opening what is shown raises it rather than standing a second place.
    ///
    /// Returns the place's id, or `None` when the desk already held one and brought it forward.
    ///
    /// # Errors
    /// The desk has no workspace in front, or `resource` names nothing published.
    pub(crate) fn open_place(
        &mut self,
        asked: &str,
        resource: &str,
        view: &str,
        beside: &str,
    ) -> Result<Option<u64>, String> {
        if let Some(held) = self.place_of_root(resource) {
            // Opening what is open, or a view of what is open, brings that place forward; one
            // already in front is where it should be, and asking to activate it again would refuse
            // ("that place is in front already").
            self.bring_forward(asked, held, Busy::default())?;
            return Ok(None);
        }
        let anchor = if beside.trim().is_empty() {
            None
        } else {
            self.place_of(beside)
        };
        let surface = self.surface_at()?;
        // Where the new place stands: the free space next to the anchor (`free_place_next_to`),
        // its trailing side first, then below it, then further down the two columns the anchor
        // starts. The point handed to the place action is free already, so the action's own
        // `free_place_near` keeps it as it is. With no anchor the place comes to the canvas'
        // origin, as a place made by `place` without a point does. Opening several things beside
        // one anchor therefore runs right, then down, never over the anchor or over each other,
        // and never above or to the left of it (`shell.md` §3, "a place next to where it came
        // from").
        let (width, height) = (clamp_side(DEFAULT_WIDTH), clamp_side(DEFAULT_HEIGHT));
        let at = anchor.and_then(|place| {
            self.handle.read(|tree| {
                let (workspace, row, group, _) = locate(tree, place)?;
                let held = tree.workspaces.get(workspace)?;
                let id = held.strips.get(row)?.window_groups.get(group)?.id;
                let subject = held.arrangement.place_of(id)?;
                let free = held
                    .arrangement
                    .free_place_next_to(subject, width, height, None);
                Some((i64::from(free.x), i64::from(free.y)))
            })
        });
        let (x, y) = at.map_or((0, 0), |at| at);
        let action = ArrangementAction::Place {
            workspace: surface.workspace,
            row: surface.row,
            strip: surface.strip,
            place: surface.place,
            resource: resource.to_owned(),
            view: view.to_owned(),
            x,
            y,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            float: None,
        };
        self.ask(asked, action, Busy::default())?;
        Ok(Some(surface.place))
    }

    /// The place of `workspace` (a workspace's slot) that already shows `resource`, if one
    /// stands: what opening an app the workspace already shows brings forward.
    fn place_in(&self, workspace: u64, resource: &str) -> Option<u64> {
        let records = self.records();
        self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .filter(|held| held.slot.0 == workspace)
                .flat_map(|held| held.strips.iter())
                .flat_map(|row| row.window_groups.iter())
                .flat_map(|group| group.tabs.iter())
                .find(|tab| records.get(&tab.id) == Some(&resource.to_owned()))
                .map(|tab| tab.id)
        })
    }

    /// The arrangement as a shell's views read it: every workspace, row, strip and place, each by
    /// its key, laid out as the shell's arrangement type holds it. This is what the session
    /// publishes on its door for a shell to draw, read off the desk as it stands.
    #[must_use]
    pub fn reading(&self) -> Reading {
        let records = self.records();
        let catalog = self.catalog.borrow();
        self.handle
            .read(|tree| reading_of(tree, &records, &catalog, &self.names))
    }

    /// The record each window shows, by tab, as the desk keeps it: what a place put on a record was
    /// placed on, and what a window took up.
    fn records(&self) -> BTreeMap<TabId, String> {
        self.desk.records().into_iter().collect()
    }

    /// Every record a place of the desk shows, by the id of the workspace (a hex device key) the
    /// place stands in: what the outside services keeping those records in step are due for. A
    /// workspace's places and the records of the apps' windows there are all of them, on every
    /// workspace of the desk, not only the one in front.
    #[must_use]
    pub fn records_by_workspace(&self) -> BTreeMap<String, std::collections::BTreeSet<String>> {
        let records = self.records();
        self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .map(|held| {
                    let shown = held
                        .strips
                        .iter()
                        .flat_map(|row| row.window_groups.iter())
                        .flat_map(|group| group.tabs.iter())
                        .filter_map(|tab| records.get(&tab.id).cloned())
                        .collect();
                    (held.id().to_string(), shown)
                })
                .collect()
        })
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

    /// Where what it publishes stands ([`Published::revision`]): what it publishes and the desk's
    /// own revision, so a place, strip or row that came, went or was renamed changes it.
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

    /// Carry out `action` as `asked`'s: one change of the record, attributed to them
    /// and undone as one step. Under an ongoing gesture or while the person types it
    /// waits ([`Self::waiting`]) and runs when they end. Returns whether it ran now.
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
        self.write(asked, &action)?;
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
        self.write(&asked, &action)
    }

    /// How many actions wait for the person to finish.
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.waiting.borrow().len()
    }

    /// The last step of the record's history, as who made it and what it did: what a history view
    /// names it by.
    #[must_use]
    pub fn last_step(&self) -> Option<(String, String)> {
        let person = self.person.clone();
        self.desk.last_step(&person)
    }

    /// The person's own change, made as their gesture's end says: their own
    /// input never waits for itself.
    ///
    /// # Errors
    /// Why the action names something the arrangement does not hold.
    pub fn ask_person(&mut self, action: &ArrangementAction) -> Result<(), String> {
        let person = self.person.clone();
        self.write(&person, action)
    }

    /// Undo `person` 's last change of the arrangement: one step of the desk's own history, taken
    /// back in the desk it was made in. Whether there was one.
    pub fn undo(&mut self, person: &str) -> bool {
        let stepped = self.desk.undo(person);
        if stepped {
            self.take_the_desk_back();
        }
        stepped
    }

    /// Redo `person` 's last undone change of the arrangement.
    pub fn redo(&mut self, person: &str) -> bool {
        let stepped = self.desk.redo(person);
        if stepped {
            self.take_the_desk_back();
        }
        stepped
    }

    /// Show the desk the step took it back to: the record is the desk, so undoing in the desk is
    /// undoing in the record.
    fn take_the_desk_back(&self) {
        if let Some(tree) = self.desk.tree() {
            self.handle.replace_tree(tree);
        }
    }

    /// Make `action` one change of the desk, attributed to `asked` as its undo step: what the
    /// arrangement is, made where the desk is.
    fn write(&self, asked: &str, action: &ArrangementAction) -> Result<(), String> {
        // An agent arranges the person's desk like the person does, except where they pinned
        // something: a pinned place is theirs to move (shell.md §5's default, "not what they
        // pinned").
        let by_the_person = asked == self.person;
        let catalog = self.catalog.clone();
        let mut tree = self.handle.read(Clone::clone);
        if !change(&mut tree, action, by_the_person, &catalog)? {
            return Ok(());
        }
        self.handle.replace_tree(tree.clone());
        // The place a resource was put on shows that resource from now on,
        // and the desk keeps it beside the window it came with.
        let shows: Vec<(TabId, String)> = if let ArrangementAction::Place {
            place, resource, ..
        } = action
        {
            vec![(*place, resource.clone())]
        } else {
            Vec::new()
        };
        // The desk is the record the workspace shares, and its own changes are its history: the
        // change this action made says who asked for it and what they did, and is one step of the
        // person's to take back.
        self.desk.record_action(
            &tree,
            ui::time::unix_now_i64(),
            &Self::step_context(asked, action),
            &shows,
        );
        Ok(())
    }

    /// What `action`, asked for by `asked`, is: who asked and what they did,
    /// as the change of the desk it makes names it.
    fn step_context(asked: &str, action: &ArrangementAction) -> tilo_actions::ActionContext {
        tilo_actions::ActionContext {
            invoker: Some(tilo_actions::ActionInvoker::actor(asked.to_owned())),
            what: Some(tilo_actions::Did::Verb(tilo_actions::VerbCall::new(
                tilo_actions::ActionKey::new("so.tilo.session", "arrangement", verb(action)),
                std::iter::empty(),
            ))),
            ..tilo_actions::ActionContext::default()
        }
    }
}

/// The shell this build's arrangement is: what every row, strip and place of a reading says it was
/// made by, so a person running a shell of their own reads what this one made and it reads theirs.
const SHELL: &str = "so.tilo.shell.strips";

/// The desk `tree` as the person's arrangement, by key: every workspace, its rows, every window
/// group and every window, each as the desk has it, with what each window shows (`records`) and
/// what the resource it shows is called (`catalog`).
fn reading_of(
    tree: &DesktopTree,
    records: &BTreeMap<TabId, String>,
    catalog: &Catalog,
    names: &Names,
) -> Reading {
    let mut reading = Reading::default();
    for workspace in &tree.workspaces {
        reading.workspaces.push(WorkspaceReading {
            key: workspace.slot.0.to_string(),
            name: workspace.name.clone(),
            // A desk holds every workspace the person is in and shows one: the shell draws the
            // rows of the one in front, so the reading says which that is.
            current: workspace.slot == tree.active,
        });
        for row in &workspace.strips {
            reading.rows.push(RowReading {
                key: row.id.0.to_string(),
                workspace: workspace.slot.0.to_string(),
                name: row.name.clone(),
                shell: SHELL.to_owned(),
            });
            for group in &row.window_groups {
                let canvas = workspace.arrangement.place_of(group.id);
                let float = workspace.arrangement.float_of(group.id);
                reading.strips.push(StripReading {
                    key: group.id.to_string(),
                    workspace: workspace.slot.0.to_string(),
                    row: row.id.0.to_string(),
                    width: i64::from(group.width.permille()),
                    maximized: group.maximized,
                    active: Some(group.active_tab)
                        .filter(|active| group.tabs.iter().any(|tab| tab.id == *active))
                        .map_or_else(String::new, |active| active.to_string()),
                    placed: canvas.is_some(),
                    canvas_x: canvas.map_or(0, |held| i64::from(held.x)),
                    canvas_y: canvas.map_or(0, |held| i64::from(held.y)),
                    canvas_width: canvas.map_or(0, |held| i64::from(held.width)),
                    canvas_height: canvas.map_or(0, |held| i64::from(held.height)),
                    floating: float.is_some(),
                    float_corner: float
                        .map_or_else(String::new, |float| float.corner.as_str().to_owned()),
                    float_dx: float.map_or(0, |float| i64::from(float.dx)),
                    float_dy: float.map_or(0, |float| i64::from(float.dy)),
                    float_width: float.map_or(0, |float| i64::from(float.width)),
                    float_height: float.map_or(0, |float| i64::from(float.height)),
                    shell: SHELL.to_owned(),
                });
                for tab in &group.tabs {
                    reading
                        .places
                        .push(place_reading(tab, group.id, records, catalog, names));
                }
            }
        }
    }
    reading
}

/// The place the window `tab` of the strip `strip` is, as a shell reads it.
fn place_reading(
    tab: &Tab,
    strip: GroupId,
    records: &BTreeMap<TabId, String>,
    catalog: &Catalog,
    names: &Names,
) -> PlaceReading {
    let record = records.get(&tab.id).cloned().unwrap_or_default();
    // A window showing a record is named by that record: its window shows it, so its title is the
    // record's, unless the person named the window itself.
    let record_title = catalog
        .get(&record)
        .map(|target| target.title.trim().to_owned())
        .unwrap_or_default();
    let title = if tab.app_title.is_none() && !record_title.is_empty() {
        record_title
    } else {
        tab.title.clone()
    };
    PlaceReading {
        key: tab.id.to_string(),
        strip: strip.to_string(),
        shows: tilo_desktop_model::apps::canonical_app_id(&tab.tile).unwrap_or_default(),
        shown_as: tab.view.clone(),
        record,
        title: tab.title.clone(),
        name: names(&shown_app(&tab.tile), &title),
        pinned: tab.pinned,
        shell: SHELL.to_owned(),
    }
}

/// The app a window belongs to, as the desk names it: the app's id, or a placeholder's own label
/// for an app this device does not have.
fn shown_app(tile: &TileContent) -> String {
    match tile {
        TileContent::AppPlaceholder { label, .. } | TileContent::Stub { label, .. } => {
            label.clone()
        }
        held => tilo_desktop_model::apps::canonical_app_id(held).unwrap_or_default(),
    }
}

/// One change of `tree` , as `action` names it. `by_the_person` is whether the person's own input
/// asked for it: an agent's move leaves a place the person pinned where it is. Whether anything
/// changed.
fn change(
    tree: &mut DesktopTree,
    action: &ArrangementAction,
    by_the_person: bool,
    catalog: &Rc<RefCell<Catalog>>,
) -> Result<bool, String> {
    match action {
        ArrangementAction::RetiredOpenInto { .. } => {
            Err("a place is made from the resource it shows, not opened as an app".into())
        }
        ArrangementAction::Move {
            place,
            row,
            before,
            strip,
        } => move_place(tree, *place, StripId(*row), *before, *strip, by_the_person),
        ArrangementAction::Resize { strip, permille } => {
            let Some(group) = group_mut(tree, *strip) else {
                return Ok(false);
            };
            let width = WindowWidth::Fraction(*permille);
            let changed = group.width != width;
            group.width = width;
            Ok(changed)
        }
        ArrangementAction::Maximize { strip, maximized } => {
            let group = group_mut(tree, *strip)
                .ok_or_else(|| "that strip is not in the arrangement".to_owned())?;
            let changed = group.maximized != *maximized;
            group.maximized = *maximized;
            Ok(changed)
        }
        ArrangementAction::Close { place } => {
            if tree.close_tab(*place).is_some() {
                Ok(true)
            } else {
                Err("that place is not in the arrangement".into())
            }
        }
        ArrangementAction::Activate { place } => {
            let Some(held) = group_holding(tree, *place) else {
                return Err("that place is not in the arrangement".into());
            };
            if held.active_tab == *place {
                return Err("that place is in front already".into());
            }
            held.activate_tab(*place);
            Ok(true)
        }
        ArrangementAction::Pin { place, pinned } => {
            let Some(tab) = group_holding(tree, *place)
                .and_then(|held| held.tabs.iter_mut().find(|tab| tab.id == *place))
            else {
                return Err("that place is not in the arrangement".into());
            };
            let changed = tab.pinned != *pinned;
            tab.pinned = *pinned;
            Ok(changed)
        }
        ArrangementAction::Position { place, x, y } => {
            position(tree, *place, clamp_coord(*x), clamp_coord(*y))
        }
        // Resolved into a `Place` at invocation, where the system's search and what is published
        // now are; the record never holds the words.
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
            float,
        } => place_at(
            tree,
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
                float: *float,
            },
        ),
        ArrangementAction::Float { place, float } => float_place(tree, *place, *float),
        // Resolved into the `Place` (or `Activate`) it makes before it reaches the record, where
        // the desk's ids and the place it stands beside are known.
        ArrangementAction::Open { .. } => {
            Err("an open is resolved into the place it makes before it reaches the record".into())
        }
        ArrangementAction::Snap { place, row, before } => {
            snap_back(tree, *place, *row, *before, by_the_person)
        }
    }
}

/// Move the place `place` to `(x, y)` on its workspace's canvas. Whether the desk changed.
fn position(tree: &mut DesktopTree, place: u64, x: i32, y: i32) -> Result<bool, String> {
    let Some((workspace, row, group, _)) = locate(tree, place) else {
        return Err("that place is not in the arrangement".into());
    };
    let Some(id) = tree
        .workspaces
        .get(workspace)
        .and_then(|held| held.strips.get(row))
        .and_then(|row| row.window_groups.get(group))
        .map(|group| group.id)
    else {
        return Err("that place is not in the arrangement".into());
    };
    let Some(held) = tree.workspaces.get_mut(workspace) else {
        return Err("that place is not in the arrangement".into());
    };
    Ok(held.arrangement.set_position(id, x, y))
}

/// Float the place `place` above its workspace's windows at `float` .
fn float_place(tree: &mut DesktopTree, place: u64, float: FloatPlace) -> Result<bool, String> {
    let Some((workspace, row, group, _)) = locate(tree, place) else {
        return Err("that place is not in the arrangement".into());
    };
    let Some(held) = tree.workspaces.get_mut(workspace) else {
        return Err("that place is not in the arrangement".into());
    };
    let Some(id) = held
        .strips
        .get(row)
        .and_then(|strip| strip.window_groups.get(group))
        .map(|group| group.id)
    else {
        return Err("that place is not in the arrangement".into());
    };
    Ok(held.arrangement.set_float(id, Some(float)))
}

/// Snap the floating place `place` back among its workspace's windows,
/// moving it into `row` before `before` when a drop named one; left out, it
/// returns to the strip it stands in, unchanged. Whether the desk changed.
fn snap_back(
    tree: &mut DesktopTree,
    place: u64,
    row: Option<u64>,
    before: Option<u64>,
    by_the_person: bool,
) -> Result<bool, String> {
    let Some((workspace, at, group, _)) = locate(tree, place) else {
        return Err("that place is not in the arrangement".into());
    };
    let Some(id) = tree
        .workspaces
        .get(workspace)
        .and_then(|held| held.strips.get(at))
        .and_then(|strip| strip.window_groups.get(group))
        .map(|group| group.id)
    else {
        return Err("that place is not in the arrangement".into());
    };
    let mut changed = match tree.workspaces.get_mut(workspace) {
        Some(held) => held.arrangement.set_float(id, None),
        None => return Err("that place is not in the arrangement".into()),
    };
    if let Some(row) = row {
        changed |= move_place(tree, place, StripId(row), before, None, by_the_person)?;
    }
    Ok(changed)
}

/// The window `place` and where it stands in `tree`: its workspace, its row,
/// its group and its index in that group.
fn locate(tree: &DesktopTree, place: u64) -> Option<(usize, usize, usize, usize)> {
    tree.workspaces
        .iter()
        .enumerate()
        .find_map(|(workspace, held)| {
            held.strips.iter().enumerate().find_map(|(row, strip)| {
                strip
                    .window_groups
                    .iter()
                    .enumerate()
                    .find_map(|(group, held)| {
                        held.tabs
                            .iter()
                            .position(|tab| tab.id == place)
                            .map(|at| (workspace, row, group, at))
                    })
            })
        })
}

/// The group holding the window `place` of `tree` , wherever it stands.
fn group_holding(tree: &mut DesktopTree, place: u64) -> Option<&mut WindowGroup> {
    tree.workspaces
        .iter_mut()
        .flat_map(|workspace| &mut workspace.strips)
        .flat_map(|strip| &mut strip.window_groups)
        .find(|held| held.tabs.iter().any(|tab| tab.id == place))
}

/// The group `id` of `tree` , wherever it stands.
fn group_mut(tree: &mut DesktopTree, id: GroupId) -> Option<&mut WindowGroup> {
    tree.workspaces
        .iter_mut()
        .flat_map(|workspace| &mut workspace.strips)
        .flat_map(|strip| &mut strip.window_groups)
        .find(|held| held.id == id)
}

/// Move the window `place` into the row `row` of its workspace, before the window `before` or last,
/// as arranging does: into the group holding `before` , else the row's last, else one derived from
/// the row so two devices making one at once agree. When `strip` names a strip of that row, the
/// window becomes a tab of it, in front (which is how a strip of several places is made: shell.md
/// §2). A window the person pinned stays where it is unless they moved it themselves.
fn move_place(
    tree: &mut DesktopTree,
    place: u64,
    row: StripId,
    before: Option<u64>,
    strip: Option<u64>,
    by_the_person: bool,
) -> Result<bool, String> {
    let Some((workspace, from, group, at)) = locate(tree, place) else {
        return Err("that place is not in the arrangement".into());
    };
    let taken = "that place is not in the arrangement";
    let Some(held) = tree.workspaces.get_mut(workspace) else {
        return Err(taken.into());
    };
    let pinned = held
        .strips
        .get(from)
        .and_then(|strip| strip.window_groups.get(group))
        .and_then(|group| group.tabs.get(at))
        .is_some_and(|tab| tab.pinned);
    if !by_the_person && pinned {
        return Err("the person pinned that place: it stays where it is".into());
    }
    // A place moves within its workspace: a row of another one is no row of its own. A strip names
    // where the place lands, so a strip of another row names the row it stands in.
    let Some(row) = held.strips.iter().position(|held| held.id == row) else {
        return Err("that row is not in the arrangement".into());
    };
    let row = strip
        .and_then(|strip| {
            held.strips
                .iter()
                .position(|held| held.window_groups.iter().any(|group| group.id == strip))
        })
        .unwrap_or(row);
    let Some(from_strip) = held.strips.get_mut(from) else {
        return Err(taken.into());
    };
    let Some(source) = from_strip.window_groups.get_mut(group) else {
        return Err(taken.into());
    };
    let tab = source.tabs.remove(at);
    if source.tabs.is_empty() {
        from_strip.window_groups.remove(group);
        // The window left the desk with its last place: its place and float go with it.
        held.forget_gone_places();
    }
    let holds = |group: &WindowGroup| {
        strip.is_some_and(|strip| group.id == strip)
            || before.is_some_and(|before| group.tabs.iter().any(|tab| tab.id == before))
    };
    let Some(target) = held.strips.get_mut(row) else {
        return Err("that row is not in the arrangement".into());
    };
    let groups = &mut target.window_groups;
    let into = groups
        .iter()
        .position(holds)
        .or_else(|| groups.len().checked_sub(1));
    let into = if let Some(into) = into {
        into
    } else {
        groups.push(WindowGroup {
            id: strip.unwrap_or_else(|| target.id.0.wrapping_mul(31).wrapping_add(1) & PEER_MASK),
            tabs: Vec::new(),
            active_tab: tab.id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        groups.len().saturating_sub(1)
    };
    let Some(group) = target.window_groups.get_mut(into) else {
        return Err(taken.into());
    };
    let at = before
        .and_then(|before| group.tabs.iter().position(|held| held.id == before))
        .unwrap_or(group.tabs.len());
    let id = tab.id;
    group.tabs.insert(at, tab);
    group.activate_tab(id);
    Ok(true)
}

/// What [`change`] is asked to place: the resource `resource` , through `view` , as the place
/// `place` in its own strip `strip` of the row `row` of `workspace` , at `(x, y)` canvas pixels,
/// `width` by `height` — or floating above them at `float` .
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
    float: Option<FloatPlace>,
}

/// Stand `asked` on the canvas as one of its published views: a window of the
/// resource's app, at the place it asked for (shell.md §8). A `strip` that
/// names a strip already on the desk puts the place in it as a tab, in front,
/// rather than in a strip of its own: how a person, an agent and the REPL open
/// something as a tab of the strip they are in. Whether the desk changed.
fn place_at(
    tree: &mut DesktopTree,
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
    let taken = "that workspace is not on this desk, or that place is taken";
    if locate(tree, asked.place).is_some() {
        return Err(taken.into());
    }
    // The strip the place would take for its own is this workspace's, or a new one: a strip another
    // workspace holds is no strip of this desk for this place. A strip on this desk is joined as a
    // tab below, whichever workspace and row it stands in.
    let on_desk = tree.workspaces.iter().any(|held| {
        held.strips.iter().any(|strip| {
            strip
                .window_groups
                .iter()
                .any(|group| group.id == asked.strip)
        })
    });
    let Some(workspace) = tree
        .workspaces
        .iter_mut()
        .find(|held| held.slot == WorkspaceSlot(asked.workspace))
    else {
        return Err(taken.into());
    };
    if on_desk
        && !workspace.strips.iter().any(|strip| {
            strip
                .window_groups
                .iter()
                .any(|group| group.id == asked.strip)
        })
    {
        return Err(taken.into());
    }
    let id = asked.place;
    let view = target.chosen_view(asked.view);
    let window = Tab {
        id,
        title: target.title.clone(),
        tile: TileContent::LocalApp {
            app_id: target.app,
            label: target.title,
            note: String::new(),
        },
        app_title: None,
        view,
        pinned: false,
    };
    // A strip already on the desk takes the place as a tab, in front: the window it is stands in
    // front showing the place just added. A strip the caller named is found wherever it stands, so
    // a place named into another row's strip joins that strip rather than taking its id.
    let held_elsewhere = workspace.strips.iter().enumerate().find_map(|(at, strip)| {
        strip
            .window_groups
            .iter()
            .any(|group| group.id == asked.strip)
            .then_some(at)
    });
    if let Some(at) = held_elsewhere {
        join_strip(workspace, at, asked.strip, window, taken)?;
        float_new(workspace, asked.strip, asked.float);
        return Ok(true);
    }
    let row = if let Some(row) = workspace
        .strips
        .iter()
        .position(|strip| strip.id == StripId(asked.row))
    {
        row
    } else {
        workspace.strips.push(Strip::new(StripId(asked.row), ""));
        workspace.strips.len().saturating_sub(1)
    };
    let Some(strip) = workspace.strips.get_mut(row) else {
        return Err(taken.into());
    };
    strip.window_groups.push(WindowGroup {
        id: asked.strip,
        tabs: vec![window],
        active_tab: id,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    // A floated place stands above the windows, not on the canvas: kept in its strip's place,
    // snapping it back is exact.
    if asked.float.is_some() {
        float_new(workspace, asked.strip, asked.float);
        return Ok(true);
    }
    // It stands on the canvas, so the workspace shows its canvas: the person
    // sees what they placed. A place never covers one already there: it takes
    // the free space nearest the point it was asked for (`canvas.md` §6,
    // `Arrangement::free_place_near`), so two places at one point, or two
    // closer together than a frame is wide, stand clear of each other. The
    // frame it stands in is the size it asks for, so the free space is that
    // size.
    let (width, height) = (clamp_side(asked.width), clamp_side(asked.height));
    let place = workspace.arrangement.free_place_near(
        (clamp_coord(asked.x), clamp_coord(asked.y)),
        width,
        height,
        None,
    );
    workspace.arrangement.places.insert(asked.strip, place);
    workspace.arrangement.kind = ArrangementKind::Canvas;
    Ok(true)
}

/// Float the place `group` at `float` , when the place asked to float at all.
fn float_new(
    workspace: &mut tilo_desktop_model::Workspace,
    group: GroupId,
    float: Option<FloatPlace>,
) {
    if let Some(float) = float {
        workspace.arrangement.set_float(group, Some(float));
    }
}

/// Take `window` into the group `group` already on the workspace's desk, as its tab in front: a
/// place of this workspace's, whichever row it stands in.
fn join_strip(
    workspace: &mut tilo_desktop_model::Workspace,
    row: usize,
    group: GroupId,
    window: Tab,
    taken: &str,
) -> Result<(), String> {
    let Some(strip) = workspace.strips.get_mut(row) else {
        return Err(taken.to_owned());
    };
    let Some(held) = strip.window_groups.iter_mut().find(|held| held.id == group) else {
        return Err(taken.to_owned());
    };
    let id = window.id;
    held.tabs.push(window);
    held.activate_tab(id);
    Ok(())
}

/// A canvas coordinate as a place keeps it.
fn clamp_coord(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
}

/// A canvas side, at least one pixel.
fn clamp_side(value: i64) -> u32 {
    u32::try_from(value.max(1)).unwrap_or(u32::MAX)
}

/// The id space's counter mask, for an id this build derives rather than mints.
const PEER_MASK: u64 = tilo_desktop_model::PEER_ID_COUNTER_MASK;

/// The verb `action` is, as its step names it.
fn verb(action: &ArrangementAction) -> &'static str {
    match action {
        ArrangementAction::Move { .. } => "move",
        ArrangementAction::Resize { .. } => "resize",
        ArrangementAction::Maximize { .. } => "maximize-strip",
        ArrangementAction::Close { .. } => "close",
        ArrangementAction::Activate { .. } => "activate",
        ArrangementAction::Pin { .. } => "pin",
        ArrangementAction::Position { .. } => "position",
        ArrangementAction::Place { .. } | ArrangementAction::PlaceFound { .. } => "place",
        ArrangementAction::Open { .. } => "open",
        ArrangementAction::Float { .. } => "float",
        ArrangementAction::Snap { .. } => "snap-back",
        // Refused before it is stepped: it names no change of the desk.
        ArrangementAction::RetiredOpenInto { .. } => "retired",
    }
}

/// The person's arranging actions, published as every other action is: a person's own drag, the
/// palette, a script and an agent all arrange through them, and the catalogue lists them. What is
/// already arranged is published per place, strip and row by [`Arrangement::resources`].
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
}

/// The record itself, whose actions step its history.
fn record_resource() -> ResourceSpec {
    ResourceSpec::new(RECORD_RESOURCE, "shell", "The person's arrangement record")
        .with_action(step_action("undo", "Undo the last arranging step"))
        .with_action(step_action("redo", "Redo the last undone arranging step"))
}

/// The resource a place's own actions are published on, for the package the session describes: no
/// key, so it stands for every place. A place's resource is named by the place's key
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

/// The resource a row is published as: [`place_kind_resource`] for a row. A row offers no action of
/// its own: it is what a place moves into, named by the place's `move` .
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
    let positioned = key.to_owned();
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
        action(
            "position",
            "Stand a place at a point on the canvas, as a drag that let it go there",
            vec![
                required("x", ParamType::Integer, "Its canvas x"),
                required("y", ParamType::Integer, "Its canvas y"),
            ],
            move |args| ArrangementAction::Position {
                place: count_of(&positioned),
                x: args.integer("x").unwrap_or_default(),
                y: args.integer("y").unwrap_or_default(),
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
            "Move a place into a row, or into a strip as a tab of it",
            vec![
                required("row", ParamType::Resource, "The row it goes into")
                    .of_resource_kind(ROW_KIND),
                optional(
                    "strip",
                    ParamType::Resource,
                    "The strip it becomes a tab of, in that strip's row",
                )
                .of_resource_kind(STRIP_KIND),
                optional(
                    "before",
                    ParamType::Resource,
                    "The place it goes before in that strip",
                )
                .of_resource_kind(PLACE_KIND),
            ],
            move |args| ArrangementAction::Move {
                place: count_of(&moved),
                row: named(args, "row", Item::Row).unwrap_or_default(),
                before: named(args, "before", Item::Place),
                strip: named(args, "strip", Item::Strip),
            },
        ),
    ]
    .into_iter()
    .chain(float_actions(key))
    .collect()
}

/// The actions that float a place and snap it back, its key bound into each as every place action's
/// is.
fn float_actions(key: &str) -> Vec<ActionSpec> {
    let floating = key.to_owned();
    let snapping = key.to_owned();
    vec![
        action(
            "float",
            "Float a place above its workspace's windows, where every member sees it",
            vec![
                optional(
                    "corner",
                    ParamType::Text,
                    "The corner of the work area it keeps its distance from: top-left, top-right, bottom-right (the default) or bottom-left",
                ),
                optional(
                    "dx",
                    ParamType::Integer,
                    "Its distance from that corner's side edge, in canvas pixels",
                ),
                optional(
                    "dy",
                    ParamType::Integer,
                    "Its distance from that corner's top or bottom edge, in canvas pixels",
                ),
                optional("width", ParamType::Integer, "Its width in canvas pixels"),
                optional("height", ParamType::Integer, "Its height in canvas pixels"),
            ],
            move |args| ArrangementAction::Float {
                place: count_of(&floating),
                float: FloatPlace::new(
                    float_corner(args.text("corner")),
                    float_coord(args.integer("dx"), FLOAT_INSET),
                    float_coord(args.integer("dy"), FLOAT_INSET),
                    float_side(args.integer("width"), FLOAT_WIDTH),
                    float_side(args.integer("height"), FLOAT_HEIGHT),
                ),
            },
        ),
        action(
            "snap-back",
            "Snap a floating place back among its workspace's windows",
            vec![
                optional(
                    "row",
                    ParamType::Resource,
                    "The row it goes into; its own when left out",
                )
                .of_resource_kind(ROW_KIND),
                optional(
                    "before",
                    ParamType::Resource,
                    "The place it goes before in that row",
                )
                .of_resource_kind(PLACE_KIND),
            ],
            move |args| ArrangementAction::Snap {
                place: count_of(&snapping),
                row: named(args, "row", Item::Row),
                before: named(args, "before", Item::Place),
            },
        ),
    ]
}

/// The corner a `float` action names, the work area's bottom-right when it names none.
fn float_corner(text: Option<&str>) -> FloatCorner {
    text.and_then(FloatCorner::parse).unwrap_or_default()
}

/// A float's distance from a corner, never negative.
fn float_coord(value: Option<i64>, default: i64) -> i32 {
    i32::try_from(value.unwrap_or(default).max(0)).unwrap_or(i32::MAX)
}

/// A floated window's side, at least one pixel.
fn float_side(value: Option<i64>, default: u32) -> u32 {
    u32::try_from(value.unwrap_or_else(|| i64::from(default)).max(1)).unwrap_or(u32::MAX)
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

/// The row `arguments` names as `name` , by the id of the row's resource. `None` for an id of
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

/// What a row is called where its own actions are listed: its name, or what it is when it has none.
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

/// Feed what `specs` publish into `hasher` : each resource's id, kind and label, and its
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

/// The actions that put a resource on the canvas: by naming it and one of its views, or by what a
/// text finds (shell.md §8).
fn placing_actions() -> Vec<ActionSpec> {
    vec![place_action(), place_found_action(), open_action()]
}

/// `place`: a resource on the canvas, as one of its views, at a point or a float's corner.
fn place_action() -> ActionSpec {
    action(
        "place",
        "Put a resource on the canvas as one of its views",
        vec![
            // A workspace's id is its own, across all of u64: text, as the arrangement record
            // names it, where an integer stops at i64. Each left out is the surface in front's:
            // its workspace and row, and a strip and place id of its own.
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
            // A place that floats stands above the windows rather than on the canvas: it has no
            // canvas x and y, and the corner and insets are its own instead (`width` and
            // `height` are its size). Left out, the place stands on the canvas.
            optional(
                "float",
                ParamType::Text,
                "Floats it above its workspace's windows at this corner of the work area: top-left, top-right, bottom-right (the default) or bottom-left",
            ),
            optional(
                "dx",
                ParamType::Integer,
                "A float's distance from that corner's side edge, in canvas pixels",
            ),
            optional(
                "dy",
                ParamType::Integer,
                "A float's distance from that corner's top or bottom edge, in canvas pixels",
            ),
        ],
        |args| {
            let float = args.text("float").map(|corner| {
                FloatPlace::new(
                    // ignored-failure: a corner's name that names none stands at the default corner
                    FloatCorner::parse(corner).unwrap_or_default(),
                    float_coord(args.integer("dx"), FLOAT_INSET),
                    float_coord(args.integer("dy"), FLOAT_INSET),
                    float_side(args.integer("width"), FLOAT_WIDTH),
                    float_side(args.integer("height"), FLOAT_HEIGHT),
                )
            });
            ArrangementAction::Place {
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
                float,
            }
        },
    )
}

/// `place-found`: what a text finds, put on the canvas at the caret, or into a strip as a tab.
fn place_found_action() -> ActionSpec {
    action(
        "place-found",
        "Put what a text finds on the canvas, or into a strip as a tab",
        vec![
            required("text", ParamType::Text, "What to find and place"),
            optional("view", ParamType::Text, "The view it shows it through"),
            optional(
                "strip",
                ParamType::Text,
                "The strip it joins as a tab, by its id; a strip of its own when left out",
            ),
            optional("x", ParamType::Integer, "Its canvas x"),
            optional("y", ParamType::Integer, "Its canvas y"),
        ],
        |args| ArrangementAction::PlaceFound {
            text: text(args, "text"),
            view: args.text("view").unwrap_or_default().to_owned(),
            strip: args
                .text("strip")
                .map(|_| id_text(args, "strip"))
                .filter(|strip| *strip != 0),
            x: args.integer("x").unwrap_or_default(),
            y: args.integer("y").unwrap_or_default(),
        },
    )
}

/// `open`: a resource opened with its own view, beside what the ask is in.
fn open_action() -> ActionSpec {
    action(
        "open",
        "Open a resource with its own view, beside what the ask is in",
        vec![
            required("resource", ParamType::Resource, "The resource it shows"),
            optional("view", ParamType::Text, "The view it shows it through"),
            optional(
                "beside",
                ParamType::Text,
                "The resource whose place it stands beside, by its address; the place in front when left out",
            ),
        ],
        |args| ArrangementAction::Open {
            resource: text(args, "resource"),
            view: args.text("view").unwrap_or_default().to_owned(),
            beside: text(args, "beside"),
        },
    )
}

/// An immediate effect action `id` the session answers itself: a step of the arrangement's history.
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

/// The arrangement's published side: the one door the palette, a script, an agent and the host's
/// own calls reach its actions through, beside the desktop's own.
pub struct Door {
    arrangement: Rc<RefCell<Arrangement>>,
    /// How the host says whether the person is mid-gesture or typing, as it last saw it.
    busy: Rc<dyn Fn() -> Busy>,
    /// The system's `find` , as the palette and an agent's `tilo` tool run it: what text typed on
    /// the canvas places (shell.md §8). Empty where the host has no index to search.
    find: Find,
    /// What the canvas asks of the host when the text it is given is a question, or names
    /// nothing (shell.md §8.1). `None` where the host has no agent to ask.
    ask: Ask,
}

impl Door {
    /// The door over `arrangement` , whose changes wait while `busy` says the person is arranging
    /// or typing.
    #[must_use]
    pub fn new(
        arrangement: Rc<RefCell<Arrangement>>,
        busy: Rc<dyn Fn() -> Busy>,
        find: Find,
        ask: Ask,
    ) -> Self {
        Self {
            arrangement,
            busy,
            find,
            ask,
        }
    }

    /// What the arrangement publishes now, as the host publishes it.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceSpec> {
        self.arrangement.borrow().resources()
    }

    /// Where what it publishes stands ([`Arrangement::revision`]).
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.arrangement.borrow().revision()
    }

    /// The names an assistive technology reads for what it publishes: none of its own.
    #[must_use]
    pub fn accessible_labels(&self) -> Vec<String> {
        Vec::new()
    }

    /// Carry out `address` with `arguments`.
    ///
    /// # Errors
    /// Why the action names nothing published, or why it could not be made.
    pub fn invoke(
        &self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, Json>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        self.invoke_by(None, address, arguments)
    }

    /// [`Self::invoke`] with the one who asks named: an actor that names no one is the person's own
    /// call.
    fn invoke_by(
        &self,
        actor: Option<&str>,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, Json>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        // The arrangement publishes its record's undo and redo beside its arranging actions: both
        // step the record, never decode as an arranging action.
        if address.resource == RECORD_RESOURCE
            || (address.resource == RESOURCE && matches!(address.action.as_str(), "undo" | "redo"))
        {
            return self.step(address);
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
        // An action that names no one is the person's own input: theirs to take back, and never
        // made to wait for themselves.
        let asked = match actor.filter(|actor| !actor.is_empty()) {
            Some(actor) => actor.to_owned(),
            None => self.person(),
        };
        // The person's own change is what their gesture is making; only anyone else's waits for
        // them to finish.
        let busy = if asked == self.person() {
            Busy::default()
        } else {
            (self.busy)()
        };
        // A place found by text is resolved here, where the search and what is published now are:
        // the record only ever holds what was placed.
        if let ArrangementAction::PlaceFound {
            text,
            view,
            strip,
            x,
            y,
        } = &action
        {
            let spot = (*x, *y);
            let find = Rc::clone(&self.find);
            let ask = Rc::clone(&self.ask);
            let placed = self.arrangement.borrow_mut().place_found(Found {
                asked: &asked,
                text,
                view,
                strip: *strip,
                at: spot,
                find: &*find,
                ask: &*ask,
                busy,
            });
            placed.map_err(|why| tilo_actions::DoorError::Unavailable {
                code: "nothing_to_place".to_owned(),
                reason: why,
            })?;
            return Ok(ui::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ));
        }
        // The system's `open` is resolved here, where the desk's ids and its places are: it
        // becomes the `Place` it makes beside its anchor, or the `Activate` that brings what is
        // open forward, and the record only ever holds ids.
        if let ArrangementAction::Open {
            resource,
            view,
            beside,
        } = &action
        {
            let opened = self
                .arrangement
                .borrow_mut()
                .open_place(&asked, resource, view, beside);
            opened.map_err(|why| tilo_actions::DoorError::Unavailable {
                code: "nowhere_to_open".to_owned(),
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

    /// [`Self::invoke`] with `actor` as the one who asks: the name their step of the desk's history
    /// carries, and whose gesture an action that is not the person's waits for.
    ///
    /// # Errors
    /// As [`Self::invoke`].
    pub fn invoke_as(
        &self,
        actor: &str,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, Json>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        self.invoke_by(Some(actor), address, arguments)
    }

    /// A `place` that names no workspace, row, strip or id of its own stands on the
    /// surface in front, in a strip of its own, under ids minted here: what a reference
    /// activated in a view asks for (shell.md §3). They go into the action itself,
    /// never back through the arguments: a desk's ids span all of `u64`, which an
    /// integer argument does not.
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
        // A row is a row of one workspace: the two come named together, or both from the surface in
        // front.
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

    /// Undo or redo the person's last change of the record.
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

#[cfg(test)]
mod tests {
    use super::{found, most_specific, placed};
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

    /// A row for an action an app publishes on `resource` : `find` answers it (it is a row a caller
    /// acts on) but a place does not stand it, so it is never what typing `text` places.
    fn acting(resource: &str, action: &str) -> Hit {
        Hit {
            row: None,
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind: ResourceKind::Action,
            provider: "test".into(),
            id: format!("{resource}#{action}"),
            title: action.into(),
            subtitle: None,
            facts: Vec::new(),
            score: 0,
            icon: None,
            target: Target::Invoke(Box::new(InvokeTarget::action("test", "", resource, action))),
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
    fn the_text_places_the_hit_whose_resource_it_names() {
        let named = [
            (0, "notes", "Groceries"),
            (1, "notes/i1", "Groceries"),
            (2, "notes/i2", "Groceries"),
        ];
        assert_eq!(
            most_specific(&named, "Groceries"),
            Some(1),
            "a note titled like its notebook is the note, and the top of them"
        );
        assert_eq!(
            most_specific(&named, "my notes"),
            Some(0),
            "a resource the text only describes stays itself"
        );
        assert_eq!(
            most_specific(&[(0, "inbox", "Inbox")], "inbox"),
            Some(0),
            "and the top stays when nothing inside it carries the same name"
        );
    }

    #[test]
    fn the_text_places_the_row_that_opens_the_resource_not_one_that_acts_on_it() {
        // "my mails" ranks Mail's actions far above Mail's root, as the search does: the root's own
        // row stands last of the rows about it.
        let hits = vec![
            acting("tl:so.tilo.lang.mail:root", "rename_label"),
            acting("tl:so.tilo.lang.mail:root/account", "set_signature"),
            opening("tl:so.tilo.lang.mail:root", "Mail", ResourceKind::Record),
        ];
        let at = placed(&hits, "my mails", |_| true);
        assert_eq!(
            at,
            Some(2),
            "the row typing the text stands is the row that opens the resource, \
             not another row about it"
        );
        assert_eq!(
            found("my mails", &|_| hits.clone(), |_| true).as_deref(),
            Some("tl:so.tilo.lang.mail:root"),
            "and the canvas stands its resource"
        );
    }
}
