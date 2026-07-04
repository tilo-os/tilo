//! Hardware-gated integration test for the DRM backend.
//!
//! Phase 5 Task 5.1: this test asserts that, on a free TTY where the
//! caller owns the seat, [`DrmBackend::new`] succeeds. It's gated by
//! the `NIRIOS_DRM_HARDWARE_TEST` env var so the default `cargo test`
//! run on a developer's desktop (where Gnome / KDE already holds the
//! seat) skips it cleanly without spurious failures.
//!
//! How to run on real hardware:
//!
//! ```bash
//! # From a TTY (Ctrl+Alt+F2, log in to a shell):
//! NIRIOS_DRM_HARDWARE_TEST=1 cargo test --test drm_backend_boot
//! ```
//!
//! The test does NOT exercise rendering — only that the backend can
//! claim the seat, enumerate at least one KMS card node, and attach
//! libinput. Pixel-on-screen validation lives in `bin/shots` (offscreen)
//! and the manual VM-boot smoke (Phase 5.2 follow-up: vello + DMA-BUF
//! blit).

use shell::compositor::backend::drm::DrmBackend;
use shell::compositor::backend::Backend;

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
            // Phase 5.2: connector enumeration must produce at least
            // one output on real hardware. If a TTY system reports
            // zero connected outputs, that's a regression in the
            // enumeration path (or a truly headless server, in which
            // case the operator can disable this assert with
            // `NIRIOS_DRM_NO_OUTPUTS_OK=1`).
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
