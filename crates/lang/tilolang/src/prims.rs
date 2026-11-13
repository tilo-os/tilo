//! The UI primitives a view can build (layout.md, "Primitives"), the sizing arguments every node
//! takes, the built-in enums of their options, and the built-in free functions. A primitive that
//! is a toolkit component (`text`, `input`, `toggle`) is only its shorter name: its arguments,
//! slots, events and bindings are the component's description's.

/// A UI primitive: its positional properties, the named properties it accepts and the bare
/// flags it accepts.
pub struct Prim {
    pub name: &'static str,
    pub positional: &'static [&'static str],
    pub named: &'static [&'static str],
    pub flags: &'static [&'static str],
    /// The toolkit component this primitive is a shorter name for, when it is one: its call is
    /// that component's call, checked and lowered from the component's description, and adds
    /// nothing of its own.
    pub component: Option<&'static str>,
    /// The toolkit component whose kind this primitive builds, when that is not its own name:
    /// `button` builds a `ui.Button`, each with arguments of its own.
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

/// A primitive that takes arguments of its own and builds the toolkit component `component`.
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

/// A primitive that is the toolkit component `component` under a shorter name (`text` is
/// `ui.Text`): it takes exactly the component's arguments, slots, events and bindings.
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
    // The toolkit's components, under the names a view writes most: each is that component's
    // call and nothing more.
    shorthand("text", "Text"),
    shorthand("rich_text", "BlockEditor"),
    shorthand("section", "Section"),
    shorthand("avatar", "Avatar"),
    shorthand("progress", "ProgressBar"),
    shorthand("toggle", "Toggle"),
    shorthand("checkbox", "Checkbox"),
    shorthand("input", "TextInput"),
    // Rich text typed, bound to a `RichText`: what a completion picks goes in as a reference
    // (`ui.RichInput`). A document's own running text is `ui.BlockEditor`, which a view places
    // and binds to the field itself.
    shorthand("rich_input", "RichInput"),
    shorthand("image", "Image"),
    // A list of resources in the content (a window's own list is `std.list`), and a page:
    // content at the readable measure, or the column's full width with `full`.
    prim_of("list", &["items"], &[]),
    Prim {
        name: "page",
        positional: &[],
        named: &[],
        flags: &["full"],
        component: None,
        builds: None,
    },
    // A resource's facts as one row of chips, each from the field it shows, with the view's own
    // chips in its block.
    prim_of("facts", &["of"], &[]),
    // Native controls that are no component: a form's fields, and the control for a view state
    // of an enum's cases.
    prim_of("form", &[], &[]),
    prim_of("field", &["label"], &[]),
    prim_of("choice", &[], &["bind"]),
    // Layout for content.
    prim_of("stack", &[], &["gap", "padding", "align"]),
    // With a block, a layout row; with a title and no block, a list row (`ListRow`).
    prim_of("row", &[], LAYOUT_ROW_ARGS),
    prim_of("grid", &[], &["min", "columns", "gap"]),
    prim_of("split", &[], &["side", "ratio", "collapse", "gap"]),
    prim_of("primary", &[], &[]),
    prim_of("secondary", &[], &[]),
    prim_of("layers", &[], &[]),
    // A column that scrolls: down, sideways (`axis: .horizontal`) or both ways (`axis: .both`,
    // a 2D surface such as a desk of strips).
    prim_of("scroll", &[], &["axis", "across", "both"]),
    prim_of("spacer", &[], &[]),
    prim_of("fragment", &[], &[]),
    // A button for a declared action, whose label, icon and role are the action's; or a plain
    // button with its own label and `on:`.
    prim_as("button", "Button", &["label"], &["on", "enabled"]),
    prim_of("divider", &[], &[]),
    // A block's source rendered inline (`tilolang_runtime::embed`). `source` is `Text`, `author` a
    // `Principal`, and `invocation` the `host.Invocation` recorded for an action block.
    prim_of("embed", &["source"], &["author", "invocation"]),
    // What waits to be proposed of a resource (`suggest`), each as the card a person accepts or
    // dismisses it with: the action's `proposal` view where it declares one, else what the
    // action would do in words. `of` is a resource the proposals are about.
    prim_of("proposals", &["of"], &[]),
    // A resource drawn through its type's default view, as its own package draws it, whatever
    // its type: what a shell composes a place from when it knows the resource by id alone
    // (`show(r)`, shell.md §8). `of` is a resource; its type must have a default view
    // (`meta(r).opens`). `view` (`Text`, a view's id or name, as an arrangement keeps it) draws
    // it through another of its package's views that takes it alone; empty for the default.
    // `row` draws it as a list of its type draws each item, through the row its type declares.
    prim_of("show", &["of"], &["view", "row"]),
];

