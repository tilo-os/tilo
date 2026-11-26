//! The app ↔ runtime interface's version and the shell-owned `tools` service, apart from the
//! transport plumbing (`runtime_portal`) so a version bump rebuilds only what names it.

pub mod tools;

/// The app ↔ runtime interface's current version: what an app and the
/// session services the launcher connects it to (collaboration, the
/// filesystem and its document media, the app host, the identity broker,
/// shell-owned tools) speak. An app's manifest names the version it was
/// built against, and it runs where the image serves that version: the
/// current one or any since [`OLDEST_RUNTIME_INTERFACE_VERSION`]
/// (docs/architecture/compatibility.md). `interface.toml` holds the schema,
/// and the repository policy fails a change to it that keeps the number.
pub const RUNTIME_INTERFACE_VERSION: u32 = 54;

/// The oldest runtime interface version this build still works with. An app
/// built against any version from it to [`RUNTIME_INTERFACE_VERSION`] runs.
///
/// A version bump keeps it where it is: it moves only when the change is
/// breaking, to the version before the change, once this build no longer
/// reads or writes the wire that version used. A bump that keeps reading the
/// previous wire leaves apps built against it running, and a bump that breaks
/// it stops them, which is the one case raising this number is for. Set it by
/// hand in the bumping change, beside the schema change that broke the wire.
///
/// The image declares the two in `os/usr/share/tilo/system-interfaces.toml`
/// (`[interfaces] runtime` and `[runtime] oldest`), and
/// `tools/policy/check_interfaces.py` fails a change that moves one without
/// the other.
pub const OLDEST_RUNTIME_INTERFACE_VERSION: u32 = 54;
