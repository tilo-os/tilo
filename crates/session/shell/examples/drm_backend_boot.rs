//! Hardware probe for the DRM backend: on a free TTY where the caller owns the
//! seat, [`DrmBackend::new`] must claim it, enumerate at least one KMS card
//! node with a connected output, and attach libinput. No rendering.
//!
//! It is a probe rather than a test because it can only observe its subject on
//! a real seat; a test would have to skip everywhere else and report a pass it
//! did not earn. Run it where it can see the hardware and read the exit
//! status: non-zero means the backend failed or could not be observed.
//!
//! ```bash
//! # From a TTY (Ctrl+Alt+F2, log in to a shell):
//! cargo run -p shell --example drm_backend_boot
//! # A headless server with no monitor attached:
//! cargo run -p shell --example drm_backend_boot -- --no-outputs-ok
//! ```

use std::process::ExitCode;

#[cfg(target_os = "linux")]
fn main() -> ExitCode {
    use shell::compositor::backend::Backend;
    use shell::compositor::backend::drm::DrmBackend;

    let no_outputs_ok = std::env::args().any(|arg| arg == "--no-outputs-ok");
    let backend = match DrmBackend::new() {
        Ok(backend) => backend,
        Err(error) => {
            eprintln!(
                "drm_backend_boot: FAIL: DrmBackend::new: {error}. libseat needs an unclaimed \
                 VT: switch with Ctrl+Alt+F2 and run this from a fresh login shell."
            );
            return ExitCode::FAILURE;
        }
    };
    let outputs = backend.outputs();
    if outputs.is_empty() && !no_outputs_ok {
        eprintln!(
            "drm_backend_boot: FAIL: the backend published no outputs. Connector enumeration \
             must find the attached monitor; pass --no-outputs-ok on a headless server."
        );
        return ExitCode::FAILURE;
    }
    for output in outputs {
        println!(
            "drm_backend_boot: output {} {}x{} at {:?} scale {}",
            output.id.0, output.size.0, output.size.1, output.position, output.scale
        );
    }
    println!("drm_backend_boot: PASS ({} outputs)", outputs.len());
    ExitCode::SUCCESS
}

#[cfg(not(target_os = "linux"))]
fn main() -> ExitCode {
    eprintln!("drm_backend_boot: FAIL: the DRM backend exists only on Linux");
    ExitCode::FAILURE
}
