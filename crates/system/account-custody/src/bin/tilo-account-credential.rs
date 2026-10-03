//! `tilo-account-credential refresh <account id> [--force] [--config-root <dir>]`
//!
//! Run by the storage daemon when an account's short-lived credential is
//! missing, due or was rejected. Publishes a fresh one in the handoff store
//! and exits; the exit code says what happened (see
//! `accounts::handoff::exit`). Nothing secret is written to stdout or stderr.

use std::path::PathBuf;
use std::process::ExitCode;

use account_custody::{Custodian, CustodyError};
use accounts::handoff::exit;

fn usage() -> ExitCode {
    eprintln!(
        "usage: tilo-account-credential refresh <account id> [--force] [--config-root <dir>]"
    );
    ExitCode::from(64)
}

fn code(value: i32) -> ExitCode {
    ExitCode::from(u8::try_from(value).unwrap_or(1))
}

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("refresh")) {
        return usage();
    }
    let Some(id) = args.next().and_then(|id| id.into_string().ok()) else {
        return usage();
    };
    let mut force = false;
    let mut config_root = None;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--force") => force = true,
            Some("--config-root") => match args.next() {
                Some(root) => config_root = Some(PathBuf::from(root)),
                None => return usage(),
            },
            _ => return usage(),
        }
    }
    let custodian = Custodian::for_root(config_root).with_apple_icloud();
    match custodian.publish(&accounts::AccountId::from(id), force) {
        Ok(()) => code(exit::PUBLISHED),
        Err(CustodyError::NeedsAuthentication) => code(exit::NEEDS_AUTHENTICATION),
        Err(CustodyError::Offline) => code(exit::OFFLINE),
        Err(error) => {
            eprintln!("tilo-account-credential: {error}");
            code(exit::FAILED)
        }
    }
}
