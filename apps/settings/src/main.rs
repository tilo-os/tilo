//! Settings — standard Tilo application entry point.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(
    settings::SettingsShell,
    init = settings::enable_provider_administration
);
