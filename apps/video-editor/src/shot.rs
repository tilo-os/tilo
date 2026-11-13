//! Fingerprinting the scene a `--shots` capture composed, so two runs of a
//! scenario can be compared by a test. The scene is exact; the pixels rasterised
//! from it are not bit-stable, so they are compared with
//! `OffscreenRenderer::pixels_differing_beyond_aa`, which tolerates one 8-bit
//! step on an antialiased edge and nothing wider.

use vello::Scene;

/// A hash of everything the draw pass encoded: the path and draw streams, the
/// transforms, the styles and the counts. Floats are hashed by their bits, since
/// a tolerance here would be a tolerance on the thing being proved.
///
/// Only comparable with another fingerprint from the same binary:
/// `DefaultHasher`'s algorithm is not specified across releases.
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

    /// A whole independent build of the app, the record and the scene, which is
    /// what `--shots --scenario` does and what a second process would do.
    fn capture(scenario: Option<&str>, size: (u32, u32)) -> TestApp<VideoEditor> {
        TestApp::<VideoEditor>::capture_at(scenario, size).expect("the scenario stages")
    }

    fn print_of(scenario: Option<&str>, size: (u32, u32)) -> u64 {
        fingerprint(capture(scenario, size).harness().scene())
    }

    const DETERMINISM_SCENARIOS: [Option<&str>; 5] = [
        None,
        Some("markers"),
        Some("filters"),
        Some("split"),
        Some("effects"),
    ];

    /// `part` of `parts` near-equal slices of `all`: each capture is a whole app
    /// build, so the list is spread over tests the runner runs in parallel. The
    /// last slice takes the remainder, so every entry lands in exactly one part.
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

    /// Three builds rather than two: collections arrive in map-key order and ids
    /// are minted per run, and two random orders agree often enough that a single
    /// pair proves little.
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

    /// No scenario is dropped from either check by the split.
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

    /// Through the real GPU path a `--shots` capture uses. A machine that cannot
    /// rasterise cannot observe the property, so failing to build a renderer is
    /// the right outcome here rather than a silent pass.
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

    /// The check above would pass on a function that returned a constant.
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

    /// A file that never decodes still settles instead of holding the capture
    /// until it gives up.
    #[test]
    fn a_capture_launched_on_an_undecodable_clip_settles() {
        let dir = tempfile::tempdir().expect("a scratch directory");
        let clip = dir.path().join("Broken.mp4");
        // No container parser recognises these bytes: imported as a video by its
        // extension, then refused by every decoder.
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

    /// The staged capture is of the scenario, not of a blank editor.
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
