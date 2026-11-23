//! What the layout publishes as actions: those that put a resource on the canvas and step the
//! layout's history, and those every place, strip, row and query offers. They are declared once
//! as [`LayoutEffect`], whose labels and arguments are messages of the session's catalogue, so a
//! palette, a script and an agent read them in the person's language; each published resource is
//! an item of a declared type ([`Place`], [`Strip`], [`LayoutRow`], [`Query`]).

use tilo_actions::{Audience, ResourceSpec};
use tilo_desktop_model::{FloatCorner, FloatPlace, WorkspaceId};
use tilo_resource::{LiveRow, offer, typed_resource};

use super::{
    DEFAULT_HEIGHT, DEFAULT_WIDTH, FLOAT_HEIGHT, FLOAT_INSET, FLOAT_WIDTH, HISTORY_RESOURCE, Item,
    LayoutAction, PlaceReading, RESOURCE, RowReading, StripReading, Words,
};

/// The person's layout actions, published as every other action is: a person's own drag, a
/// script and an agent all change the layout through them, and the catalogue lists them for
/// agents. What is already arranged is published per place, strip and row by
/// [`Layout::resources`](super::Layout::resources).
#[must_use]
pub fn resources(words: &Words) -> Vec<ResourceSpec> {
    vec![layout_resource(words), history_resource(words)]
}

/// The layout itself, as a declared type: what puts a resource on the canvas and steps its
/// history.
#[derive(tilo_resource::Row)]
#[row(kind = "shell", label = "Tree")]
pub struct Layout;

/// The layout's history, as a declared type: a person is offered undo and redo once, on the
/// layout.
#[derive(tilo_resource::Row)]
#[row(kind = "shell", label = "Tree history")]
pub struct LayoutHistory;

/// A place, as a declared type: what its own actions act on.
#[derive(tilo_resource::Row)]
#[row(kind = "layout.place", label = "Place", effects = LayoutEffect)]
pub struct Place;

/// A strip, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "layout.strip", label = "Strip", effects = LayoutEffect)]
pub struct LayoutStrip;

/// A row, as a declared type: what a place moves into.
#[derive(tilo_resource::Row)]
#[row(kind = "layout.row", label = "Row")]
pub struct LayoutRow;

/// A query a place shows while its app answers it, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "layout.query", label = "Search", effects = LayoutEffect)]
pub struct Query;

/// What puts a resource on the canvas and steps the layout's history.
pub(super) fn layout_resource(words: &Words) -> ResourceSpec {
    let mut resource = offer::<LayoutEffect>(typed_resource::<Layout>(
        RESOURCE,
        words("session-resource-layout", &[], "Tree"),
    ));
    // A place's, a strip's and a query's own actions are offered bound on each of them; an
    // unbound copy here would be a second `move`, `size` or `close` that a caller looking an
    // action up by its name finds first, taking a place instead of acting on one.
    resource
        .actions
        .retain(|action| action.contract.item.is_none());
    carried(resource)
}

/// The record itself, whose actions step its history by the record's address: a person is offered
/// undo and redo once, on the layout.
pub(super) fn history_resource(words: &Words) -> ResourceSpec {
    let mut resource = typed_resource::<LayoutHistory>(
        HISTORY_RESOURCE,
        words("session-resource-layout-history", &[], "Tree history"),
    );
    for spec in [LayoutEffect::UNDO.spec(), LayoutEffect::REDO.spec()]
        .into_iter()
        .flatten()
    {
        resource = resource.with_action(spec.for_audience(Audience::Agents));
    }
    resource
}

/// The resource a place's own actions are published on, for the package the session describes: no
/// key, so it stands for every place. A place's resource is named by the place's key
/// ([`Layout::resources`](super::Layout::resources)).
#[must_use]
pub fn place_kind_resource(_words: &Words) -> ResourceSpec {
    carried(LiveRow::typed::<Place>(String::new(), String::new()).into_resource())
}

/// The resource a strip's own actions are published on: [`place_kind_resource`] for a strip.
#[must_use]
pub fn strip_kind_resource(_words: &Words) -> ResourceSpec {
    carried(LiveRow::typed::<LayoutStrip>(String::new(), String::new()).into_resource())
}

