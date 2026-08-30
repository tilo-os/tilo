//! Photos — view a photograph, adjust it, and get the result out.
//!
//! A viewer with a small, honest set of edits:
//!
//!   * **Open** an image through the host XDG portal chooser (never a bespoke
//!     dialog), or from a path on the command line.
//!   * **Display** it fit to the window — contain-scaled, so a photograph
//!     larger than the viewport is scaled down rather than cropped by the
//!     frame. Scroll to zoom, drag to pan.
//!   * **Adjust** it: quarter turns either way, and a crop of whatever the
//!     viewport is currently framing. Both change the pixels.
//!   * **Get it out**: copy the edited pixels to the system clipboard, or save
//!     them as a PNG beside the original.
//!
//! # Where state lives
//!
//! The CRDT holds the file's path and the edits in force ([`doc`]); it never
//! holds the pixels. The decode and the render live in the app instance. That
//! division is the same one `crates/doc/qdoc/src/blobstore.rs` enforces for a
//! document's images, and for the same reason — an automerge change big enough
//! to carry a photograph is dropped by the sync transport and freezes every
//! peer's replica permanently.
//!
//! # The shape of the surface
//!
//! [`actions`] publishes the open photograph as a `Resource` whose edits are
//! `ActionSpec`s, addressed by stable id. A caller with no display can list
//! what is open and act on it without a coordinate ever entering the picture.
//!
//! `app_root()` is the single integration point for both the live event loop
//! and the `--shots` offscreen renderer. The live app starts **empty** — there
//! is no sample photograph anywhere in this crate; the `--shots` scenarios
//! below render a generated test pattern that exists only on that path.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
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
        reason = "a test that unwraps is stating an invariant"
    )
)]

pub mod actions;
pub mod app;
pub mod doc;
pub mod model;
pub mod pixels;
pub mod reference;
pub mod strings;

pub use app::PhotosApp;
pub use model::{Edits, OpenImage, Rect01};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    strings::install();
    Box::new(PhotosApp::new())
}

/// The ordered `--shots` scenarios, named so the offscreen renderer can walk
/// them and the gallery reads top-to-bottom.
pub const SHOT_SCENARIOS: &[&str] = &["empty", "opened", "cropping", "edited"];

/// A generated test pattern, used **only** by `--shots`.
///
/// Deliberately not a photograph and deliberately not shipped as a file: it is
/// drawn here so a screenshot is deterministic offline and so no sample image
/// can leak into the live app. Wide (3:2) and asymmetric, so a shot proves
/// letterboxing, orientation and crop rather than looking plausible either way
/// round.
pub fn shots_pattern(w: u32, h: u32) -> pixels::Rgba {
    let mut bytes = Vec::with_capacity((w as usize) * (h as usize) * 4);
    for y in 0..h {
        for x in 0..w {
            let fx = x as f32 / w as f32;
            let fy = y as f32 / h as f32;
            // A sky-to-water gradient with a horizon and a sun, so the image
            // has an unambiguous up, left and right.
            let horizon = 0.55;
            let (r, g, b) = if fy < horizon {
                let t = fy / horizon;
                (
                    (40.0 + 150.0 * t) as u8,
                    (90.0 + 110.0 * t) as u8,
                    (160.0 + 70.0 * t) as u8,
                )
            } else {
                let t = (fy - horizon) / (1.0 - horizon);
                (
                    (20.0 + 40.0 * t) as u8,
                    (60.0 + 60.0 * t) as u8,
                    (110.0 - 40.0 * t) as u8,
                )
            };
            // The sun sits left of centre and above the horizon.
            let dx = fx - 0.28;
            let dy = fy - 0.3;
            let sun = (dx * dx + (dy * dy) * 2.25).sqrt();
            let (r, g, b) = if sun < 0.09 {
                (255, 226, 160)
            } else {
                (r, g, b)
            };
            bytes.extend_from_slice(&[r, g, b, 255]);
        }
    }
    pixels::Rgba::new(w, h, bytes).expect("generated pattern is well formed")
}

/// Pose the app for one named `--shots` scenario.
///
/// The generated pattern is written to `dir` and opened through the ordinary
/// path, so the screenshot exercises the same decode the live app runs rather
/// than a special case that could hide a bug in it.
pub fn seed_scenario(
    app: &mut PhotosApp,
    doc: &mut automerge::AutoCommit,
    scenario: &str,
    dir: &std::path::Path,
) {
    use crate::doc::PhotosDoc;

    PhotosDoc::seed(doc);
    if scenario == "empty" {
        return;
    }

    let path = dir.join("harbour.png");
    let pattern = shots_pattern(1200, 800);
    if let Some(png) = pixels::encode_png(&pattern) {
        std::fs::create_dir_all(dir).ok();
        if std::fs::write(&path, png).is_err() {
            return;
        }
    }
    app.open_path(doc, &path.to_string_lossy());

    match scenario {
        // Turned a quarter. The zoom that frames the crop is applied by the
        // caller between layout passes — the viewer does not exist until the
        // first `sync`, so there is nothing here yet to zoom.
        "cropping" => {
            PhotosDoc::set_edits(doc, &PhotosDoc::edits(doc).rotated_right());
        }
        // A crop and a turn together, which is the pair worth showing: the
        // crop is stored against the *original*, so it keeps naming the same
        // region of the photograph however the view is later turned.
        "edited" => {
            let e = PhotosDoc::edits(doc)
                .cropped_from_view(Rect01 {
                    x: 0.06,
                    y: 0.06,
                    w: 0.46,
                    h: 0.5,
                })
                .rotated_right();
            PhotosDoc::set_edits(doc, &e);
        }
        // "opened" and any unknown name: the photograph as it arrived.
        _ => {}
    }
}

/// Zoom the `cropping` scenario applies once the viewer has been laid out.
///
/// Framing a crop is what the viewport is *for*, so the scenario named for it
/// has to actually be zoomed in — otherwise it shows the whole photograph and
/// is indistinguishable from a plain rotate.
pub const CROPPING_SCENARIO_ZOOM: f32 = 2.2;
