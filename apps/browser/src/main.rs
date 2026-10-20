//! Thin binary entry point: the [`browser::BrowserShell`] framework app.
//! Every launch mode — a window showing one page, a private window
//! (`--private`), a page named on the command line, `--shots` scenarios and
//! the live-engine captures (`--shots-live`) — is the framework's.

#[cfg(feature = "thin")]
extern crate runtime_dylib;
// The import from another browser's profile comes from its engine library,
// not the tapp.
#[cfg(feature = "runtime-engine")]
extern crate tilo_browser_import_engine;
// Website-session custody comes from the accounts engine.
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
    // Every Browser tab of this profile browses through one Chromium host.
    // When it is this process's, the window closing must not take the
    // other tabs' pages down with it.
    ui::webview::chromium::serve_shared_until_idle();
}
