//! The actions that put a resource on the canvas, as the arrangement publishes them
//! (`super::resources`): by naming it and one of its views, by what a text finds, and the
//! system's `open`.

use super::{
    ActionSpec, ArrangementAction, DEFAULT_HEIGHT, DEFAULT_WIDTH, FLOAT_HEIGHT, FLOAT_INSET,
    FLOAT_WIDTH, FloatCorner, FloatPlace, Item, PLACE_KIND, ParamType, action, count, float_coord,
    float_side, id_text, named, optional, required, text,
};

/// The actions that put a resource on the canvas: by naming it and one of its views, or by what a
/// text finds (shell.md §8).
pub(super) fn placing_actions() -> Vec<ActionSpec> {
    vec![
        place_action(),
        place_found_action(),
        open_action(),
        show_action(),
        retry_query_action(),
    ]
}

/// `show`: a place shows another resource, keeping its id and its place on the canvas. What an
/// answered canvas query's place becomes, and Undo puts the query back (shell.md §8).
fn show_action() -> ActionSpec {
    action(
        "show",
        "Show another resource in a place, keeping the place where it is",
        vec![
            required("place", ParamType::Resource, "The place, by its resource")
                .of_resource_kind(PLACE_KIND),
            required(
                "resource",
                ParamType::Resource,
                "The resource it shows from now on",
            ),
            optional("view", ParamType::Text, "The view it shows it through"),
        ],
        |args| ArrangementAction::Show {
            place: named(args, "place", Item::Place).unwrap_or_default(),
            resource: text(args, "resource"),
            view: args.text("view").unwrap_or_default().to_owned(),
        },
    )
}

/// `retry-query`: ask the app the query a place shows again for what the person typed.
fn retry_query_action() -> ActionSpec {
    action(
        "retry-query",
        "Look for what a place's search names again",
        vec![
            required("place", ParamType::Resource, "The place, by its resource")
                .of_resource_kind(PLACE_KIND),
        ],
        |args| ArrangementAction::RetryQuery {
            place: named(args, "place", Item::Place).unwrap_or_default(),
        },
    )
}

/// Show a resource on the canvas, as one of its views.
fn place_action() -> ActionSpec {
    action(
        "place",
        "Show a resource on the canvas, as one of its views",
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
            // Where it stands when it names no point: beside the place showing this resource, in
            // the free space nearest it. An agent's step names the conversation it was asked in,
            // so what it places stands next to that conversation (`shell.md` §8.1).
            optional(
                "beside",
                ParamType::Text,
                "Without x and y, it stands beside the place showing this resource, by its address; an agent's step stands it beside the conversation it was asked in",
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
                // Named no height, it hugs what it shows up to a window's (canvas.md §7).
                hugs: args.integer("height").is_none(),
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
            // A rectangle the person dragged: what is found is laid out to it, and an answer's
            // views fill it. Left out, it stands at its own size.
            optional(
                "width",
                ParamType::Integer,
                "The width of the rectangle it fills, in canvas pixels; its own when left out",
            ),
            optional(
                "height",
                ParamType::Integer,
                "The height of the rectangle it fills, in canvas pixels; its own when left out",
            ),
            // What the person saw when they pointed: an answer's views stand in free room
            // inside it, so everything it places stays in view.
            optional(
                "seen_x",
                ParamType::Integer,
                "The canvas x of the part of the canvas the person sees",
            ),
            optional(
                "seen_y",
                ParamType::Integer,
                "The canvas y of the part of the canvas the person sees",
            ),
            optional(
                "seen_width",
                ParamType::Integer,
                "The width of the part of the canvas the person sees, in canvas pixels",
            ),
            optional(
                "seen_height",
                ParamType::Integer,
                "The height of the part of the canvas the person sees, in canvas pixels",
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
            width: args.integer("width").unwrap_or_default(),
            height: args.integer("height").unwrap_or_default(),
            seen_x: args.integer("seen_x").unwrap_or_default(),
            seen_y: args.integer("seen_y").unwrap_or_default(),
            seen_width: args.integer("seen_width").unwrap_or_default(),
            seen_height: args.integer("seen_height").unwrap_or_default(),
        },
    )
}

/// Open a resource with its own view, beside what the ask is in.
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
