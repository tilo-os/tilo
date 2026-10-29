//! Settings — standard Tilo application entry point.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(not(feature = "runtime-engine"))]
ui::main!(
    settings::SettingsShell,
    init = settings::enable_provider_administration
);

// The release build runs the whole app from the Settings engine library in
// the runtime bundle (libtilo_settings_engine.so, crates/runtime/engines/
// settings), which carries this package's library: the binary in the
// package is only this call. The engine depends on this package, so the
// binary names the library itself rather than through Cargo.
#[cfg(feature = "runtime-engine")]
#[link(name = "tilo_settings_engine", kind = "dylib")]
unsafe extern "C-unwind" {
    safe fn tilo_settings_engine_run();
}

#[cfg(feature = "runtime-engine")]
fn main() {
    tilo_settings_engine_run();
}