/// The resource a row is published as: [`place_kind_resource`] for a row. A row offers no action of
/// its own: it is what a place moves into, named by the place's `move` .
#[must_use]
pub fn row_kind_resource() -> ResourceSpec {
    typed_resource::<LayoutRow>(String::new(), String::new())
}

/// Every change the layout publishes, declared once: what a person's drag, a script, an agent or
/// the session's own calls make. Each is answered by [`LayoutAction`], which the layout's document
/// keeps.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "tilo.shell.layout", catalogue = "../session/locales")]
pub enum LayoutEffect {
    /// Show a resource on the canvas, as one of its views
    #[effect(kind = "undoable", audience = "agents", label = "session-action-place")]
    Place {
        /// The workspace whose canvas it stands on, by its id; the one in front when left out
        #[param(
            "The workspace whose canvas it stands on, by its id; the one in front when left \
                 out"
        )]
        workspace: Option<String>,
        /// The row it stands in; the row in front when left out
        #[param("The row it stands in; the row in front when left out")]
        row: Option<u64>,
        /// The strip it stands in; a new one when left out
        #[param("The strip it stands in; a new one when left out")]
        strip: Option<u64>,
        /// The id the new place takes; a new one when left out
        #[param("The id the new place takes; a new one when left out")]
        place: Option<u64>,
        /// The resource it shows
        #[param(resource = "", "The resource it shows")]
        resource: String,
        /// The view it shows it through
        #[param("The view it shows it through")]
        view: Option<String>,
        /// Its canvas x
        #[param("Its canvas x")]
        x: Option<i64>,
        /// Its canvas y
        #[param("Its canvas y")]
        y: Option<i64>,
        /// Without x and y, it stands beside the place showing this resource, by its address; an
        /// agent's step stands it beside the conversation it was asked in
        #[param(
            "Without x and y, it stands beside the place showing this resource, by its \
                 address; an agent's step stands it beside the conversation it was asked in"
        )]
        beside: Option<String>,
        /// Its width in canvas pixels
        #[param("Its width in canvas pixels")]
        width: Option<i64>,
        /// Its height in canvas pixels
        #[param("Its height in canvas pixels")]
        height: Option<i64>,
        /// Floats it above its workspace's windows at this corner of the work area: top-left,
        /// top-right, bottom-right (the default) or bottom-left
        #[param(
            "Floats it above its workspace's windows at this corner of the work area: top-left, \
             top-right, bottom-right (the default) or bottom-left"
        )]
        float: Option<String>,
        /// A float's distance from that corner's side edge, in canvas pixels
        #[param("A float's distance from that corner's side edge, in canvas pixels")]
        dx: Option<i64>,
        /// A float's distance from that corner's top or bottom edge, in canvas pixels
        #[param("A float's distance from that corner's top or bottom edge, in canvas pixels")]
        dy: Option<i64>,
    },
    /// Put what a text finds on the canvas, or into a strip as a tab
    #[effect(
        kind = "undoable",
        audience = "agents",
        label = "session-action-place-found"
    )]
    PlaceFound {
        /// What to find and place
        #[param("What to find and place")]
        text: String,
        /// The view it shows it through
        #[param("The view it shows it through")]
        view: Option<String>,
        /// The strip it joins as a tab, by its id; a strip of its own when left out
        #[param("The strip it joins as a tab, by its id; a strip of its own when left out")]
        strip: Option<String>,
        /// Its canvas x
        #[param("Its canvas x")]
        x: Option<i64>,
        /// Its canvas y
        #[param("Its canvas y")]
        y: Option<i64>,
        /// The width of the rectangle it fills, in canvas pixels; its own when left out
        #[param("The width of the rectangle it fills, in canvas pixels; its own when left out")]
        width: Option<i64>,
        /// The height of the rectangle it fills, in canvas pixels; its own when left out
        #[param("The height of the rectangle it fills, in canvas pixels; its own when left out")]
        height: Option<i64>,
        /// The canvas x of the part of the canvas the person sees
        #[param("The canvas x of the part of the canvas the person sees")]
        seen_x: Option<i64>,
        /// The canvas y of the part of the canvas the person sees
        #[param("The canvas y of the part of the canvas the person sees")]
        seen_y: Option<i64>,
        /// The width of the part of the canvas the person sees, in canvas pixels
        #[param("The width of the part of the canvas the person sees, in canvas pixels")]
        seen_width: Option<i64>,
        /// The height of the part of the canvas the person sees, in canvas pixels
        #[param("The height of the part of the canvas the person sees, in canvas pixels")]
        seen_height: Option<i64>,
    },
    /// Open a resource with its own view, beside what the ask is in
    #[effect(kind = "undoable", audience = "agents", label = "session-action-open")]
    Open {
        /// The resource it shows
        #[param(resource = "", "The resource it shows")]
        resource: String,
        /// The view it shows it through
        #[param("The view it shows it through")]
        view: Option<String>,
        /// The resource whose place it stands beside, by its address; the place in front when
        /// left out
        #[param(
            "The resource whose place it stands beside, by its address; the place in front when \
             left out"
        )]
        beside: Option<String>,
    },
    /// Show another resource in a place, keeping the place where it is
    #[effect(kind = "undoable", audience = "agents", label = "session-action-show")]
    Show {
        /// The place, by its resource
        #[param(resource = "layout.place", "The place, by its resource")]
        place: String,
        /// The resource it shows from now on
        #[param(resource = "", "The resource it shows from now on")]
        resource: String,
        /// The view it shows it through
        #[param("The view it shows it through")]
        view: Option<String>,
    },
    /// Look for what a place's search names again
    #[effect(
        kind = "undoable",
        audience = "agents",
        label = "session-action-retry-query"
    )]
    RetryQuery {
        /// The place, by its resource
        #[param(resource = "layout.place", "The place, by its resource")]
        place: String,
    },
    /// Undo the last layout change
    #[effect(kind = "undoable", label = "session-action-undo")]
    Undo,
    /// Redo the last layout change
    #[effect(kind = "undoable", label = "session-action-redo")]
    Redo,
    /// Pin a place, so an agent's changes leave it alone
    #[effect(on = Place, kind = "undoable", audience = "agents", label = "session-action-pin")]
    Pin {
        /// The place
        #[param("The place")]
        place: String,
        /// Whether it is pinned
        #[param("Whether it is pinned")]
        pinned: Option<bool>,
    },
    /// Stand a place at a point on the canvas, as a drag that let it go there
    #[effect(
        on = Place,
        kind = "undoable",
        audience = "agents",
        label = "session-action-position"
    )]
    Position {
        /// The place
        #[param("The place")]
        place: String,
        /// Its canvas x
        #[param("Its canvas x")]
        x: Option<i64>,
        /// Its canvas y
        #[param("Its canvas y")]
        y: Option<i64>,
        /// Without x and y, it stands in the free space beside this: a place
        /// (`shell:place:<key>`) or the resource a place shows, by its address
        #[param(
            "Without x and y, it stands in the free space beside this: a place (`shell:place:<key>`) \
             or the resource a place shows, by its address"
        )]
        beside: Option<String>,
    },
    /// Give a place a size, as dragging its edge does
    #[effect(
        on = Place,
        kind = "undoable",
        audience = "agents",
        label = "session-action-size"
    )]
    Size {
        /// The place
        #[param("The place")]
        place: String,
        /// Its width in canvas pixels
        #[param("Its width in canvas pixels")]
        width: i64,
        /// Its height in canvas pixels
        #[param("Its height in canvas pixels")]
        height: i64,
    },
    /// Report how tall a hugging place is drawn now, as its canvas sees it
    #[effect(
        on = Place,
        kind = "read",
        audience = "agents",
        label = "session-action-drawn"
    )]
    Drawn {
        /// The place
        #[param("The place")]
        place: String,
        /// Its drawn height in canvas pixels
        #[param("Its drawn height in canvas pixels")]
        height: i64,
    },
    /// Close a place
    #[effect(on = Place, kind = "undoable", audience = "agents", label = "session-action-close")]
    Close {
        /// The place
        #[param("The place")]
        place: String,
    },
    /// Bring a place in front of its strip
    #[effect(
        on = Place,
        kind = "undoable",
        audience = "agents",
        label = "session-action-activate"
    )]
    Activate {
        /// The place
        #[param("The place")]
        place: String,
    },
    /// Move a place into a row, or into a strip as a tab of it
    #[effect(on = Place, kind = "undoable", audience = "agents", label = "session-action-move")]
    Move {
        /// The place
        #[param("The place")]
        place: String,
        /// The row it goes into
        #[param(resource = "layout.row", "The row it goes into")]
        row: String,
        /// The strip it becomes a tab of, in that strip's row
        #[param(
            resource = "layout.strip",
            "The strip it becomes a tab of, in that strip's row"
        )]
        strip: Option<String>,
        /// The place it goes before in that strip
        #[param(resource = "layout.place", "The place it goes before in that strip")]
        before: Option<String>,
    },
    /// Float a place above its workspace's windows, where every member sees it
    #[effect(on = Place, kind = "undoable", audience = "agents", label = "session-action-float")]
    Float {
        /// The place
        #[param("The place")]
        place: String,
        /// The corner of the work area it keeps its distance from: top-left, top-right,
        /// bottom-right (the default) or bottom-left
        #[param(
            "The corner of the work area it keeps its distance from: top-left, top-right, \
             bottom-right (the default) or bottom-left"
        )]
        corner: Option<String>,
        /// Its distance from that corner's side edge, in canvas pixels
        #[param("Its distance from that corner's side edge, in canvas pixels")]
        dx: Option<i64>,
        /// Its distance from that corner's top or bottom edge, in canvas pixels
        #[param("Its distance from that corner's top or bottom edge, in canvas pixels")]
        dy: Option<i64>,
        /// Its width in canvas pixels
        #[param("Its width in canvas pixels")]
        width: Option<i64>,
        /// Its height in canvas pixels
        #[param("Its height in canvas pixels")]
        height: Option<i64>,
    },
    /// Snap a floating place back among its workspace's windows
    #[effect(
        on = Place,
        kind = "undoable",
        audience = "agents",
        label = "session-action-snap-back"
    )]
    Snap {
        /// The place
        #[param("The place")]
        place: String,
        /// The row it goes into; its own when left out
        #[param(resource = "layout.row", "The row it goes into; its own when left out")]
        row: Option<String>,
        /// The place it goes before in that row
        #[param(resource = "layout.place", "The place it goes before in that row")]
        before: Option<String>,
    },
    /// Resize a strip
    #[effect(on = LayoutStrip, kind = "undoable", audience = "agents", label = "session-action-resize")]
    Resize {
        /// The strip
        #[param("The strip")]
        strip: String,
        /// Its width in per-mille of the row
        #[param("Its width in per-mille of the row")]
        permille: i64,
    },
    /// Maximize a strip, so it stands alone and fills the shell
    #[effect(
        on = LayoutStrip,
        kind = "undoable",
        audience = "agents",
        label = "session-action-maximize-strip"
    )]
    MaximizeStrip {
        /// The strip
        #[param("The strip")]
        strip: String,
        /// Whether it fills the shell
        #[param("Whether it fills the shell")]
        on: Option<bool>,
    },
    /// Show a match of the search in this place
    #[effect(
        on = Query,
        kind = "undoable",
        audience = "agents",
        label = "session-action-show-match"
    )]
    ShowMatch {
        /// The query
        #[param("The query")]
        query: String,
        /// The resource the row names
        #[param(resource = "", "The resource the row names")]
        resource: String,
    },
    /// Look for it again
    #[effect(on = Query, kind = "undoable", audience = "agents", label = "session-action-retry")]
    Retry {
        /// The query
        #[param("The query")]
        query: String,
    },
    /// Remove an empty row (a strip of the bar's own list) from its workspace
    /// A row that still holds a window, or a workspace's only row, refuses: the windows go first,
    /// and every workspace keeps a place to work.
    #[effect(
        kind = "undoable",
        audience = "agents",
        label = "session-action-remove-row"
    )]
    DropRow {
        /// The row, by its key
        #[param("The row, by its key")]
        row: String,
    },
}

