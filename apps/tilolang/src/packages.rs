//! The built-in packages: tilolang sources embedded at build time.
//!
//! Compiling a package takes milliseconds, so the host compiles each at startup instead of
//! shipping wasm. The ids lock sits next to the source and is committed, so every replica of a
//! room compiles the same declaration ids.

/// One embedded package.
pub struct Embedded {
    /// The package's directory under `apps/tl`.
    pub dir: &'static str,
    pub source: &'static str,
    pub ids: &'static str,
}

/// Every built-in package, in the order they load: a package comes after those it imports.
pub const EMBEDDED: &[Embedded] = &[
    Embedded {
        dir: "counter",
        source: include_str!("../../tl/counter/app.tl"),
        ids: include_str!("../../tl/counter/app.ids"),
    },
    Embedded {
        dir: "notes",
        source: include_str!("../../tl/notes/app.tl"),
        ids: include_str!("../../tl/notes/app.ids"),
    },
];
