//! `tilo-runtime-update`: keep the macOS `Tilo.app` current.
//!
//! ```text
//! tilo-runtime-update                  install a newer signed release
//! tilo-runtime-update --check          report whether one is available
//! tilo-runtime-update sign --key FILE FEED   print FEED's detached signature
//! ```
//!
//! The launchd agent `so.tilo.runtime.update` runs the first form daily.

use std::process::ExitCode;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let result = match arguments.as_slice() {
        ["sign", "--key", key, feed] => sign(key, feed),
        [] => update(false),
        ["--check"] => update(true),
        _ => Err("usage: tilo-runtime-update [--check] | sign --key FILE FEED".to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tilo-runtime-update: {error}");
            ExitCode::FAILURE
        }
    }
}

fn sign(key: &str, feed: &str) -> Result<(), String> {
    let read = |path: &str| std::fs::read(path).map_err(|error| format!("{path}: {error}"));
    let key = runtime_update::release::parse_signing_key(&String::from_utf8_lossy(&read(key)?))
        .map_err(|error| error.to_string())?;
    println!("{}", runtime_update::release::sign(&read(feed)?, &key));
    Ok(())
}

#[cfg(target_os = "macos")]
fn update(check_only: bool) -> Result<(), String> {
    use runtime_update::macos::{Bundle, Outcome, update};
    let bundle = Bundle::running().map_err(|error| error.to_string())?;
    match update(&bundle, check_only).map_err(|error| error.to_string())? {
        Outcome::Disabled => println!("this build updates through its installer"),
        Outcome::UpToDate { version } => println!("Tilo {version} is current"),
        Outcome::Available(release) => println!("Tilo {} is available", release.version),
        Outcome::Installed(release) => println!("installed Tilo {}", release.version),
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn update(_check_only: bool) -> Result<(), String> {
    Err("only the macOS bundle updates itself; update Linux packages with the distribution's package manager".to_owned())
}