impl LayoutEffect {
    /// The change this effect makes, as the layout's document keeps it; `None` for a step of the
    /// history, which the layout steps itself.
    #[expect(
        clippy::too_many_lines,
        reason = "each declared change maps to the layout change it makes in one match, so the two read together"
    )]
    pub(super) fn layout(&self) -> Option<LayoutAction> {
        Some(match self {
            Self::Place {
                workspace,
                row,
                strip,
                place,
                resource,
                view,
                x,
                y,
                beside: _,
                width,
                height,
                float,
                dx,
                dy,
            } => LayoutAction::Place {
                workspace: workspace
                    .as_deref()
                    .and_then(|id| WorkspaceId::parse(id.trim()))
                    .unwrap_or_default(),
                row: count(*row),
                strip: count(*strip),
                place: count(*place),
                resource: resource.clone(),
                view: view.clone().unwrap_or_default(),
                x: x.unwrap_or_default(),
                y: y.unwrap_or_default(),
                width: width.unwrap_or(DEFAULT_WIDTH),
                height: height.unwrap_or(DEFAULT_HEIGHT),
                float: float.as_deref().map(|corner| {
                    FloatPlace::new(
                        float_corner(Some(corner)),
                        float_coord(*dx, FLOAT_INSET),
                        float_coord(*dy, FLOAT_INSET),
                        float_side(*width, FLOAT_WIDTH),
                        float_side(*height, FLOAT_HEIGHT),
                    )
                }),
                // Named no height, it hugs what it shows up to a window's (canvas.md §7).
                hugs: height.is_none(),
            },
            Self::PlaceFound {
                text,
                view,
                strip,
                x,
                y,
                width,
                height,
                seen_x,
                seen_y,
                seen_width,
                seen_height,
            } => LayoutAction::PlaceFound {
                text: text.clone(),
                view: view.clone().unwrap_or_default(),
                strip: strip
                    .as_deref()
                    .and_then(|strip| strip.trim().parse().ok())
                    .filter(|strip| *strip != 0),
                x: x.unwrap_or_default(),
                y: y.unwrap_or_default(),
                width: width.unwrap_or_default(),
                height: height.unwrap_or_default(),
                seen_x: seen_x.unwrap_or_default(),
                seen_y: seen_y.unwrap_or_default(),
                seen_width: seen_width.unwrap_or_default(),
                seen_height: seen_height.unwrap_or_default(),
            },
            Self::Open {
                resource,
                view,
                beside,
            } => LayoutAction::Open {
                resource: resource.clone(),
                view: view.clone().unwrap_or_default(),
                beside: beside.clone().unwrap_or_default(),
            },
            Self::Show {
                place,
                resource,
                view,
            } => LayoutAction::Show {
                place: named(Some(place), Item::Place).unwrap_or_default(),
                resource: resource.clone(),
                view: view.clone().unwrap_or_default(),
            },
            Self::RetryQuery { place } => LayoutAction::RetryQuery {
                place: named(Some(place), Item::Place).unwrap_or_default(),
            },
            Self::Undo | Self::Redo => return None,
            Self::Pin { place, pinned } => LayoutAction::Pin {
                place: key_of(place),
                pinned: pinned.unwrap_or(true),
            },
            Self::Position {
                place,
                x,
                y,
                beside: _,
            } => LayoutAction::Position {
                place: key_of(place),
                x: x.unwrap_or_default(),
                y: y.unwrap_or_default(),
            },
            Self::Size {
                place,
                width,
                height,
            } => LayoutAction::Size {
                place: key_of(place),
                width: *width,
                height: *height,
            },
            Self::Drawn { place, height } => LayoutAction::Drawn {
                place: key_of(place),
                height: *height,
            },
            Self::Close { place } => LayoutAction::Close {
                place: key_of(place),
            },
            Self::Activate { place } => LayoutAction::Activate {
                place: key_of(place),
            },
            Self::Move {
                place,
                row,
                strip,
                before,
            } => LayoutAction::Move {
                place: key_of(place),
                row: named(Some(row), Item::Row).unwrap_or_default(),
                before: named(before.as_deref(), Item::Place),
                strip: named(strip.as_deref(), Item::Strip),
            },
            Self::Float {
                place,
                corner,
                dx,
                dy,
                width,
                height,
            } => LayoutAction::Float {
                place: key_of(place),
                float: FloatPlace::new(
                    float_corner(corner.as_deref()),
                    float_coord(*dx, FLOAT_INSET),
                    float_coord(*dy, FLOAT_INSET),
                    float_side(*width, FLOAT_WIDTH),
                    float_side(*height, FLOAT_HEIGHT),
                ),
            },
            Self::Snap { place, row, before } => LayoutAction::Snap {
                place: key_of(place),
                row: named(row.as_deref(), Item::Row),
                before: named(before.as_deref(), Item::Place),
            },
            Self::Resize { strip, permille } => LayoutAction::Resize {
                strip: key_of(strip),
                permille: u32::try_from(*permille).unwrap_or(1),
            },
            Self::MaximizeStrip { strip, on } => LayoutAction::Maximize {
                strip: key_of(strip),
                maximized: on.unwrap_or(true),
            },
            Self::ShowMatch { query, resource } => LayoutAction::Show {
                place: key_of(query),
                resource: resource.clone(),
                view: String::new(),
            },
            Self::Retry { query } => LayoutAction::RetryQuery {
                place: key_of(query),
            },
            Self::DropRow { row } => LayoutAction::RemoveRow {
                row: row.trim().parse().unwrap_or(0),
            },
        })
    }
}

