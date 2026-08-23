//! Composing a one-shot `--shots` capture, and fingerprinting what it composed.
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

use std::collections::HashMap;

use taffy::prelude::*;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};

/// Build the scene one `--shots` frame is rasterised from: sync, publish the
/// history the transport strip draws from, lay out at `w`×`h`, and draw.
///
/// The PNG path and the fingerprint path both come through here, so a
/// fingerprint always describes the very scene the picture was made from
/// rather than a second composition that could drift from it.
pub fn compose(
    widget: &mut dyn Widget,
    (w, h): (u32, u32),
    theme: &Theme,
    state: &mut ui::State,
    text_renderer: &mut TextRenderer,
) -> anyhow::Result<Scene> {
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    widget.sync(&mut state.doc, &state.root);
    // The host publishes the history's state every frame, and the transport
    // strip's undo button is drawn from it. A capture that skipped this step
    // would photograph that button permanently dead however much the scenario
    // had actually done — the picture has to be of the real control, not of a
    // control the capture path starved.
    let status = state.undo_status(None);
    widget.set_undo_status(&status);

    let child = widget.build_layout(&mut taffy, theme, &mut node_map);
    let root = taffy.new_with_children(
        Style {
            size: Size {
                width: Dimension::length(w as f32),
                height: Dimension::length(h as f32),
            },
            ..Default::default()
        },
        &[child],
    )?;

    {
        let measure_map = node_map.clone();
        let tr = &mut *text_renderer;
        taffy.compute_layout_with_measure(
            root,
            taffy::Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )?;
    }
    widget.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        text_renderer,
        theme,
        &presence,
        &permission,
    );
    Ok(scene)
}

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
    use crate::app::ShotScenario;
    use crate::VideoEditorApp;

    /// Compose one scenario from scratch and fingerprint it — a whole
    /// independent build of the app, the document and the scene, which is
    /// what a second process would do.
    fn print_of(scenario: ShotScenario, size: (u32, u32)) -> u64 {
        let mut app: Box<dyn Widget> = Box::new(VideoEditorApp::new_for_shot(scenario));
        let mut state = ui::State::new();
        let mut tr = TextRenderer::new();
        let scene = compose(&mut *app, size, &Theme::dark(), &mut state, &mut tr)
            .expect("the scenario composes");
        fingerprint(&scene)
    }

    /// Two builds of one scenario must describe the same picture exactly.
    ///
    /// This is the check that would have caught the bin highlighting a
    /// different row each run: the opening selection was taken from the
    /// document's map-key order, which is re-rolled every run because the ids
    /// are fresh, and a differently-highlighted row is a different scene.
    #[test]
    fn a_scenario_composes_the_same_scene_twice() {
        for scenario in [
            ShotScenario::Live,
            ShotScenario::Markers,
            ShotScenario::SelectedFilters,
            ShotScenario::JustSplit,
            ShotScenario::Effects,
        ] {
            let a = print_of(scenario, (1280, 800));
            let b = print_of(scenario, (1280, 800));
            assert_eq!(a, b, "{scenario:?} composed two different scenes");
        }
    }

    /// And the fingerprint has to be able to tell scenes apart, or the check
    /// above would pass on a function that returned a constant.
    #[test]
    fn the_fingerprint_separates_scenes_that_differ() {
        let live = print_of(ShotScenario::Live, (1280, 800));
        assert_ne!(
            live,
            print_of(ShotScenario::Markers, (1280, 800)),
            "two different scenarios are two different scenes",
        );
        assert_ne!(
            live,
            print_of(ShotScenario::Live, (900, 700)),
            "the same scenario at another size is a different scene",
        );
    }
}
