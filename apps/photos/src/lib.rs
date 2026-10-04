//! Photos — view a photograph, adjust it, and get the result out.
//!
//! A viewer with a small set of edits:
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
//! A photograph is its original plus what was done to it. Importing puts the
//! original's bytes in the app's content store (`tilo-content`, a handle the
//! app is given at startup) and records an `ImportPhoto` action naming them by
//! hash; every edit after that is a `SetEdits` action replayed onto that
//! original. The CRDT holds the hash, small metadata and the edits in force
//! ([`doc`]); it never holds the pixels, and a rendered edit (Copy, Save, the
//! export operation) is an output that never replaces the original. The decode
//! and the render live in the app instance, because an automerge change big
//! enough to carry a photograph is dropped by the sync transport and freezes
//! every peer's replica permanently.
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
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod analysis;
pub mod app;
pub mod doc;
mod files;
pub mod framework_history;
/// Bounded, cancellation-aware real-file discovery and metadata ingestion.
pub mod import;
pub mod library_analysis;
pub mod model;
pub mod people;
pub mod pixels;
/// Offline-capable place grouping and map projection shared by the timeline
/// surface and resource/action routing. This module deliberately has no tile
/// client, so opening a Photos location never leaks it to a network provider.
pub mod places;
mod presentation;
pub mod reference;
pub mod resource;
pub mod search;
pub mod strings;
mod timeline_projection;

pub use app::PhotosApp;
pub use model::{Edits, LocationProvenance, OpenImage, PhotoLocation, Rect01};

/// Import a Camera portable-media reference into Photos without ever trusting
/// a machine-local path.  The content is read, verified against its hash, from
/// `store` first; missing bytes are an actionable refusal rather than a
/// phantom library entry.
///
/// # Errors
///
/// Returns an error when the Camera identity is malformed, the verified bytes
/// have not arrived on this device, exceed the import budget, fail integrity
/// validation, or are not a safely decodable still image.
pub fn camera_import_photo(
    media_id: &str,
    name: &str,
    store: &tilo_content::ContentStore,
) -> Result<model::LibraryPhoto, String> {
    let hash = media_id
        .strip_prefix("media:blake3:")
        .filter(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| crate::strings::tr("photos-camera-error-identity"))?;
    // The store verifies what it reads against the hash; content that fails
    // that check reads as absent.
    let bytes = store
        .read(hash)
        .ok_or_else(|| crate::strings::tr("photos-camera-error-not-here"))?;
    if bytes.is_empty() || bytes.len() > 64 * 1024 * 1024 {
        return Err(crate::strings::tr("photos-camera-error-integrity"));
    }
    let image = pixels::decode(&bytes)
        .ok_or_else(|| crate::strings::tr("photos-camera-error-unsupported"))?;
    let open = OpenImage {
        path: name.to_owned(),
        name: name.to_owned(),
        width: image.w,
        height: image.h,
        content: Some(hash.to_owned()),
    }
    .portable();
    Ok(model::LibraryPhoto::from_image(open))
}

/// Validate and import a Camera portable-media reference. This convenience
/// boundary is useful to callers that already own a mutable Photos document;
/// brokered callers use the same validation then dispatch `ImportPhoto` so
/// history and collaboration observe the canonical action.
///
/// # Errors
///
/// Returns the same validation errors as [`camera_import_photo`], or an error
/// if the validated import cannot be selected in the shared library.
pub fn import_camera_media(
    doc: &mut automerge::AutoCommit,
    media_id: &str,
    name: &str,
    store: &tilo_content::ContentStore,
) -> Result<String, String> {
    let photo = camera_import_photo(media_id, name, store)?;
    let id = photo.id.clone();
    let mut library = doc::PhotosDoc::library(doc);
    library.add_photo(photo);
    doc::PhotosDoc::set_library(doc, &library);
    let selected = library
        .photo(&id)
        .ok_or_else(|| crate::strings::tr("photos-camera-error-failed"))?;
    doc::PhotosDoc::set_image(doc, Some(&selected.image));
    Ok(id)
}

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    strings::install();
    Box::new(PhotosApp::new_persistent(
        tilo_content::ContentStore::for_app(),
    ))
}

