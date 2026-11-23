//! The UI primitives a view can build (layout.md, "Primitives"): their arguments, the built-in enums
//! of their options, and the built-in free functions.

/// A UI primitive: its positional and named properties and its bare flags.
pub struct Prim {
    pub name: &'static str,
    pub positional: &'static [&'static str],
    pub named: &'static [&'static str],
    pub flags: &'static [&'static str],
    /// The toolkit component this primitive is a shorter name for: its call is that component's
    /// call.
    pub component: Option<&'static str>,
    /// The toolkit component whose kind this primitive builds, when that is not its own name.
    pub builds: Option<&'static str>,
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
        component: None,
        builds: None,
    }
}

const fn prim_as(
    name: &'static str,
    component: &'static str,
    positional: &'static [&'static str],
    named: &'static [&'static str],
) -> Prim {
    Prim {
        name,
        positional,
        named,
        flags: &[],
        component: None,
        builds: Some(component),
    }
}

const fn shorthand(name: &'static str, component: &'static str) -> Prim {
    Prim {
        name,
        positional: &[],
        named: &[],
        flags: &[],
        component: Some(component),
        builds: None,
    }
}

pub const PRIMS: &[Prim] = &[
    // The toolkit's components under the names a view writes most.
    shorthand("text", "Text"),
    shorthand("rich_text", "BlockEditor"),
    shorthand("section", "Section"),
    shorthand("avatar", "Avatar"),
    shorthand("progress", "ProgressBar"),
    shorthand("toggle", "Toggle"),
    shorthand("checkbox", "Checkbox"),
    shorthand("input", "TextInput"),
    // Rich text bound to a `RichText`; a document's running text is `ui.BlockEditor`.
    shorthand("rich_input", "RichInput"),
    shorthand("image", "Image"),
    // A list of resources in the content (a window's own list is `std.list`); with `full`, the
    // column's width.
    prim_of("list", &["items"], &[]),
    Prim {
        name: "page",
        positional: &[],
        named: &[],
        flags: &["full"],
        component: None,
        builds: None,
    },
    // A resource's facts as one row of chips, each from the field it shows.
    prim_of("facts", &["of"], &[]),
    // A form's fields, and the control for a view state of an enum's cases.
    prim_of("form", &[], &[]),
    prim_of("field", &["label"], &["help"]),
    prim_of("choice", &[], &["bind"]),
    // `motion:` names how what a container holds moves when it changes: `.list` (the default)
    // slides, fades in and fades out; `.cross_fade` swaps one child for another in place; `.none`
    // lands at once.
    prim_of("stack", &[], &["gap", "padding", "align", "motion"]),
    // With a block, a layout row; with a title and no block, a list row.
    prim_of("row", &[], LAYOUT_ROW_ARGS),
    prim_of("grid", &[], &["min", "columns", "gap", "motion"]),
    prim_of("split", &[], &["side", "ratio", "collapse", "gap"]),
    prim_of("primary", &[], &[]),
    prim_of("secondary", &[], &[]),
    prim_of("layers", &[], &[]),
    // A column that scrolls: down, sideways (`axis: .horizontal`) or both ways.
    prim_of("scroll", &[], &["axis", "across", "both"]),
    prim_of("spacer", &[], &[]),
    prim_of("fragment", &[], &[]),
    // A button for a declared action, whose label, icon and role are the action's; or a plain
    // button with its own label and `on:`.
    prim_as(
        "button",
        "Button",
        &["label"],
        &["on", "enabled", "description"],
    ),
    prim_of("divider", &[], &[]),
    // A block's source rendered inline. `source` is `Text`, `author` an `Actor`, `invocation`
    // the `host.Invocation` recorded for an action block.
    prim_of("embed", &["source"], &["author", "invocation"]),
    // What waits to be proposed of a resource (`suggest`), each as the card a person accepts or
    // dismisses it with.
    prim_of("proposals", &["of"], &[]),
    // A resource drawn through its type's default view, whatever its type (`show(r)`); its type
    // must have a default view (`meta(r).opens`).
    prim_of("show", &["of"], &["view", "row"]),
];

/// How many rows a `list` inside a `ui.Card` shows: a card is a summary, which it opens for the
/// whole list.
pub const PREVIEW_ROWS: i64 = 5;

