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
/// The last is no app of the platform's: a shelf that composes its own list window from the
/// toolkit's components, which shows an app can do what the standard library does.
pub const EMBEDDED: &[Embedded] = &[
    embedded!("counter"),
    embedded!("tasks"),
    embedded!("notes"),
    // A desk of letters, the shape the outbox and suggestion tests were written against.
    Embedded {
        dir: "letters",
        source: include_str!("../../../crates/lang/tilolang/tests/fixtures/letters/app.tl"),
        ids: include_str!("../../../crates/lang/tilolang/tests/fixtures/letters/app.ids"),
    },
    embedded!("chat"),
    embedded!("mail"),
    embedded!("today"),
    Embedded {
        dir: "handmade",
        source: include_str!("../tests/handmade/app.tl"),
        ids: "",
    },
];

/// The surface of a host running every built-in package.
///
/// # Errors
/// A built-in package does not compile, or the surface cannot be written.
pub fn surface() -> Result<Vec<u8>, String> {
    let sources: Vec<(&str, &str)> = EMBEDDED
        .iter()
        .map(|package| (package.source, package.ids))
        .collect();
    tilolang_tilo::host::builtin_surface(&sources)
}
