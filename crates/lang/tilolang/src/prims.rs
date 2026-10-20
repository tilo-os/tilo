//! The UI primitives a view can build, and the built-in free functions.

/// A UI primitive: its positional properties and the named properties it accepts.
pub struct Prim {
    pub name: &'static str,
    pub positional: &'static [&'static str],
    pub named: &'static [&'static str],
}

pub const PRIMS: &[Prim] = &[
    Prim {
        name: "column",
        positional: &[],
        named: &["gap", "padding", "align"],
    },
    Prim {
        name: "row",
        positional: &[],
        named: &["gap", "padding", "align"],
    },
    Prim {
        name: "fragment",
        positional: &[],
        named: &[],
    },
    Prim {
        name: "scroll",
        positional: &[],
        named: &[],
    },
    Prim {
        name: "text",
        positional: &["value"],
        named: &["style"],
    },
    Prim {
        name: "heading",
        positional: &["value"],
        named: &["level"],
    },
    Prim {
        name: "button",
        positional: &["label"],
        named: &["on", "style", "enabled"],
    },
    Prim {
        name: "checkbox",
        positional: &["label"],
        named: &["bind", "on_change", "enabled"],
    },
    Prim {
        name: "input",
        positional: &[],
        named: &["bind", "placeholder", "on_change", "enabled"],
    },
    Prim {
        name: "image",
        positional: &["src"],
        named: &["alt", "width", "height"],
    },
    Prim {
        name: "progress",
        positional: &["value"],
        named: &[],
    },
    Prim {
        name: "spacer",
        positional: &[],
        named: &[],
    },
    Prim {
        name: "divider",
        positional: &[],
        named: &[],
    },
];

/// Free functions of the language itself.
pub const BUILTINS: &[&str] = &["outside", "model_call"];

pub fn prim(name: &str) -> Option<&'static Prim> {
    PRIMS.iter().find(|p| p.name == name)
}

pub fn prim_names() -> String {
    PRIMS.iter().map(|p| p.name).collect::<Vec<_>>().join(", ")
}
