//! VPN — the app's entry point: the live window and the standard `--shots`,
//! `--headless-drive` and `--native-audit` modes.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(vpn::VpnShell);
