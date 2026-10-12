//! Fingerprinting what a one-shot `--shots` capture composed.
//!
//! A screenshot path that differs run to run cannot be diffed, and a picture
//! nobody can diff is a picture nobody is checking — which is how a solid-black
//! window survived here for hours. So "two runs agree" has to be a thing a test
//! can assert, not a thing a person eyeballs.
//!
//! It is exactly one layer down from the PNG, because the two layers have
//! different guarantees and only one of them is the app's:
//!
//! * The **scene** the draw pass builds is exact. The same scenario encodes the
//!   same bytes, in this process and in another one. Everything the editor
//!   decides — which bin row is highlighted, where the playhead sits, what the
//!   inspector lists — lands here, so this is the layer the app is answerable
//!   for and the layer [`fingerprint`] measures.
//! * The **pixels** vello rasterises from that scene are not bit-stable. Its
//!   `path_count` stage assigns tile segments with an `atomicAdd`, `fine`
//!   accumulates coverage over that order, and float addition is not
//!   associative — so a pixel whose coverage sits on a quantisation boundary
//!   lands one 8-bit step either side of it. `ui::offscreen`'s module docs
//!   describe the mechanism; measured on this app it is four pixels out of
//!   1 024 000, always ±1 in a single channel, and it never changes which
//!   pixels carry ink.
//!
//! A capture is therefore compared with
//! `OffscreenRenderer::pixels_differing_beyond_aa`, which tolerates that one
//! step and nothing wider, while a scenario's own determinism is asserted on
//! the fingerprint, where it is exact and where an app-level bug — a set
//! iterated in hash order, an id freshly rolled each run — actually shows up.

use vello::Scene;

