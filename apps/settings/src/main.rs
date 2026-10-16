//! Settings — standard Tilo application entry point.

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// Account custody and the IMAP client come from their engine library, not
// the runtime.
#[cfg(feature = "runtime-engine")]
extern crate tilo_accounts_engine;

ui::main!(
    settings::SettingsShell,
    init = settings::enable_provider_administration
);
