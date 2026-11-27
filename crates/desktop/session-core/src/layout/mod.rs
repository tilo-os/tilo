//! The person's layout: what is open where, shared with the workspace and read off the layout
//! itself, kept in its own record
//! ([`tilo_desktop_model::shared_resource`]) with history and undo, so every member
//! of the workspace reads the same one. A place's view and its pin are the
//! window's own ([`tilo_desktop_model::Tab`]).
//! Each layout action ([`LayoutAction`]) is one change of the layout,
//! attributed to whoever asked for it and undone as that one step; what the layout
//! changes on its own is a change like any other.
//! One rule is fixed (shell.md §1): nothing moves or takes focus under an ongoing
//! gesture or while the person types. An action that arrives then waits here and
//! runs when they end; it is never dropped.
//! Both hosts run this: the native session and the page the browser shows.
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use crate::desktop::state::DesktopHandle;
use tilo_actions::remote::session_actions;
use tilo_actions::{Actor, MemberId, ResourceSpec};
use tilo_desktop_model::{
    FloatPlace, GroupId, Layout as Tree, LayoutKind, Strip, StripId, Tab, TabId, TileContent,
    WindowGroup, WindowWidth, Workspace, WorkspaceId,
};

mod document;
pub use document::*;
mod readings;
pub use readings::*;
mod beside;
mod sizing;
use sizing::canvas_size;
use sizing::width_in_view;
mod published;
use published::{
    LayoutRow, LayoutStrip, Place, history_resource, layout_resource, place_label, row_label,
    strip_label,
};
pub use published::{place_kind_resource, resources, row_kind_resource, strip_kind_resource};
mod queries;
use queries::{query_resource, query_state_from_outcome, query_target, settle};

/// How many of one app's resources a person's recents list: a list of what was opened lately,
/// never the app's whole catalogue.
pub const RECENTS_PER_APP: usize = 100;

/// The size a place made from a resource takes on the canvas when the caller names none: the framed
/// canvas's window, in canvas pixels.
const DEFAULT_WIDTH: i64 = 720;
/// See [`DEFAULT_WIDTH`].
const DEFAULT_HEIGHT: i64 = 520;

/// How long a text typed on the canvas waits for its search before answering with the hits that
/// came: a source reading a disk, a mailbox or a service has its own time, and the pass that placed
/// waits for none of them. The canvas is the person's own typing, so it is heard within a moment;
/// a search that has not answered by then places what it found so far, or sends the text on as a
/// question to their agent as it always did when nothing matched.
#[must_use]
pub const fn canvas_find_within() -> std::time::Duration {
    std::time::Duration::from_secs(2)
}

/// Where a place floats when the action names nothing else: the work area's bottom-right corner, a
/// window's inset from its edges, at a floating size smaller than a canvas window.
const FLOAT_INSET: i64 = 24;
const FLOAT_WIDTH: u32 = 640;
const FLOAT_HEIGHT: u32 = 480;

/// The operation type every layout action carries.
pub const OPERATION: &str = "tilo.shell.layout";
/// The resource the person's layout is published as. Its id is the one every api that names
/// the system's `open` names ([`session_actions::LAYOUT_RESOURCE`]): the broker rewrites a
/// served app's `open` into the layout's own, so the two are one address.
pub const RESOURCE: &str = session_actions::LAYOUT_RESOURCE;
/// The resource its history is published as: its undo and redo.
pub const HISTORY_RESOURCE: &str = "shell:layout-history";
/// The kind of a place's own resource.
pub const PLACE_KIND: &str = "layout.place";
/// The kind of a strip's own resource.
pub const STRIP_KIND: &str = "layout.strip";
/// The kind of a row's own resource.
pub const ROW_KIND: &str = "layout.row";
/// The kind of a query's own resource: what a place shows while its app answers what the person
/// typed (`session.layout_query`, shell.md §8).
pub const QUERY_KIND: &str = "layout.query";

/// What a place, a strip or a row of the layout is published as a resource of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Place,
    Strip,
    Row,
    /// A query a place shows while its app answers it; its key is the id of that place.
    Query,
}

impl Item {
    /// The id each of its resources begins with: its id is the prefix and the record's key, so a
    /// resource is named by what it is and a key never has to be read as a number.
    const fn prefix(self) -> &'static str {
        match self {
            Self::Place => "shell:place:",
            Self::Strip => "shell:strip:",
            Self::Row => "shell:row:",
            Self::Query => "shell:query:",
        }
    }

    /// The resource the layout publishes the thing `key` names as.
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

/// A layout action: one change of the person's layout.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "do", rename_all = "kebab-case")]
pub enum LayoutAction {
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
    /// Maximize the strip `strip` (`maximized`), so it stands alone and fills the shell, or
    /// restore it beside its row's other windows.
    Maximize { strip: u64, maximized: bool },
    /// Close the place `place` ; its window goes with it.
    Close { place: u64 },
    /// Bring the place `place` in front of the strip it stands in, as choosing its tab does.
    Activate { place: u64 },
    /// Pin the place `place` (`pinned`), so an agent's layout leaves it alone (shell.md §5),
    /// or unpin it.
    Pin { place: u64, pinned: bool },
    /// Stand the place `place` at `(x, y)` canvas pixels, keeping every other place where it is:
    /// what a drag that let a place go on the canvas runs (`canvas.md` §6, "moving a window is one
    /// layout action", shell.md §2 `move`). The place's strip takes the point; nothing else
    /// changes. Appended last: existing encodings are unchanged.
    Position { place: u64, x: i64, y: i64 },
    /// Stand the resource `resource` on the canvas as its view `view`
    /// (empty for its primary published view), at `(x, y)` canvas pixels.
    /// The place is the new entry `place`, alone in its own strip `strip`,
    /// in the row `row`; `shows` is the resource's app, resolved here and
    /// never passed in (shell.md §8: showing something is an action on the
    /// layout, not on the resource).
    Place {
        workspace: WorkspaceId,
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
        /// Float the place above the workspace's windows at `float` , instead of setting it on the
        /// canvas: how a conversation popped out of its app opens, its draft and scroll kept in the
        /// window it already was. Left out, the place stands on the canvas as it always did.
        #[serde(default)]
        float: Option<FloatPlace>,
        /// `height` is the most it takes: what it shows hugs what it holds up to it
        /// (`CanvasPlace::hugs`, canvas.md §7) — a place put with no size of its own, by a click
        /// or by an agent beside its turn. A height of zero or less hugs up to a window's too.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        hugs: bool,
    },
    /// Text typed on the canvas: the top result of the system's `find` stands there as its primary
    /// view (or `view` ). The session resolves it into a [`Place`] when the search answers, so
    /// the record only ever holds what was placed, never the words it was found by. A `strip` names
    /// the strip the found resource joins as a tab, in front, instead of a strip of its own: how a
    /// person opens a found thing beside what they are in (shell.md §2, §3), as pressing a
    /// reference does. The search is asked off the pass that invoked this — its sources may read a
    /// disk, a mailbox or a service — so the place stands on a later pass, never a frame's cost.
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
        /// The size of the rectangle the person dragged, zero for a click: what is found is laid
        /// out to it.
        #[serde(default)]
        width: i64,
        #[serde(default)]
        height: i64,
        /// The part of the canvas the person saw when they pointed, in canvas pixels (zero size
        /// when the caller names none): an answer's views stand in free room inside it.
        #[serde(default)]
        seen_x: i64,
        #[serde(default)]
        seen_y: i64,
        #[serde(default)]
        seen_width: i64,
        #[serde(default)]
        seen_height: i64,
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
    /// `beside` — the turn that asked, by the address of its message — or beside the place in
    /// front when it names none. A resource already open is brought forward instead. The action API
    /// resolves it into the `Place` (or `Activate`) it makes before the step is recorded, so the
    /// record holds ids. Appended last.
    Open {
        resource: String,
        #[serde(default)]
        view: String,
        #[serde(default)]
        beside: String,
    },
    /// A place shows another resource: the place keeping its id, its strip and its place on the
    /// canvas, and the window it is now showing `resource` through `view` (empty for the
    /// resource's primary one). This is the one step an answered canvas query takes
    /// (shell.md §8, "Finding what to place"): the place the query stood in re-points at the
    /// channel it found, so the person's place is the same place, and Undo puts the query back.
    /// A resource a place cannot show refuses. Appended last.
    Show {
        place: u64,
        resource: String,
        #[serde(default)]
        view: String,
    },
    /// Ask the app the query `place` shows again for what the person typed: the query goes back to
    /// searching and the app's search action runs once more. Resolved where the app's action is
    /// ([`LayoutActions`]), never in the record: the record holds no running query. Appended last.
    RetryQuery { place: u64 },
    /// Give the place `place` the size `width` × `height` canvas pixels, its corner where it
    /// stands and every other place where it is: what dragging a place's edge or corner on the
    /// canvas runs (`canvas.md` §2). Appended last.
    Size { place: u64, width: i64, height: i64 },
    /// Report that the hugging place `place` is drawn `height` canvas pixels tall now: what its
    /// canvas sees of what it shows, up to the place's own height. View state, never an
    /// arranging step: it is kept as nobody's change of the layout, and the free area beside
    /// a place checks what it takes on the canvas by it (`CanvasPlace::occupied`, canvas.md §1,
    /// §7). Appended last.
    Drawn { place: u64, height: i64 },
    /// Remove the empty row `row` (a strip in a shell's bar) from its workspace, as the strip's own
    /// menu does. A row that holds a window, or a workspace's only row, refuses: the windows go
    /// first, and every workspace keeps a place to work. Appended last.
    RemoveRow { row: u64 },
}

/// Where a place made on the layout now goes: the workspace in front, its row, and the ids the
/// place's own strip and the place take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Surface {
    pub workspace: WorkspaceId,
    pub row: u64,
    pub strip: u64,
    pub place: u64,
}

/// The system's search, as the layout reaches it: the same `find` the
/// palette and an agent's `tilo` tool run. The session sets it, so text typed
/// on the canvas finds a resource through the one index and the one ranking,
/// never a search of its own (shell.md §8, "Finding what to place").
///
/// A host whose search may wait for a source (its sources read a disk, a
/// mailbox or a service off the thread that asked) hands the find to `done`
/// later rather than answering it here: the host's own loop, which runs the
/// action, must never wait on one.
pub type Find = Rc<dyn Fn(&str, FindDone)>;

/// Where a [`Find`] puts what its text found, when it answers later than the
/// call: the hits, in the ranking's order.
pub type FindDone = Box<dyn FnOnce(Vec<tilo_desktop_search::Hit>)>;

