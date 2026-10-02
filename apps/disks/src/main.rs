//! Disks — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!     (headless screenshot; identical passes to the live app).

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use disks::DisksApp;
use disks::resource::DisksAction;
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec, ShotFrame};
use ui::theme::Theme;
use ui::tilo_actions::Action;
use ui::widget::Widget;

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);
/// A phone-width capture used to exercise the responsive overlay flap. Below
/// the app's 900px breakpoint the drive list collapses to an overlay.
const NARROW_SHOTS_VIEWPORT: (u32, u32) = (640, 800);

#[allow(
    clippy::disallowed_methods,
    reason = "the --shots handler runs on the main thread before the offscreen harness \
              or any app thread exists; the app reads DISKS_LIVE on this thread while \
              it is constructed"
)]
fn shot_handler(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args
        .shots
        .as_deref()
        .ok_or_else(|| LaunchError::Operation("missing screenshot output".into()))?;
    // `--live` probes the real system (lsblk + smartctl) for the capture,
    // so the shot shows this machine's actual drives — degrading to the
    // honest empty state if nothing is found. Without it the shot uses the
    // curated offline sample (fully-populated, deterministic).
    if args.has("--live") {
        // SAFETY: set once at startup before any threads read the env.
        unsafe {
            std::env::set_var("DISKS_LIVE", "1");
        }
    }
    // `--narrow` (or a sub-breakpoint `--viewport WxH`) renders the
    // collapsed layout with the drive-list flap revealed over its scrim.
    let narrow = args.has("--narrow") || args.viewport.is_some_and(|(w, _)| f64::from(w) < 900.0);
    let viewport = args.viewport.unwrap_or(if narrow {
        NARROW_SHOTS_VIEWPORT
    } else {
        DEFAULT_SHOTS_VIEWPORT
    });
    // Optional preset so a capture can pin a specific drive / partition /
    // tab (used to verify the selection states offscreen).
    let preset = Preset {
        drive: args.value("--drive").and_then(|value| value.parse().ok()),
        part: args.value("--part").and_then(|value| value.parse().ok()),
        tab: args.value("--tab").map(str::to_owned),
        pending: args.value("--pending").map(str::to_owned),
        status: args.value("--status").map(str::to_owned),
    };
    render_shot_to_png(
        DisksApp::new(),
        viewport,
        ui::launch::shot_theme(args),
        narrow,
        &preset,
        out,
    )
    .map_err(|error| LaunchError::Operation(error.to_string()))
}

#[allow(
    clippy::disallowed_methods,
    reason = "the launch spec calls the root factory on the main thread before the \
              window, event loop or any app worker thread exists; the app reads \
              DISKS_LIVE on this thread while it is constructed"
)]
fn live_root() -> Box<dyn Widget> {
    // Live mode: probe the real system (lsblk + smartctl). The `--shots`
    // path above never sets this, so the screenshot stays on the curated
    // sample topology and is fully populated offline.
    // SAFETY: set once at startup before any threads read the environment.
    unsafe {
        std::env::set_var("DISKS_LIVE", "1");
    }
    disks::app_root()
}

#[cfg(feature = "thin")]
extern crate runtime_dylib;

fn wants_standalone(args: impl Iterator<Item = String>) -> bool {
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let _ = args.next();
            }
            "--shots" | "--responsive-matrix" | "--native-audit" | "--headless" => return true,
            flag if flag.starts_with('-') => {}
            _ => {}
        }
    }
    false
}

#[allow(
    clippy::disallowed_methods,
    reason = "set DISKS_LIVE once on the main thread before the framework host starts threads"
)]
pub fn main() {
    if !wants_standalone(std::env::args().skip(1)) {
        // Live mode: probe the real system (lsblk + smartctl), same as
        // [`live_root`]. Set once before the framework host starts threads.
        // SAFETY: set once at startup before any threads read the environment.
        unsafe {
            std::env::set_var("DISKS_LIVE", "1");
        }
        ui::framework::run::<disks::DisksShell>();
        return;
    }
    ui::launch::run_or_exit(
        &LaunchSpec::new("so.tilo.disks", live_root)
            .shot_handler(shot_handler)
            .allowed_flags(&["--live", "--narrow"])
            .allowed_options(&["--drive", "--part", "--tab", "--pending", "--status"]),
    );
}

/// Optional pinned state for a `--shots` capture.
#[derive(Default)]
struct Preset {
    drive: Option<usize>,
    part: Option<usize>,
    tab: Option<String>,
    /// Pre-arm a confirm modal, e.g. `format:0:2`, to capture the
    /// type-to-confirm gate offscreen.
    pending: Option<String>,
    /// Pin a success or failure toast for deterministic visual regression
    /// captures without invoking a real storage operation.
    status: Option<String>,
}

/// Run one offscreen frame against the app and write RGBA to `out` as PNG.
/// `narrow` snaps the drive-list flap open *after* layout so the collapsed
/// overlay is captured settled rather than mid-animation.
fn render_shot_to_png(
    mut widget: DisksApp,
    viewport: (u32, u32),
    theme: Theme,
    narrow: bool,
    preset: &Preset,
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut frame = ShotFrame::new(viewport, theme);
    frame.sync(&mut widget);
    // Apply any pinned selection, then re-sync so the widget adopts it.
    if preset.drive.is_some()
        || preset.part.is_some()
        || preset.tab.is_some()
        || preset.pending.is_some()
        || preset.status.is_some()
    {
        let doc = &mut frame.state.doc;
        if let Some(d) = preset.drive {
            let _ = DisksAction::SetSelected { index: d as u64 }.apply(doc);
        }
        if let Some(p) = preset.part {
            let _ = DisksAction::SetSelectedPart { part: p as i64 }.apply(doc);
        }
        if let Some(t) = &preset.tab {
            let _ = DisksAction::SetTab { tab: t.clone() }.apply(doc);
        }
        if let Some(p) = &preset.pending {
            let _ = DisksAction::SetPending { pending: p.clone() }.apply(doc);
        }
        if let Some(status) = &preset.status {
            let _ = DisksAction::SetStatus {
                status: status.clone(),
            }
            .apply(doc);
        }
        frame.sync(&mut widget);
    }
    frame.layout(&mut widget)?;
    // After layout the width (hence the docked/overlay split) is settled, so
    // snapping the flap here lands a fully-revealed overlay for the capture.
    if narrow {
        widget.snap_sidebar(true);
    }
    let scene = frame.draw(&mut widget);
    frame.write_png(&scene, out);
    println!("disks: wrote shot to {}", out.display());
    Ok(())
}
