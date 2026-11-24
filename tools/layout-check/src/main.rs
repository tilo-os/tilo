//! `tilolang layout-check` as Tilo's own checks run it over the apps in `apps/tl`
//! (`tools/ci/layout-check.sh`): the same command, with the sample data of the outside services
//! the Mail and Tube apps stand on registered first, so their scenarios lay out over a sample
//! inbox and feed. The published `tilolang` links neither service's engine, so it lays those apps
//! out without them; this tool stays in the repository.
//!
//!   tilo-layout-check [DIR]... [the options `tilolang layout-check` takes]

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
