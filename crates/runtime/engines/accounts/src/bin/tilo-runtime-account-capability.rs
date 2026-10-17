//! Certify the protected-grant protocol of the actual loaded native SDK.
//!
//! Build with the runtime's thin-app graph and include in its signed inventory.
//! Account custody lives in the accounts engine library, which this binary
//! links as Mail and Settings do, so its non-inlined function call resolves
//! through the very `libtilo_accounts_engine.so` the runtime ships; the
//! service-session capability resolves through `libruntime_dylib.so`. The
//! publisher separately probes the installed credential helper and hashes
//! both artifacts.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1
        && args
            .first()
            .is_some_and(|arg| arg == "--service-session-capability")
    {
        return match runtime_dylib::ui::webview::chromium::service_session_capability() {
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
    let protocol = tilo_accounts_engine::account_custody::protected_grant_protocol();
    println!("{{\"schema\":1,\"protected_grant_protocol\":{protocol}}}");
    ExitCode::SUCCESS
}