/// A hash of everything the draw pass encoded: the path and draw streams, the
/// transforms, the styles and the counts.
///
/// Bitwise throughout — floats are hashed by their bits, because two scenes
/// are the same scene only if they describe the same geometry exactly, and a
/// tolerance here would be a tolerance on the thing being proved. Vello's own
/// encoding stores path coordinates as `u32` for the same reason.
///
/// The value is only meaningful against another fingerprint taken from the
/// same binary: `DefaultHasher`'s algorithm is not specified across releases.
/// That is all the comparison needs — it asks whether one build gives one
/// answer twice, not what the answer is.
pub fn fingerprint(scene: &Scene) -> u64 {
    use std::hash::{Hash, Hasher};
    let e = scene.encoding();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for t in &e.path_tags {
        t.0.hash(&mut h);
    }
    e.path_data.hash(&mut h);
    for t in &e.draw_tags {
        t.0.hash(&mut h);
    }
    e.draw_data.hash(&mut h);
    for t in &e.transforms {
        for v in t.matrix.iter().chain(t.translation.iter()) {
            v.to_bits().hash(&mut h);
        }
    }
    for s in &e.styles {
        s.flags_and_miter_limit.hash(&mut h);
        s.line_width.to_bits().hash(&mut h);
    }
    e.n_paths.hash(&mut h);
    e.n_path_segments.hash(&mut h);
    e.n_clips.hash(&mut h);
    e.n_open_clips.hash(&mut h);
    e.flags.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VideoEditor;
    use ui::prelude::TestApp;

    /// Capture one scenario from scratch, the way `--shots --scenario`
    /// does: a fresh framework host, the scenario staged through it, one
    /// frame composed at `size`. A whole independent build of the app, the
    /// record and the scene, which is what a second process would do.
    fn capture(scenario: Option<&str>, size: (u32, u32)) -> TestApp<VideoEditor> {
        TestApp::<VideoEditor>::capture_at(scenario, size).expect("the scenario stages")
    }

    fn print_of(scenario: Option<&str>, size: (u32, u32)) -> u64 {
        fingerprint(capture(scenario, size).harness().scene())
    }

    /// The scenarios whose determinism is asserted: the plain demo frame and
    /// the ones whose content arrives from the document's collections.
    const DETERMINISM_SCENARIOS: [Option<&str>; 5] = [
        None,
        Some("markers"),
        Some("filters"),
        Some("split"),
        Some("effects"),
    ];

    /// `part` of `parts` near-equal slices of `all`. Each capture is a whole
    /// app build, so a scenario list is spread over a few tests that the
    /// runner schedules in parallel; the last slice takes the remainder, so
    /// every entry lands in exactly one part however the list grows.
    fn part_of<T>(all: &[T], part: usize, parts: usize) -> &[T] {
        let per = all.len().div_ceil(parts);
        let start = (per * part).min(all.len());
        let end = if part + 1 == parts {
            all.len()
        } else {
            (start + per).min(all.len())
        };
        &all[start..end]
    }

    /// Every build of one scenario must describe the same picture exactly.
    ///
    /// Three builds rather than two, because the failures this exists for do
    /// not show every time: the project's collections arrive from the document
    /// in map-key order, the ids are minted fresh each run, and two random
    /// orders of a handful of clips agree often enough that a single pair
    /// proves little. Three independent builds make an accidental agreement
    /// unlikely enough to trust.
    ///
    /// It has caught two real ones. The bin used to highlight a different row
    /// each run, because the opening selection was taken from that same
    /// map-key order. And the timeline used to draw its clips in a different
    /// order each run, which is the same set of shapes emitted in a different
    /// sequence -- identical counts, identical stream lengths, different
    /// bytes, and a capture nobody could diff.
    fn assert_scenarios_compose_the_same_scene_every_time(part: usize) {
        for &scenario in part_of(&DETERMINISM_SCENARIOS, part, 3) {
            let first = print_of(scenario, (1280, 800));
            for again in 1..3 {
                assert_eq!(
                    first,
                    print_of(scenario, (1280, 800)),
                    "{scenario:?} composed a different scene on build {again}",
                );
            }
        }
    }

    #[test]
    fn a_scenario_composes_the_same_scene_every_time_part_1() {
        assert_scenarios_compose_the_same_scene_every_time(0);
    }

    #[test]
    fn a_scenario_composes_the_same_scene_every_time_part_2() {
        assert_scenarios_compose_the_same_scene_every_time(1);
    }

    #[test]
    fn a_scenario_composes_the_same_scene_every_time_part_3() {
        assert_scenarios_compose_the_same_scene_every_time(2);
    }

    /// The parts above and below cover their lists exactly once: no scenario
    /// is dropped from the determinism or staging check by the split.
    #[test]
    fn the_scenario_parts_cover_every_scenario_once() {
        fn joined<T: Clone>(all: &[T], parts: usize) -> Vec<T> {
            (0..parts)
                .flat_map(|part| part_of(all, part, parts).to_vec())
                .collect()
        }
        assert_eq!(joined(&DETERMINISM_SCENARIOS, 3), DETERMINISM_SCENARIOS);
        assert_eq!(
            joined(crate::app::ShotScenario::ALL_NAMES, STAGING_PARTS),
            crate::app::ShotScenario::ALL_NAMES,
        );
    }

    /// The picture itself, twice, through the real GPU path a `--shots`
    /// capture uses.
    ///
    /// Two rasterisations of one scene are allowed to disagree by one 8-bit
    /// step on antialiased edge pixels and by nothing else — measured on this
    /// app, four pixels of 1 024 000, each +/-1 in a single channel, with the
    /// set of pixels carrying ink unchanged. `pixels_differing_beyond_aa`
    /// tolerates exactly that step, so a zero here means the two runs made the
    /// same picture and anything above zero is a real difference in what was
    /// drawn. A machine that cannot rasterise cannot observe the property, so
    /// failing to build a renderer is the correct outcome here rather than a
    /// silent pass.
    #[test]
    fn two_captures_of_one_scenario_are_the_same_picture() {
        let shoot = || capture(Some("markers"), (640, 400)).harness().render_rgba();
        let a = shoot();
        let b = shoot();
        assert!(!a.is_empty(), "the capture produced pixels");
        assert_eq!(
            ui::offscreen::OffscreenRenderer::pixels_differing_beyond_aa(&a, &b),
            0,
            "two runs of one scenario drew different pictures",
        );
    }

    /// And the fingerprint has to be able to tell scenes apart, or the check
    /// above would pass on a function that returned a constant.
    #[test]
    fn the_fingerprint_separates_scenes_that_differ() {
        let live = print_of(None, (1280, 800));
        assert_ne!(
            live,
            print_of(Some("markers"), (1280, 800)),
            "two different scenarios are two different scenes",
        );
        assert_ne!(
            live,
            print_of(None, (900, 700)),
            "the same scenario at another size is a different scene",
        );
        assert_ne!(
            live,
            print_of(Some(crate::app::EMPTY_CAPTURE), (1280, 800)),
            "the blank capture is not the demo project",
        );
    }

    /// How many tests the launcher's scenario list is spread over.
    const STAGING_PARTS: usize = 4;

    /// Every scenario the launcher accepts stages on the framework host, and
    /// its view state lands: the capture differs from the plain demo frame.
    fn assert_scenarios_stage_through_the_host(part: usize) {
        let plain = print_of(None, (1280, 800));
        for name in part_of(crate::app::ShotScenario::ALL_NAMES, part, STAGING_PARTS) {
            assert_ne!(
                print_of(Some(name), (1280, 800)),
                plain,
                "{name} staged nothing visible"
            );
        }
    }

    #[test]
    fn every_scenario_stages_through_the_host_part_1() {
        assert_scenarios_stage_through_the_host(0);
    }

    #[test]
    fn every_scenario_stages_through_the_host_part_2() {
        assert_scenarios_stage_through_the_host(1);
    }

    #[test]
    fn every_scenario_stages_through_the_host_part_3() {
        assert_scenarios_stage_through_the_host(2);
    }

    #[test]
    fn every_scenario_stages_through_the_host_part_4() {
        assert_scenarios_stage_through_the_host(3);
    }

    /// `--narrow` reveals the collapsed bin, which only collapses narrow.
    #[test]
    fn the_narrow_capture_opens_the_bin_overlay() {
        assert_ne!(
            print_of(Some(crate::app::NARROW_CAPTURE), (720, 820)),
            print_of(None, (720, 820)),
            "the narrow capture opens the bin overlay"
        );
    }

    /// A capture launched on a file imports it, places it on the timeline
    /// and waits for its picture — and a file that never decodes still
    /// settles, showing what the editor shows for it, instead of holding the
    /// capture until it gives up.
    #[test]
    fn a_capture_launched_on_an_undecodable_clip_settles() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let clip = dir.path().join("Broken.mp4");
        // Binary bytes no container parser recognises: imported as a video
        // by its extension, then refused by every decoder.
        let bytes: Vec<u8> = (0..4096u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        std::fs::write(&clip, bytes).expect("write the clip");
        let path = clip.to_string_lossy().into_owned();
        let app =
            TestApp::<VideoEditor>::capture_launched(Some(crate::app::EMPTY_CAPTURE), &[&path])
                .expect("the capture settles");
        let project = app.state();
        assert_eq!(project.media.len(), 1, "the launched file was imported");
        assert_eq!(project.clips.len(), 1, "and placed on the timeline");
    }

    /// The staged capture is of the scenario, not of a blank editor: the
    /// demo project went in through the host, as one attributed action.
    #[test]
    fn a_capture_loads_the_demo_project_through_the_host() {
        let app = capture(Some("split"), (1280, 800));
        assert!(!app.state().clips.is_empty());
        assert!(
            app.versions()
                .iter()
                .any(|version| version.context.as_ref().is_some_and(|c| c.what.is_some())),
            "the load is an attributed version"
        );
    }
}
