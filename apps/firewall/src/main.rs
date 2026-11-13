//! Firewall's binary: the launcher serves the live window and the captures.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(firewall::FirewallShell);
