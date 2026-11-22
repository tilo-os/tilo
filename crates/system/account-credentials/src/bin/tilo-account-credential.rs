//! `tilo-account-credential capabilities`
//!
//! Says which protected-grant protocol this build's credential store speaks, for whoever hands
//! it a copied grant. Nothing secret is written to stdout or stderr.

use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!("usage: tilo-account-credential capabilities");
    ExitCode::from(64)
}

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    match args.next().as_deref().and_then(std::ffi::OsStr::to_str) {
        Some("capabilities") if args.next().is_none() => {
            println!(
                "{{\"schema\":1,\"protected_grant_protocol\":{}}}",
                account_credentials::protected_grant_protocol()
            );
            ExitCode::SUCCESS
        }
        _ => usage(),
    }
}
