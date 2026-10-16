//! The Tilo Store binary: the framework app, its launch on a `.flatpakref`
//! or a `tilo-store:app/<id>` link, and its screenshots.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(store::StoreShell);
