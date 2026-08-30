//! Hardware-gated integration test for the DRM backend: on a free TTY
//! where the caller owns the seat, [`DrmBackend::new`] must succeed. Gated
//! by `NIRIOS_DRM_HARDWARE_TEST` so the default run on a desktop (where
//! Gnome/KDE already holds the seat) skips cleanly.
//!
//! ```bash
//! # From a TTY (Ctrl+Alt+F2, log in to a shell):
//! NIRIOS_DRM_HARDWARE_TEST=1 cargo test --test drm_backend_boot
//! ```
//!
//! No rendering — only that the backend claims the seat, enumerates at
//! least one KMS card node, and attaches libinput.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use shell::compositor::backend::Backend;
use shell::compositor::backend::drm::DrmBackend;

#[test]
fn drm_backend_boot_smoke() {
    if std::env::var("NIRIOS_DRM_HARDWARE_TEST").is_err() {
        eprintln!(
            "Skipping drm_backend_boot_smoke (set NIRIOS_DRM_HARDWARE_TEST=1 \
             from a real TTY to exercise it)."
        );
        return;
    }
    let backend = DrmBackend::new();
    match backend {
        Ok(b) => {
            // Connector enumeration must produce at least one output on real
            // hardware — zero on a TTY system is a regression (disable via
            // NIRIOS_DRM_NO_OUTPUTS_OK=1 for a headless server).
            let outputs = b.outputs();
            if std::env::var("NIRIOS_DRM_NO_OUTPUTS_OK").is_err() {
                assert!(
                    !outputs.is_empty(),
                    "DrmBackend::outputs() returned an empty list on a hardware run. \
                     Phase 5.2's connector enumeration should produce at least one output \
                     when a monitor is attached. Set NIRIOS_DRM_NO_OUTPUTS_OK=1 to bypass \
                     on a truly headless server."
                );
            }
        }
        Err(e) => {
            panic!(
                "DrmBackend::new failed on a hardware run: {e}. \
                 Are you running this from a free TTY where you own \
                 the seat? libseat needs an unclaimed VT — switch with \
                 Ctrl+Alt+F2 and log in to a fresh shell."
            );
        }
    }
}
