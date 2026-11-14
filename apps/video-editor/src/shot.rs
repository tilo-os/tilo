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

    /// The viewport of a plain `--shots`.
    const FULL: (u32, u32) = (1280, 800);

    /// The captures built three times over.
    const DETERMINISM_SCENARIOS: [Option<&str>; 5] = [
        None,
        Some("markers"),
        Some("filters"),
        Some("split"),
        Some("effects"),
    ];

    /// One capture the launcher can be asked for, and what must hold of it. Every row but the
    /// plain capture itself must compose a scene the plain capture does not: a scenario that
    /// staged nothing visible, a viewport that changed nothing and a fingerprint that returned a
    /// constant would each show the plain scene again.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Row {
        scenario: Option<&'static str>,
        viewport: (u32, u32),
        /// How many independent builds must compose the same scene. Three rather than two:
        /// collections arrive in map-key order and ids are minted per run, and two random
        /// orders agree often enough that a single pair proves little.
        builds: usize,
        /// Whether the capture opens a project. The staged capture is of the scenario, not of
        /// a blank editor; only the blank capture shows one.
        opens_a_project: bool,
    }

    impl Row {
        fn name(self) -> String {
            let (width, height) = self.viewport;
            format!(
                "{} at {width}x{height}",
                self.scenario.unwrap_or("the plain capture")
            )
        }

        /// One independent build of the capture: its scene's fingerprint, once the build is
        /// seen to hold what the row says it opens.
        fn build(self) -> u64 {
            let name = self.name();
            let mut app = capture(self.scenario, self.viewport);
            let print = fingerprint(app.harness().scene());
            let project = app.state();
            if self.opens_a_project {
                assert!(!project.clips.is_empty(), "{name} shows no timeline");
                assert!(
                    app.versions()
                        .iter()
                        .any(|version| version.context.as_ref().is_some_and(|c| c.what.is_some())),
                    "{name}: the load is an attributed version"
                );
            } else {
                assert!(project.clips.is_empty(), "{name} opened a project");
            }
            print
        }
    }

    /// The plain capture, every scenario the launcher lists, the plain capture at another
    /// size and the blank capture. The thrice-built rows come first, the plain capture at
    /// their head, so that dealing the rows out gives each part one of them, as its first row.
    fn rows() -> Vec<Row> {
        let row = |scenario: Option<&'static str>| Row {
            scenario,
            viewport: FULL,
            builds: if DETERMINISM_SCENARIOS.contains(&scenario) {
                3
            } else {
                1
            },
            opens_a_project: true,
        };
        let mut rows = vec![row(None)];
        rows.extend(
            crate::app::ShotScenario::ALL_NAMES
                .iter()
                .map(|&name| row(Some(name))),
        );
        rows.push(Row {
            viewport: (900, 700),
            builds: 1,
            ..row(None)
        });
        rows.push(Row {
            opens_a_project: false,
            ..row(Some(crate::app::EMPTY_CAPTURE))
        });
        // Stable, so each group keeps the order it was listed in.
        rows.sort_by_key(|row| std::cmp::Reverse(row.builds));
        rows
    }

    /// How many tests the rows are dealt over: each capture is a whole app build, so the
    /// table is spread over tests the runner runs in parallel.
    const PARTS: usize = 5;

    /// Every `PARTS`th row from the `part`th on, so every row lands in exactly one part.
    fn part_of(rows: &[Row], part: usize) -> Vec<Row> {
        rows.iter().copied().skip(part).step_by(PARTS).collect()
    }

    /// A part's thrice-built row is its first, and it is built before anything else, the plain
    /// capture included. Under a runner that gives each test a process, its first build is
    /// then the first capture that process makes, as a `--shots` run's is, and the builds it
    /// must equal are not: a scene that depended on what an earlier capture left in the
    /// process would differ between them.
    fn assert_part(part: usize) {
        let rows = rows();
        let plain_row = *rows.first().expect("the table has rows");
        assert_eq!(
            (plain_row.scenario, plain_row.viewport),
            (None, FULL),
            "the table starts at the plain capture"
        );
        let mut plain_print = None;
        for row in part_of(&rows, part) {
            let name = row.name();
            let first = row.build();
            let plain = *plain_print.get_or_insert_with(|| {
                if row == plain_row {
                    first
                } else {
                    plain_row.build()
                }
            });
            if row != plain_row {
                assert_ne!(
                    first, plain,
                    "{name} composed the plain capture's scene: it staged nothing visible"
                );
            }
            for again in 1..row.builds {
                assert_eq!(
                    first,
                    row.build(),
                    "{name} composed a different scene on build {again}"
                );
            }
        }
    }

    #[test]
    fn every_capture_stages_and_composes_one_scene_part_1() {
        assert_part(0);
    }

    #[test]
    fn every_capture_stages_and_composes_one_scene_part_2() {
        assert_part(1);
    }

    #[test]
    fn every_capture_stages_and_composes_one_scene_part_3() {
        assert_part(2);
    }

    #[test]
    fn every_capture_stages_and_composes_one_scene_part_4() {
        assert_part(3);
    }

    #[test]
    fn every_capture_stages_and_composes_one_scene_part_5() {
        assert_part(4);
    }

    /// No row is dropped by the split, and the table holds every capture it is meant to.
    #[test]
    fn the_scenario_parts_cover_every_scenario_once() {
        let rows = rows();
        let dealt: Vec<Row> = (0..PARTS).flat_map(|part| part_of(&rows, part)).collect();
        assert_eq!(dealt.len(), rows.len(), "the parts hold every row once");
        for row in &rows {
            assert_eq!(
                dealt.iter().filter(|other| *other == row).count(),
                1,
                "{} is checked by exactly one part",
                row.name()
            );
        }
        let full = |scenario: Option<&str>| -> Vec<Row> {
            rows.iter()
                .copied()
                .filter(|row| row.scenario == scenario && row.viewport == FULL)
                .collect()
        };
        for &name in crate::app::ShotScenario::ALL_NAMES {
            assert_eq!(full(Some(name)).len(), 1, "{name} has one row");
        }
        for scenario in DETERMINISM_SCENARIOS {
            let found = full(scenario);
            assert_eq!(found.len(), 1, "{scenario:?} has one row");
            assert!(
                found.iter().all(|row| row.builds == 3),
                "{scenario:?} is built three times"
            );
        }
        assert_eq!(
            rows.iter().filter(|row| row.builds == 3).count(),
            DETERMINISM_SCENARIOS.len(),
            "nothing else claims to be"
        );
        for part in 0..PARTS {
            let of_part = part_of(&rows, part);
            assert_eq!(
                of_part.iter().filter(|row| row.builds == 3).count(),
                1,
                "part {part} builds one capture three times"
            );
            assert_eq!(
                of_part.first().map(|row| row.builds),
                Some(3),
                "part {part} builds it first, before its process has made any other capture"
            );
        }
        let blank: Vec<Row> = rows
            .iter()
            .copied()
            .filter(|row| !row.opens_a_project)
            .collect();
        assert_eq!(blank.len(), 1, "only the blank capture opens no project");
        assert!(
            blank
                .iter()
                .all(|row| row.scenario == Some(crate::app::EMPTY_CAPTURE))
        );
        assert!(
            rows.iter()
                .any(|row| row.scenario.is_none() && row.viewport == (900, 700)),
            "the plain capture at another size is a row"
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
}