/// The app as every host mounts it: the live root.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.photos", app_root)
        .component_actions(actions::register)
        .as_definition("Photos")
}

/// The ordered `--shots` scenarios, named so the offscreen renderer can walk
/// them and the gallery reads top-to-bottom.
pub const SHOT_SCENARIOS: &[&str] = &[
    "empty",
    "loading",
    "error",
    "opened",
    "library",
    "albums",
    "people",
    "people-detail",
    "places",
    "analysis",
    "import-progress",
    "import-errors",
    "selection",
    "selection-many-albums",
    "album-manage",
    "metadata-edit",
    "metadata-batch",
    "cropping",
    "edited",
];

/// A generated test pattern, used **only** by `--shots`.
///
/// Generated in memory for deterministic offscreen screenshots: a sea under
/// a sky with a sun left of centre, so the picture has an unambiguous up,
/// left and right for letterboxing, orientation and crop.
pub fn shots_pattern(w: u32, h: u32) -> pixels::Rgba {
    shots_pattern_variant(w, h, 0)
}

/// One generated landscape: its sky, its land or water, what stands on the
/// horizon and where the sun or moon is.
struct FixtureScene {
    sky_top: [f32; 3],
    sky_bottom: [f32; 3],
    ground_near: [f32; 3],
    ground_far: [f32; 3],
    /// Height of the horizon, as a fraction of the picture from the top.
    horizon: f32,
    /// What rises above the horizon.
    skyline: Skyline,
    /// Whether below the horizon is water, which mirrors the sun.
    water: bool,
    skyline_colour: [f32; 3],
    /// Centre of the sun as fractions of the width and height, its radius
    /// as a fraction of the shorter side, and its colour.
    sun: (f32, f32, f32),
    sun_colour: [f32; 3],
}

#[derive(Clone, Copy)]
enum Skyline {
    /// Open water or a plain: nothing on the horizon.
    Open,
    /// A ridge of mountains.
    Mountains,
    /// Soft hills.
    Hills,
    /// A row of buildings.
    City,
}

