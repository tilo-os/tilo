#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "fixed native-4K test allocation/index arithmetic and explicitly clamped diagnostic geometry"
)]

use super::*;

use crate::compositor::backend::drm::paint::{SceneDamage, VelloDrmBridge};

const NATIVE_WIDTH: u32 = 3_840;
const NATIVE_HEIGHT: u32 = 2_160;

fn xrgb_pitch() -> usize {
    NATIVE_WIDTH as usize * 4
}

fn bridge() -> VelloDrmBridge {
    VelloDrmBridge::new(NATIVE_WIDTH, NATIVE_HEIGHT)
        .expect("native retained-frame oracle requires a working wgpu adapter")
}

fn render_dense(bridge: &mut VelloDrmBridge, scene: &Scene) -> Vec<u8> {
    let mut pixels = vec![0; xrgb_pitch() * NATIVE_HEIGHT as usize];
    bridge
        .paint_into_xrgb8888(scene, &mut pixels, xrgb_pitch())
        .expect("dense native-pixel oracle render");
    pixels
}

fn assert_xrgb_pixels_eq(actual: &[u8], expected: &[u8], label: &str) {
    assert_eq!(actual.len(), expected.len(), "{label}: oracle buffer sizes");
    let mut mismatches = 0_usize;
    let mut max_delta = 0_u8;
    let mut first = None;
    for (index, (&actual, &expected)) in actual.iter().zip(expected).enumerate() {
        if actual != expected {
            mismatches += 1;
            max_delta = max_delta.max(actual.abs_diff(expected));
            first.get_or_insert_with(|| {
                (
                    index / 4 % NATIVE_WIDTH as usize,
                    index / xrgb_pitch(),
                    index % 4,
                    actual,
                    expected,
                )
            });
        }
    }
    assert!(
        mismatches == 0,
        "{label}: {mismatches} XRGB bytes differ; max channel delta {max_delta}; first mismatch {first:?}"
    );
}

fn settled_builder() -> FrameBuilder {
    let mut frame = FrameBuilder::new((NATIVE_WIDTH, NATIVE_HEIGHT));
    // The test needs state transitions, not a time-dependent animation sample. This is a local
    // theme override; benchmark and compositor inputs keep their normal motion policy.
    frame.theme = Theme::dark().with_reduce_motion(true);
    frame.relayout();
    let warm = frame.paint();
    assert!(
        !warm.encoding().is_empty(),
        "the desktop warm paint must record source commands"
    );
    frame
}

fn assert_legacy_and_segmented_match(
    frame: &mut FrameBuilder,
    label: &str,
    expect_quick_settings: bool,
) {
    // Keep both recordings on the same builder and unchanged Chrome state. `Scene::encoding_eq`
    // is intentionally not an oracle here: independently recorded text/image Blob identities are
    // conservative, while the native pixels are the contract this test protects.
    let legacy = frame.paint();
    let segmented = frame.paint_with_damage();
    assert!(
        !segmented.scene.encoding().is_empty(),
        "{label}: segmented frame must record source commands"
    );
    let sections = frame
        .last_produced_sections
        .as_ref()
        .expect("paint_with_damage retains its source sections");
    assert_eq!(
        !sections.quick_settings.encoding().is_empty(),
        expect_quick_settings,
        "{label}: Quick Settings settled state"
    );
    if expect_quick_settings {
        assert!(
            sections.quick_settings_bounds.is_some(),
            "{label}: a visible Quick Settings scene must publish bounded damage"
        );
    }

    let mut legacy_bridge = bridge();
    let mut segmented_bridge = bridge();
    let legacy_pixels = render_dense(&mut legacy_bridge, &legacy);
    let segmented_pixels = render_dense(&mut segmented_bridge, &segmented.scene);
    assert_xrgb_pixels_eq(&segmented_pixels, &legacy_pixels, label);
}

fn assert_damage_covers(rect: Rect, damage: &[FineDamageRect], label: &str) {
    // The recorded visual extent is deliberately unconstrained so it can prove shadow reach;
    // native damage is correctly clipped to the output before it reaches the sparse renderer.
    let rect = Rect::new(
        rect.x0.clamp(0.0, f64::from(NATIVE_WIDTH)),
        rect.y0.clamp(0.0, f64::from(NATIVE_HEIGHT)),
        rect.x1.clamp(0.0, f64::from(NATIVE_WIDTH)),
        rect.y1.clamp(0.0, f64::from(NATIVE_HEIGHT)),
    );
    let x0 = rect.x0.floor() as u32;
    let y0 = rect.y0.floor() as u32;
    let x1 = rect.x1.ceil() as u32;
    let y1 = rect.y1.ceil() as u32;
    assert!(
        damage.iter().any(|region| {
            region.x <= x0
                && region.y <= y0
                && region.x.saturating_add(region.width) >= x1
                && region.y.saturating_add(region.height) >= y1
        }),
        "{label}: damage {damage:?} does not cover settled visual extent {rect:?}"
    );
}

