//! Maps: OpenStreetMap places, search, routes and public transport.
//!
//! The binary runs [`maps::MapsShell`]: the live window, or a `--shots` capture of a scenario
//! (`--scenario NAME`, `--shots-dir DIR`).

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(maps::MapsShell);
