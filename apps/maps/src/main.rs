//! Maps — thin binary entry point. All logic lives in the library.
//!
//! The live window and `--shots` both run on the app framework; a capture
//! stages the bundled sample city through `MapsShell::scenario` and
//! `ResourceSurface::stage` (`--scenario NAME`, see `MapsShell::scenarios`).

#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::framework::run::<maps::MapsShell>();
}