/// The thirteen fixture scenes, one per photograph in the shot library.
#[expect(
    clippy::too_many_lines,
    reason = "one literal entry per fixture scene reads better than a table of bare numbers"
)]
fn fixture_scene(variant: u8) -> FixtureScene {
    let c = |r: u8, g: u8, b: u8| [f32::from(r), f32::from(g), f32::from(b)];
    match variant % 13 {
        // Harbour at dusk: violet sky, dark water, a low pink sun.
        0 => FixtureScene {
            sky_top: c(46, 52, 112),
            sky_bottom: c(214, 128, 132),
            ground_near: c(20, 26, 52),
            ground_far: c(70, 64, 104),
            horizon: 0.62,
            skyline: Skyline::City,
            water: true,
            skyline_colour: c(32, 30, 58),
            sun: (0.7, 0.5, 0.07),
            sun_colour: c(255, 178, 150),
        },
        // A summer trail: clear blue sky over green hills.
        1 => FixtureScene {
            sky_top: c(54, 132, 214),
            sky_bottom: c(184, 222, 246),
            ground_near: c(70, 120, 46),
            ground_far: c(126, 170, 82),
            horizon: 0.58,
            skyline: Skyline::Hills,
            water: false,
            skyline_colour: c(96, 146, 70),
            sun: (0.28, 0.32, 0.08),
            sun_colour: c(255, 246, 204),
        },
        // Museum light: a pale, overcast plain with a white sun.
        2 => FixtureScene {
            sky_top: c(150, 160, 172),
            sky_bottom: c(222, 220, 212),
            ground_near: c(118, 104, 88),
            ground_far: c(176, 160, 138),
            horizon: 0.66,
            skyline: Skyline::City,
            water: false,
            skyline_colour: c(134, 132, 136),
            sun: (0.62, 0.34, 0.06),
            sun_colour: c(250, 248, 238),
        },
        // A rainy market: grey-green, the sun hidden behind cloud.
        3 => FixtureScene {
            sky_top: c(84, 96, 104),
            sky_bottom: c(150, 162, 164),
            ground_near: c(44, 52, 54),
            ground_far: c(92, 104, 102),
            horizon: 0.55,
            skyline: Skyline::City,
            water: false,
            skyline_colour: c(62, 72, 76),
            sun: (0.3, 0.32, 0.05),
            sun_colour: c(186, 194, 192),
        },
        // A lake in the afternoon: turquoise water under mountains.
        4 => FixtureScene {
            sky_top: c(40, 110, 196),
            sky_bottom: c(166, 210, 236),
            ground_near: c(14, 88, 110),
            ground_far: c(46, 150, 160),
            horizon: 0.52,
            skyline: Skyline::Mountains,
            water: true,
            skyline_colour: c(70, 92, 120),
            sun: (0.26, 0.3, 0.07),
            sun_colour: c(255, 238, 186),
        },
        // A window at night: deep blue with a cool moon.
        5 => FixtureScene {
            sky_top: c(10, 16, 46),
            sky_bottom: c(40, 58, 110),
            ground_near: c(12, 14, 24),
            ground_far: c(30, 36, 60),
            horizon: 0.7,
            skyline: Skyline::City,
            water: true,
            skyline_colour: c(18, 22, 40),
            sun: (0.7, 0.32, 0.06),
            sun_colour: c(226, 234, 250),
        },
        // A sunset ferry: orange sky over the sea, the sun on the water.
        6 => FixtureScene {
            sky_top: c(120, 70, 120),
            sky_bottom: c(250, 160, 84),
            ground_near: c(48, 40, 70),
            ground_far: c(178, 106, 86),
            horizon: 0.6,
            skyline: Skyline::Open,
            water: true,
            skyline_colour: c(0, 0, 0),
            sun: (0.5, 0.56, 0.09),
            sun_colour: c(255, 214, 120),
        },
        // A garden supper: warm green evening light.
        7 => FixtureScene {
            sky_top: c(92, 120, 150),
            sky_bottom: c(236, 206, 150),
            ground_near: c(48, 78, 40),
            ground_far: c(110, 132, 66),
            horizon: 0.5,
            skyline: Skyline::Hills,
            water: false,
            skyline_colour: c(66, 98, 54),
            sun: (0.82, 0.36, 0.06),
            sun_colour: c(255, 222, 160),
        },
        // Paper lanterns: a dark red festival night.
        8 => FixtureScene {
            sky_top: c(34, 14, 30),
            sky_bottom: c(110, 40, 52),
            ground_near: c(26, 12, 16),
            ground_far: c(80, 30, 34),
            horizon: 0.64,
            skyline: Skyline::City,
            water: true,
            skyline_colour: c(46, 16, 26),
            sun: (0.36, 0.3, 0.05),
            sun_colour: c(255, 170, 96),
        },
        // A mountain lookout: crisp morning over snow peaks.
        9 => FixtureScene {
            sky_top: c(70, 120, 186),
            sky_bottom: c(214, 226, 236),
            ground_near: c(84, 96, 88),
            ground_far: c(150, 160, 156),
            horizon: 0.6,
            skyline: Skyline::Mountains,
            water: true,
            skyline_colour: c(196, 206, 220),
            sun: (0.22, 0.3, 0.06),
            sun_colour: c(255, 250, 230),
        },
        // A studio morning: soft peach and sand.
        10 => FixtureScene {
            sky_top: c(226, 186, 170),
            sky_bottom: c(246, 226, 204),
            ground_near: c(168, 132, 102),
            ground_far: c(214, 184, 150),
            horizon: 0.57,
            skyline: Skyline::Open,
            water: false,
            skyline_colour: c(0, 0, 0),
            sun: (0.66, 0.3, 0.08),
            sun_colour: c(255, 244, 226),
        },
        // A first swim: bright midday sea.
        11 => FixtureScene {
            sky_top: c(20, 120, 220),
            sky_bottom: c(140, 206, 246),
            ground_near: c(10, 96, 150),
            ground_far: c(60, 170, 200),
            horizon: 0.45,
            skyline: Skyline::Open,
            water: true,
            skyline_colour: c(0, 0, 0),
            sun: (0.74, 0.3, 0.07),
            sun_colour: c(255, 252, 220),
        },
        // The harbour in golden hour: teal water, a gold sky, the quay.
        _ => FixtureScene {
            sky_top: c(60, 96, 130),
            sky_bottom: c(240, 196, 120),
            ground_near: c(16, 60, 70),
            ground_far: c(80, 130, 128),
            horizon: 0.6,
            skyline: Skyline::City,
            water: true,
            skyline_colour: c(40, 52, 62),
            sun: (0.3, 0.42, 0.07),
            sun_colour: c(255, 230, 160),
        },
    }
}

