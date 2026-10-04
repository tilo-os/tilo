//! Thin binary entry point: the [`browser::BrowserShell`] framework app.
//! Every launch mode — a window, a private window (`--private`), a page
//! named on the command line, `--shots` scenarios and the live-engine
//! captures (`--shots-live`) — is the framework's.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() {
    ui::framework::run::<browser::BrowserShell>();
}