fn paint_retained_and_compare(retained: &mut VelloDrmBridge, frame: &FramePaint, label: &str) {
    let mut retained_pixels = vec![0; xrgb_pitch() * NATIVE_HEIGHT as usize];
    retained
        .paint_damage_into_xrgb8888(
            &frame.scene,
            SceneDamage {
                revision: frame.revision,
                previous_revision: frame.previous_revision,
                regions: frame.damage_regions.as_deref(),
            },
            &mut retained_pixels,
            xrgb_pitch(),
        )
        .expect("retained native-pixel render");
    let mut dense = bridge();
    let dense_pixels = render_dense(&mut dense, &frame.scene);
    assert_xrgb_pixels_eq(&retained_pixels, &dense_pixels, label);
}

#[test]
fn native_4k_legacy_and_segmented_shell_frames_match_through_quick_settings_cycle() {
    let mut frame = settled_builder();

    assert_legacy_and_segmented_match(&mut frame, "initial desktop", false);
    frame.with_chrome(|chrome| chrome.overlays.quick_settings = true);
    assert_legacy_and_segmented_match(&mut frame, "Quick Settings open", true);
    frame.with_chrome(|chrome| chrome.overlays.quick_settings = false);
    assert_legacy_and_segmented_match(&mut frame, "Quick Settings close", false);
}

#[test]
fn native_4k_retained_quick_settings_damage_covers_large_shadow_and_matches_dense() {
    let mut frame = settled_builder();
    frame.theme.tokens.elevation.e4.blur = 120.0;
    frame.theme.tokens.elevation.e4.offset_y = 40.0;

    // Preserve an independently rendered dense equivalence proof for the settled, enlarged-shadow
    // desktop before using it as the retained base.
    assert_legacy_and_segmented_match(&mut frame, "large-shadow desktop", false);

    let closed = frame.paint_with_damage();
    let mut retained = bridge();
    let mut seed_pixels = vec![0; xrgb_pitch() * NATIVE_HEIGHT as usize];
    retained
        .paint_damage_into_xrgb8888(
            &closed.scene,
            SceneDamage {
                revision: closed.revision,
                previous_revision: closed.previous_revision,
                regions: closed.damage_regions.as_deref(),
            },
            &mut seed_pixels,
            xrgb_pitch(),
        )
        .expect("seed retained desktop frame");
    drop(seed_pixels);

    frame.with_chrome(|chrome| chrome.overlays.quick_settings = true);
    let open = frame.paint_with_damage();
    let open_damage = open
        .damage_regions
        .as_deref()
        .expect("settled Quick Settings opening should be sparse-eligible");
    let quick_settings_bounds = frame
        .last_produced_sections
        .as_ref()
        .and_then(|sections| sections.quick_settings_bounds)
        .expect("settled Quick Settings publishes its shadow-inclusive bounds");
    let panel = frame.host.quick_settings_rect_for_tests();
    let shadow_reach = ui::surface::std_dev_for(frame.theme.tokens.elevation.e4.blur) * 3.0
        + f64::from(frame.theme.tokens.elevation.e4.offset_y)
        + 0.5;
    assert!(
        quick_settings_bounds.x0 <= panel.x0 - shadow_reach
            && quick_settings_bounds.y0 <= panel.y0 - shadow_reach
            && quick_settings_bounds.x1 >= panel.x1 + shadow_reach
            && quick_settings_bounds.y1 >= panel.y1 + shadow_reach,
        "the default settled damage bounds must include the enlarged e4 shadow"
    );
    assert_damage_covers(
        quick_settings_bounds,
        open_damage,
        "large-shadow Quick Settings open",
    );
    paint_retained_and_compare(&mut retained, &open, "large-shadow Quick Settings open");
    assert!(
        retained.last_sparse_render_attempts() >= 1,
        "the bounded Quick Settings opening must submit a real sparse render"
    );

    frame.with_chrome(|chrome| chrome.overlays.quick_settings = false);
    let close = frame.paint_with_damage();
    paint_retained_and_compare(&mut retained, &close, "large-shadow Quick Settings close");
    assert!(
        retained.last_sparse_render_attempts() >= 1,
        "the bounded Quick Settings close must submit a real sparse render"
    );

    // Re-open only for the independent dense oracle: the retained chain above has already
    // exercised its close edge, while this proves the settled enlarged-shadow QS recording is
    // pixel-identical to the legacy composition too.
    frame.with_chrome(|chrome| chrome.overlays.quick_settings = true);
    assert_legacy_and_segmented_match(&mut frame, "large-shadow Quick Settings open", true);
}

