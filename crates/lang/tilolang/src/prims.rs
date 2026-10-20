//! The UI primitives a view can build (layout.md section 3), the sizing arguments every node
//! takes, the built-in enums of their options, and the built-in free functions.

/// A UI primitive: its positional properties, the named properties it accepts and the bare
/// flags it accepts.
pub struct Prim {
    pub name: &'static str,
    pub positional: &'static [&'static str],
    pub named: &'static [&'static str],
    pub flags: &'static [&'static str],
}

const fn prim_of(
    name: &'static str,
    positional: &'static [&'static str],
    named: &'static [&'static str],
) -> Prim {
    Prim {
        name,
        positional,
        named,
        flags: &[],
    }
}

pub const PRIMS: &[Prim] = &[
    prim_of("stack", &[], &["gap", "align"]),
    // `column` is the older name of `stack`.
    prim_of("column", &[], &["gap", "padding", "align"]),
    prim_of(
        "row",
        &[],
        &["gap", "padding", "align", "distribute", "wrap"],
    ),
    prim_of("grid", &[], &["min", "columns", "gap"]),
    prim_of("split", &[], &["side", "ratio", "collapse", "gap"]),
    prim_of("primary", &[], &[]),
    prim_of("secondary", &[], &[]),
    Prim {
        name: "page",
        positional: &[],
        named: &[],
        flags: &["full"],
    },
    prim_of("layers", &[], &[]),
    prim_of("scroll", &[], &["axis"]),
    prim_of("spacer", &[], &[]),
    prim_of("fragment", &[], &[]),
    prim_of("text", &["value"], &["style"]),
    prim_of("heading", &["value"], &["level"]),
    prim_of("button", &["label"], &["on", "style", "size", "enabled"]),
    // A list of rows: each child is a row. A row that is a view call is evaluated only when the
    // host shows it (visible rows plus overscan); `empty` is what an empty list says.
    prim_of("list", &[], &["empty"]),
    // A row of a list: a title, a secondary line, activation and whether it is the selected one.
    prim_of(
        "list_row",
        &["title"],
        &["secondary", "on", "selected", "checked"],
    ),
    prim_of("checkbox", &["label"], &["bind", "on_change", "enabled"]),
    prim_of(
        "input",
        &[],
        &["bind", "placeholder", "on_change", "enabled"],
    ),
    prim_of("image", &["src"], &["alt", "width", "height"]),
    prim_of("progress", &["value"], &[]),
    prim_of("divider", &[], &[]),
    // A block's source rendered inline (implementation.md 7.8). `source` is `Text`, `author` a
    // `Principal`, and `invocation` the `host.Invocation` recorded for an action block.
    prim_of("embed", &["source"], &["author", "invocation"]),
];

/// Sizing and placement arguments accepted by every UI call, including view calls.
pub const NODE_ARGS: &[&str] = &[
    "fill", "min", "max", "aspect", "span", "lines", "truncate", "at",
];

/// Bare flags accepted by every UI call: `truncate`, `fill`.
pub const NODE_FLAGS: &[&str] = &["fill", "truncate"];

/// A built-in enum of the platform: the options of UI primitives and the enums of `space`.
/// Its values are written as dot literals (`gap: .section`); in wasm a value is the index of
/// its variant, and a UI property receives the variant's name.
pub struct Choice {
    pub kind: &'static str,
    /// The name errors use.
    pub name: &'static str,
    pub variants: &'static [&'static str],
}

pub const CHOICES: &[Choice] = &[
    Choice {
        kind: "gap",
        name: "Gap",
        variants: &["tight", "related", "sibling", "section", "group"],
    },
    Choice {
        kind: "align",
        name: "Align",
        variants: &["start", "center", "end", "stretch"],
    },
    Choice {
        kind: "distribute",
        name: "Distribute",
        variants: &["start", "center", "end", "between"],
    },
    Choice {
        kind: "collapse",
        name: "Collapse",
        variants: &["stack", "drawer", "navigate"],
    },
    Choice {
        kind: "side",
        name: "Side",
        variants: &["leading", "trailing"],
    },
    Choice {
        kind: "at",
        name: "Position",
        variants: &[
            "top_leading",
            "top",
            "top_trailing",
            "leading",
            "center",
            "trailing",
            "bottom_leading",
            "bottom",
            "bottom_trailing",
        ],
    },
    Choice {
        kind: "axis",
        name: "Axis",
        variants: &["vertical", "horizontal"],
    },
    Choice {
        kind: "button_style",
        name: "ButtonStyle",
        variants: &["secondary", "primary", "destructive", "plain"],
    },
    Choice {
        kind: "control_size",
        name: "ControlSize",
        variants: &["small", "regular", "large"],
    },
    Choice {
        kind: "text_style",
        name: "TextStyle",
        variants: &[
            "body", "title", "heading", "caption", "label", "code", "display",
        ],
    },
    Choice {
        kind: "width_class",
        name: "WidthClass",
        variants: &["compact", "medium", "expanded", "wide"],
    },
    Choice {
        kind: "orientation",
        name: "Orientation",
        variants: &["portrait", "landscape", "ultrawide"],
    },
];

pub fn choice(kind: &str) -> Option<&'static Choice> {
    CHOICES.iter().find(|c| c.kind == kind)
}

/// The name of a built-in enum, for messages.
pub fn choice_name(kind: &str) -> &'static str {
    choice(kind).map_or("?", |c| c.name)
}

/// The built-in enum an argument of a UI call takes, if it takes one.
pub fn arg_choice(prim: &str, arg: &str) -> Option<&'static str> {
    Some(match (prim, arg) {
        (_, "gap" | "padding") => "gap",
        (_, "align") => "align",
        (_, "distribute") => "distribute",
        (_, "collapse") => "collapse",
        (_, "side") => "side",
        (_, "at") => "at",
        (_, "axis") => "axis",
        ("button", "style") => "button_style",
        ("button", "size") => "control_size",
        ("text", "style") => "text_style",
        _ => return None,
    })
}

/// Free functions of the language itself; `open` and `set` are the platform's actions on the
/// workspace and on view state.
pub const BUILTINS: &[&str] = &[
    "outside",
    "model_call",
    "run_block",
    "invoker",
    "open",
    "set",
];

/// The fields of `space` (layout.md section 4).
pub const SPACE_FIELDS: &str = "width_class, compact, wide, orientation, width, height";

pub fn prim(name: &str) -> Option<&'static Prim> {
    PRIMS.iter().find(|p| p.name == name)
}

pub fn prim_names() -> String {
    PRIMS.iter().map(|p| p.name).collect::<Vec<_>>().join(", ")
}
