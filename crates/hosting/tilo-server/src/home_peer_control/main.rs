//! `tilo-home-peer-control`: the polkit helper a Linux home server's Settings
//! runs through `pkexec` to change its relay and home-peer configuration.
//!
//! It restarts systemd units, takes the invoking person's credentials with
//! Linux's `getgrouplist`/`setgroups` and reaches their session under
//! `/run/user`, so it exists only on Linux; elsewhere the binary says so and
//! fails, which keeps `cargo check --tests` of this package working on a Mac.
#[cfg(target_os = "linux")]
mod broker;

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<std::process::ExitCode> {
    broker::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("tilo-home-peer-control runs only on a Linux home server");
    std::process::ExitCode::FAILURE
}
