#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "fixed native-4K test allocation and index arithmetic"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]

use super::*;

use crate::shell::render::paint::{SceneDamage, VelloDrmBridge};
use crate::shell::test_support::{
    DenyAmbientStarts, PopulatedShellFixture, dispatch_populated_fixture_click,
};

const NATIVE_WIDTH: u32 = 3_840;
const NATIVE_HEIGHT: u32 = 2_160;

fn xrgb_pitch() -> usize {
    NATIVE_WIDTH as usize * 4
}

fn bridge() -> VelloDrmBridge {
    let mut bridge = VelloDrmBridge::new(NATIVE_WIDTH, NATIVE_HEIGHT)
        .expect("native retained-frame oracle requires a working wgpu adapter");
    // These oracles compare native 4K pixels; how long llvmpipe takes to produce them on a
    // loaded host is not what they check.
    bridge.wait_for_gpu_completion_for_test();
    eprintln!(
        "retained renderer gate adapter: {:?}",
        bridge
            .adapter_info()
            .expect("retained bridge adapter identity")
    );
    bridge
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
    // Independent WGPU recordings can differ by a one-unit rounding result at a
    // handful of blend edges. Keep the oracle tight enough to catch structural
    // divergence while admitting that bounded backend raster noise.
    assert!(
        mismatches <= 16 && max_delta <= 1,
        "{label}: {mismatches} XRGB bytes differ; max channel delta {max_delta}; first mismatch {first:?}"
    );
}

fn settled_builder() -> PopulatedShellFixture {
    // The test needs state transitions, not a time-dependent animation sample. The explicit
    // fixture receives reduced motion before its initial layout; benchmark inputs stay normal.
    let mut frame = PopulatedShellFixture::new(
        (NATIVE_WIDTH, NATIVE_HEIGHT),
        Theme::dark().with_reduce_motion(true),
    );
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
    let _ = legacy_and_segmented_pixels(frame, label, expect_quick_settings);
}

