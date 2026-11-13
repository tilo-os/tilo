//! `thumbnail-batch`: the compute entrypoint of the thumbnails package.

use std::process::ExitCode;

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn main() -> ExitCode {
    match thumbnail::batch::run_from_args(std::env::args().skip(1)) {
        Ok(failed) => {
            for failure in failed {
                eprintln!("thumbnail-batch: {failure}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("thumbnail-batch: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}