/// The key of the item the published resource id names: the prefix each item's resources
/// begin with, then the key. `0` for an id naming nothing, which the action then refuses.
fn key_of(id: &str) -> u64 {
    [Item::Place, Item::Strip, Item::Row, Item::Query]
        .into_iter()
        .find_map(|item| item.key(id))
        .map_or(0, count_of)
}

/// The key of the item the resource id `id` names, when it names one.
fn named(id: Option<&str>, item: Item) -> Option<u64> {
    id.and_then(|id| item.key(id.trim())).and_then(|key| {
        tilo_failure::ignored!(
            "an id that is not a number names no row, which the action refuses",
            key.parse().ok()
        )
    })
}

/// The count `value` names, `0` where it is absent.
fn count(value: Option<u64>) -> u64 {
    value.unwrap_or_default()
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

/// What a change of the layout did, as its history says it: the words the hand-built specs
/// declared beside each action, kept on the published spec.
const DIDS: &[(&str, &str)] = &[
    ("pin", "pinned"),
    ("position", "moved on the canvas"),
    ("size", "sized"),
    ("close", "closed"),
    ("activate", "brought to the front"),
    ("move", "moved"),
    ("show", "showed in its place"),
    ("place", "showed"),
    ("place-found", "put on the canvas"),
    ("open", "opened"),
];

/// `resource` with each of its actions that a change of the layout says what it did by.
pub(super) fn carried(mut resource: ResourceSpec) -> ResourceSpec {
    for action in &mut resource.actions {
        if let Some((_, did)) = DIDS.iter().find(|(id, _)| *id == action.id) {
            action.type_mut().did = Some((*did).to_owned());
        }
    }
    resource
}

/// The record key of a place, a strip or a row, as its actions take it. `0` for the keyless
/// resource a package describes the kind with, which no api publishes.
pub(super) fn count_of(key: &str) -> u64 {
    tilo_failure::ignored!(
        "a keyless resource has no count, and 0 is its count",
        key.parse().unwrap_or_default()
    )
}

/// What a row is called where its own actions are listed.
pub(super) fn row_label(row: &RowReading) -> String {
    row.name.clone()
}

/// What a strip is called where its own actions are listed: how wide it is.
pub(super) fn strip_label(words: &Words, strip: &StripReading) -> String {
    let width = (strip.width / 10).to_string();
    words(
        "session-strip-width",
        &[("width", width.clone())],
        &format!("Strip, {width}% wide"),
    )
}

/// What a place is called where its own actions are listed: the window it holds.
pub(super) fn place_label(words: &Words, place: &PlaceReading) -> String {
    if place.name.is_empty() {
        words("session-place", &[], "Place")
    } else {
        place.name.clone()
    }
}

/// Feed what `specs` publish into `hasher` : each resource's id, kind and label, and its
/// children's, so two revisions are equal exactly when the action API publishes the same.
pub(super) fn hash_resources(specs: &[ResourceSpec], hasher: &mut std::hash::DefaultHasher) {
    use std::hash::{Hash as _, Hasher as _};
    for spec in specs {
        spec.id.0.hash(hasher);
        spec.kind.hash(hasher);
        spec.label.hash(hasher);
        hash_resources(&spec.children, hasher);
    }
    hasher.write_u8(0);
}
