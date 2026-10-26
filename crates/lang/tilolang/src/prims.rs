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
    // Frames: what a window's view is (native-apps.md section 3.1).
    prim_of("list", &["items"], &["sort", "filter", "empty"]),
    prim_of("document", &[], &[]),
    prim_of("page", &[], &[]),
    prim_of("canvas", &[], &[]),
    // The frame's side columns.
    prim_of("sidebar", &[], &[]),
    prim_of("inspector", &["item"], &[]),
    // The frame's entry bar along its foot: what a chat types and sends.
    prim_of("composer", &[], &[]),
    // Content groups and native controls (section 3.3).
    prim_of("section", &["title"], &[]),
    prim_of("facts", &[], &[]),
    prim_of("form", &[], &[]),
    prim_of("field", &["label"], &[]),
    prim_of("toggle", &["label"], &["bind", "enabled"]),
    prim_of("choice", &[], &["bind"]),
    prim_of("date", &[], &["bind"]),
    prim_of("card", &["item"], &["count"]),
    prim_of("avatar", &["who"], &[]),
    // Layout for content.
    prim_of("stack", &[], &["gap", "align"]),
    // `column` is the older name of `stack`.
    prim_of("column", &[], &["gap", "padding", "align"]),
    // With a block, a layout row; with a title and no block, a list row (`ListRow`).
    prim_of("row", &[], LAYOUT_ROW_ARGS),
    prim_of("grid", &[], &["min", "columns", "gap"]),
    prim_of("split", &[], &["side", "ratio", "collapse", "gap"]),
    prim_of("primary", &[], &[]),
    prim_of("secondary", &[], &[]),
    prim_of("layers", &[], &[]),
    prim_of("scroll", &[], &["axis"]),
    prim_of("spacer", &[], &[]),
    prim_of("fragment", &[], &[]),
    prim_of("text", &["value"], &["role"]),
    prim_of("button", &["label"], &["on", "enabled"]),
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

/// How many rows a `list` inside a `card` shows: a card is a summary of its resource, which it
/// opens for the whole list. The list node carries it as its `preview` property.
pub const PREVIEW_ROWS: i64 = 5;

/// The frames: one of them is the whole of a window's view.
pub const FRAMES: &[&str] = &["list", "document", "page", "canvas"];

/// The named arguments of a layout row: `row(gap: ...) { ... }`.
pub const LAYOUT_ROW_ARGS: &[&str] = &["gap", "padding", "align", "distribute", "wrap"];

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
        kind: "text_role",
        name: "TextRole",
        variants: &["body", "title", "label", "caption", "numeric"],
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
        ("text", "role") => "text_role",
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
    "now",
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