/// Deterministic, hermetic gallery fixtures: thirteen procedural landscapes,
/// each with its own light, colour and horizon, so an offscreen library reads
/// as a collection rather than copies of one placeholder. The sun is round
/// whatever the picture's shape.
///
/// # Panics
///
/// Panics if `w` or `h` is `0`; every call site passes a fixed, non-zero
/// scenario size, so this never fires in practice.
#[allow(
    clippy::many_single_char_names,
    reason = "x/y/t are the idiomatic names for pixel coordinates and an interpolation factor"
)]
#[allow(
    clippy::expect_used,
    reason = "the buffer this builds is always exactly w*h*4 bytes, and every \
              call site passes a fixed non-zero w/h"
)]
pub fn shots_pattern_variant(w: u32, h: u32, variant: u8) -> pixels::Rgba {
    let scene = fixture_scene(variant);
    let mix = |a: [f32; 3], b: [f32; 3], t: f32| {
        let t = t.clamp(0.0, 1.0);
        [
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
        ]
    };
    let (wf, hf) = (w as f32, h as f32);
    let short = wf.min(hf);
    let (sun_x, sun_y, sun_r) = (scene.sun.0 * wf, scene.sun.1 * hf, scene.sun.2 * short);
    let phase = f32::from(variant) * 1.7;
    let mut bytes = Vec::with_capacity((w as usize) * (h as usize) * 4);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / wf, y as f32 / hf);
            let ridge = match scene.skyline {
                Skyline::Open => 0.0,
                Skyline::Mountains => {
                    let peaks = (fx * 9.0 + phase).sin().abs() * 0.6
                        + (fx * 23.0 + phase * 2.0).sin().abs() * 0.25;
                    peaks * 0.2
                }
                Skyline::Hills => ((fx * 4.0 + phase).sin() * 0.5 + 0.5) * 0.08,
                Skyline::City => {
                    // Blocks of varied height, each a twentieth of the width.
                    let block = (fx * 20.0).floor();
                    let height = ((block * 12.9898 + phase).sin() * 43_758.547).fract().abs();
                    0.04 + height * 0.14
                }
            };
            let mut colour = if fy < scene.horizon {
                mix(scene.sky_top, scene.sky_bottom, fy / scene.horizon)
            } else {
                mix(
                    scene.ground_far,
                    scene.ground_near,
                    (fy - scene.horizon) / (1.0 - scene.horizon),
                )
            };
            let (dx, dy) = (x as f32 - sun_x, y as f32 - sun_y);
            let distance = (dx * dx + dy * dy).sqrt();
            if fy < scene.horizon {
                if distance < sun_r {
                    colour = scene.sun_colour;
                } else if distance < sun_r * 2.2 {
                    // A soft glow around the sun.
                    let glow = 1.0 - (distance - sun_r) / (sun_r * 1.2);
                    colour = mix(colour, scene.sun_colour, glow * glow * 0.35);
                }
                if ridge > 0.0 && fy > scene.horizon - ridge {
                    colour = scene.skyline_colour;
                }
            } else if scene.water
                && dx.abs() < sun_r * 1.4
                && (y as f32 - hf * scene.horizon) as u32 % 6 < 3
            {
                // The sun's reflection on water, broken into ripples.
                let fade = 1.0 - (y as f32 - hf * scene.horizon) / (hf * (1.0 - scene.horizon));
                colour = mix(colour, scene.sun_colour, fade * 0.25);
            }
            bytes.extend_from_slice(&[colour[0] as u8, colour[1] as u8, colour[2] as u8, 255]);
        }
    }
    pixels::Rgba::new(w, h, bytes).expect("generated pattern is well formed")
}

