//! The built-in packages: tilolang sources embedded at build time.
//!
//! Compiling a package takes milliseconds, so the host compiles each at startup instead of
//! shipping wasm. The ids lock sits next to the source and is committed, so every replica of a
//! room compiles the same declaration ids.

macro_rules! embedded {
    ($dir:literal) => {
        Embedded {
            dir: $dir,
            source: include_str!(concat!("../../tl/", $dir, "/app.tl")),
            ids: include_str!(concat!("../../tl/", $dir, "/app.ids")),
        }
    };
}

/// One embedded package.
pub struct Embedded {
    /// The package's directory under `apps/tl`.
    pub dir: &'static str,
    pub source: &'static str,
    pub ids: &'static str,
}

/// Every built-in package, in the order they load: a package comes after those it imports.
pub const EMBEDDED: &[Embedded] = &[
    embedded!("counter"),
    embedded!("todo"),
    embedded!("notes"),
    embedded!("letters"),
    embedded!("chat"),
    embedded!("board"),
];
