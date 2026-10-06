//! Firewall — thin binary entry point. All logic lives in the library.
//!
//! The standard launcher serves every mode: the live window, `--shots`
//! captures (with `--scenario NAME`, `--viewport WxH`, `--theme light`) and
//! the headless drives. A capture shows the bundled example ruleset; only the
//! live window reads the host firewall.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

ui::main!(firewall::FirewallShell);
