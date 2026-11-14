//! What the layout publishes as actions: those that put a resource on the canvas and step the
//! layout's history, and those every place, strip and row offers. Each label and each argument is
//! a message of the host's catalogue, so a palette, a script and an agent read them in the
//! person's language.

use serde_json::Value as Json;
use tilo_actions::{
    ActionArgs, ActionContract, ActionParam, ActionSpec, Audience, EncodedAction, ExecutionMode,
    OperationKind, ParamType, ResourceSpec, ResultSpec,
};
use tilo_desktop_model::{FloatCorner, FloatPlace, WorkspaceId};

use super::{
    ArrangementAction, DEFAULT_HEIGHT, DEFAULT_WIDTH, FLOAT_HEIGHT, FLOAT_INSET, FLOAT_WIDTH, Item,
    OPERATION, PLACE_KIND, PlaceReading, RECORD_RESOURCE, RESOURCE, ROW_KIND, RowReading,
    STRIP_KIND, StripReading, Words,
};

/// What the layout calls the action `id`, and its argument `argument`, in the person's language:
/// the catalogue's `session-action-<id>`, and `session-action-<id>-<argument>`, `source` where it
/// has no such message.
fn action_word(words: &Words, id: &str, argument: Option<&str>, source: &str) -> String {
    let key = match argument {
        Some(argument) => format!("session-action-{id}-{argument}"),
        None => format!("session-action-{id}"),
    };
    words(&key, &[], source)
}

/// The person's layout actions, published as every other action is: a person's own drag, a
/// script and an agent all change the layout through them, and the catalogue lists them for
/// agents. What is already arranged is published per place, strip and row by
/// [`Arrangement::resources`].
#[must_use]
pub fn resources(words: &Words) -> Vec<ResourceSpec> {
    vec![arrangement_resource(words), record_resource(words)]
}

/// What puts a resource on the canvas and steps the layout's history.
pub(super) fn arrangement_resource(words: &Words) -> ResourceSpec {
    placing_actions(words)
        .into_iter()
        .fold(
            ResourceSpec::new(
                RESOURCE,
                "shell",
                words("session-resource-layout", &[], "Layout"),
            ),
            ResourceSpec::with_action,
        )
        .with_action(step_action(words, "undo", "Undo the last layout change"))
        .with_action(step_action(words, "redo", "Redo the last layout change"))
}

/// The record itself, whose actions step its history by the record's address: a person is offered
/// undo and redo once, on the layout.
pub(super) fn record_resource(words: &Words) -> ResourceSpec {
    ResourceSpec::new(
        RECORD_RESOURCE,
        "shell",
        words("session-resource-layout-history", &[], "Layout history"),
    )
    .with_action(
        step_action(words, "undo", "Undo the last layout change").for_audience(Audience::Agents),
    )
    .with_action(
        step_action(words, "redo", "Redo the last layout change").for_audience(Audience::Agents),
    )
}

/// The resource a place's own actions are published on, for the package the session describes: no
/// key, so it stands for every place. A place's resource is named by the place's key
/// ([`Arrangement::resources`]).
#[must_use]
pub fn place_kind_resource(words: &Words) -> ResourceSpec {
    place_actions("", words).into_iter().fold(
        ResourceSpec::new(String::new(), PLACE_KIND, String::new()),
        ResourceSpec::with_action,
    )
}