fn legacy_and_segmented_pixels(
    frame: &mut FrameBuilder,
    label: &str,
    expect_quick_settings: bool,
) -> Vec<u8> {
    // Keep both recordings on the same builder and unchanged Chrome state. `Scene::encoding_eq`
    // is intentionally not an oracle here: independently recorded text/image Blob identities are
    // conservative, while the native pixels are the contract this test protects.
    let legacy = frame.paint();
    assert!(
        !legacy.encoding().is_empty(),
        "{label}: legacy frame must record source commands"
    );
    let segmented = frame.paint_with_damage();
    assert!(
        !segmented.scene.encoding().is_empty(),
        "{label}: segmented frame must record source commands"
    );
    let sections = frame
        .last_produced_sections
        .as_ref()
        .expect("paint_with_damage retains its source sections");
    if expect_quick_settings {
        assert!(
            !sections.quick_settings.encoding().is_empty(),
            "{label}: a visible Quick Settings scene must record source commands"
        );
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
    legacy_pixels
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

fn paint_retained_and_compare(
    retained: &mut VelloDrmBridge,
    dense_oracle: &mut VelloDrmBridge,
    frame: &FramePaint,
    label: &str,
) {
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
    let dense_pixels = render_dense(dense_oracle, &frame.scene);
    assert_xrgb_pixels_eq(&retained_pixels, &dense_pixels, label);
}

#[test]
fn native_4k_legacy_and_segmented_shell_frames_match_through_quick_settings_cycle() {
    let mut frame = settled_builder();

    assert!(!frame.chrome().overlays.quick_settings);
    let initial_pixels = legacy_and_segmented_pixels(&mut frame, "initial desktop", false);
    frame.with_chrome(|chrome| chrome.overlays.quick_settings = true);
    frame.host.set_quick_settings_capture_openness(1_000);
    assert!(frame.chrome().overlays.quick_settings);
    let open_pixels = legacy_and_segmented_pixels(&mut frame, "Quick Settings open", true);
    assert!(
        open_pixels != initial_pixels,
        "opening Quick Settings must change actual output pixels"
    );
    frame.host.set_quick_settings_capture_openness(0);
    frame.with_chrome(|chrome| chrome.overlays.quick_settings = false);
    assert!(!frame.chrome().overlays.quick_settings);
    assert_legacy_and_segmented_match(&mut frame, "Quick Settings close", false);
}

#[test]
fn native_4k_retained_quick_settings_damage_covers_large_shadow_and_matches_dense() {
    let mut frame = settled_builder();
    frame.theme.tokens.elevation.floating.blur = 120.0;
    frame.theme.tokens.elevation.floating.offset_y = 40.0;

    // Preserve an independently rendered dense equivalence proof for the settled, enlarged-shadow
    // desktop before using it as the retained base.
    assert_legacy_and_segmented_match(&mut frame, "large-shadow desktop", false);

    let closed = frame.paint_with_damage();
    let mut retained = bridge();
    let mut dense_oracle = bridge();
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
    let closed_dense_pixels = render_dense(&mut dense_oracle, &closed.scene);
    drop(seed_pixels);
    drop(closed_dense_pixels);

    frame.with_chrome(|chrome| chrome.overlays.quick_settings = true);
    frame.host.set_quick_settings_capture_openness(1_000);
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
    let shadow_reach = ui::surface::std_dev_for(frame.theme.tokens.elevation.floating.blur) * 3.0
        + f64::from(frame.theme.tokens.elevation.floating.offset_y)
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
    paint_retained_and_compare(
        &mut retained,
        &mut dense_oracle,
        &open,
        "large-shadow Quick Settings open",
    );
    assert!(
        retained.last_sparse_render_attempts() >= 1,
        "the bounded Quick Settings opening must submit a real sparse render"
    );

    frame.host.set_quick_settings_capture_openness(0);
    frame.with_chrome(|chrome| chrome.overlays.quick_settings = false);
    let close = frame.paint_with_damage();
    paint_retained_and_compare(
        &mut retained,
        &mut dense_oracle,
        &close,
        "large-shadow Quick Settings close",
    );
    assert!(
        retained.last_sparse_render_attempts() >= 1,
        "the bounded Quick Settings close must submit a real sparse render"
    );

    // Re-open only for the independent dense oracle: the retained chain above has already
    // exercised its close edge, while this proves the settled enlarged-shadow QS recording is
    // pixel-identical to the legacy composition too.
    frame.with_chrome(|chrome| chrome.overlays.quick_settings = true);
    frame.host.set_quick_settings_capture_openness(1_000);
    assert_legacy_and_segmented_match(&mut frame, "large-shadow Quick Settings open", true);
}

#[test]
fn workspace_root_rebuild_forces_dense_boundary_and_keeps_live_topbar_pixels() {
    let mut frame = settled_builder();
    let baseline = frame.paint_with_damage();
    assert!(!baseline.scene.encoding().is_empty());
    assert!(
        baseline.damage_regions.is_none(),
        "the first retained frame has no complete predecessor"
    );
    let mut liveness_bridge = bridge();
    let baseline_pixels = render_dense(&mut liveness_bridge, &baseline.scene);
    let prior_workspace_count = frame.chrome().tree().desktop.workspaces.len();

    frame.with_chrome(|chrome| {
        chrome.add_workspace("retained root rebuild");
    });
    let mut document = AutoCommit::new();
    let document_root = automerge::ROOT;
    frame.pump_apps(&mut document, &document_root);
    frame.relayout();
    let changed = frame.paint_with_damage();
    assert_eq!(
        frame.chrome().tree().desktop.workspaces.len(),
        prior_workspace_count + 1,
        "the root rebuild action must add its workspace"
    );
    assert!(!changed.scene.encoding().is_empty());
    let changed_pixels = render_dense(&mut liveness_bridge, &changed.scene);
    assert!(
        changed_pixels != baseline_pixels,
        "the rebuilt workspace root must change actual output pixels"
    );
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

#[test]
fn workspace_root_rebuild_encodes_the_new_scene_before_the_clip_run() {
    let mut frame = PopulatedShellFixture::new((NATIVE_WIDTH, NATIVE_HEIGHT), Theme::dark());
    frame.relayout();
    let warm = frame.paint_with_damage();
    assert!(
        !warm.scene.encoding().is_empty(),
        "the desktop before the switch records source commands"
    );
    frame.with_chrome(|chrome| {
        chrome.add_workspace("clip after rebuild");
    });
    let mut document = AutoCommit::new();
    let document_root = automerge::ROOT;
    frame.pump_apps(&mut document, &document_root);
    frame.relayout();
    let rebuilt = frame.paint_with_damage();
    assert!(
        !rebuilt.scene.encoding().is_empty(),
        "the rebuilt workspace root has to reach the screen before the clip run"
    );
    let following = frame.paint_with_damage();
    assert!(
        following.scene.encoding().is_empty() && following.workspace_hold,
        "the rest of the switch stays on the content clip"
    );
}

fn assert_populated_tree_inventory(fixture: &PopulatedShellFixture) {
    assert_eq!(fixture.viewport(), (3840.0, 2160.0));
    assert_eq!(
        fixture.host().fixture_provider_ids(),
        vec!["actions", "notes", "editor", "records"]
    );

    let chrome = fixture.chrome();
    assert_eq!(chrome.active_local_user(), "shell-fixture");
    let tree = chrome.tree();
    assert_eq!(
        tree.desktop
            .workspaces
            .iter()
            .map(|workspace| workspace.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Home", "Research", "Design"]
    );
    let home = tree.desktop.active_workspace().expect("Home stays active");
    assert_eq!(home.name, "Home");
    let group = &home.strips[0].window_groups[0];
    assert_eq!(
        group
            .tabs
            .iter()
            .map(|tab| tab.title.as_str())
            .collect::<Vec<_>>(),
        vec!["Notes", "Files", "Terminal", "Document"]
    );
    assert_eq!(group.active_tab, group.tabs[0].id, "Notes stays active");
    assert_eq!(group.width, crate::shell::state::WindowWidth::Wide);
    assert_eq!(
        home.collaborators
            .iter()
            .map(|person| (
                person.id.as_str(),
                person.name.as_str(),
                person.cursor_color.as_deref(),
                person.role_id.as_deref(),
            ))
            .collect::<Vec<_>>(),
        vec![("alice", "Alice", Some("#ec4899"), Some("editor"))]
    );
    let research = &tree.desktop.workspaces[1];
    assert_eq!(
        research
            .collaborators
            .iter()
            .map(|person| (
                person.id.as_str(),
                person.name.as_str(),
                person.cursor_color.as_deref(),
                person.role_id.as_deref(),
            ))
            .collect::<Vec<_>>(),
        vec![
            ("alice", "Alice", Some("#ec4899"), Some("editor")),
            ("bob", "Bob", Some("#f97316"), Some("viewer")),
        ]
    );
    assert!(tree.desktop.workspaces[2].collaborators.is_empty());
    // The fixture is the owning action for its presented tabs: it creates
    // exactly their records, and constructing the tree fabricates no others.
    let expected_records = group
        .tabs
        .iter()
        .map(|tab| ui::record::RecordId::for_tab(tab.id).to_string())
        .collect::<std::collections::BTreeSet<_>>();
    drop(tree);
    let actual_records = chrome
        .records()
        .open_records()
        .into_iter()
        .map(|record| record.id.to_string())
        .collect::<std::collections::BTreeSet<_>>();
    drop(chrome);
    assert_eq!(
        actual_records, expected_records,
        "only the fixture's explicit creations may hold document authority"
    );
}

fn assert_owned_files_fixture(fixture: &PopulatedShellFixture) {
    let mut fixture_children = std::fs::read_dir(fixture.parent.path())
        .expect("fixture parent remains owned")
        .collect::<Result<Vec<_>, _>>()
        .expect("fixture child entries");
    assert_eq!(fixture_children.len(), 1, "one owned Files fixture child");
    let child = fixture_children
        .pop()
        .expect("owned Files fixture child")
        .path();
    assert_eq!(
        std::fs::read(child.join("Welcome.txt")).expect("fixture Welcome.txt"),
        b"Shell capture fixture\n"
    );
}

fn assert_populated_control_targets(fixture: &PopulatedShellFixture) {
    for target in ["topbar_tray_pill", "topbar_overview_button"] {
        assert!(
            fixture
                .widget_registry()
                .get(target)
                .is_some_and(|rect| rect.area() > 0.0),
            "{target} remains an actual painted control"
        );
    }
}

#[test]
fn retained_damage_policy_bounds_changes_and_keeps_semantic_inventory() {
    fn explicit_populated_fixture_preserves_semantic_inventory_and_cpu_actions() {
        let guard = DenyAmbientStarts::new();
        let mut fixture = PopulatedShellFixture::new((NATIVE_WIDTH, NATIVE_HEIGHT), Theme::dark());
        let initial_scene = fixture.paint();
        assert!(!initial_scene.encoding().is_empty());
        assert_populated_tree_inventory(&fixture);
        assert_owned_files_fixture(&fixture);
        assert_populated_control_targets(&fixture);

        let mut document = AutoCommit::new();
        let mut focus = None;
        let open_qs = dispatch_populated_fixture_click(
            &mut fixture,
            "topbar_tray_pill",
            &mut document,
            &mut focus,
        );
        assert!(!open_qs.encoding().is_empty());
        assert!(fixture.chrome().overlays.quick_settings);
        let close_qs = dispatch_populated_fixture_click(
            &mut fixture,
            "topbar_tray_pill",
            &mut document,
            &mut focus,
        );
        assert!(!close_qs.encoding().is_empty());
        assert!(!fixture.chrome().overlays.quick_settings);
        let open_overview = dispatch_populated_fixture_click(
            &mut fixture,
            "topbar_overview_button",
            &mut document,
            &mut focus,
        );
        assert!(!open_overview.encoding().is_empty());
        assert!(fixture.chrome().overlays.overview);
        assert_eq!(guard.attempts(), 0);
    }

    fn damage_policy_allows_only_a_bounded_quick_settings_change() {
        let mut frame = PopulatedShellFixture::new((1_600, 900), Theme::dark());
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

    fn damage_policy_rejects_an_opaque_section_change_after_a_proven_baseline() {
        let mut frame = PopulatedShellFixture::new((1_600, 900), Theme::dark());
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

    explicit_populated_fixture_preserves_semantic_inventory_and_cpu_actions();
    damage_policy_allows_only_a_bounded_quick_settings_change();
    damage_policy_rejects_an_opaque_section_change_after_a_proven_baseline();
}

fn rect_scene() -> Scene {
    let mut scene = Scene::new();
    // Use the product accent token so the fixture remains within the same
    // palette contract as the scenes whose retained composition it probes.
    let fixture_color = Theme::dark().tokens.colors.intent.inverted;
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
