//! Settings — standard Tilo application entry point.

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// Account custody and the IMAP client come from their engine library, not
// the runtime; the libraries only Settings uses from theirs, not the tapp.
#[cfg(feature = "runtime-engine")]
extern crate tilo_accounts_engine;
#[cfg(feature = "runtime-engine")]
extern crate tilo_settings_engine;

ui::main!(
    settings::SettingsShell,
    init = settings::enable_provider_administration
);
