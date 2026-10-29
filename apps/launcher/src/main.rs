//! The launcher on its own: a window searching a fixed desktop, and the
//! standard `--shots`, `--headless-drive` and `--native-audit` modes.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(launcher_app::LauncherShell);