/// How many rows a `list` inside a `ui.Card` shows: a card is a summary of its resource, which it
/// opens for the whole list. The list node carries it as its `preview` property.
pub const PREVIEW_ROWS: i64 = 5;

/// The named arguments of a layout row: `row(gap: ...) { ... }`.
pub const LAYOUT_ROW_ARGS: &[&str] = &["gap", "padding", "align", "distribute", "wrap"];

/// Sizing and placement arguments accepted by every UI call, including view calls.
///
/// `lead` counts row leads — the gutter a row's leading control stands in, its cell and the
/// gap after it: a container that says `lead: 1` stands its content one lead in, under the
/// text a heading's control leads to.
pub const NODE_ARGS: &[&str] = &[
    "fill", "min", "max", "aspect", "span", "lines", "truncate", "at", "lead",
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
    // What a principal is (`p.kind()`): a person, or software acting in the workspace.
    Choice {
        kind: "principal_kind",
        name: "PrincipalKind",
        variants: &["person", "agent"],
    },
    // The day of the week a day is (`d.weekday()`), Monday first, as a date library counts
    // them; `d.start_of_week()` is the one a region starts its week on.
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
        _ => return None,
    })
}

/// Free functions of the language itself, written as a bare name; `open` and `set` are the
/// platform's actions on the workspace and on view state. A word an app naturally takes for one
/// of its own actions is not here: the words marker is `std.say`, in the library's namespace
/// (`stdlib::NS`), so a chat app's `say` action keeps the bare name.
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

