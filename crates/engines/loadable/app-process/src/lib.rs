//! The catalogue Store browses, as its own library in the runtime bundle
//! (`libtilo_app_process_engine.so`).
//!
//! Like `runtime-dylib` it re-exports what it provides, which records it
//! as provided by this library for the apps' `-C prefer-dynamic` link.
//! Outside the thin build it is empty.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use app_catalogue;

/// Run the engine's own code: block an app in an empty block list, which must
/// then block it whatever its case.
///
/// The engine's check binary calls it, so a release proves the library loads
/// and runs against the runtime it ships with.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    let mut blocks = app_catalogue::BlockList::empty();
    blocks.block("so.example.app");
    if !blocks.is_blocked("SO.EXAMPLE.APP") || blocks.is_blocked("so.example.other") {
        return Err("the block list blocked the wrong apps".to_owned());
    }
    Ok("blocked 1 app".to_owned())
}
