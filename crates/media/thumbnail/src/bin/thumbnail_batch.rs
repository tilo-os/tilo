//! `thumbnail-batch`: the compute entrypoint of the thumbnails package. The
//! compute worker launches it under the package sandbox with a staged
//! request and an output directory; see [`thumbnail::batch`].

use std::process::ExitCode;

fn main() -> ExitCode {
    match thumbnail::batch::run_from_args(std::env::args().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // The worker returns the tail of stderr to the requester.
            eprintln!("thumbnail-batch: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}