/// The resource a strip's own actions are published on: [`place_kind_resource`] for a strip.
#[must_use]
pub fn strip_kind_resource(words: &Words) -> ResourceSpec {
    strip_actions("", words).into_iter().fold(
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
pub(super) fn place_actions(key: &str, words: &Words) -> Vec<ActionSpec> {
    let pinned = key.to_owned();
    let closed = key.to_owned();
    let moved = key.to_owned();
    let in_front = key.to_owned();
    let positioned = key.to_owned();
    vec![
        action(
            words,
            "pin",
            "Pin a place, so an agent's changes leave it alone",
            vec![optional(
                words,
                "pin",
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
            words,
            "position",
            "Stand a place at a point on the canvas, as a drag that let it go there",
            vec![
                required(words, "position", "x", ParamType::Integer, "Its canvas x"),
                required(words, "position", "y", ParamType::Integer, "Its canvas y"),
            ],
            move |args| ArrangementAction::Position {
                place: count_of(&positioned),
                x: args.integer("x").unwrap_or_default(),
                y: args.integer("y").unwrap_or_default(),
            },
        ),
        action(words, "close", "Close a place", Vec::new(), move |_| {
            ArrangementAction::Close {
                place: count_of(&closed),
            }
        }),
        action(
            words,
            "activate",
            "Bring a place in front of its strip",
            Vec::new(),
            move |_| ArrangementAction::Activate {
                place: count_of(&in_front),
            },
        ),
        action(
            words,
            "move",
            "Move a place into a row, or into a strip as a tab of it",
            vec![
                required(
                    words,
                    "move",
                    "row",
                    ParamType::Resource,
                    "The row it goes into",
                )
                .of_resource_kind(ROW_KIND),
                optional(
                    words,
                    "move",
                    "strip",
                    ParamType::Resource,
                    "The strip it becomes a tab of, in that strip's row",
                )
                .of_resource_kind(STRIP_KIND),
                optional(
                    words,
                    "move",
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
    .chain(float_actions(key, words))
    .collect()
}

/// The actions that float a place and snap it back, its key bound into each as every place action's
/// is.
fn float_actions(key: &str, words: &Words) -> Vec<ActionSpec> {
    let floating = key.to_owned();
    let snapping = key.to_owned();
    vec![
        action(
            words,
            "float",
            "Float a place above its workspace's windows, where every member sees it",
            vec![
                optional(
                    words,
                    "float",
                    "corner",
                    ParamType::Text,
                    "The corner of the work area it keeps its distance from: top-left, top-right, bottom-right (the default) or bottom-left",
                ),
                optional(
                    words,
                    "float",
                    "dx",
                    ParamType::Integer,
                    "Its distance from that corner's side edge, in canvas pixels",
                ),
                optional(
                    words,
                    "float",
                    "dy",
                    ParamType::Integer,
                    "Its distance from that corner's top or bottom edge, in canvas pixels",
                ),
                optional(
                    words,
                    "float",
                    "width",
                    ParamType::Integer,
                    "Its width in canvas pixels",
                ),
                optional(
                    words,
                    "float",
                    "height",
                    ParamType::Integer,
                    "Its height in canvas pixels",
                ),
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
            words,
            "snap-back",
            "Snap a floating place back among its workspace's windows",
            vec![
                optional(
                    words,
                    "snap-back",
                    "row",
                    ParamType::Resource,
                    "The row it goes into; its own when left out",
                )
                .of_resource_kind(ROW_KIND),
                optional(
                    words,
                    "snap-back",
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
pub(super) fn strip_actions(key: &str, words: &Words) -> Vec<ActionSpec> {
    let resized = key.to_owned();
    let maximized = key.to_owned();
    vec![
        action(
            words,
            "resize",
            "Resize a strip",
            vec![required(
                words,
                "resize",
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
            words,
            "maximize-strip",
            "Maximize a strip, so it stands alone and fills the shell",
            vec![optional(
                words,
                "maximize-strip",
                "on",
                ParamType::Boolean,
                "Whether it fills the shell",
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
        .and_then(|key| {
            tilo_failure::ignored!(
                "an id that is not a number names no row, which the action refuses",
                key.parse().ok()
            )
        })
}

/// The record key of a place, a strip or a row, as its actions take it. `0` for the keyless
/// resource a package describes the kind with, which no door publishes.
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

/// The state a query's matches settle it at, and the kind of resource the query names: `failed`
/// when the app gave a reason, else `one` for exactly one match a place can show, `several` for
/// more, `none` for none — and `searching` while a state is still being waited for (the caller
/// passes `""` for `state` only once the app answered). `kind` is the one every match shares, or
/// the first's, as the app publishes it.
/// The state and reason a query starts in, from the app's own search invocation's result: a
/// refusal or failure is the query `.failed` with the action's own reason; a run that was taken is
/// `searching`, answered later into the app's records (shell.md §8). The invocation itself says,
/// never a guess from the app's records.
/// Feed what `specs` publish into `hasher` : each resource's id, kind and label, and its
/// children's, so two revisions are equal exactly when the door publishes the same.
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

/// The actions that put a resource on the canvas: by naming it and one of its views, or by what a
/// text finds (shell.md §8).
fn placing_actions(words: &Words) -> Vec<ActionSpec> {
    vec![
        place_action(words),
        place_found_action(words),
        open_action(words),
        show_action(words),
        retry_query_action(words),
    ]
}

/// `show`: a place shows another resource, keeping its id and its place on the canvas. What an
/// answered canvas query's place becomes, and Undo puts the query back (shell.md §8).
fn show_action(words: &Words) -> ActionSpec {
    action(
        words,
        "show",
        "Show another resource in a place, keeping the place where it is",
        vec![
            required(
                words,
                "show",
                "place",
                ParamType::Resource,
                "The place, by its resource",
            )
            .of_resource_kind(PLACE_KIND),
            required(
                words,
                "show",
                "resource",
                ParamType::Resource,
                "The resource it shows from now on",
            ),
            optional(
                words,
                "show",
                "view",
                ParamType::Text,
                "The view it shows it through",
            ),
        ],
        |args| ArrangementAction::Show {
            place: named(args, "place", Item::Place).unwrap_or_default(),
            resource: text(args, "resource"),
            view: args.text("view").unwrap_or_default().to_owned(),
        },
    )
}

/// `retry-query`: ask the app the query a place shows again for what the person typed.
fn retry_query_action(words: &Words) -> ActionSpec {
    action(
        words,
        "retry-query",
        "Look for what a place's search names again",
        vec![
            required(
                words,
                "retry-query",
                "place",
                ParamType::Resource,
                "The place, by its resource",
            )
            .of_resource_kind(PLACE_KIND),
        ],
        |args| ArrangementAction::RetryQuery {
            place: named(args, "place", Item::Place).unwrap_or_default(),
        },
    )
}

/// What a `place` names: the surface in front's workspace and row when left out, the place's own
/// strip and id when it makes them, and where it stands and what it shows.
fn place_params(words: &Words) -> Vec<ActionParam> {
    vec![
        // Each left out is the surface in front's: its workspace and row, and a strip and place
        // id of its own.
        optional(
            words,
            "place",
            "workspace",
            ParamType::Text,
            "The workspace whose canvas it stands on, by its id; the one in front when left out",
        ),
        optional(
            words,
            "place",
            "row",
            ParamType::Integer,
            "The row it stands in; the row in front when left out",
        ),
        optional(
            words,
            "place",
            "strip",
            ParamType::Integer,
            "The strip it stands in; a new one when left out",
        ),
        optional(
            words,
            "place",
            "place",
            ParamType::Integer,
            "The id the new place takes; a new one when left out",
        ),
        required(
            words,
            "place",
            "resource",
            ParamType::Resource,
            "The resource it shows",
        ),
        optional(
            words,
            "place",
            "view",
            ParamType::Text,
            "The view it shows it through",
        ),
        optional(words, "place", "x", ParamType::Integer, "Its canvas x"),
        optional(words, "place", "y", ParamType::Integer, "Its canvas y"),
        optional(
            words,
            "place",
            "width",
            ParamType::Integer,
            "Its width in canvas pixels",
        ),
        optional(
            words,
            "place",
            "height",
            ParamType::Integer,
            "Its height in canvas pixels",
        ),
        // A place that floats stands above the windows rather than on the canvas: it has no
        // canvas x and y, and the corner and insets are its own instead (`width` and
        // `height` are its size). Left out, the place stands on the canvas.
        optional(
            words,
            "place",
            "float",
            ParamType::Text,
            "Floats it above its workspace's windows at this corner of the work area: top-left, top-right, bottom-right (the default) or bottom-left",
        ),
        optional(
            words,
            "place",
            "dx",
            ParamType::Integer,
            "A float's distance from that corner's side edge, in canvas pixels",
        ),
        optional(
            words,
            "place",
            "dy",
            ParamType::Integer,
            "A float's distance from that corner's top or bottom edge, in canvas pixels",
        ),
    ]
}

/// Put a resource on the canvas as one of its views.
fn place_action(words: &Words) -> ActionSpec {
    action(
        words,
        "place",
        "Put a resource on the canvas as one of its views",
        place_params(words),
        |args| {
            let float = args.text("float").map(|corner| {
                FloatPlace::new(
                    tilo_failure::ignored!(
                        "a corner's name that names none stands at the default corner",
                        FloatCorner::parse(corner).unwrap_or_default()
                    ),
                    float_coord(args.integer("dx"), FLOAT_INSET),
                    float_coord(args.integer("dy"), FLOAT_INSET),
                    float_side(args.integer("width"), FLOAT_WIDTH),
                    float_side(args.integer("height"), FLOAT_HEIGHT),
                )
            });
            ArrangementAction::Place {
                workspace: workspace_text(args, "workspace"),
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

/// Put what a text finds on the canvas, or into a strip as a tab.
fn place_found_action(words: &Words) -> ActionSpec {
    action(
        words,
        "place-found",
        "Put what a text finds on the canvas, or into a strip as a tab",
        vec![
            required(
                words,
                "place-found",
                "text",
                ParamType::Text,
                "What to find and place",
            ),
            optional(
                words,
                "place-found",
                "view",
                ParamType::Text,
                "The view it shows it through",
            ),
            optional(
                words,
                "place-found",
                "strip",
                ParamType::Text,
                "The strip it joins as a tab, by its id; a strip of its own when left out",
            ),
            optional(
                words,
                "place-found",
                "x",
                ParamType::Integer,
                "Its canvas x",
            ),
            optional(
                words,
                "place-found",
                "y",
                ParamType::Integer,
                "Its canvas y",
            ),
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

/// Open a resource with its own view, beside what the ask is in.
fn open_action(words: &Words) -> ActionSpec {
    action(
        words,
        "open",
        "Open a resource with its own view, beside what the ask is in",
        vec![
            required(
                words,
                "open",
                "resource",
                ParamType::Resource,
                "The resource it shows",
            ),
            optional(
                words,
                "open",
                "view",
                ParamType::Text,
                "The view it shows it through",
            ),
            optional(
                words,
                "open",
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
fn step_action(words: &Words, id: &'static str, label: &str) -> ActionSpec {
    let mut spec = ActionSpec::typed_operation(
        id,
        action_word(words, id, None, label),
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

/// An immediate effect action `id` that builds an [`ArrangementAction`]. It is listed for agents and
/// scripts, never for a person to pick: a person arranges by dragging and through the window bar,
/// which submit it by address, and a place's actions name no place a listing could tell apart
/// (three windows were three identical "Stand a place" rows in search).
#[expect(
    clippy::expect_used,
    reason = "an arranging action always serializes to JSON"
)]
pub(super) fn action(
    words: &Words,
    id: &'static str,
    label: &str,
    params: Vec<ActionParam>,
    build: impl Fn(&ActionArgs) -> ArrangementAction + Send + Sync + 'static,
) -> ActionSpec {
    let mut spec = ActionSpec::typed_operation(
        id,
        action_word(words, id, None, label),
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
    spec.for_audience(Audience::Agents)
}

pub(super) fn required(
    words: &Words,
    id: &str,
    name: &'static str,
    ty: ParamType,
    help: &str,
) -> ActionParam {
    ActionParam::required_owned(name, ty, action_word(words, id, Some(name), help))
}

fn optional(words: &Words, id: &str, name: &'static str, ty: ParamType, help: &str) -> ActionParam {
    ActionParam::optional_owned(name, ty, action_word(words, id, Some(name), help))
}

fn count(args: &ActionArgs, name: &str) -> u64 {
    args.whole(name).unwrap_or_default()
}

/// An id given as text: one no workspace has (`0`) when it is not a number,
/// which the action then refuses as naming no workspace.
fn id_text(args: &ActionArgs, name: &str) -> u64 {
    tilo_failure::ignored!(
        "text that is not a number names nothing, which the action refuses",
        args.text(name)
            .and_then(|id| id.trim().parse().ok())
            .unwrap_or_default()
    )
}

fn workspace_text(args: &ActionArgs, name: &str) -> WorkspaceId {
    tilo_failure::ignored!(
        "text that is not a workspace id names no workspace, which the action refuses",
        args.text(name)
            .and_then(|id| WorkspaceId::parse(id.trim()))
            .unwrap_or_default()
    )
}

pub(super) fn text(args: &ActionArgs, name: &str) -> String {
    args.text(name).unwrap_or_default().to_owned()
}