/// What a text typed on the canvas names among `hits`, as the canvas places it:
/// the resource a place can show ([`placed`]), or `None`. The one reading of a
/// search's order the canvas and an agent's `find` share, so both lead with the
/// same row.
#[must_use]
pub fn found_resource(
    hits: &[tilo_desktop_search::Hit],
    text: &str,
    shows: impl Fn(&str) -> bool,
) -> Option<String> {
    let at = placed(hits, text, shows)?;
    let (resource, _) = placeable(hits.get(at)?)?;
    Some(resource.to_owned())
}

/// What a text typed on the canvas places: the resource it names ([`found_resource`]) and the view
/// that resource stands through for these words ([`Target::view_for`]): "my mails" is Mail's root
/// through its place view, the inbox's list, and "Mail" the same root through its whole window.
/// `target` reads a resource as the session publishes it, `None` for one no place can show. Text
/// typed on the canvas and the REPL's `place_found` resolve through it; an agent's `find` leads with
/// the same resource, and its `place` of it takes the same place view (`LayoutActions::place_as_content`).
#[must_use]
pub fn found_place(
    hits: &[tilo_desktop_search::Hit],
    text: &str,
    target: impl Fn(&str) -> Option<Target>,
) -> Option<(String, String)> {
    let resource = found_resource(hits, text, |resource| target(resource).is_some())?;
    let view = target(&resource).map_or_else(String::new, |target| target.view_for(text));
    Some((resource, view))
}

/// Whether `text` names the app called `title` and nothing else: its name, with at most the words
/// that ask for an app ([`ui::search::system::APP_WORDS`]) around it. "Mail" and "open Mail" do;
/// "my mails", "mails" and "my inbox" name what Mail holds.
fn names_app(text: &str, title: &str) -> bool {
    let title = tilo_ui_base::text_match::fold_search_text(title);
    let title: Vec<&str> = title.split_whitespace().collect();
    if title.is_empty() {
        return false;
    }
    let text = tilo_ui_base::text_match::fold_search_text(text);
    let words: Vec<&str> = text
        .split_whitespace()
        .filter(|word| !ui::search::system::APP_WORDS.contains(word))
        .collect();
    words == title
}

/// What the canvas does with text that is a question, or that nothing matches (shell.md §8.1,
/// "Asking from the canvas"; canvas.md §3): the session sends it as the person to the workspace's
/// agent, as a send-to box sends, and Chat posts it in their conversation with the agent. It
/// answers the number the ask goes by: the turn its message opens stands where the person typed
/// once Chat said which message it posted ([`Layout::stand_asked`]). `None` when the session
/// cannot ask — no workspace to ask in, or the send was refused — and the text then fails to
/// place as it always did.
pub type Ask = Rc<dyn Fn(&str) -> Option<u64>>;

/// What an ask made, as Chat's record holds it: the conversation the person's message was posted
/// in, and the turn it opened (the message and the agent's reply to it), each by the resource it
/// is published as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asked {
    /// The conversation, by its address (`chat.workspace/channels/<id>`).
    pub conversation: String,
    /// The person's message that asked, by its id in the conversation: with the conversation, what
    /// an agent's step in the turn names as the turn it is in ([`turn_key`]).
    pub message: String,
    /// The turn, by its address: the resource whose `turn` view stands at the caret.
    pub turn: String,
    /// The conversation as its app publishes it (`chat.workspace.channels:<id>`): what the turn
    /// places beside itself when the person asks for the whole conversation.
    pub opened_as: String,
}

/// The query the platform has indexed instead of an answer: the resources it found, with the
/// name each goes by and the kind of resource it is, or why it could not find them.
pub type Answer = Result<Vec<AnswerRef>, String>;

/// One resource an outside origin's query found: its address in its app, the name it goes by, and
/// its kind as its app publishes it (`tube.channel`). The same three words a place's record is
/// named by (`session.layout_ref`), so the query's rows and a place's record are one shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnswerRef {
    pub address: String,
    pub title: String,
    pub kind: String,
}

/// What an app answered an origin's search with: the app asked, its kind as the app publishes it
/// (`tube.channel`), what it held of that kind before the search (so what it holds afterwards that
/// it did not then is the answer), and how the invocation went. `None` when the app could not be
/// asked at all.
pub type OriginAnswer = Option<AskOriginAnswer>;

/// What a text typed on the canvas does when no resource it names is held: ask the app whose type
/// it names for what it says, through that app's own search action
/// (`outside:<origin>.search`, shell.md §8, "Finding what to place", search.md §7, "Who
/// indexes"). The app's own action runs the text, and its result is read here: a refusal, a
/// failure, or the run itself is the answer's outcome (shell.md §8). `None` for a text that names
/// no app's type the platform can ask, so the text fails to place as it always did; `Some(Err)` is
/// the invocation's own failure, which the query stands as `.failed` with.
///
/// Answers the app asked, its kind as the app publishes it (`tube.channel`), what it held of that
/// kind before the search (so what it holds afterwards that it did not then is the answer), and
/// how the invocation went.
pub type AskOrigin = Rc<dyn Fn(&str) -> Option<AskOriginAnswer>>;

/// [`AskOrigin`] as one pass lends it: what it answers, without owning the ask.
pub type AskOriginRef<'a> = &'a dyn Fn(&str) -> Option<AskOriginAnswer>;

/// What the app's own search action answered the ask with, as [`AskOrigin`] gives it: the text it
/// asked, its kind as the app publishes it (`tube.channel`), what it held of that kind before the
/// search (so what it holds afterwards that it did not then is the answer), and how the invocation
/// went.
pub type AskOriginAnswer = (
    String,
    String,
    std::collections::BTreeSet<String>,
    SearchOutcome,
);

/// The outcome of the invocation of an app's search action: what the query is told when the app
/// refused to search or the run failed before it could.
pub type SearchOutcome = Result<(), String>;

/// Where the person pointed when they typed: the canvas point, the size of the rectangle they
/// dragged (`None` for a click), and the part of the canvas they saw.
type Pointed = ((i64, i64), Option<(i64, i64)>, Option<CanvasRect>);

/// What a text typed on the canvas found, and where it stands: the inputs of
/// [`Layout::place_found`] besides the layout itself.
#[derive(Clone, Copy)]
pub struct Found<'a> {
    /// The person the place is made as.
    pub asked: &'a Actor,
    /// The text they typed.
    pub text: &'a str,
    /// The resource's view, empty for its primary one.
    pub view: &'a str,
    /// The strip the place joins as a tab, else one of its own.
    pub strip: Option<u64>,
    /// Where on the canvas it stands.
    pub at: (i64, i64),
    /// The size of the rectangle the person dragged, when they dragged one: what is found fills
    /// it, and an answer's views fill it (canvas.md §1). `None` for a click.
    pub size: Option<(i64, i64)>,
    /// The part of the canvas the person saw when they pointed, when the caller says: the views
    /// an answer stands beside its turn take free room inside it (canvas.md §7, "Everything
    /// stays in view").
    pub seen: Option<CanvasRect>,
    /// The hits the text found, in the ranking's order, from the system's [`Find`]: answered
    /// before the place is made, so a host whose search waits for a source does so on its own
    /// thread and never in the pass that places.
    pub hits: &'a [tilo_desktop_search::Hit],
    /// The ask for text that asks, [`Ask`].
    pub ask: &'a dyn Fn(&str) -> Option<u64>,
    /// The ask of an app whose type the text names, when nothing it names is held, [`AskOrigin`]:
    /// `None` when the platform can ask no app for the text.
    pub ask_origin: AskOriginRef<'a>,
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
/// hit the layout cannot place does not stand in the way of one it can.
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