pub const LAYOUT_ROW_ARGS: &[&str] = &["gap", "padding", "align", "distribute", "wrap", "motion"];

/// Sizing and placement arguments every UI call takes. `lead` counts row leads: a container that
/// says `lead: 1` stands its content one lead in, under the text a heading's control leads to.
pub const NODE_ARGS: &[&str] = &[
    "fill", "min", "max", "aspect", "span", "lines", "truncate", "at", "lead",
];

/// Bare flags every UI call takes.
pub const NODE_FLAGS: &[&str] = &["fill", "truncate"];

/// A built-in enum of the platform. Values are dot literals (`gap: .section`); in wasm a value is
/// the index of its variant.
pub struct Choice {
    pub kind: &'static str,
    pub name: &'static str,
    pub variants: &'static [&'static str],
}

pub const CHOICES: &[Choice] = &[
    Choice {
        kind: "gap",
        name: "Gap",
        variants: &["tight", "related", "sibling", "section", "group", "none"],
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
        kind: "motion",
        name: "Motion",
        variants: &["list", "cross_fade", "none"],
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
        variants: &["vertical", "horizontal", "both"],
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
    Choice {
        kind: "actor_kind",
        name: "ActorKind",
        variants: &["person", "agent"],
    },
    Choice {
        kind: "weekday",
        name: "Weekday",
        variants: &[
            "monday",
            "tuesday",
            "wednesday",
            "thursday",
            "friday",
            "saturday",
            "sunday",
        ],
    },
];

pub fn choice(kind: &str) -> Option<&'static Choice> {
    CHOICES.iter().find(|c| c.kind == kind)
}

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
        (_, "motion") => "motion",
        (_, "side") => "side",
        (_, "at") => "at",
        (_, "axis") => "axis",
        _ => return None,
    })
}

/// Free functions of the language itself, written as a bare name; `open` and `set` are the
/// platform's actions. The words marker is `std.say`, so a chat app's `say` action keeps the bare
/// name.
pub const BUILTINS: &[&str] = &[
    "outside",
    "model_call",
    "run_block",
    "invoker",
    "now",
    "today",
    "Duration",
    "open",
    "set",
    "resource",
    "preview_rows",
];

/// The fields of `workspace`.
pub const WORKSPACE_FIELDS: &str = "members, me";

/// The fields of `space` (layout.md, "Responding to space").
pub const SPACE_FIELDS: &str = "width_class, compact, wide, orientation, width, height";

pub fn prim(name: &str) -> Option<&'static Prim> {
    PRIMS.iter().find(|p| p.name == name)
}

pub fn prim_names() -> String {
    PRIMS.iter().map(|p| p.name).collect::<Vec<_>>().join(", ")
}

// The platform's icon set (`ui::icons`), in its kebab-case names, from the toolkit's icon sources
// (`build.rs`). Source writes them as dot literals in snake case (`.more_horizontal`).
include!(concat!(env!("OUT_DIR"), "/icons.rs"));

/// The kebab-case icon a dot literal names (`more_horizontal` is `more-horizontal`).
pub fn icon(snake: &str) -> Option<&'static str> {
    let kebab = snake.replace('_', "-");
    ICONS.iter().find(|i| **i == kebab).copied()
}

pub fn unknown_icon(snake: &str) -> String {
    let close = icon_suggestions(snake);
    let hint = if close.is_empty() {
        "; icons are the platform's icon set in snake case, such as `.plus`, `.pin` or `.more_horizontal`".to_string()
    } else {
        format!("; close ones: {}", close.join(", "))
    };
    format!("there is no icon `.{snake}`{hint}")
}

fn icon_suggestions(snake: &str) -> Vec<String> {
    let words: Vec<&str> = snake.split('_').filter(|w| w.len() > 2).collect();
    let mut out: Vec<String> = ICONS
        .iter()
        .filter(|i| {
            i.split('-').filter(|part| part.len() > 2).any(|part| {
                words
                    .iter()
                    .any(|w| part.starts_with(w) || w.starts_with(part))
            })
        })
        .map(|i| format!(".{}", i.replace('-', "_")))
        .collect();
    out.truncate(3);
    out
}
