//! The layout check Tilo's own checks run over the apps in `apps/tl`
//! (`tools/ci/layout-check.sh`), with the sample data of the outside services the Mail and Tube
//! apps stand on registered first, so their scenarios lay out over a sample inbox and feed. It
//! reads the apps from this repository and stays here; the published `tilolang` has no
//! `layout-check`.
//!
//!   tilo-layout-check [DIR]... [OPTION]... (`--help` lists them)

use std::process::ExitCode;

fn main() -> ExitCode {
    if let Err(error) = tilolang_cli::load_checkout_stdlib() {
        eprintln!("tilo-layout-check: {error}");
        return ExitCode::FAILURE;
    }
    tilolang_mail::sample::register();
    tilolang_youtube::sample::register();
    let args: Vec<String> = std::env::args().skip(1).collect();
    tilolang_cli::layout_check::command::run(&args)
}
