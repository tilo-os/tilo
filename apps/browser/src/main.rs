//! Thin entry point: the [`browser::BrowserShell`] app.

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// Profile import comes from the import engine's library, not the tapp.
#[cfg(feature = "runtime-engine")]
extern crate tilo_browser_import_engine;
// Website-session custody comes from the accounts engine the import engine links.
#[cfg(feature = "runtime-engine")]
extern crate tilo_accounts_engine;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1
        && args
            .first()
            .is_some_and(|arg| arg == "--service-session-capability")
    {
        match browser::service_session_capability() {
            Ok(value) => println!("{value}"),
            Err(_) => std::process::exit(1),
        }
        return;
    }
    ui::framework::run::<browser::BrowserShell>();
    // Every tab of this profile browses through one Chromium host; closing the
    // window must not take the other tabs' pages down with it.
    ui::webview::chromium::serve_shared_until_idle();
}
