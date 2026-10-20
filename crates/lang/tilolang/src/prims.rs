//! The UI primitives a view can build (layout.md section 3), the sizing arguments every node
//! takes, the bare tokens that are valid as UI arguments, and the built-in free functions.

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
    prim_of("button", &["label"], &["on", "style", "enabled"]),
    prim_of("checkbox", &["label"], &["bind", "on_change", "enabled"]),
    prim_of(
        "input",
        &[],
        &["bind", "placeholder", "on_change", "enabled"],
    ),
    prim_of("image", &["src"], &["alt", "width", "height"]),
    prim_of("progress", &["value"], &[]),
    prim_of("divider", &[], &[]),
];

/// Sizing and placement arguments accepted by every UI call, including view calls.
pub const NODE_ARGS: &[&str] = &[
    "fill", "min", "max", "aspect", "span", "lines", "truncate", "at",
];

/// Bare flags accepted by every UI call: `truncate`, `fill`.
pub const NODE_FLAGS: &[&str] = &["fill", "truncate"];

/// Bare identifiers that are values only as named UI arguments: rhythm tokens, alignments,
/// sides, positions and collapse modes.
pub const TOKENS: &[&str] = &[
    "tight",
    "related",
    "sibling",
    "section",
    "group",
    "start",
    "center",
    "end",
    "stretch",
    "between",
    "leading",
    "trailing",
    "top_leading",
    "top",
    "top_trailing",
    "bottom_leading",
    "bottom",
    "bottom_trailing",
    "stack",
    "drawer",
    "navigate",
];

/// Free functions of the language itself.
pub const BUILTINS: &[&str] = &["outside", "model_call"];

/// The variants of the enums that `space` exposes, in the order of their integer values.
pub fn enum_variants(kind: &str) -> &'static [&'static str] {
    match kind {
        "width_class" => &["compact", "medium", "expanded", "wide"],
        "orientation" => &["portrait", "landscape", "ultrawide"],
        _ => &[],
    }
}

/// The fields of `space` (layout.md section 4).
pub const SPACE_FIELDS: &str = "width_class, compact, wide, orientation, width, height";

pub fn prim(name: &str) -> Option<&'static Prim> {
    PRIMS.iter().find(|p| p.name == name)
}

pub fn prim_names() -> String {
    PRIMS.iter().map(|p| p.name).collect::<Vec<_>>().join(", ")
}