/// The app a window shows, by its id, when its tab holds a window at all.
pub fn tab_app(tile: &tilo_desktop_model::TileContent) -> Option<String> {
    use tilo_desktop_model::TileContent;
    match tile {
        TileContent::Foreign { app_id, .. }
        | TileContent::LocalApp { app_id, .. }
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
/// `root-<type>` after the app's own id, so `tl:so.tilo.mail:root-su4h67i5/ie99c2c19-72`
/// has the root `tl:so.tilo.mail:root-su4h67i5`. `None` for a resource with no app id or no
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

/// A resource as the layout makes a place of it: the app publishing it, its views (the primary
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
    /// The view a place shows the resource through when it stands as content on its own
    /// (`place view v for T`), when its app declares one: an app's root whose default view is the
    /// app's whole window shows this when the words that placed it name the resource rather than
    /// the app. `None` where the default view is the resource's own.
    pub place_view: Option<String>,
    /// What [`Self::place_view`] is called, as its app publishes the view (`All inboxes`, `All
    /// notes`): what a place shown through it is captioned, since it shows the thing the view
    /// names rather than the record the resource's own label is (an app's root goes by its first
    /// account or record).
    pub place_title: String,
    /// What its app is called (`Mail`): the words that, typed alone, name the app rather than the
    /// resource, so its whole window stands (canvas.md §7, "The thing, never the app").
    pub app_title: String,
}

impl Target {
    /// The view a place made from `text` shows the resource through: its place view, unless the
    /// text names the app alone ("Mail", "open Mail"), which is the app's whole window, its primary
    /// view. Empty for the primary view, as `place` takes it (canvas.md §7).
    #[must_use]
    pub fn view_for(&self, text: &str) -> String {
        match &self.place_view {
            Some(view) if !names_app(text, &self.app_title) => view.clone(),
            _ => String::new(),
        }
    }

    /// What a place showing the resource through `view` is called: the place view's own name when
    /// it stands through that view ([`Self::place_title`]), else the resource's label.
    #[must_use]
    pub fn title_through(&self, view: &str) -> String {
        let placed = self
            .place_view
            .as_deref()
            .is_some_and(|place| place == view.trim());
        if placed && !self.place_title.trim().is_empty() {
            self.place_title.trim().to_owned()
        } else {
            self.title.clone()
        }
    }

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

impl LayoutAction {
    /// The action as an operation's payload decodes, when it is one.
    #[must_use]
    pub fn decode(type_id: &str, payload: &[u8]) -> Option<Self> {
        tilo_failure::ignored!(
            "an operation of another type, or one that does not read back, is not this action",
            (type_id == OPERATION)
                .then(|| serde_json::from_slice(payload).ok())
                .flatten()
        )
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
    /// Whether a layout must wait.
    #[must_use]
    pub const fn holds(self) -> bool {
        self.gesture || self.typing
    }
}

/// What a window showing `app_id` and calling itself `title` is called to the person
/// ("Groceries — Notes"), as the host names it: the session's app lists and desktop entries on a
/// device, the registry's names on the page.
pub type Names = Rc<dyn Fn(&str, &str) -> String>;

/// What a host calls what the layout publishes, in the person's language: the message `key`
/// of its catalogue with `args`, and `source` where its catalogue holds none.
pub type Words = Rc<dyn Fn(&str, &[(&str, String)], &str) -> String>;

/// The words of a host that keeps no catalogue: what each source says.
#[must_use]
pub fn source_words() -> Words {
    Rc::new(|_, _, source| source.to_owned())
}

/// Where a host keeps a layout: the document its changes are written in, and the history a
/// step of it is taken back from. The session keeps the workspace's layout, which every member
/// shows; a page keeps its own copy of the layout.
pub trait Layouts {
    /// Take back (`redo` false) or bring back `person`'s last step of the layout.
    fn step(&self, person: &str, redo: bool) -> Stepped;
    /// The last step of the layout `person` can take back, as who made it and what it did.
    fn last_step(&self, person: &str) -> Option<(tilo_actions::Actor, String)>;
    /// Take back every change the invocation `invocation` made of the layout, for `person`:
    /// whether any was. A host that keeps no shared history has none to take back.
    fn take_back(&self, _person: &str, _invocation: &str) -> Result<bool, String> {
        Ok(false)
    }
    /// Note that `asked` made `action` of the layout `before`, which made it `tree`, and that the
    /// windows in `shows` show the records they name: what the change the layout now makes is named
    /// by.
    fn acted(
        &self,
        before: &Tree,
        tree: &Tree,
        asked: &Actor,
        action: &LayoutAction,
        shows: &[(TabId, String)],
    );
    /// The record each window shows, by its tab: what a place put on a record was placed on.
    fn shown(&self) -> BTreeMap<TabId, String>;
    /// The window the person has in front, read off the layout this host keeps: the active tab
    /// of the workspace in front's strip and group in its record, and, where the record names
    /// none, the first window it holds. `None` for a host that keeps no such record, and for a
    /// layout whose record holds no window.
    fn front_tab(&self) -> Option<TabId> {
        None
    }
    /// What `person`'s recents are read from in the workspace in front: every open its layout's
    /// history holds, and when they cleared their recents. A host that keeps no shared history
    /// has none.
    fn recent_opens(&self, _person: &str) -> RecentOpens {
        RecentOpens::default()
    }
    /// Clear `person`'s recents of `scope` up to `at` (seconds since the epoch), in their own view
    /// of the workspace in front's layout: the layout's history is the workspace's and stays whole.
    /// Whether it was kept.
    fn hide_recents(
        &self,
        _person: &str,
        _scope: &tilo_desktop_model::recents::Scope,
        _at: i64,
    ) -> bool {
        false
    }
}

/// `person`'s recents among `opens`, as a shell reads them: each resource once, at its latest open
/// after what they cleared of its app or type, theirs first and then the others', newest first,
/// at most [`RECENTS_PER_APP`] of each app. A resource `catalog` does not hold — one the session
/// does not publish to this person, because they cannot read it or it is gone — is left out.
/// `name` gives the name a member goes by.
#[must_use]
pub fn recent_readings(
    opens: &RecentOpens,
    person: &str,
    catalog: &Catalog,
    name: impl Fn(&str) -> String,
) -> Vec<LayoutRecentReading> {
    let ranked = tilo_desktop_model::recents::recent(
        &opens.opened,
        person,
        |open| {
            catalog.get(&open.resource).is_some_and(|target| {
                open.at
                    > tilo_desktop_model::recents::hidden_before(
                        &opens.hidden,
                        &target.app,
                        &target.kind,
                    )
            })
        },
        usize::MAX,
    );
    let mut per_app: BTreeMap<&str, usize> = BTreeMap::new();
    let mut recents = Vec::new();
    for recent in &ranked {
        let Some(target) = catalog.get(&recent.resource) else {
            continue;
        };
        let listed = per_app.entry(target.app.as_str()).or_default();
        if *listed >= RECENTS_PER_APP {
            continue;
        }
        *listed = listed.saturating_add(1);
        recents.push(LayoutRecentReading {
            key: recent.resource.clone(),
            resource: recent.resource.clone(),
            title: target.title.clone(),
            origin_app: target.app.clone(),
            app_title: target.app_title.clone(),
            kind: target.kind.clone(),
            by: name(&recent.by),
            at: recent.at,
            mine: recent.mine,
        });
    }
    recents
}

/// What a person's recents are read from ([`Layouts::recent_opens`]).
#[derive(Clone, Debug, Default)]
pub struct RecentOpens {
    /// Every open of the workspace in front's layout, oldest first.
    pub opened: Rc<Vec<tilo_desktop_model::recents::Opened>>,
    /// When the person cleared their recents, by scope
    /// ([`tilo_desktop_model::recents::Scope::key`]).
    pub hidden: BTreeMap<String, i64>,
    /// Moves whenever either does.
    pub revision: u64,
}

/// What a step of the layout came to.
pub enum Stepped {
    /// There was no step of the person's to take.
    None,
    /// The step was taken, and the host shows what it came to itself.
    Taken,
    /// The step was taken, and this is the layout it left.
    Tree(Box<Tree>),
}

impl Layouts for LayoutDocument {
    fn step(&self, person: &str, redo: bool) -> Stepped {
        let stepped = if redo {
            self.redo(person)
        } else {
            self.undo(person)
        };
        match stepped.then(|| self.tree()).flatten() {
            Some(tree) => Stepped::Tree(Box::new(tree)),
            None => Stepped::None,
        }
    }

    fn last_step(&self, person: &str) -> Option<(tilo_actions::Actor, String)> {
        LayoutDocument::last_step(self, person)
    }

    fn acted(
        &self,
        _before: &Tree,
        tree: &Tree,
        asked: &Actor,
        action: &LayoutAction,
        shows: &[(TabId, String)],
    ) {
        self.resource_action(
            tree,
            ui::time::unix_now_i64(),
            &crate::layout::step_context(asked, action, self.person()),
            shows,
        );
    }

    fn shown(&self) -> BTreeMap<TabId, String> {
        self.records().into_iter().collect()
    }
}

/// The person's layout, and the changes waiting for the person to finish what they are doing.
pub struct Layout {
    /// The person whose record it is.
    person: String,
    /// The shell itself: what the layout reads, and whose change every layout action is.
    handle: DesktopHandle,
    /// Where the layout is kept: the record the workspace shares, whose own changes are its history.
    layouts: Rc<dyn Layouts>,
    /// What the host calls a window, by [`Names`].
    names: Names,
    /// What the host calls what the layout publishes, by [`Words`].
    words: Words,
    /// The resources the session publishes now, by id: what a place made from a resource reads its
    /// app and views from ([`Catalog`]).
    catalog: Rc<RefCell<Catalog>>,
    /// Actions that arrived under a gesture or while the person typed: they run in order once it
    /// ends.
    waiting: RefCell<VecDeque<(Actor, LayoutAction, Option<tilo_actions::ActionContext>)>>,
    /// Each turn asked on the canvas, by the message that asked it ([`turn_key`]): where the places
    /// an agent's step in it makes stand. A turn is the session's while it runs, never the
    /// record's: the record holds the places it made.
    turns: RefCell<BTreeMap<String, TurnAnchor>>,
    /// Each turn's conversation as its app publishes it, by that address: the conversation's
    /// address the turns above were asked in.
    opened_as: RefCell<BTreeMap<String, String>>,
    /// The asks sent from the canvas whose turn Chat's record does not hold yet, oldest first.
    awaiting: RefCell<Vec<AwaitedTurn>>,
    /// Which message Chat posted for an ask, once it answered: what names a waiting ask's turn.
    posted: RefCell<Option<Posted>>,
    /// The queries the canvas stood while their app answers them, by the key each place shows it
    /// under (`session.layout_query`, shell.md §8): a query is no part of the layout's record
    /// — a place still shows one while it runs — so the session keeps it here, in step with the
    /// place that shows it.
    queries: RefCell<BTreeMap<String, LayoutQueryReading>>,
    /// The resources of a query's kind its app held when the query went out, by place: what the
    /// app holds now of that kind that it did not then is what the search found — the answer is
    /// told apart from what the app already had without a service's private words (shell.md §8).
    query_held: RefCell<BTreeMap<String, std::collections::BTreeSet<String>>>,
    /// The person's recents as last read, with the revisions of the opens and the catalogue they
    /// were read at: a reading made again with neither moved reads none of the history again.
    recents: RefCell<Option<(u64, u64, Vec<LayoutRecentReading>)>>,
    /// The windows that just closed, and what the last reading held of every window: what keeps a
    /// closed window in the reading while its frame flies back into the control it came from.
    departures: RefCell<Departures>,
}

impl Layout {
    /// `person` 's layout of `handle` 's layout, which `layouts` keeps, naming windows
    /// through `names` and what it publishes through `words`. A host brings over whatever an older
    /// build kept of the layout before it makes one: what it holds is the layout.
    #[must_use]
    pub fn new(
        person: &str,
        handle: DesktopHandle,
        layouts: Rc<dyn Layouts>,
        names: Names,
        words: Words,
    ) -> Self {
        Self {
            person: person.to_owned(),
            handle,
            layouts,
            names,
            words,
            catalog: Rc::new(RefCell::new(Catalog::default())),
            waiting: RefCell::new(VecDeque::new()),
            turns: RefCell::new(BTreeMap::new()),
            opened_as: RefCell::new(BTreeMap::new()),
            awaiting: RefCell::new(Vec::new()),
            posted: RefCell::new(None),
            queries: RefCell::new(BTreeMap::new()),
            query_held: RefCell::new(BTreeMap::new()),
            recents: RefCell::new(None),
            departures: RefCell::new(Departures::default()),
        }
    }

    /// The resources the session publishes now, by id: what a place made from a resource reads its
    /// app and views from. The session keeps it in step with the API's tree.
    #[must_use]
    pub fn targets(&self) -> Rc<RefCell<Catalog>> {
        Rc::clone(&self.catalog)
    }

    /// The surface the layout shows at `(x, y)` as a place made there stands: the
    /// workspace in front, its row, and the ids its strip and place take. Every id is
    /// this device's. A workspace with no row yet — one just joined, whose first window
    /// makes its row — is given a row here, so a person who joined a workspace can
    /// place something in it.
    /// # Errors
    /// The layout has no workspace in front.
    pub fn surface_at(&self) -> Result<Surface, String> {
        self.surface_at_strip(None)
    }

    /// The surface a place made in `strip` stands on: the strip's own workspace and row
    /// when it names one on the layout, so the place joins that strip as a tab; the
    /// workspace and row in front otherwise. The place's id is minted here.
    /// # Errors
    /// The layout has no workspace in front, or `strip` is not a strip on this layout.
    pub fn surface_at_strip(&self, strip: Option<u64>) -> Result<Surface, String> {
        self.surface_on(None, strip)
    }

    /// The surface a place made in `workspace` in `strip` stands on: as
    /// [`Self::surface_at_strip`], for a place the session makes in a workspace of its own
    /// choosing rather than the one in front. `workspace` `None` is the workspace in front.
    ///
    /// # Errors
    /// The layout has no such workspace, no workspace in front, or `strip` is not a strip on
    /// this layout.
    fn surface_on(
        &self,
        workspace: Option<WorkspaceId>,
        strip: Option<u64>,
    ) -> Result<Surface, String> {
        let tree = self.handle.read(Clone::clone);
        if let Some(strip) = strip {
            let held = tree.workspaces.iter().find_map(|workspace| {
                workspace.strips.iter().find_map(|row| {
                    row.window_groups
                        .iter()
                        .any(|group| group.id == strip)
                        .then_some((workspace.id(), row.id.0))
                })
            });
            let Some((workspace, row)) = held else {
                return Err("that strip is not in the layout".into());
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
                Some(asked) => tree
                    .workspaces
                    .iter()
                    .find(|held| held.id() == asked)
                    .ok_or("that workspace is not on this layout")?,
                None => tree
                    .active_workspace()
                    .ok_or("the layout has no workspace in front")?,
            };
            let row = tree.active_strip_for(workspace);
            (workspace.id(), row)
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
            workspace,
            row: row.0,
            strip,
            place,
        })
    }

    /// A row for the workspace `workspace` that holds none: the layout's own first row of it, named
    /// `Main` , which its first window stands in — as a workspace made here starts. Made before the
    /// place, so the record the place is put in is one the layout shows.
    fn make_row(&self, workspace: WorkspaceId) -> Result<StripId, String> {
        let id = StripId(self.handle.mint_id());
        let mut tree = self.handle.read(Clone::clone);
        let Some(held) = tree
            .workspaces
            .iter_mut()
            .find(|held| held.id() == workspace)
        else {
            return Err("the workspace in front went while a place was made".to_owned());
        };
        if let Some(row) = held.strips.first() {
            // Another part of the layout made it first: its row stands.
            return Ok(row.id);
        }
        held.strips.push(Strip::new(id, ""));
        tree.active_strip.insert(workspace, id);
        self.handle.replace_tree(tree);
        Ok(id)
    }

    /// Place the resource a text typed at `at` finds on the canvas, as `asked`'s:
    /// the top of the `hits` the system's search answered for it that a place can
    /// show ([`found_resource`]), resolved against what the session publishes now
    /// ([`Catalog`]), at `at` through the layout action
    /// [`LayoutAction::Place`] — one attributed, undoable change of the
    /// person's record (shell.md §8). `view` empty shows the resource's primary
    /// view. `strip` names the strip the found resource joins as a tab (a strip
    /// id on the layout), else it takes a strip of its own.
    ///
    /// The hits are the search's answer, which a host whose sources read a disk,
    /// a mailbox or a service asks for off the loop and passes here when it
    /// comes (`Find`, `FindDone`): this never searches.
    ///
    /// Returns the place's id, or `None` when nothing the text finds is a
    /// resource the layout can place.
    ///
    /// Text that *asks* — a question ([`tilo_desktop_search::answers::question::asks`]),
    /// or text nothing matches — goes to `ask` instead (shell.md §8.1,
    /// "Asking from the canvas"): it is sent to the workspace's agent, and the
    /// turn it opens stands where the person asked once Chat holds the message
    /// ([`Self::stand_asked`], canvas.md §3), so nothing is placed here yet and
    /// this answers `None`.
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
            size,
            seen,
            hits,
            ask,
            ask_origin,
            busy,
        } = inputs;
        let text = text.trim();
        if text.is_empty() {
            return Err("nothing is typed to place".into());
        }
        // A rectangle the person dragged is its size; a click names none, and what stands there
        // takes its own.
        let size = size.filter(|(width, height)| *width > 0 && *height > 0);
        let found = found_place(hits, text, |resource| {
            self.catalog.borrow().get(resource).cloned()
        });
        let (named, found_view) = match found {
            Some((resource, view)) => (Some(resource), view),
            None => (None, String::new()),
        };
        // A text naming an app's type (shell.md §8) that names nothing held asks that app, through
        // its own search action, for what it says: the query stands as a place of its own while
        // the app answers, and answers into it (`Self::answer_query`). A question goes to the
        // person's agent instead (shell.md §8.1), never both.
        if named.is_none()
            && !tilo_desktop_search::answers::question::asks(text)
            && let Some((app, kind, held, outcome)) = ask_origin(text)
        {
            let surface = self.surface_at_strip(strip)?;
            let key = surface.place.to_string();
            // The app's own search either ran (and its answer settles the query as it lands) or
            // refused / failed, which is this invocation's own result: the query stands `.failed`
            // with the reason it gave (shell.md §8).
            let (state, reason) = query_state_from_outcome(outcome);
            let query = LayoutQueryReading {
                key: key.clone(),
                text: text.to_owned(),
                app,
                kind,
                state,
                reason,
                matches: Vec::new(),
            };
            let query_resource = Item::Query.resource(&key);
            self.catalog
                .borrow_mut()
                .remember(&query_resource, query_target(&query));
            self.queries.borrow_mut().insert(key.clone(), query);
            // What the app held of the kind when it was asked: what it holds of that kind
            // afterwards that it did not hold then is what the search found.
            self.query_held.borrow_mut().insert(key, held);
            let action = LayoutAction::Place {
                workspace: surface.workspace,
                row: surface.row,
                strip: surface.strip,
                place: surface.place,
                resource: query_resource,
                view: String::new(),
                x: at.0,
                y: at.1,
                // The query stands where the person pointed, filling the rectangle they dragged,
                // so what it finds takes that place.
                width: size.map_or(DEFAULT_WIDTH, |(width, _)| width),
                height: size.map_or(DEFAULT_HEIGHT, |(_, height)| height),
                float: None,
                hugs: false,
            };
            self.ask(asked, action, busy)?;
            return Ok(Some(surface.place));
        }
        // A question is answered even when it happens to match something the search holds: the
        // person asked, so the text they typed is a message to their agent, not a thing to stand
        // (shell.md §8.1).
        let asks_agent = tilo_desktop_search::answers::question::asks(text) || named.is_none();
        if asks_agent {
            let Some(sent) = ask(text) else {
                return Err(format!("nothing matches {text:?}"));
            };
            // The turn stands where the person asked once Chat says which message it posted.
            self.await_turn(asked, sent, strip, (at, size), seen);
            return Ok(None);
        }
        let Some(resource) = named else {
            return Err(format!("nothing matches {text:?}"));
        };
        // What the text finds stands where the person pointed: at the point at its own size, or
        // laid out to the rectangle they dragged (canvas.md §1). A second place of a note is the
        // person's to make.
        let (at, width, height, hugs) = self.found_size(at, size, seen);
        let surface = self.surface_at_strip(strip)?;
        let action = LayoutAction::Place {
            workspace: surface.workspace,
            row: surface.row,
            strip: surface.strip,
            place: surface.place,
            resource,
            // The view the caller named, else the one these words place it through.
            view: if view.trim().is_empty() {
                found_view
            } else {
                view.to_owned()
            },
            x: at.0,
            y: at.1,
            width,
            height,
            float: None,
            hugs,
        };
        self.ask(asked, action, busy)?;
        Ok(Some(surface.place))
    }

    /// The place `place` shows what `answer` says, when the app the query asked has answered:
    /// exactly one match of its type re-points the place at that resource (one layout step, so
    /// Undo puts the query back); several or none leave the query with what it found or
    /// did not. The layout is unchanged by this call alone: the re-point is one [Show] step.
    ///
    /// Returns the resource the place now shows, when it re-pointed, so a caller says so.
    fn answer_query(
        &mut self,
        asked: &Actor,
        place: u64,
        answer: Answer,
        busy: Busy,
    ) -> Option<String> {
        let mut query = self.queries.borrow().get(&place.to_string()).cloned()?;
        let (state, reason, found) = match answer {
            Answer::Ok(matches) => (String::new(), String::new(), matches),
            Answer::Err(reason) => ("failed".to_owned(), reason, Vec::new()),
        };
        let key = query.key.clone();
        query.matches = found
            .into_iter()
            .enumerate()
            .map(|(at, found)| LayoutRefReading {
                key: format!("{key}/{at}"),
                address: found.address,
                title: found.title,
                kind: found.kind,
            })
            .collect();
        let (state, kind) = settle(&state, &query.matches);
        query.state.clone_from(&state);
        query.kind.clone_from(&kind);
        query.reason.clone_from(&reason);
        // One confident match of the type: the place becomes it, the same place.
        if state == "one"
            && let Some(only) = query.matches.first().cloned()
        {
            let action = LayoutAction::Show {
                place,
                resource: only.address.clone(),
                view: String::new(),
            };
            // A query answered while the person drags or types waits, as any layout action does;
            // the answer is kept either way and the place re-points when they end.
            if self.ask(asked, action, busy).is_ok() {
                self.queries.borrow_mut().remove(&place.to_string());
                self.query_held.borrow_mut().remove(&place.to_string());
                return Some(only.address);
            }
        }
        self.queries.borrow_mut().insert(place.to_string(), query);
        None
    }

    /// Ask the app again what the query `place` shows looks for: its state goes back to
    /// `searching` with nothing found yet, and the app's own search runs the text once more
    /// (`ask_origin`). What the app answers settles the query as any answer does
    /// ([`Self::answer_query`]).
    ///
    /// # Errors
    /// `place` shows no query, or the platform can ask no app for its text.
    pub fn retry_query(
        &mut self,
        _asked: &Actor,
        place: u64,
        ask_origin: AskOriginRef<'_>,
        _busy: Busy,
    ) -> Result<(), String> {
        let Some(mut query) = self.queries.borrow().get(&place.to_string()).cloned() else {
            return Err("that place shows no search to look for again".to_owned());
        };
        let Some((app, kind, held, outcome)) = ask_origin(&query.text) else {
            return Err("the platform can ask no app for it".to_owned());
        };
        // The retried invocation's own result: running again, or the reason it failed.
        let (state, reason) = match outcome {
            Ok(()) => ("searching".to_owned(), String::new()),
            Err(reason) => ("failed".to_owned(), reason),
        };
        query.reason = reason;
        query.matches.clear();
        query.app = app;
        query.kind = kind;
        query.state = state;
        self.queries.borrow_mut().insert(place.to_string(), query);
        self.query_held.borrow_mut().insert(place.to_string(), held);
        Ok(())
    }

    /// The queries the canvas stands that still search, by place: what the host answers with
    /// [`Self::answer_query_of`] once the app's own records hold what it found (shell.md §8).
    #[must_use]
    pub fn queries_running(&self) -> Vec<LayoutQueryReading> {
        self.queries
            .borrow()
            .values()
            .filter(|query| query.state == "searching")
            .cloned()
            .collect()
    }

    /// Settle the query a place stands with what its app answered, by the query's key: exactly one
    /// match re-points the place ([`LayoutAction::Show`], one step Undo takes back), and
    /// several, none or a failure leave the query with what it found or did not. The one
    /// layout step is made as the person's.
    pub fn answer_query_of(&mut self, key: &str, answer: Answer, busy: Busy) {
        let Ok(place) = key.parse::<u64>() else {
            return;
        };
        let asked = Actor::from(MemberId::person(self.person.clone()));
        self.answer_query(&asked, place, answer, busy);
    }

    /// The place already showing `resource` , if one stands: what text typed again brings forward
    /// rather than placing a second time.
    #[must_use]
    pub fn place_of(&self, resource: &str) -> Option<u64> {
        let records = self.window_resources();
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
    /// mailbox, a conversation, a notebook or one of its entries brings forward rather than opening
    /// a second place of the same app's record (`shell.md` §3, "Activating something already shown
    /// goes to it"). Opening the root and then one view inside it is one thing open, not two
    /// ([`Self::open_place`] re-points that place at the entry asked for).
    #[must_use]
    pub fn place_of_root(&self, resource: &str) -> Option<u64> {
        let records = self.window_resources();
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

    /// Bring the place `place` forward as `asked`'s step, for a caller that is raising what
    /// already stands rather than making it: a place already in front is where it should be, so
    /// the activation that would refuse ("that place is in front already") is no failure — a
    /// text that asks posts its message and then brings the conversation forward, and the ask
    /// has succeeded whether or not that place was in front.
    ///
    /// # Errors
    /// Why the place could not be activated at all.
    fn bring_forward(&mut self, asked: &Actor, place: u64, busy: Busy) -> Result<(), String> {
        match self.ask(asked, LayoutAction::Activate { place }, busy) {
            Ok(_) => Ok(()),
            Err(why) if why == "that place is in front already" => Ok(()),
            Err(why) => Err(why),
        }
    }

    /// The system's `open` (`docs/designs/app-language/one-app-model.md` §3): stand `resource`
    /// with its view `view` (empty for its primary one) beside the place that shows `beside`'s
    /// resource — the turn that asked, by its [`turn_key`], or its conversation — or beside the
    /// place in front when
    /// `beside` names nothing. A resource already on the layout is brought forward instead,
    /// so opening what is shown raises it rather than opening a second place.
    ///
    /// Returns the place's id, or `None` when the layout already held one and brought it forward.
    ///
    /// # Errors
    /// The layout has no workspace in front, or `resource` names nothing published.
    pub(crate) fn open_place(
        &mut self,
        asked: &Actor,
        resource: &str,
        view: &str,
        beside: &str,
    ) -> Result<Option<u64>, String> {
        if let Some(held) = self.place_of_root(resource) {
            // Opening what is open, or a view of what is open, brings that place forward; one
            // already in front is where it should be, and asking to activate it again would refuse
            // ("that place is in front already"). Another entry of the same root — a second note
            // of the notebook a place shows — is what the person asked to see, so the place shows
            // it from now on: one place per root still, and the open is in the layout's history
            // like any other. The root itself asked again keeps what the place shows.
            let showing = self.window_resources().get(&held).cloned();
            if showing.as_deref() != Some(resource) && root_of(resource) != Some(resource) {
                let show = LayoutAction::Show {
                    place: held,
                    resource: resource.to_owned(),
                    view: view.to_owned(),
                };
                self.ask(asked, show, Busy::default())?;
            }
            self.bring_forward(asked, held, Busy::default())?;
            return Ok(None);
        }
        // A turn asked on the canvas anchors what its agent's steps make (canvas.md §3): the
        // first place fills the rectangle the person dragged for the answer, and any other stands
        // beside the turn's own place. `beside` names the turn by the message that asked it, so
        // each turn of a conversation keeps its own room, and a step taken before the turn's view
        // stands goes where the turn will.
        // The room is the agent's answer's: the conversation itself, opened on request, stands
        // beside the turn instead.
        let room = if self.is_conversation_of(resource, beside) {
            self.turn_of(beside)
                .map(|turn| TurnRoom::Beside(turn.place, turn.seen))
        } else {
            self.take_turn_room(beside)
        };
        if let Some(TurnRoom::Fill(room)) = room {
            let surface = self.surface_at()?;
            let action = LayoutAction::Place {
                workspace: surface.workspace,
                row: surface.row,
                strip: surface.strip,
                place: surface.place,
                resource: resource.to_owned(),
                view: view.to_owned(),
                x: room.x,
                y: room.y,
                width: room.width,
                height: room.height,
                float: None,
                hugs: false,
            };
            self.ask(asked, action, Busy::default())?;
            return Ok(Some(surface.place));
        }
        // A turn asked elsewhere than the canvas stands what it opens beside its conversation.
        let conversation = beside
            .trim()
            .rsplit_once("/messages/")
            .map_or(beside, |(conversation, _)| conversation);
        // Beside a turn asked on the canvas, the answer's views take the free spot adjacent to
        // the turn, sized so the two stand on the screen together where they can, and the camera
        // pans to both (canvas.md §3, §7); with no spot adjacent free, the free space further
        // along the turn's columns.
        let in_view = match room {
            Some(TurnRoom::BesideAsked(turn, Some(seen))) => {
                self.room_in_view(Beside::Asked(turn), seen)
            }
            Some(TurnRoom::Beside(place, Some(seen))) => {
                self.room_in_view(Beside::Place(place), seen)
            }
            _ => None,
        };
        let at = match (in_view, room) {
            (Some(room), _) => Some((i64::from(room.x), i64::from(room.y))),
            (None, Some(TurnRoom::BesideAsked(turn, _))) => self.point_beside_rect(turn),
            (None, Some(TurnRoom::Beside(place, _))) => self.point_beside(place, None),
            _ => self
                .anchor_of(beside)
                .or_else(|| self.anchor_of(conversation))
                .and_then(|place| self.point_beside(place, None)),
        };
        let (width, height) = in_view.map_or((DEFAULT_WIDTH, DEFAULT_HEIGHT), |room| {
            (i64::from(room.width), i64::from(room.height))
        });
        let surface = self.surface_at()?;
        // Where the new place stands: the free space next to the anchor (`free_place_next_to`),
        // its trailing side first, then below it, then further down the two columns the anchor
        // starts. The point handed to the place action is free already, so the action's own
        // `free_place_near` keeps it as it is. With no anchor the place comes to the canvas'
        // origin, as a place made by `place` without a point does. Opening several things beside
        // one anchor therefore runs right, then down, never over the anchor or over each other,
        // and never above or to the left of it (`shell.md` §3, "a place next to where it came
        // from").
        let (x, y) = at.map_or((0, 0), |at| at);
        let action = LayoutAction::Place {
            workspace: surface.workspace,
            row: surface.row,
            strip: surface.strip,
            place: surface.place,
            resource: resource.to_owned(),
            view: view.to_owned(),
            x,
            y,
            width,
            height,
            float: None,
            // It names no size of its own: it hugs what it shows, up to the room beside the turn,
            // as a thing found at a click does (canvas.md §7, "Places hug what they show").
            hugs: true,
        };
        self.ask(asked, action, Busy::default())?;
        Ok(Some(surface.place))
    }

    /// The layout as a shell's views read it: every workspace, row, strip and place, each by
    /// its key, laid out as the shell's layout type holds it. This is what the session
    /// publishes on its api for a shell to draw, read off the layout as it stands.
    #[must_use]
    pub fn reading(&self) -> Reading {
        let records = self.window_resources();
        let catalog = self.catalog.borrow();
        let mut reading = self
            .handle
            .read(|tree| reading_of(tree, &records, &catalog, &self.names));
        reading.person = self.person_name();
        // How far the row in front is scrolled, as the desktop beside the tree presents it: the
        // shell draws that row at this offset, and what the person's wheel or fingers ask of it
        // goes back as the `pan` verb. A page showing a workspace on the web has no desktop
        // here, so its rows present no pan and scroll themselves.
        if let Some(pan) = self.handle.presentation().pan
            && let Some(row) = reading
                .rows
                .iter_mut()
                .find(|row| row.current && row.key == pan.strip)
        {
            row.pan = Some(pan.offset);
        }
        // A floating window is drawn where this device lays it out: where the record keeps it on
        // this display, or where the person's hand has it while they carry it.
        for (group, at) in self.handle.floats_drawn() {
            let key = group.to_string();
            if let Some(strip) = reading.strips.iter_mut().find(|strip| strip.key == key) {
                strip.float_at = [at.x0, at.y0, at.width(), at.height()]
                    .map(|side| tilo_ui_base::num::i64_of_f64(side.round()))
                    .to_vec();
            }
        }
        // A query lives while its place does: one whose place has gone (closed, replaced, undone)
        // is no longer the person's to see, and drops here.
        let places: std::collections::BTreeSet<String> = reading
            .places
            .iter()
            .map(|place| place.key.clone())
            .collect();
        self.queries
            .borrow_mut()
            .retain(|key, _| places.contains(key));
        self.query_held
            .borrow_mut()
            .retain(|key, _| places.contains(key));
        reading
            .queries
            .extend(self.queries.borrow().values().cloned());
        // A window that just closed keeps its frame, where it stood, while the shell draws it back
        // into the control the person opened it from; a window opening now comes out of the
        // control they pressed.
        let now = ui::time::now();
        let control = tilo_ui_base::motion::recent_input(now)
            .filter(|source| !source.lands_at_once())
            .and_then(|_| tilo_ui_base::motion::last_pointer());
        self.departures
            .borrow_mut()
            .keep(&mut reading, now, control);
        reading.recents = self.recents();
        reading
    }

    /// The person's recents in the workspace in front: each resource a window there came to show,
    /// once, at its latest open, theirs first and then the others', newest first, and at most
    /// [`RECENTS_PER_APP`] of each app. A resource the session does not publish to this person —
    /// one they cannot read, or that is gone — is left out, and so is what they cleared.
    fn recents(&self) -> Vec<LayoutRecentReading> {
        let opens = self.layouts.recent_opens(&self.person);
        let catalog = self.catalog.borrow();
        if let Some((opened, listed, held)) = self.recents.borrow().as_ref()
            && *opened == opens.revision
            && *listed == catalog.revision()
        {
            return held.clone();
        }
        let recents = recent_readings(&opens, &self.person, &catalog, |person| {
            self.member_name(person)
        });
        *self.recents.borrow_mut() = Some((opens.revision, catalog.revision(), recents.clone()));
        recents
    }

    /// Clear the person's recents of `scope` up to now: what they opened before stays in the
    /// workspace's history and leaves their list. Whether it was kept.
    pub fn clear_recents(&self, scope: &tilo_desktop_model::recents::Scope) -> bool {
        self.layouts
            .hide_recents(&self.person, scope, ui::time::unix_now_i64())
    }

    /// The name the workspace in front lists `person` by, or their id where it lists none.
    fn member_name(&self, person: &str) -> String {
        self.handle
            .read(|tree| {
                tree.active_workspace()?
                    .members
                    .iter()
                    .find(|member| member.id.person_id() == Some(person) && !member.name.is_empty())
                    .map(|member| member.name.clone())
            })
            .unwrap_or_else(|| person.to_owned())
    }

    /// The person's name as the workspace in front lists them, or their id where it lists no name.
    fn person_name(&self) -> String {
        self.handle
            .read(|tree| {
                tree.active_workspace()?
                    .members
                    .iter()
                    .find(|member| {
                        member.id.person_id() == Some(self.person.as_str())
                            && !member.name.is_empty()
                    })
                    .map(|member| member.name.clone())
            })
            .unwrap_or_else(|| self.person.clone())
    }

    /// The query `place` shows, if it shows one.
    #[must_use]
    pub fn query_of(&self, place: u64) -> Option<LayoutQueryReading> {
        self.queries.borrow().get(&place.to_string()).cloned()
    }

    /// Note the resources of a query's kind its app held when the query went out (`held`, their
    /// ids), so what the app holds of that kind afterwards that it did not hold before is what
    /// the search found. Called for a place showing a query and no others, by the host, which
    /// reads the app's records.
    pub fn note_held(&self, place: u64, held: std::collections::BTreeSet<String>) {
        self.query_held
            .borrow_mut()
            .entry(place.to_string())
            .or_insert(held);
    }

    /// The resources of a query's kind its app held when the query went out, by place: what
    /// [`Self::note_held`] kept.
    #[must_use]
    pub fn held_when_asked(&self, place: u64) -> Option<std::collections::BTreeSet<String>> {
        self.query_held.borrow().get(&place.to_string()).cloned()
    }

    /// The resource each window shows, by tab, as the layout keeps it: what a place put on a
    /// resource was placed on, what a window took up, and what its bar carries when a person picks
    /// it up, on every device of theirs that reads this layout.
    #[must_use]
    pub fn window_resources(&self) -> BTreeMap<TabId, String> {
        self.layouts.shown()
    }

    /// The window this device has in front: what the layout's own record says the person brought
    /// forward ([`Layouts::front_tab`]) — how a shell that draws its windows itself has none of
    /// the keyboard's — and, where the record names none yet, the window in front of the tree the
    /// shell drew.
    #[must_use]
    pub fn tab_in_front(&self) -> Option<TabId> {
        self.layouts
            .front_tab()
            .or_else(|| self.handle.read(Tree::tab_in_front))
    }

    /// Every record a place of the layout shows, by the id of the workspace (a hex device key) the
    /// place stands in: what the outside services keeping those records in step are due for. A
    /// workspace's places and the records of the apps' windows there are all of them, on every
    /// workspace of the layout, not only the one in front.
    #[must_use]
    pub fn records_by_workspace(&self) -> BTreeMap<String, std::collections::BTreeSet<String>> {
        let records = self.window_resources();
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

    /// The layout as the session publishes it now: what [`resources`] publishes, with a
    /// resource of its own for every row, strip and place, named by its record key. A press
    /// names the thing it presses (`shell:place:<key>`), so a key is never read as a number,
    /// and a place's own actions act on that place.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceSpec> {
        let reading = self.reading();
        let rows = reading.rows.iter().map(|row| {
            tilo_resource::LiveRow::typed::<LayoutRow>(Item::Row.resource(&row.key), row_label(row))
                .into_resource()
        });
        let strips = reading.strips.iter().map(|strip| {
            published::carried(
                tilo_resource::LiveRow::typed::<LayoutStrip>(
                    Item::Strip.resource(&strip.key),
                    strip_label(&self.words, strip),
                )
                .into_resource(),
            )
        });
        let places = reading.places.iter().map(|place| {
            published::carried(
                tilo_resource::LiveRow::typed::<Place>(
                    Item::Place.resource(&place.key),
                    place_label(&self.words, place),
                )
                .into_resource(),
            )
        });
        // A query a place shows while its app answers is a resource of its own beside the places,
        // so an open place draws it and its rows may be pressed (`query_resource`).
        let queries = reading
            .queries
            .iter()
            .map(|query| query_resource(query, &self.words));
        vec![
            layout_resource(&self.words)
                .with_children(rows.chain(strips).chain(places).chain(queries)),
            history_resource(&self.words),
        ]
    }

    /// Where what it publishes stands ([`Published::revision`]): the key and label of each row,
    /// strip, place and query [`Self::resources`] publishes, and the layout's own revision, so one
    /// that came, went or was renamed changes it. Their actions follow from their kinds alone, and
    /// what the layout and its history are called from the locale the reader keys on, so neither
    /// is built to be told apart.
    #[must_use]
    pub fn revision(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::hash::DefaultHasher::new();
        let reading = self.reading();
        for row in &reading.rows {
            (0_u8, &row.key, row_label(row)).hash(&mut hasher);
        }
        for strip in &reading.strips {
            (1_u8, &strip.key, strip_label(&self.words, strip)).hash(&mut hasher);
        }
        for place in &reading.places {
            (2_u8, &place.key, place_label(&self.words, place)).hash(&mut hasher);
        }
        for query in &reading.queries {
            (3_u8, &query.key, &query.text).hash(&mut hasher);
        }
        self.handle.revision().hash(&mut hasher);
        hasher.finish()
    }

    /// The person whose layout it is.
    #[must_use]
    pub fn person(&self) -> &str {
        &self.person
    }

    /// Carry out `action` as `asked`'s: one change of the record, attributed to them
    /// and undone as one step. Under an ongoing gesture or while the person types it
    /// waits ([`Self::waiting`]) and runs when they end. Returns whether it ran now.
    /// # Errors
    /// Why the action names something the layout does not hold.
    pub fn ask(&mut self, asked: &Actor, action: LayoutAction, busy: Busy) -> Result<bool, String> {
        if busy.holds() {
            // It runs later as the invocation it serves now.
            self.waiting.borrow_mut().push_back((
                asked.to_owned(),
                action,
                tilo_actions::invoking_context(),
            ));
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
        let Some((asked, action, serving)) = self.waiting.borrow_mut().pop_front() else {
            return Ok(());
        };
        tilo_actions::with_invoking_context(serving, || self.write(&asked, &action))
    }

    /// How many actions wait for the person to finish.
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.waiting.borrow().len()
    }

    /// The last step of the record's history, as who made it and what it did: what a history view
    /// names it by.
    #[must_use]
    pub fn last_step(&self) -> Option<(tilo_actions::Actor, String)> {
        let person = self.person.clone();
        self.layouts.last_step(&person)
    }

    /// The person's own change, made as their gesture's end says: their own
    /// input never waits for itself.
    ///
    /// # Errors
    /// Why the action names something the layout does not hold.
    pub fn ask_person(&mut self, action: &LayoutAction) -> Result<(), String> {
        self.write(&MemberId::person(self.person.clone()).into(), action)
    }

    /// Make each window the person floated or put back on the layout since the last call their own
    /// layout action: [`LayoutAction::Float`] or [`LayoutAction::Snap`].
    ///
    /// # Errors
    /// Why one of them could not be made; the others are made all the same.
    pub fn take_asked_floats(&mut self) -> Result<(), String> {
        let mut refused = Ok(());
        for asked in self.handle.take_asked_floats() {
            let action = match asked.float {
                Some(float) => LayoutAction::Float {
                    place: asked.tab,
                    float,
                },
                None => LayoutAction::Snap {
                    place: asked.tab,
                    row: asked.into.map(|(row, _)| row.0),
                    before: asked.into.and_then(|(_, before)| before),
                },
            };
            if let Err(why) = self.ask_person(&action) {
                refused = Err(why);
            }
        }
        refused
    }

    /// Undo `person` 's last change of the layout: one step of the layout's own history, taken
    /// back in the layout it was made in. Whether there was one.
    pub fn undo(&mut self, person: &str) -> bool {
        self.show(self.layouts.step(person, false))
    }

    /// Take back every change the invocation `invocation` made of the layout, wherever it stands
    /// in the layout's history. Whether any was.
    ///
    /// # Errors
    /// Why the layout could not be written.
    pub fn take_back(&self, invocation: &str) -> Result<bool, String> {
        self.layouts.take_back(&self.person, invocation)
    }

    /// Redo `person` 's last undone change of the layout.
    pub fn redo(&mut self, person: &str) -> bool {
        self.show(self.layouts.step(person, true))
    }

    /// Show a step of the layout, and whether there was one to take.
    fn show(&self, stepped: Stepped) -> bool {
        match stepped {
            Stepped::None => false,
            Stepped::Taken => true,
            Stepped::Tree(tree) => {
                self.handle.replace_tree(*tree);
                true
            }
        }
    }

    /// Make `action` one change of the layout, attributed to `asked` as its undo step: what the
    /// layout is, made where the layout is.
    fn write(&self, asked: &Actor, action: &LayoutAction) -> Result<(), String> {
        // An agent arranges the person's layout like the person does, except where they pinned
        // something: a pinned place is theirs to move (shell.md §5's default, "not what they
        // pinned").
        let by_the_person =
            asked.member_id().and_then(MemberId::person_id) == Some(self.person.as_str());
        let catalog = self.catalog.clone();
        let before = self.handle.read(Clone::clone);
        // How tall a place is drawn is the person's own canvas saying what it sees, never a step
        // of theirs and never a change of the layout's shape. It is kept on this shell's tree where
        // the canvas reads it, without building every view again: a canvas that rebuilt itself to
        // report would measure and report once more, and report again, a frame at a time. Nothing
        // is recorded, so Undo takes back the person's last step of arranging, never a place
        // growing as its words arrive.
        if let LayoutAction::Drawn { place, height } = action {
            let Some((workspace, group)) = self.handle.read(|tree| {
                tree.workspaces.iter().find_map(|held| {
                    let group = held
                        .strips
                        .iter()
                        .flat_map(|strip| &strip.window_groups)
                        .find(|group| group.tabs.iter().any(|tab| tab.id == *place))?
                        .id;
                    Some((held.id(), group))
                })
            }) else {
                return Err("that place is not in the layout".to_owned());
            };
            self.handle
                .report_drawn(workspace, group, clamp_side(*height));
            return Ok(());
        }
        // Bringing a place to the front gives its window the keyboard: the window in front is this
        // device's, never a change of the layout. A place already shown in its window only comes
        // forward, and the layout records nothing: pressing the window in front again is nothing.
        let brought = match action {
            LayoutAction::Activate { place } => Some(*place),
            _ => None,
        };
        if let Some(place) = brought {
            let (shown, in_front) = self.handle.read(|tree| {
                let shown = tree
                    .workspaces
                    .iter()
                    .flat_map(|workspace| &workspace.strips)
                    .flat_map(|strip| &strip.window_groups)
                    .any(|group| group.active_tab == place);
                let in_front = crate::desktop::navigation::active_route(tree)
                    .is_some_and(|route| route.tab == place);
                (shown, in_front)
            });
            if shown {
                if !in_front {
                    self.handle.focus_tab(place);
                }
                return Ok(());
            }
        }
        let mut tree = before.clone();
        if !change(&mut tree, action, by_the_person, &catalog)? {
            return Ok(());
        }
        // A place the person puts on the layout comes to the front in the same change, so the shell
        // shows what just arrived; one an agent puts beside its turn leaves the keyboard where the
        // person has it.
        if let LayoutAction::Place { place, .. } = action
            && by_the_person
        {
            bring_to_front(&mut tree, *place);
        }
        self.handle.replace_tree(tree.clone());
        if let Some(place) = brought {
            self.handle.focus_tab(place);
        }
        // The place a resource was put on shows that resource from now on,
        // and the layout keeps it beside the window it came with.
        // A place that shows another resource from now on says so in the layout's own record map,
        // so a restart opens the place on what it shows, as a place put on the canvas does.
        let shows: Vec<(TabId, String)> = match action {
            LayoutAction::Place {
                place, resource, ..
            }
            | LayoutAction::Show {
                place, resource, ..
            } => vec![(*place, resource.clone())],
            _ => Vec::new(),
        };
        // The layout is the record the workspace shares, and its own changes are its history: the
        // change this action made says who asked for it and what they did, and is one step of the
        // person's to take back.
        self.layouts.acted(&before, &tree, asked, action, &shows);
        Ok(())
    }
}

/// What `action`, asked for by `asked`, is: who asked and what they did, as the change of the layout
/// it makes names it. The layout is `person`'s, so an actor that is not them acts for them: an agent
/// arranging their layout works for them, and their undo takes its step back. A step of an
/// invocation ([`tilo_actions::invoking_context`]) names it and its run, so taking that invocation
/// back takes this step back too.
///
/// The person's own device hands its agent the session it runs, which is the delegation its change
/// carries: every workspace admits the agent's change by the preset that delegation gives it, as it
/// admits the agent's other calls, and an agent no grant names is refused there like anyone else.
pub fn step_context(
    asked: &Actor,
    action: &LayoutAction,
    person: &str,
) -> tilo_actions::ActionContext {
    let mut acting: tilo_actions::Acting = asked.clone().into();
    if !acting.is_person() {
        acting = acting
            .handed()
            .acting_for(tilo_actions::Acting::person(person));
    }
    tilo_actions::ActionContext {
        invoker: Some(tilo_actions::ActionInvoker::admitted(&acting)),
        what: Some(tilo_actions::Did::Verb(tilo_actions::VerbCall::new(
            tilo_actions::ActionKey::new("so.tilo.session", "layout", verb(action)),
            std::iter::empty(),
        ))),
        // A step of its own, and when it was made beside the person's steps of other records.
        gesture: Some(tilo_actions::ActionId::new()),
        ..tilo_actions::reading_invoking_context(|serving| tilo_actions::ActionContext {
            source_invocation: serving.and_then(|serving| serving.source_invocation),
            group: serving.and_then(|serving| serving.group.clone()),
            ..tilo_actions::ActionContext::default()
        })
    }
}

/// Make the place `place` the one in front of `tree`: its workspace, its strip, its window and
/// its tab, as a focus of it would.
fn bring_to_front(tree: &mut Tree, place: TabId) {
    let found = tree.workspaces.iter().find_map(|workspace| {
        workspace.strips.iter().find_map(|strip| {
            strip
                .window_groups
                .iter()
                .find(|group| group.tabs.iter().any(|tab| tab.id == place))
                .map(|group| (workspace.id(), strip.id, group.id))
        })
    });
    let Some((workspace, strip, group)) = found else {
        return;
    };
    tree.active = workspace;
    tree.active_strip.insert(workspace, strip);
    tree.set_active_group(strip, group);
    if let Some(held) = tree
        .workspaces
        .iter_mut()
        .flat_map(|workspace| &mut workspace.strips)
        .flat_map(|strip| &mut strip.window_groups)
        .find(|held| held.id == group)
    {
        held.activate_tab(place);
    }
}

/// One change of `tree` , as `action` names it. `by_the_person` is whether the person's own input
/// asked for it: an agent's move leaves a place the person pinned where it is. Whether anything
/// changed.
fn change(
    tree: &mut Tree,
    action: &LayoutAction,
    by_the_person: bool,
    catalog: &Rc<RefCell<Catalog>>,
) -> Result<bool, String> {
    match action {
        LayoutAction::Move {
            place,
            row,
            before,
            strip,
        } => move_place(tree, *place, StripId(*row), *before, *strip, by_the_person),
        LayoutAction::Resize { strip, permille } => Ok(resize(tree, *strip, *permille)),
        LayoutAction::Maximize { strip, maximized } => maximize(tree, *strip, *maximized),
        LayoutAction::Size {
            place,
            width,
            height,
        } => size(tree, *place, clamp_side(*width), clamp_side(*height)),
        LayoutAction::Drawn { place, height } => drawn(tree, *place, clamp_side(*height)),
        LayoutAction::Close { place } => close(tree, *place),
        LayoutAction::RemoveRow { row } => remove_row(tree, *row),
        LayoutAction::Activate { place } => activate(tree, *place),
        LayoutAction::Pin { place, pinned } => pin(tree, *place, *pinned),
        LayoutAction::Position { place, x, y } => {
            position(tree, *place, clamp_coord(*x), clamp_coord(*y))
        }
        // Resolved into a `Place` at invocation, where the system's search and what is published
        // now are; the record never holds the words.
        LayoutAction::PlaceFound { .. } => {
            Err("a place found by text is resolved before it reaches the record".into())
        }
        LayoutAction::Place {
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
            hugs,
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
                hugs: *hugs,
            },
        ),
        LayoutAction::Float { place, float } => float_place(tree, *place, *float),
        // Resolved into the `Place` (or `Activate`) it makes before it reaches the record, where
        // the layout's ids and the place it stands beside are known.
        LayoutAction::Open { .. } => {
            Err("an open is resolved into the place it makes before it reaches the record".into())
        }
        LayoutAction::Snap { place, row, before } => {
            snap_back(tree, *place, *row, *before, by_the_person)
        }
        LayoutAction::Show {
            place,
            resource,
            view,
        } => show_at(tree, catalog, *place, resource, view),
        // Resolved where the app's own search action is ([`LayoutActions`]), never in the record.
        LayoutAction::RetryQuery { .. } => {
            Err("a retried search is resolved before it reaches the record".into())
        }
    }
}

/// Give the strip `strip` the width `permille` of the workspace asks for. Whether the layout
/// changed.
fn resize(tree: &mut Tree, strip: GroupId, permille: u32) -> bool {
    let Some(group) = group_mut(tree, strip) else {
        return false;
    };
    let width = WindowWidth::Fraction(permille);
    let changed = group.width != width;
    group.width = width;
    changed
}

/// Maximize the strip `strip` (`maximized`), or restore it beside its row's other windows.
/// Whether the layout changed.
fn maximize(tree: &mut Tree, strip: GroupId, maximized: bool) -> Result<bool, String> {
    let group =
        group_mut(tree, strip).ok_or_else(|| "that strip is not in the layout".to_owned())?;
    let changed = group.maximized != maximized;
    group.maximized = maximized;
    Ok(changed)
}

/// Close the place `place`; its window goes with it. Whether the layout changed.
fn close(tree: &mut Tree, place: u64) -> Result<bool, String> {
    if tree.close_tab(place).is_some() {
        Ok(true)
    } else {
        Err("that place is not in the layout".into())
    }
}

/// Remove the empty row `row` from its workspace, as a strip's own menu does. A row holding a
/// window, or a workspace's only row, is refused. Whether the layout changed.
fn remove_row(tree: &mut Tree, row: u64) -> Result<bool, String> {
    let strip = tilo_desktop_model::StripId(row);
    for workspace in &mut tree.workspaces {
        let Some(at) = workspace.strips.iter().position(|held| held.id == strip) else {
            continue;
        };
        if workspace
            .strips
            .get(at)
            .is_some_and(|held| !held.window_groups.is_empty())
        {
            return Err("the row still holds a window".into());
        }
        if workspace.strips.len() <= 1 {
            return Err("a workspace keeps its last row".into());
        }
        workspace.strips.remove(at);
        return Ok(true);
    }
    Err("that row is not in the layout".into())
}

/// Bring the place `place` in front of the strip it stands in, as choosing its tab does. Whether
/// the layout changed.
fn activate(tree: &mut Tree, place: u64) -> Result<bool, String> {
    let Some(held) = group_holding(tree, place) else {
        return Err("that place is not in the layout".into());
    };
    if held.active_tab == place {
        return Err("that place is in front already".into());
    }
    held.activate_tab(place);
    Ok(true)
}

/// Pin the place `place` (`pinned`), so an agent's layout leaves it alone, or unpin it.
/// Whether the layout changed.
fn pin(tree: &mut Tree, place: u64, pinned: bool) -> Result<bool, String> {
    let Some(tab) = group_holding(tree, place)
        .and_then(|held| held.tabs.iter_mut().find(|tab| tab.id == place))
    else {
        return Err("that place is not in the layout".into());
    };
    let changed = tab.pinned != pinned;
    tab.pinned = pinned;
    Ok(changed)
}

/// Move the place `place` to `(x, y)` on its workspace's canvas. Whether the layout changed.
fn position(tree: &mut Tree, place: u64, x: i32, y: i32) -> Result<bool, String> {
    let (held, group) = holding(tree, place)?;
    Ok(held.layout.set_position(group, x, y))
}

/// Give the place `place` the canvas size `width` × `height`; whether the shell changed.
fn size(tree: &mut Tree, place: u64, width: u32, height: u32) -> Result<bool, String> {
    let Some((workspace, row, group, _)) = locate(tree, place) else {
        return Err("that place is not in the layout".into());
    };
    let Some(held) = tree.workspaces.get_mut(workspace) else {
        return Err("that place is not in the layout".into());
    };
    let Some(id) = held
        .strips
        .get(row)
        .and_then(|strip| strip.window_groups.get(group))
        .map(|group| group.id)
    else {
        return Err("that place is not in the layout".into());
    };
    Ok(held.layout.set_size(id, width, height))
}

/// Report how tall the hugging place `place` is drawn now, `height` canvas pixels: view state the
/// person's own canvas sends, never an arranging step. Whether the layout changed.
fn drawn(tree: &mut Tree, place: u64, height: u32) -> Result<bool, String> {
    let (held, group) = holding(tree, place)?;
    Ok(held.layout.set_drawn_height(group, height))
}

/// Float the place `place` above its workspace's windows at `float` .
fn float_place(tree: &mut Tree, place: u64, float: FloatPlace) -> Result<bool, String> {
    let (held, group) = holding(tree, place)?;
    Ok(held.layout.set_float(group, Some(float)))
}

/// Snap the floating place `place` back among its workspace's windows. When a drop named a row,
/// its window goes into that row whole, as a window of its own in the gap the drop named: before
/// the window holding the place `before`, or after the row's last. Left out, it returns to the
/// strip it stands in, unchanged. Whether the layout changed.
fn snap_back(
    tree: &mut Tree,
    place: u64,
    row: Option<u64>,
    before: Option<u64>,
    by_the_person: bool,
) -> Result<bool, String> {
    let (held, group) = holding(tree, place)?;
    let mut changed = held.layout.set_float(group, None);
    if let Some(row) = row {
        changed |= move_window(tree, group, StripId(row), before, by_the_person)?;
    }
    Ok(changed)
}

/// Move the window `group` whole into the row `row` of its workspace, before the window holding
/// the place `before` or after the row's last, as its own window. A window holding a place the
/// person pinned stays where it is unless they moved it themselves. Whether it moved.
fn move_window(
    tree: &mut Tree,
    group: GroupId,
    row: StripId,
    before: Option<u64>,
    by_the_person: bool,
) -> Result<bool, String> {
    let gone = "that window is not in the layout";
    let held = tree
        .workspaces
        .iter_mut()
        .find(|held| {
            held.strips
                .iter()
                .any(|strip| strip.window_groups.iter().any(|window| window.id == group))
        })
        .ok_or(gone)?;
    let windows = || held.strips.iter().flat_map(|strip| &strip.window_groups);
    let pinned = windows()
        .find(|window| window.id == group)
        .is_some_and(|window| window.tabs.iter().any(|tab| tab.pinned));
    if !by_the_person && pinned {
        return Err("the person pinned that place: it stays where it is".into());
    }
    let before = before
        .map(|place| {
            windows()
                .find(|window| window.tabs.iter().any(|tab| tab.id == place))
                .map(|window| window.id)
                .ok_or("that place is not in the layout")
        })
        .transpose()?;
    if before == Some(group) {
        return Ok(false);
    }
    let order = |held: &Workspace| -> Vec<(StripId, GroupId)> {
        held.strips
            .iter()
            .flat_map(|strip| {
                strip
                    .window_groups
                    .iter()
                    .map(|window| (strip.id, window.id))
            })
            .collect()
    };
    let was = order(held);
    tilo_desktop_model::commands::apply_command(
        held,
        &tilo_desktop_model::commands::DesktopCommand::MoveGroup {
            group,
            strip: row.0,
            before,
        },
        &mut || Err("a move mints nothing".into()),
        &mut |_, _| Err("a move opens no window".into()),
    )?;
    let moved = order(held) != was;
    if moved && by_the_person {
        let workspace = held.id();
        tree.active_strip.insert(workspace, row);
        tree.set_active_group(row, group);
    }
    Ok(moved)
}

/// The workspace of `tree` holding the window `place`, and the group it stands in.
fn holding(tree: &mut Tree, place: u64) -> Result<(&mut Workspace, GroupId), String> {
    tree.workspaces
        .iter_mut()
        .find_map(|held| {
            let group = held
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .find(|group| group.tabs.iter().any(|tab| tab.id == place))?
                .id;
            Some((held, group))
        })
        .ok_or_else(|| "that place is not in the layout".to_owned())
}

/// The window `place` and where it stands in `tree`: its workspace, its row,
/// its group and its index in that group.
fn locate(tree: &Tree, place: u64) -> Option<(usize, usize, usize, usize)> {
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
fn group_holding(tree: &mut Tree, place: u64) -> Option<&mut WindowGroup> {
    tree.workspaces
        .iter_mut()
        .flat_map(|workspace| &mut workspace.strips)
        .flat_map(|strip| &mut strip.window_groups)
        .find(|held| held.tabs.iter().any(|tab| tab.id == place))
}

/// The group `id` of `tree` , wherever it stands.
fn group_mut(tree: &mut Tree, id: GroupId) -> Option<&mut WindowGroup> {
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
    tree: &mut Tree,
    place: u64,
    row: StripId,
    before: Option<u64>,
    strip: Option<u64>,
    by_the_person: bool,
) -> Result<bool, String> {
    let Some((workspace, from, group, at)) = locate(tree, place) else {
        return Err("that place is not in the layout".into());
    };
    let taken = "that place is not in the layout";
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
        return Err("that row is not in the layout".into());
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
        // The window left the layout with its last place: its place and float go with it.
        held.forget_gone_places();
    }
    let holds = |group: &WindowGroup| {
        strip.is_some_and(|strip| group.id == strip)
            || before.is_some_and(|before| group.tabs.iter().any(|tab| tab.id == before))
    };
    let workspace_id = held.id();
    let Some(target) = held.strips.get_mut(row) else {
        return Err("that row is not in the layout".into());
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
    // A place the person moved comes to the front with the window it went into, as one they
    // open does.
    if by_the_person {
        let (row, into) = (target.id, group.id);
        tree.active_strip.insert(workspace_id, row);
        tree.set_active_group(row, into);
    }
    Ok(true)
}

/// What [`change`] is asked to place: the resource `resource` , through `view` , as the place
/// `place` in its own strip `strip` of the row `row` of `workspace` , at `(x, y)` canvas pixels,
/// `width` by `height` — or floating above them at `float` .
#[derive(Clone, Copy)]
struct Placed<'a> {
    workspace: WorkspaceId,
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
    hugs: bool,
}

/// Stand `asked` on the canvas as one of its published views: a window of the
/// resource's app, at the place it asked for (shell.md §8). A `strip` that
/// names a strip already on the layout puts the place in it as a tab, in front,
/// rather than in a strip of its own: how a person, an agent and the REPL open
/// something as a tab of the strip they are in. Whether the layout changed.
fn place_at(
    tree: &mut Tree,
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
    let taken = "that workspace is not on this layout, or that place is taken";
    if locate(tree, asked.place).is_some() {
        return Err(taken.into());
    }
    // The strip the place would take for its own is this workspace's, or a new one: a strip another
    // workspace holds is no strip of this layout for this place. A strip on this layout is joined as a
    // tab below, whichever workspace and row it stands in.
    let on_layout = tree.workspaces.iter().any(|held| {
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
        .find(|held| held.id() == asked.workspace)
    else {
        return Err(taken.into());
    };
    if on_layout
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
        title: target.title_through(&view),
        tile: TileContent::LocalApp {
            app_id: target.app,
            label: target.title,
            note: String::new(),
        },
        app_title: None,
        view,
        pinned: false,
    };
    // A strip already on the layout takes the place as a tab, in front: the window it is stands in
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
    // `Layout::free_place_near`), so two places at one point, or two
    // closer together than a frame is wide, stand clear of each other. The
    // frame it stands in is the size it asks for, so the free space is that
    // size.
    let (width, height, hugs) = canvas_size(asked.width, asked.height, asked.hugs);
    let place = workspace.layout.free_place_near(
        (clamp_coord(asked.x), clamp_coord(asked.y)),
        width,
        height,
        None,
    );
    let place = if hugs { place.hugging() } else { place };
    workspace.layout.places.insert(asked.strip, place);
    workspace.layout.kind = LayoutKind::Canvas;
    Ok(true)
}

/// The place `place` shows the resource `resource` through `view` (empty for its primary one),
/// keeping its id, its strip and its place on the canvas. What a canvas query's place becomes
/// when the app answered it: the same place, now the found resource, so Undo puts the query
/// back. Whether the layout changed; a place already showing that resource through that view is
/// no change.
fn show_at(
    tree: &mut Tree,
    catalog: &Rc<RefCell<Catalog>>,
    place: u64,
    resource: &str,
    view: &str,
) -> Result<bool, String> {
    let resource = resource.trim();
    if resource.is_empty() {
        return Err("a place names the resource it shows".into());
    }
    let target = catalog
        .borrow()
        .get(resource)
        .cloned()
        .ok_or_else(|| format!("nothing published is `{resource}`"))?;
    let Some(tab) = group_holding(tree, place)
        .and_then(|held| held.tabs.iter_mut().find(|tab| tab.id == place))
    else {
        return Err("that place is not in the layout".into());
    };
    let view = target.chosen_view(view);
    // The window the place is shows the resource from now on: its app, the record `Place` writes
    // for it (`LayoutDocument::resource_action`'s `shows`), and the view it shows it through. Everything
    // else the place keeps — its id, its strip, its place on the canvas, whether it is pinned.
    let window = Tab {
        id: place,
        title: target.title_through(&view),
        tile: TileContent::LocalApp {
            app_id: target.app,
            label: target.title,
            note: String::new(),
        },
        app_title: None,
        view,
        pinned: tab.pinned,
    };
    let changed = tab.title != window.title || tab.view != window.view || tab.tile != window.tile;
    tab.title = window.title;
    tab.tile = window.tile;
    tab.view = window.view;
    tab.app_title = None;
    Ok(changed)
}

/// Float the place `group` at `float` , when the place asked to float at all.
fn float_new(
    workspace: &mut tilo_desktop_model::Workspace,
    group: GroupId,
    float: Option<FloatPlace>,
) {
    if let Some(float) = float {
        workspace.layout.set_float(group, Some(float));
    }
}

/// Take `window` into the group `group` already on the workspace's layout, as its tab in front: a
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
pub fn verb(action: &LayoutAction) -> &'static str {
    match action {
        LayoutAction::Move { .. } => "move",
        LayoutAction::Resize { .. } => "resize",
        LayoutAction::Maximize { .. } => "maximize-strip",
        LayoutAction::Close { .. } => "close",
        LayoutAction::Activate { .. } => "activate",
        LayoutAction::Pin { .. } => "pin",
        LayoutAction::Position { .. } => "position",
        LayoutAction::Size { .. } => "size",
        LayoutAction::Drawn { .. } => "drawn",
        LayoutAction::RemoveRow { .. } => "drop-row",
        LayoutAction::Place { .. } | LayoutAction::PlaceFound { .. } => "place",
        LayoutAction::Open { .. } => "open",
        LayoutAction::Float { .. } => "float",
        LayoutAction::Snap { .. } => "snap-back",
        LayoutAction::Show { .. } => "show",
        LayoutAction::RetryQuery { .. } => "retry",
    }
}

mod actions;

pub use actions::LayoutActions;

mod turns;
use beside::Beside;
pub use turns::CanvasRect;
use turns::{AwaitedTurn, TurnAnchor, TurnRoom};
pub use turns::{FindTurn, Posted, TURN_VIEW, turn_key};

#[cfg(test)]
mod tests;