#[test]
fn workspace_root_rebuild_forces_dense_boundary_and_keeps_live_topbar_pixels() {
    let mut frame = settled_builder();
    let baseline = frame.paint_with_damage();
    assert!(
        baseline.damage_regions.is_none(),
        "the first retained frame has no complete predecessor"
    );

    frame.with_chrome(|chrome| {
        chrome.add_workspace("retained root rebuild");
    });
    let mut document = AutoCommit::new();
    let document_root = automerge::ROOT;
    frame.pump_apps(&mut document, &document_root);
    frame.relayout();
    let changed = frame.paint_with_damage();
    assert!(
        changed.damage_regions.is_none(),
        "workspace content root replacement is an opaque retained boundary"
    );
    let sections = frame
        .last_produced_sections
        .as_ref()
        .expect("the changed frame retains the live source sections");
    let topbar_bounds = sections
        .topbar_bounds
        .expect("the rebuilt live topbar reports visual bounds");
    assert!(
        !sections.topbar.encoding().is_empty()
            && topbar_bounds.width() > 0.0
            && topbar_bounds.height() > 0.0,
        "root rebuild must refresh the topbar's live commands and geometry: {topbar_bounds:?}"
    );

    assert_legacy_and_segmented_match(&mut frame, "workspace root rebuild", false);
}

fn rect_scene() -> Scene {
    let mut scene = Scene::new();
    // Fixed fixture ink: the retained-scene tests compare composition and
    // damage behavior, not application styling or theme selection.
    #[allow(clippy::disallowed_methods)]
    let fixture_color = Color::from_rgb8(0x2A, 0x80, 0xC4);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        fixture_color,
        None,
        &Rect::new(10.0, 20.0, 100.0, 120.0),
    );
    scene
}

fn sections(
    quick_settings: Scene,
    workspace: Scene,
) -> crate::shell::overlay_host::ShellSceneSections {
    crate::shell::overlay_host::ShellSceneSections {
        backdrop: Scene::new(),
        topbar: Scene::new(),
        workspace,
        topbar_bounds: None,
        quick_settings,
        above_quick_settings: Scene::new(),
        quick_settings_bounds: Some(Rect::new(10.0, 20.0, 100.0, 120.0)),
    }
}

fn previous_sections(
    global_filter: bool,
    quick_settings: Scene,
    workspace: Scene,
) -> ProducedSections {
    ProducedSections {
        backdrop: Scene::new(),
        topbar: Scene::new(),
        workspace,
        topbar_bounds: None,
        quick_settings,
        above_quick_settings: Scene::new(),
        quick_settings_bounds: Some(Rect::new(10.0, 20.0, 100.0, 120.0)),
        global_filter,
        revision: 1,
    }
}

#[test]
fn damage_policy_allows_only_a_bounded_quick_settings_change() {
    let mut frame = FrameBuilder::new((1_600, 900));
    frame.last_produced_sections = Some(previous_sections(false, Scene::new(), Scene::new()));
    let candidate = sections(rect_scene(), Scene::new());
    assert!(
        frame.damage_from_sections(&candidate, false).is_some(),
        "matching opaque sections and bounded Quick Settings change must establish a sparse baseline"
    );

    frame.last_produced_sections = Some(previous_sections(true, Scene::new(), Scene::new()));
    let candidate = sections(rect_scene(), Scene::new());
    assert!(
        frame.damage_from_sections(&candidate, false).is_none(),
        "a prior whole-frame filter is an explicit retained boundary"
    );
}

#[test]
fn damage_policy_rejects_an_opaque_section_change_after_a_proven_baseline() {
    let mut frame = FrameBuilder::new((1_600, 900));
    frame.last_produced_sections = Some(previous_sections(false, Scene::new(), Scene::new()));
    assert!(
        frame
            .damage_from_sections(&sections(rect_scene(), Scene::new()), false)
            .is_some(),
        "the baseline must prove the Quick Settings candidate itself is otherwise eligible"
    );

    frame.last_produced_sections = Some(previous_sections(false, Scene::new(), Scene::new()));
    assert!(
        frame
            .damage_from_sections(&sections(rect_scene(), rect_scene()), false)
            .is_none(),
        "a workspace recording change is opaque and must force a dense frame"
    );
}
