//! Bounded, descriptor-relative custody of opaque prepared installer payloads.
//! Authentication and completed-generation authority stay with the native issuer.
#[cfg(target_os = "linux")]
mod linux;

fn main() -> std::process::ExitCode {
    #[cfg(target_os = "linux")]
    if linux::run().is_ok() {
        return std::process::ExitCode::SUCCESS;
    }
    // Neither parser errors, NSS records, helper stderr nor hostile JSON are
    // echoed: authenticated input can contain private setup material.
    eprintln!(
        "Prepared setup custody could not continue. Verify the selected medium and installer grants."
    );
    std::process::ExitCode::FAILURE
}
