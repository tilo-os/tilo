//! The actions that float a place and snap it back, and a strip's own actions, as the
//! arrangement publishes them (`super::resources`).

use super::{
    ActionSpec, ArrangementAction, FLOAT_HEIGHT, FLOAT_INSET, FLOAT_WIDTH, FloatCorner, FloatPlace,
    Item, PLACE_KIND, ParamType, ROW_KIND, action, count_of, named, optional, required,
};

/// The actions that float a place and snap it back, its key bound into each as every place action's
/// is.
pub(super) fn float_actions(key: &str) -> Vec<ActionSpec> {
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
pub(super) fn float_coord(value: Option<i64>, default: i64) -> i32 {
    i32::try_from(value.unwrap_or(default).max(0)).unwrap_or(i32::MAX)
}

/// A floated window's side, at least one pixel.
pub(super) fn float_side(value: Option<i64>, default: u32) -> u32 {
    u32::try_from(value.unwrap_or_else(|| i64::from(default)).max(1)).unwrap_or(u32::MAX)
}

/// The actions a strip's own resource offers, its key bound into each: the strip is the resource
/// they are pressed on.
pub(super) fn strip_actions(key: &str) -> Vec<ActionSpec> {
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
