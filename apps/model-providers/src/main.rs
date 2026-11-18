#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

// The release build starts the app from the Settings engine library in the
// runtime bundle, which holds the panel for Settings already; this binary is
// only that call.
ui::main!(
    model_providers::ProviderSettingsApp,
    init = || model_providers::ProviderSettingsApp.init(),
    engine = "tilo_settings_engine",
    entry = tilo_model_providers_engine_main
);