/// Pose the app for one named `--shots` scenario.
///
/// The generated pattern is written to `dir` and opened through the ordinary
/// path, so the screenshot exercises the same decode the live app runs rather
/// than a special case that could hide a bug in it.
#[allow(
    clippy::too_many_lines,
    reason = "offscreen scenarios intentionally keep all deterministic fixtures and their visible state in one place"
)]
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

    // These two poses exercise the states a collaborator can see
    // before their content arrives, and after bytes turn out not to be an
    // image.  They intentionally write only tiny document metadata: neither
    // state adds picture bytes solely for a screenshot.
    if scenario == "loading" {
        let open = OpenImage::from_path("/photos-shots/not-here-yet.png", 1200, 800)
            .with_content("0".repeat(64));
        PhotosDoc::set_image(doc, Some(&open));
        return;
    }
    if scenario == "error" {
        let path = dir.join("unreadable-image.png");
        if files::StateFile::at(&path).is_some_and(|file| file.write(b"this is not an image")) {
            let open = OpenImage::from_path(&path.to_string_lossy(), 1200, 800);
            PhotosDoc::set_image(doc, Some(&open));
        }
        return;
    }

    let path = dir.join("harbour.png");
    let pattern = shots_pattern_variant(1200, 800, 12);
    if let Some(png) = pixels::encode_png(&pattern)
        && !files::StateFile::at(&path).is_some_and(|file| file.write(&png))
    {
        return;
    }
    app.open_path(doc, &path.to_string_lossy());
    // The first imported photograph is part of the timeline fixture too.
    // Give it an honest capture day so it joins the newest group instead of
    // producing an orphaned "Undated" hero tile ahead of the collection.
    let mut seeded_library = PhotosDoc::library(doc);
    if let Some(active) = PhotosDoc::image(doc)
        && let Some(photo) =
            seeded_library.photo_mut(&crate::model::LibraryPhoto::from_image(active).id)
    {
        photo.captured_at = Some("2026-07-12 18:16".into());
        photo.folder = Some("Travel 2026".into());
        photo.people = vec!["Élodie".into()];
        photo.tags = vec!["golden hour".into()];
        PhotosDoc::set_library(doc, &seeded_library);
    }

    match scenario {
        // A library has useful context only when it contains more than the
        // active image. The fixtures are procedural and hermetic, while their
        // different light treatments make thumbnail, selection and metadata
        // hierarchy legible in the gallery instead of simulating a library
        // with repeated placeholder art.
        "library"
        | "albums"
        | "people"
        | "people-detail"
        | "places"
        | "analysis"
        | "import-progress"
        | "import-errors"
        | "selection"
        | "selection-many-albums"
        | "album-manage"
        | "metadata-edit"
        | "metadata-batch" => {
            let mut library = PhotosDoc::library(doc);
            let mut travel = Vec::new();
            for (variant, (name, captured_at)) in [
                ("Harbour dusk", "2026-07-12 20:24"),
                ("Summer trail", "2026-06-15 09:18"),
                ("Museum light", "2026-06-15 14:42"),
                ("Rainy market", "2026-06-16 10:06"),
                ("Dog at the lake", "2026-07-12 17:30"),
                ("Window cat", "2026-07-12 08:11"),
                ("Sunset ferry", "2026-07-12 21:02"),
                ("Garden supper", "2026-07-12 19:14"),
                ("Paper lanterns", "2026-07-12 22:09"),
                ("Mountain lookout", "2026-07-12 11:28"),
                ("Studio morning", "2026-07-12 07:51"),
                ("First swim", "2026-07-12 15:45"),
            ]
            .into_iter()
            .enumerate()
            {
                let sample_path = dir.join(format!("{name}.png"));
                // Exercise the real justified chronology with the landscape,
                // portrait, square and panoramic shapes a photo library
                // actually contains. Keeping acceptance fixtures all 3:2
                // made serious row-packing regressions visually invisible.
                let (width, height) = match variant % 5 {
                    0 => (900, 600),
                    1 => (600, 900),
                    2 => (800, 800),
                    3 => (1_200, 675),
                    _ => (720, 960),
                };
                let fixture = shots_pattern_variant(width, height, variant as u8);
                if let Some(bytes) = pixels::encode_png(&fixture)
                    && let Some(file) = files::StateFile::at(&sample_path)
                {
                    let _ = file.write(&bytes);
                }
                let image = OpenImage::from_path(&sample_path.to_string_lossy(), width, height);
                let mut photo = crate::model::LibraryPhoto::from_image(image);
                photo.favorite =
                    matches!(name, "Summer trail" | "Dog at the lake" | "Sunset ferry");
                photo.folder = Some(if name.contains("Museum") || name.contains("Studio") {
                    "City walks".into()
                } else {
                    "Travel 2026".into()
                });
                photo.people = if name.contains("Summer") || name.contains("Dog") {
                    vec!["Élodie".into(), "Sam".into()]
                } else if name.contains("cat") {
                    vec!["Noor".into()]
                } else {
                    vec!["Élodie".into()]
                };
                photo.tags = vec![if name.contains("Rainy") {
                    "rain".into()
                } else if name.contains("Dog") || name.contains("cat") {
                    "pets".into()
                } else {
                    "golden hour".into()
                }];
                photo.captured_at = Some(captured_at.into());
                if scenario == "places" {
                    let locations = [
                        (37.7749, -122.4194, "San Francisco"),
                        (48.8566, 2.3522, "Paris"),
                        (35.6762, 139.6503, "Tokyo"),
                    ];
                    #[expect(
                        clippy::indexing_slicing,
                        reason = "the index is reduced modulo the array length"
                    )]
                    let (latitude, longitude, label) = locations[variant % locations.len()];
                    photo.location = Some(crate::model::PhotoLocation {
                        latitude,
                        longitude,
                        label: Some(label.into()),
                        provenance: crate::model::LocationProvenance {
                            source: "user".into(),
                            scope: "shared".into(),
                        },
                    });
                }
                travel.push(photo.id.clone());
                library.add_photo(photo);
            }
            library.albums.push(crate::model::Album {
                id: "photos.album:travel".into(),
                name: "Travel".into(),
                photo_ids: travel.clone(),
            });
            // Each album opens with a different photograph, so its cover is
            // its own.
            let pick = |indices: &[usize]| -> Vec<String> {
                indices
                    .iter()
                    .filter_map(|index| travel.get(*index).cloned())
                    .collect()
            };
            if scenario == "albums" {
                library.albums.extend([
                    crate::model::Album {
                        id: "photos.album:summer-weekends".into(),
                        name: "Summer weekends".into(),
                        photo_ids: travel.iter().skip(1).take(6).cloned().collect(),
                    },
                    crate::model::Album {
                        id: "photos.album:family-favorites".into(),
                        name: "Family favorites".into(),
                        photo_ids: pick(&[7, 4, 11, 5, 1]),
                    },
                    crate::model::Album {
                        id: "photos.album:ideas".into(),
                        name: "Ideas for next time".into(),
                        photo_ids: travel.iter().rev().take(3).cloned().collect(),
                    },
                    crate::model::Album {
                        id: "photos.album:new-home".into(),
                        name: "New home".into(),
                        photo_ids: pick(&[10, 5]),
                    },
                ]);
            }
            if scenario == "selection-many-albums" {
                for (id, name, members) in [
                    ("summer-weekends", "Summer weekends", &[1, 2, 3, 4][..]),
                    ("family-favorites", "Family favorites", &[7, 4, 11, 5, 1]),
                    ("ideas", "Ideas for next time", &[11, 10, 9]),
                    ("new-home", "New home", &[10, 5]),
                    ("lisbon", "A weekend in Lisbon", &[3, 8, 0]),
                    ("birthday", "Birthday dinner", &[7, 8]),
                ] {
                    library.albums.push(crate::model::Album {
                        id: format!("photos.album:{id}"),
                        name: name.into(),
                        photo_ids: pick(members),
                    });
                }
            }
            PhotosDoc::set_library(doc, &library);
            if scenario == "albums" {
                app.show_albums_for_shot();
            } else if matches!(scenario, "selection" | "selection-many-albums") {
                app.show_selection_for_shot(doc);
                if scenario == "selection-many-albums" {
                    app.show_selection_destination_chooser_for_shot(doc);
                }
            } else if scenario == "album-manage" {
                app.show_album_editor_for_shot(doc);
            } else if scenario == "metadata-edit" {
                app.show_metadata_editor_for_shot(doc);
            } else if scenario == "metadata-batch" {
                app.show_batch_metadata_editor_for_shot(doc);
            } else if matches!(scenario, "people" | "people-detail") {
                // A manual, explicitly labelled fixture exercises the review
                // layout without pretending that a face model recognized
                // anyone. Live launches never seed People data.
                let _ = app.people_controller_mut().add_cluster(
                    "shots-imported-suggestion",
                    travel.into_iter().take(4),
                    crate::people::ProposalOrigin::ImportedOrManual,
                );
                if scenario == "people-detail" {
                    app.show_people_suggestion_for_shot("shots-imported-suggestion");
                } else {
                    app.show_people();
                }
            } else if scenario == "places" {
                app.show_places_for_shot();
            } else if scenario == "analysis" {
                app.show_library_analysis();
            } else if scenario == "import-progress" {
                app.show_directory_import_progress_for_shot();
            } else if scenario == "import-errors" {
                app.show_directory_import_errors_for_shot();
            } else {
                app.show_library();
            }
        }
        // A completed crop, rendered fit-to-canvas. A screenshot must show a
        // truthful edit, not a deliberately zoomed inspection state that can
        // be mistaken for a broken initial fit on a narrow viewport.
        "cropping" => {
            let e = PhotosDoc::edits(doc).cropped_from_view(Rect01 {
                x: 0.12,
                y: 0.08,
                w: 0.76,
                h: 0.84,
            });
            PhotosDoc::set_edits(doc, &e);
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

pub use resource::{Photos, PhotosShell, PhotosState, launch_spec, standalone_launch};

#[cfg(test)]
mod framework_history_tests;

#[cfg(test)]
mod camera_import_tests {
    use automerge::AutoCommit;
    use tilo_content::ContentStore;

    use super::{camera_import_photo, import_camera_media, pixels, shots_pattern_variant};
    use crate::doc::PhotosDoc;

    #[test]
    fn camera_import_and_gallery_fixtures_verify_real_content() {
        eprintln!("phase camera_import_verifies_bytes_and_selects_one_content_addressed_photo");
        {
            let rgba = pixels::Rgba::new(1, 1, vec![12, 34, 56, 255]).expect("fixture rgba");
            let bytes = pixels::encode_png(&rgba).expect("fixture png");
            let store = ContentStore::memory();
            let hash = store.put(&bytes).expect("fixture content");
            let media_id = format!("media:blake3:{hash}");
            let mut doc = AutoCommit::new();

            let id = import_camera_media(&mut doc, &media_id, "Camera photo.png", &store)
                .expect("verified import");
            let again = import_camera_media(&mut doc, &media_id, "renamed.png", &store)
                .expect("idempotent import");

            assert_eq!(id, again);
            let library = PhotosDoc::library(&doc);
            assert_eq!(library.photos.len(), 1);
            assert_eq!(
                PhotosDoc::image(&doc).map(|image| image.content),
                Some(Some(hash))
            );
        }

        eprintln!("phase camera_import_refuses_missing_or_invalid_portable_bytes");
        {
            let store = ContentStore::memory();
            assert!(camera_import_photo("media:blake3:not-a-hash", "x.png", &store).is_err());
            let missing = format!("media:blake3:{}", "0".repeat(64));
            assert!(camera_import_photo(&missing, "x.png", &store).is_err());
        }

        eprintln!("phase gallery_fixtures_are_distinct_but_deterministic");
        {
            let harbour = shots_pattern_variant(32, 20, 0);
            let trail = shots_pattern_variant(32, 20, 1);
            assert_eq!(harbour.w, 32);
            assert_eq!(harbour.h, 20);
            assert_ne!(harbour.bytes, trail.bytes);
            assert_eq!(trail, shots_pattern_variant(32, 20, 1));
        }
    }
}