/// The icons a declaration or a row can name: the platform's icon set (`ui::icons`), in its
/// kebab-case names. Source writes them as dot literals in snake case (`.more_horizontal`).
pub const ICONS: &[&str] = &[
    "a-arrow-down",
    "a-arrow-up",
    "accessibility",
    "a-large-small",
    "alarm-clock",
    "align-center",
    "align-center-horizontal",
    "align-center-vertical",
    "align-end-horizontal",
    "align-end-vertical",
    "align-horizontal-distribute-center",
    "align-justify",
    "align-left",
    "align-right",
    "align-start-horizontal",
    "align-start-vertical",
    "align-vertical-distribute-center",
    "app-window",
    "archive",
    "arrow-big-up",
    "arrow-down",
    "arrow-down-left",
    "arrow-down-right",
    "arrow-left",
    "arrow-right",
    "arrow-up",
    "arrow-up-down",
    "arrow-up-left",
    "arrow-up-right",
    "ban",
    "bar-chart",
    "baseline",
    "battery",
    "battery-charging",
    "bed-double",
    "beer",
    "bell",
    "bell-off",
    "bluetooth",
    "bold",
    "bookmark",
    "bot",
    "box",
    "bring-to-front",
    "brush",
    "calendar",
    "camera",
    "check",
    "chevron-down",
    "chevron-left",
    "chevron-right",
    "chevron-up",
    "circle",
    "circle-alert",
    "circle-check",
    "circle-dashed",
    "clipboard-paste",
    "clock",
    "cloud",
    "cloud-download",
    "cloud-rain",
    "cloud-snow",
    "code",
    "coffee",
    "columns-2",
    "command",
    "contrast",
    "copy",
    "corner-down-left",
    "corner-up-left",
    "corner-up-right",
    "crop",
    "delete",
    "disc-3",
    "download",
    "droplet",
    "eraser",
    "ethernet-port",
    "external-link",
    "eye",
    "eye-off",
    "file",
    "file-code",
    "file-plus",
    "file-text",
    "film",
    "fingerprint",
    "flag",
    "flame",
    "folder",
    "folder-open",
    "folder-plus",
    "forward",
    "gallery-vertical-end",
    "gauge",
    "globe",
    "hand",
    "hard-drive",
    "hash",
    "heading-1",
    "heading-2",
    "headphones",
    "heart",
    "highlighter",
    "history",
    "home",
    "hospital",
    "hourglass",
    "image",
    "inbox",
    "info",
    "italic",
    "keyboard",
    "landmark",
    "lasso",
    "layers",
    "layout-grid",
    "lightbulb",
    "link",
    "list",
    "list-checks",
    "list-music",
    "list-ordered",
    "list-tree",
    "list-video",
    "locate-fixed",
    "lock",
    "lock-open",
    "mail",
    "mail-open",
    "map-pin",
    "maximize-2",
    "menu",
    "message-circle",
    "message-square",
    "mic",
    "mic-off",
    "minimize-2",
    "minus",
    "monitor",
    "monitor-smartphone",
    "moon",
    "more-horizontal",
    "mouse",
    "mouse-pointer-2",
    "music",
    "navigation",
    "paint-bucket",
    "palette",
    "panel-left",
    "panel-right",
    "paperclip",
    "pause",
    "paw-print",
    "pencil",
    "pen-tool",
    "phone",
    "phone-off",
    "picture-in-picture-2",
    "pin",
    "pipette",
    "plane",
    "play",
    "plus",
    "power",
    "printer",
    "qr-code",
    "quote",
    "redo-2",
    "refresh",
    "repeat",
    "repeat-1",
    "reply",
    "reply-all",
    "rotate-ccw",
    "rotate-cw",
    "rss",
    "ruler",
    "scan-line",
    "scissors",
    "screen-share",
    "sd-card",
    "search",
    "send",
    "send-to-back",
    "settings",
    "settings-2",
    "shield",
    "shopping-bag",
    "shuffle",
    "skip-back",
    "skip-forward",
    "slash",
    "smile",
    "smile-plus",
    "sparkles",
    "speaker",
    "square",
    "square-dashed",
    "squares-exclude",
    "squares-intersect",
    "squares-subtract",
    "squares-unite",
    "square-terminal",
    "star",
    "strikethrough",
    "subscript",
    "sun",
    "superscript",
    "switch-camera",
    "table",
    "tag",
    "terminal",
    "thumbs-down",
    "thumbs-up",
    "tilo",
    "timer",
    "train-front",
    "trash",
    "trash-2",
    "trees",
    "triangle-alert",
    "trophy",
    "type",
    "underline",
    "undo-2",
    "unlink",
    "upload",
    "usb",
    "user",
    "user-plus",
    "users",
    "utensils",
    "video",
    "volume-2",
    "volume-x",
    "wand-sparkles",
    "wifi",
    "wifi-off",
    "wind",
    "x",
];

/// The kebab-case icon a dot literal names (`more_horizontal` is `more-horizontal`), when the
/// platform has it.
pub fn icon(snake: &str) -> Option<&'static str> {
    let kebab = snake.replace('_', "-");
    ICONS.iter().find(|i| **i == kebab).copied()
}

/// The error for the unknown icon `.snake`, with close ones when there are.
pub fn unknown_icon(snake: &str) -> String {
    let close = icon_suggestions(snake);
    let hint = if close.is_empty() {
        "; icons are the platform's icon set in snake case, such as `.plus`, `.pin` or `.more_horizontal`".to_string()
    } else {
        format!("; close ones: {}", close.join(", "))
    };
    format!("there is no icon `.{snake}`{hint}")
}

/// Up to three known icons whose names share a word with `snake`, for messages.
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
