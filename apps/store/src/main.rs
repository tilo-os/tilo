//! The Tilo Store binary: the framework app, its launch on a `.flatpakref`
//! or a `tilo-store:app/<id>` link, and its screenshots.

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// The catalogue comes from its engine library, not the tapp.
#[cfg(feature = "runtime-engine")]
extern crate tilo_app_process_engine;

ui::main!(store::StoreShell);
