//! Settings — standard Tilo application entry point.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

// The release build runs the whole app from the Settings engine library in
// the runtime bundle (libtilo_settings_engine.so, crates/runtime/engines/
// settings), which carries this package's library: the binary in the package
// is only that call.
ui::main!(
    settings::SettingsShell,
    init = settings::enable_provider_administration,
    engine = "tilo_settings_engine"
);
