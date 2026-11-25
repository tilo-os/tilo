//! `tilolang-std DIR --out FILE`: compile the standard library whose package directory is `DIR`
//! (a checkout's `crates/lang/tilolang/std`) and write its module to `FILE`, then print its
//! digest ([`tilolang::stdlib::digest`]).
//!
//! What a tilolang runtime bundle carries as `lib/tilo.std.wasm`: the library every package
//! built against that digest runs beside, which no package carries itself. The module is wasm, the
//! same for every device, so a cross build runs this tool on the machine it builds on.

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: tilolang-std DIR --out FILE";

fn main() -> ExitCode {
    match run() {
        Ok(digest) => {
            println!("{digest}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("tilolang-std: {message}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<&'static str, String> {
    let mut dir = None;
    let mut out = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--out" {
            out = Some(PathBuf::from(args.next().ok_or(USAGE)?));
        } else if arg == "-h" || arg == "--help" {
            return Err(USAGE.to_owned());
        } else if dir.is_none() {
            dir = Some(PathBuf::from(arg));
        } else {
            return Err(USAGE.to_owned());
        }
    }
    let (Some(dir), Some(out)) = (dir, out) else {
        return Err(USAGE.to_owned());
    };
    tilolang::stdlib::load_from(&dir)?;
    std::fs::write(&out, &tilolang::stdlib::compiled().wasm)
        .map_err(|error| format!("{}: {error}", out.display()))?;
    Ok(tilolang::stdlib::digest())
}
