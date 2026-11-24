//! Certify the protected-grant protocol of the actual loaded native SDK.
//!
//! Build with the runtime's thin-app graph and include in its signed inventory.
//! Account credentials lives in the accounts engine library, which this binary
//! links by the name of its entry point, so the call resolves through the very
//! `libtilo_accounts_engine.so` the runtime ships; the
//! service-session capability resolves through `libruntime_dylib.so`. The
//! publisher separately probes the installed credential helper and hashes
//! both artifacts.

use std::process::ExitCode;

// The library exports only its unmangled entry points (no crate compiles
// against it), so this binary links it by name, not as a crate.
#[link(name = "tilo_accounts_engine", kind = "dylib")]
unsafe extern "C-unwind" {
    safe fn tilo_accounts_engine_protected_grant_protocol() -> u32;
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1
        && args
            .first()
            .is_some_and(|arg| arg == "--service-session-capability")
    {
        return match runtime_dylib::tilo_webview::chromium::service_session_capability() {
            Ok(value) => {
                println!("{value}");
                ExitCode::SUCCESS
            }
            Err(_) => ExitCode::from(1),
        };
    }
    if !args.is_empty() {
        return ExitCode::from(64);
    }
    let protocol = tilo_accounts_engine_protected_grant_protocol();
    println!("{{\"schema\":1,\"protected_grant_protocol\":{protocol}}}");
    ExitCode::SUCCESS
}
