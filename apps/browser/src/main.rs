//! Thin binary entry point: the [`browser::BrowserShell`] framework app.
//! Every launch mode — a window showing one page, a private window
//! (`--private`), a page named on the command line, `--shots` scenarios and
//! the live-engine captures (`--shots-live`) — is the framework's.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    ui::framework::run::<browser::BrowserShell>();
    // Every Browser tab of this profile browses through one Chromium host.
    // When it is this process's, the window closing must not take the
    // other tabs' pages down with it.
    ui::webview::chromium::serve_shared_until_idle();
}
