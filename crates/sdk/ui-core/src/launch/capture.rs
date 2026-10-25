//! Captures: what writes frames, what those frames must not lie about, and the
//! numbers a run prints about them.
//!
//! A capture is evidence, so it is refused rather than produced when it would
//! not be about this product. Frames set in the host's font faces look almost
//! right and are wrong in a way only a review of the frame notices, and a
//! measurement that reported nothing must not exit as though it had.

use super::*;

/// Modes that write frames or print measurements about this build.
pub(super) fn captures_frames(args: &LaunchArgs) -> bool {
    args.shots.is_some()
        || args.shots_dir.is_some()
        || args.responsive_matrix.is_some()
        || args.native_audit
        || args.first_frame
}

/// A capture set in the host's faces is not evidence about this product: every
/// overflow, truncation and weight judgement made from it is about a font Tilo
/// does not ship. So it does not happen, and the message says what to point
/// [`tilo_font_catalog::OVERRIDE_ENV`] at.
#[cfg(not(test))]
pub(super) fn capture_font_gate(args: &LaunchArgs) -> Result<(), LaunchError> {
    if !captures_frames(args) {
        return Ok(());
    }
    if let Some(problem) = crate::text_render::font_catalog_problem() {
        return Err(LaunchError::SubstituteFont(problem));
    }
    // Say which catalog set the text, once, so a review of these frames knows
    // what it is looking at instead of assuming.
    if let Some(root) = crate::font_catalog::catalog_root() {
        eprintln!("font catalog: {}", root.display());
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn capture_font_gate(_args: &LaunchArgs) -> Result<(), LaunchError> {
    Ok(())
}

/// Create the directory a capture mode (`option`) writes its PNGs into.
#[cfg(feature = "native")]
pub(super) fn capture_directory(
    directory: &std::path::Path,
    option: &str,
) -> Result<(), LaunchError> {
    std::fs::create_dir_all(directory).map_err(|error| {
        LaunchError::Operation(format!(
            "could not create the {option} directory {}: {error}",
            directory.display()
        ))
    })
}

/// `--first-frame`: the app constructed as a live launch constructs it, then
/// its first frame synced, composed, laid out and drawn as its window would
/// draw it ([`App::surface_frame`]), with no window, GPU or runtime. Prints
/// the process's startup line (with `TILO_STARTUP_METRICS_PATH` set) and one
/// `FIRST-FRAME-SCOPE` line per named scope the construction and the frame
/// ran, so a cold launch can be measured one process at a time.
#[allow(
    clippy::print_stdout,
    reason = "the measurement is this mode's output, as --native-audit prints its audit"
)]
#[cfg(feature = "native")]
pub(super) fn first_frame(spec: &LaunchSpec, args: &LaunchArgs) -> Result<(), LaunchError> {
    struct Offscreen((u32, u32));
    impl crate::app::AppSurface for Offscreen {
        fn scale_factor(&self) -> f64 {
            1.0
        }
        fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
            winit::dpi::PhysicalSize::new(self.0.0, self.0.1)
        }
        fn request_redraw(&self) {}
        fn set_cursor(&self, _: winit::window::CursorIcon) {}
    }
    let viewport = args.viewport.unwrap_or_else(|| (spec.shot_viewport)(args));
    let (constructed, construction) =
        tilo_resource::profiling::observe(|| dispatch_definition(spec, args));
    let DefinitionDispatch::Native(mut app) = constructed? else {
        return Err(LaunchError::Operation(
            "--first-frame constructs the live app".into(),
        ));
    };
    tilo_startup_metrics::mark("first-frame-start");
    let ((), frame) = tilo_resource::profiling::observe(|| {
        let mut scene = vello::Scene::new();
        app.surface_frame(&Offscreen(viewport), &mut scene);
    });
    tilo_startup_metrics::mark("first-frame-done");
    if let Some(line) = tilo_startup_metrics::summary_line(spec.app_id) {
        println!("{line}");
    }
    for (name, ns) in tilo_startup_metrics::events() {
        println!("FIRST-FRAME-MARK name={name} us={}", ns / 1000);
    }
    for (stage, scopes) in [("construction", construction), ("frame", frame)] {
        for scope in scopes {
            println!(
                "FIRST-FRAME-SCOPE stage={stage} phase={} us={} calls={}",
                scope.phase,
                scope.elapsed_ns / 1000,
                scope.calls
            );
        }
    }
    app.close_components();
    Ok(())
}
