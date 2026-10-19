//! Store's side of the app host, as an engine library beside the shared
//! runtime (`libtilo_app_host_engine.so`).
//!
//! Store installs, updates and removes apps through the app host, a
//! privileged daemon it reaches over Cap'n Proto RPC (`app-host`'s client),
//! and browses the catalogue the desktop's search shares (`app-catalogue`).
//! No other app talks to the app host, so neither belongs in the runtime
//! every app maps; linked into the tapp, the client with its RPC and the
//! catalogue were about a fifth of Store's code. Here they are a library of
//! their own in the runtime bundle that only Store names `DT_NEEDED`, and the
//! tapp carries Store's own code.
//!
//! Like `runtime-dylib`, this crate re-exports the crates it provides, which
//! records them as provided by this library for the apps' `-C prefer-dynamic`
//! link. Outside the thin build (`thin`) it is empty: see the manifest.

// Load the runtime explicitly, as thin apps do: rustc ignores an `--extern`
// nothing names, and without it in the crate graph this library would link
// its own copy of every crate the runtime already provides, which the app's
// link then refuses (each crate may show up once).
#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use app_catalogue;
#[cfg(feature = "thin")]
pub use app_host;

/// Run the engine's own code: review bytes that are no app archive through
/// the app host's client, which must refuse them, and block an app in an
/// empty block list, which must then block it whatever its case.
///
/// The engine's check binary calls it, so a release proves the library loads
/// and runs against the runtime it ships with.
///
/// # Errors
/// Names what the client or the block list did wrongly.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    use app_host::local_package::{ArchiveReviewError, review_archive};

    match review_archive(b"not an app archive") {
        Err(ArchiveReviewError::Invalid) => {}
        Err(other) => return Err(format!("the client refused bytes as {other}")),
        Ok(_) => return Err("the client accepted bytes that are no archive".to_owned()),
    }
    let mut blocks = app_catalogue::BlockList::empty();
    blocks.block("so.example.app");
    if !blocks.is_blocked("SO.EXAMPLE.APP") || blocks.is_blocked("so.example.other") {
        return Err("the block list blocked the wrong apps".to_owned());
    }
    Ok("refused an invalid archive, blocked 1 app".to_owned())
}
