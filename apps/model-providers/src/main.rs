#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(
    model_providers::ProviderSettingsApp,
    init = || model_providers::ProviderSettingsApp.init()
);
