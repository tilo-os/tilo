//! The built-in packages as test fixtures: their sources, compiled in the test process.
//!
//! The runtime binary compiles nothing; an installed app's package arrives compiled
//! (`TILO_APP_MODULE`). The window's own tests still run several packages in one host, which
//! is what they were written against.

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

/// The surface of a host running every built-in package.
#[must_use]
pub fn surface() -> Vec<u8> {
    let sources: Vec<(&str, &str)> = EMBEDDED
        .iter()
        .map(|package| (package.source, package.ids))
        .collect();
    tilolang_tilo::host::builtin_surface(&sources)
}
