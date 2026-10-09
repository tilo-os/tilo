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

/// A settled fixture, and the guard pinning the bar's clock while it lives: two paints
/// compared pixel for pixel must not straddle a minute.
fn settled_builder() -> (PopulatedShellFixture, tilo_test_env::ScopedEnv) {
    settled_builder_in(Theme::dark())
}

/// [`settled_builder`] dressed in `theme`.
fn settled_builder_in(theme: Theme) -> (PopulatedShellFixture, tilo_test_env::ScopedEnv) {
    let mut clock = tilo_test_env::ScopedEnv::new();
    clock.set("TILO_CLOCK", "09:41");
    // The test needs state transitions, not a time-dependent animation sample. The explicit
    // fixture receives reduced motion before its initial layout; benchmark inputs stay normal.
    let mut frame = PopulatedShellFixture::new(
        (NATIVE_WIDTH, NATIVE_HEIGHT),
        theme.with_reduce_motion(true),
    );
    frame.relayout();
    let _ = frame.paint();
    frame.relayout();
    // Pin Notes' real status presentation with the pointer. Reduced motion
    // does not extend its transient lifetime, so renderer speed must not
    // remove this unrelated app layer during the retained QS comparison.
    let mut document = automerge::AutoCommit::new();
    let mut focus = None;
    let _ = frame.dispatch_event(
        &winit::event::WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(
                f64::from(NATIVE_WIDTH) - 50.0,
                f64::from(NATIVE_HEIGHT) - 50.0,
            ),
        },
        &winit::event::Modifiers::default(),
        &mut ui::widget::Edits::lend(&mut document),
        &automerge::ROOT,
        &mut focus,
    );
    let warm = frame.paint();
    assert!(
        !warm.encoding().is_empty(),
        "the desktop warm paint must record source commands"
    );
    (frame, clock)
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
    let (mut frame, _clock) = settled_builder();

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

fn sparse_refusal_details(previous: Option<&ProducedSections>, frame: &FrameBuilder) -> String {
    let current = frame.last_produced_sections.as_ref();
    match (previous, current) {
        (Some(old), Some(new)) => format!(
            "filters={}/{} backdrop_same={} workspace_same={} above_same={} topbar_bounds={:?}/{:?} quick_settings_bounds={:?}/{:?}",
            old.global_filter,
            new.global_filter,
            old.backdrop.encoding_eq(&new.backdrop),
            old.workspace.encoding_eq(&new.workspace),
            old.above_quick_settings
                .encoding_eq(&new.above_quick_settings),
            old.topbar_bounds,
            new.topbar_bounds,
            old.quick_settings_bounds,
            new.quick_settings_bounds,
        ),
        _ => "missing source sections".into(),
    }
}

#[test]
fn native_4k_retained_quick_settings_damage_covers_large_shadow_and_matches_dense() {
    let (mut frame, _clock) = settled_builder();
    frame.theme.tokens.elevation.floating.blur = 120.0;
    frame.theme.tokens.elevation.floating.offset_y = 40.0;

    // Preserve an independently rendered dense equivalence proof for the settled, enlarged-shadow
    // desktop before using it as the retained base.
    assert_legacy_and_segmented_match(&mut frame, "large-shadow desktop", false);

    let closed = frame.paint_with_damage();
    let closed_sections = frame.last_produced_sections.clone();
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
    let open_damage = open.damage_regions.as_deref().unwrap_or_else(|| {
        panic!(
            "settled Quick Settings opening should be sparse-eligible: {}",
            sparse_refusal_details(closed_sections.as_ref(), &frame)
        )
    });
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
    let (mut frame, _clock) = settled_builder();
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
    frame.set_workspace_clip_supported(true);
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
    // Hot-unplugging to a single scanout changes presenter capability while
    // the same transition is in flight. Its next frame must be complete.
    frame.set_workspace_clip_supported(false);
    let single = frame.paint_with_damage();
    assert!(!single.workspace_hold && single.workspace_clip.is_none());
    assert!(
        !single.scene.encoding().is_empty(),
        "single-output presenter needs actual scene pixels"
    );
    frame.set_workspace_clip_supported(true);
    let multiple = frame.paint_with_damage();
    assert!(
        multiple.workspace_hold && multiple.scene.encoding().is_empty(),
        "restored multi-output replay capability keeps its clip optimisation"
    );
}

fn assert_populated_tree_inventory(fixture: &PopulatedShellFixture) {
    assert_eq!(fixture.viewport(), (3840.0, 2160.0));
    assert_eq!(
        fixture.host().fixture_provider_ids(),
        vec!["actions", "records"]
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
    let entries = std::fs::read_dir(fixture.parent.path())
        .expect("fixture parent remains owned")
        .collect::<Result<Vec<_>, _>>()
        .expect("fixture child entries");
    // Beside the Files folder, the profile the fixture's apps keep their
    // records in.
    let (profiles, mut fixture_children): (Vec<_>, Vec<_>) =
        entries.into_iter().partition(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("tilo-shell-fixture-apps-")
        });
    assert_eq!(profiles.len(), 1, "one owned apps profile");
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
        frame.last_produced_sections = Some(previous_sections(
            frame.tint(),
            false,
            Scene::new(),
            Scene::new(),
        ));
        let candidate = sections(rect_scene(), Scene::new());
        assert!(
            frame.damage_from_sections(&candidate, false).is_some(),
            "matching opaque sections and bounded Quick Settings change must establish a sparse baseline"
        );

        frame.last_produced_sections = Some(previous_sections(
            frame.tint(),
            true,
            Scene::new(),
            Scene::new(),
        ));
        let candidate = sections(rect_scene(), Scene::new());
        assert!(
            frame.damage_from_sections(&candidate, false).is_none(),
            "a prior whole-frame filter is an explicit retained boundary"
        );
    }

    fn damage_policy_rejects_an_opaque_section_change_after_a_proven_baseline() {
        let mut frame = PopulatedShellFixture::new((1_600, 900), Theme::dark());
        frame.last_produced_sections = Some(previous_sections(
            frame.tint(),
            false,
            Scene::new(),
            Scene::new(),
        ));
        assert!(
            frame
                .damage_from_sections(&sections(rect_scene(), Scene::new()), false)
                .is_some(),
            "the baseline must prove the Quick Settings candidate itself is otherwise eligible"
        );

        frame.last_produced_sections = Some(previous_sections(
            frame.tint(),
            false,
            Scene::new(),
            Scene::new(),
        ));
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
        workspace_overlays: Scene::new(),
        topbar_bounds: None,
        quick_settings,
        above_quick_settings: Scene::new(),
        quick_settings_bounds: Some(Rect::new(10.0, 20.0, 100.0, 120.0)),
    }
}

fn previous_sections(
    tint: Tint,
    global_filter: bool,
    quick_settings: Scene,
    workspace: Scene,
) -> ProducedSections {
    ProducedSections {
        parts: OpaqueParts::default(),
        tint,
        foreign_images: Vec::new(),
        backdrop: Scene::new(),
        topbar: Scene::new(),
        workspace,
        workspace_overlays: Scene::new(),
        strips: None,
        topbar_bounds: None,
        quick_settings,
        above_quick_settings: Scene::new(),
        quick_settings_bounds: Some(Rect::new(10.0, 20.0, 100.0, 120.0)),
        global_filter,
        revision: 1,
    }
}

/// The demo window, holding the demo's apps plus Editor, with
/// `active` shown, repeated `windows` times along the strip.
fn demo_windows(active: &str, windows: usize) -> crate::shell::state::WorkspaceTree {
    use crate::shell::state::{AppKind, TileContent};
    let mut tree = crate::shell::state::WorkspaceTree::bootstrap_demo();
    if let Some(strip) = tree
        .desktop
        .active_workspace_mut()
        .and_then(|workspace| workspace.strips.first_mut())
        && let Some(mut group) = strip.window_groups.first().cloned()
    {
        if let Some(mut tab) = group.tabs.first().cloned() {
            tab.id = crate::shell::state::next_id();
            tab.title = "Editor".into();
            tab.tile = TileContent::AppPlaceholder {
                kind: AppKind::Other("Editor".into()),
                label: "Editor".into(),
            };
            group.tabs.push(tab);
        }
        strip.window_groups.clear();
        for _ in 0..windows {
            let mut copy = group.clone();
            copy.id = crate::shell::state::next_id();
            for tab in &mut copy.tabs {
                tab.id = crate::shell::state::next_id();
            }
            copy.active_tab = copy
                .tabs
                .iter()
                .find(|tab| tab.title == active)
                .map_or(0, |tab| tab.id);
            strip.window_groups.push(copy);
        }
    }
    tree.seed_doc_from_cache();
    tree
}

#[test]
fn a_settled_desktop_does_not_rebuild_its_layout_whichever_tab_is_shown() {
    // Each app shown in turn, with the others as tabs behind it: a tab that is
    // never built (Files behind Notes, say) must not rebuild the desktop's
    // layout tree on every frame, nor may the app that is shown.
    for active in ["Notes", "Files", "Terminal", "Document", "Editor"] {
        let mut frame = PopulatedShellFixture::with_tree(
            (1280, 720),
            Theme::dark().with_reduce_motion(true),
            demo_windows(active, 3),
        );
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let mut iterate = |frame: &mut PopulatedShellFixture| {
            frame.pump_apps(&mut doc, &root);
            frame.relayout();
            let _ = frame.paint_with_damage();
        };
        for _ in 0..8 {
            iterate(&mut frame);
        }
        for step in 0..4 {
            frame
                .host_mut()
                .set_strip_scroll_for_test(f64::from(step) * 40.0);
            frame.pump_apps(&mut doc, &root);
            assert!(
                !ui::widget::Widget::take_structure_dirty(frame.host_mut()),
                "{active} shown, frame {step}: the settled desktop asked for a new layout tree"
            );
            frame.relayout();
            let _ = frame.paint_with_damage();
        }
    }
}

#[test]
fn foreign_pixel_replacement_uses_bounded_damage_and_matches_dense_gpu_pixels() {
    foreign_replacement_pixel_oracle(false, ui::widget::WindowWidth::Narrow, true);
}

#[test]
fn imported_foreign_pixel_replacement_uses_bounded_damage_and_matches_dense_gpu_pixels() {
    foreign_replacement_pixel_oracle(true, ui::widget::WindowWidth::Narrow, true);
}

#[test]
fn wide_foreign_pixel_replacement_keeps_dense_admission_and_matches_gpu_pixels() {
    foreign_replacement_pixel_oracle(false, ui::widget::WindowWidth::Wide, false);
}

fn foreign_replacement_pixel_oracle(
    imported: bool,
    window_width: ui::widget::WindowWidth,
    expect_sparse: bool,
) {
    let _ambient = DenyAmbientStarts::new();
    let mut clock = tilo_test_env::ScopedEnv::new();
    clock.set("TILO_CLOCK", "09:41");
    let surface = if imported { 8_493_104 } else { 8_493_103 };
    let (mut fixture, size) = foreign_damage_fixture(window_width, surface);
    let store = crate::shell::foreign::store();
    let mut retained = bridge();
    let mut oracle = bridge();
    publish_foreign_fixture_frame(
        surface,
        size,
        [255, 0, 0, 255],
        imported.then_some(&retained),
    );
    let first = fixture.paint_with_damage();
    assert_eq!(
        fixture
            .last_produced_sections
            .as_ref()
            .expect("sections")
            .foreign_images
            .len(),
        1,
        "the actual Foreign widget must record its drawn image"
    );
    paint_retained_and_compare(&mut retained, &mut oracle, &first, "first foreign frame");
    let mounted = fixture
        .foreign_surface_rect(surface)
        .expect("mounted foreign rect");
    let pixel = fixture
        .layout_rect_to_scene(mounted)
        .expect("real scene placement")
        .center();
    assert_foreign_pixel(&mut oracle, &first, pixel, [0, 0, 255]);
    let previous_sections = fixture.last_produced_sections.as_ref().unwrap().clone();
    publish_foreign_fixture_frame(
        surface,
        size,
        [0, 255, 0, 255],
        imported.then_some(&retained),
    );
    let changed = fixture.paint_with_damage();
    let current_sections = fixture.last_produced_sections.as_ref().unwrap();
    assert_foreign_metadata_is_strict(&previous_sections, current_sections, &changed);
    let damage = changed
        .damage_regions
        .as_ref()
        .expect("foreign-only update has bounded damage");
    assert!(!damage.is_empty(), "replacement pixels must be repainted");
    assert!(
        damage
            .iter()
            .all(|rect| rect.width < NATIVE_WIDTH || rect.height < NATIVE_HEIGHT),
        "an app buffer update must not invalidate the entire output"
    );
    let dirty_pixels: u64 = damage
        .iter()
        .map(|rect| u64::from(rect.width) * u64::from(rect.height))
        .sum();
    assert_eq!(
        dirty_pixels * 2 < u64::from(NATIVE_WIDTH) * u64::from(NATIVE_HEIGHT),
        expect_sparse,
        "the ordinary width must exercise the unchanged sparse admission boundary"
    );
    paint_retained_and_compare(
        &mut retained,
        &mut oracle,
        &changed,
        "replacement foreign pixels",
    );
    assert_foreign_pixel(&mut oracle, &changed, pixel, [0, 255, 0]);
    if expect_sparse {
        assert!(
            retained.last_sparse_render_attempts() >= 1,
            "replacement pixels must exercise the production sparse render path"
        );
    } else {
        assert_eq!(
            retained.last_sparse_render_attempts(),
            0,
            "a wide whole-buffer replacement must retain conservative dense admission"
        );
    }
    let _settings = foreign_filter_settings(&fixture);
    clock.set("TILO_SHOTS_THEME", "dark");
    fixture.night_light = true;
    fixture.watch_configured_theme = true;
    // What `enable_configured_theme` reads from that variable once, when
    // watching starts; the settings file stays unwatched.
    fixture.shots_theme_pinned = true;
    assert!(
        fixture.paint_with_damage().damage_regions.is_none(),
        "global filters require dense repair"
    );
    store.forget(surface);
}

fn foreign_damage_fixture(
    window_width: ui::widget::WindowWidth,
    surface: u64,
) -> (PopulatedShellFixture, (u32, u32)) {
    let mut tree = crate::shell::state::WorkspaceTree::bootstrap_demo();
    let workspace = tree
        .desktop
        .active_workspace_mut()
        .expect("active workspace");
    workspace.strips.truncate(1);
    let strip = &mut workspace.strips[0];
    strip.window_groups.truncate(1);
    let group = &mut strip.window_groups[0];
    group.tabs.truncate(1);
    group.active_tab = group.tabs[0].id;
    let tab = group.active_tab;
    group.width = window_width;
    tree.seed_doc_from_cache();
    let mut fixture = PopulatedShellFixture::with_tree(
        (NATIVE_WIDTH, NATIVE_HEIGHT),
        Theme::dark().with_reduce_motion(true),
        tree,
    );
    let store = crate::shell::foreign::store();
    store.note_adopted(surface);
    fixture.with_chrome(|chrome| {
        {
            let mut tree = chrome.tree_mut();
            crate::shell::tab_binding::adopt_toplevel_at(
                &mut tree,
                surface,
                Some("so.tilo.notes".into()),
                crate::shell::tab_binding::Placement::IntoTab(tab),
            );
        }
        chrome.bump();
    });
    fixture.relayout();
    let rect = fixture
        .foreign_surface_rect(surface)
        .expect("normal adoption mounts the real Foreign widget");
    // Answer the actual configure size. The ordinary opening guard deliberately
    // withholds a tiny initial buffer that does not fill the newly laid-out tab.
    let width = rect.width().round() as u32;
    let height = rect.height().round() as u32;
    (fixture, (width, height))
}

fn publish_foreign_fixture_frame(
    surface: u64,
    size: (u32, u32),
    texel: [u8; 4],
    importing_bridge: Option<&VelloDrmBridge>,
) {
    let store = crate::shell::foreign::store();
    if let Some(bridge) = importing_bridge {
        let (device, queue) = bridge.gpu();
        let frame = crate::shell::gpu_frames::frame_for_test(
            device,
            queue,
            bridge
                .present_generation_for_test()
                .expect("presenting device"),
            size,
            texel,
            surface,
        );
        assert!(matches!(
            store.publish_gpu_for_test(surface, frame),
            Some(crate::shell::foreign::CaptureOutcome::Dmabuf { .. })
        ));
        assert!(
            store
                .frame_image(surface)
                .expect("activated GPU pixels")
                .0
                .data
                .is_empty(),
            "the real owned GPU frame must carry no CPU pixel blob"
        );
    } else {
        store.publish_rgba(
            surface,
            &texel.repeat(size.0 as usize * size.1 as usize),
            size.0,
            size.1,
        );
    }
}

fn assert_foreign_metadata_is_strict(
    previous_sections: &ProducedSections,
    current_sections: &ProducedSections,
    changed: &FramePaint,
) {
    if changed.damage_regions.is_none() {
        for (name, old, new) in [
            (
                "backdrop",
                &previous_sections.backdrop,
                &current_sections.backdrop,
            ),
            (
                "workspace",
                &previous_sections.workspace,
                &current_sections.workspace,
            ),
            (
                "topbar",
                &previous_sections.topbar,
                &current_sections.topbar,
            ),
            (
                "quick_settings",
                &previous_sections.quick_settings,
                &current_sections.quick_settings,
            ),
            (
                "above",
                &previous_sections.above_quick_settings,
                &current_sections.above_quick_settings,
            ),
        ] {
            eprintln!(
                "foreign damage comparison {name}: strict={} normalized={:?}",
                old.encoding_eq(new),
                super::super::foreign_damage::image_only_damage(
                    old,
                    new,
                    &previous_sections.foreign_images,
                    &current_sections.foreign_images
                )
            );
        }
    }
    let mut changed_metadata = current_sections.workspace.clone();
    let image = changed_metadata
        .encoding_mut()
        .resources
        .patches
        .iter_mut()
        .find_map(|patch| match patch {
            vello_encoding::Patch::Image { image, .. } => Some(image),
            _ => None,
        })
        .expect("actual Foreign widget encoded its image");
    image.width = image.width.saturating_add(1);
    assert!(
        super::super::foreign_damage::image_only_damage(
            &previous_sections.workspace,
            &changed_metadata,
            &previous_sections.foreign_images,
            &current_sections.foreign_images
        )
        .is_none(),
        "normalization must not hide encoded image metadata changes"
    );
}

fn assert_foreign_pixel(
    bridge: &mut VelloDrmBridge,
    frame: &FramePaint,
    point: Point,
    xrgb: [u8; 3],
) {
    let pixels = render_dense(bridge, &frame.scene);
    let at = (point.y as usize * NATIVE_WIDTH as usize + point.x as usize) * 4;
    assert_eq!(&pixels[at..at + 3], &xrgb, "actual foreign interior pixels");
}

struct ForeignFilterSettings(Option<std::sync::Arc<dyn settings_store::SettingsSource>>);
impl Drop for ForeignFilterSettings {
    fn drop(&mut self) {
        settings_store::install_source(self.0.take());
    }
}

fn foreign_filter_settings(frame: &FrameBuilder) -> ForeignFilterSettings {
    let previous = settings_store::installed_source();
    let temperatures: serde_json::Map<_, _> = frame
        .chrome()
        .tree()
        .desktop
        .workspaces
        .iter()
        .map(|workspace| (workspace.id.0.to_string(), serde_json::json!(0.0)))
        .collect();
    let settings = serde_json::json!({"appearance":{"workspace_temperatures":temperatures}})
        .as_object()
        .expect("fixture settings object")
        .clone();
    // Supply the actual host settings source, so the live theme/filter path
    // never falls back to native wallpaper services in this owned fixture.
    let source = settings_store::HostDocument::new(settings, |_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "read-only fixture settings",
        ))
    });
    settings_store::install_source(Some(std::sync::Arc::new(source)));
    ForeignFilterSettings(previous)
}

#[test]
fn wide_foreign_commit_damage_tracks_exact_buffers_and_matches_dense_gpu_pixels() {
    for (imported, density) in [(false, 1), (false, 2), (true, 1), (true, 2)] {
        let _ambient = DenyAmbientStarts::new();
        let mut clock = tilo_test_env::ScopedEnv::new();
        clock.set("TILO_CLOCK", "09:41");
        let surface = 8_493_206 + u64::from(imported) * 2 + u64::from(density == 2);
        let (mut fixture, logical_size) =
            foreign_damage_fixture(ui::widget::WindowWidth::Wide, surface);
        // Native HiDPI clients render two buffer pixels per logical pixel.
        // The oracle must prove real filtered GPU output at both densities.
        let size = (logical_size.0 * density, logical_size.1 * density);
        let mut retained = bridge();
        let mut oracle = bridge();
        publish_foreign_fixture_frame(
            surface,
            size,
            [255, 0, 0, 255],
            imported.then_some(&retained),
        );
        let first = fixture.paint_with_damage();
        paint_retained_and_compare(&mut retained, &mut oracle, &first, "wide original buffer");
        let logical_rect = fixture.foreign_surface_rect(surface).unwrap();
        let placement = fixture.layout_rect_to_scene(logical_rect).unwrap();
        assert!(
            placement.area() * 2.0 >= f64::from(NATIVE_WIDTH) * f64::from(NATIVE_HEIGHT),
            "this fixture must exercise a genuinely wide tile, not the narrow performance case"
        );
        let mut pixels = [255, 0, 0, 255].repeat(size.0.saturating_mul(size.1) as usize);
        let first_dirty = Rect::new(128.0, 128.0, 160.0, 160.0);
        let second_dirty = Rect::new(256.0, 256.0, 288.0, 288.0);
        // Two captured buffers arrive before the compositor paints. Both
        // edits must survive the exact old->new cached sequence join.
        for dirty in [first_dirty, second_dirty] {
            for y in dirty.y0 as usize..dirty.y1 as usize {
                for x in dirty.x0 as usize..dirty.x1 as usize {
                    let index = (y * size.0 as usize + x) * 4;
                    pixels[index..index + 4].copy_from_slice(&[0, 255, 0, 255]);
                }
            }
            let store = crate::shell::foreign::store();
            if imported {
                let (device, queue) = retained.gpu();
                let frame = crate::shell::gpu_frames::frame_pixels_for_test(
                    device,
                    queue,
                    retained.present_generation_for_test().unwrap(),
                    size,
                    &pixels,
                    surface,
                );
                assert!(
                    store
                        .publish_damaged_gpu_for_test(surface, frame, Some(vec![dirty]))
                        .is_some()
                );
            } else {
                store.publish_damaged_rgba_for_test(surface, &pixels, size, vec![dirty]);
            }
        }
        let changed = fixture.paint_with_damage();
        let damage = changed
            .damage_regions
            .as_ref()
            .expect("exact buffer damage must stay bounded");
        let dirty_pixels: u64 = damage
            .iter()
            .map(|r| u64::from(r.width) * u64::from(r.height))
            .sum();
        assert!(
            dirty_pixels * 2 < u64::from(NATIVE_WIDTH) * u64::from(NATIVE_HEIGHT),
            "a wide tile's two small edits must exercise sparse admission"
        );
        paint_retained_and_compare(
            &mut retained,
            &mut oracle,
            &changed,
            "wide coalesced dirty buffers",
        );
        assert!(retained.last_sparse_render_attempts() >= 1);
        for dirty in [first_dirty, second_dirty] {
            let p = dirty.center();
            let output = Point::new(
                placement.x0 + p.x * placement.width() / f64::from(size.0),
                placement.y0 + p.y * placement.height() / f64::from(size.1),
            );
            assert_foreign_pixel(&mut oracle, &changed, output, [0, 255, 0]);
        }
        assert_foreign_pixel(&mut oracle, &changed, placement.center(), [0, 0, 255]);
        crate::shell::foreign::store().forget(surface);
    }
}

#[cfg(target_os = "linux")]
fn render_cpu(scene: &Scene, (width, height): (u32, u32)) -> Vec<u8> {
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    let mut target =
        tilo_cpu_raster::Target::new(&mut pixels, width, height, width as usize * 4).unwrap();
    tilo_cpu_raster::CpuRasterizer::new(tilo_cpu_raster::Options::default()).render(
        scene.encoding(),
        &mut target,
        None,
        vello::peniko::Color::BLACK,
    );
    pixels
}

#[cfg(target_os = "linux")]
fn assert_layer_cpu_pixels_match(composed: &Scene, dense: &Scene, size: (u32, u32)) {
    let composed = render_cpu(composed, size);
    let dense = render_cpu(dense, size);
    let differing = composed
        .chunks_exact(4)
        .zip(dense.chunks_exact(4))
        .filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 1))
        .count();
    let difference_bounds = composed
        .chunks_exact(4)
        .zip(dense.chunks_exact(4))
        .enumerate()
        .filter(|(_, (a, b))| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 1))
        .fold(
            None::<(usize, usize, usize, usize)>,
            |bounds, (index, _)| {
                let x = index % size.0 as usize;
                let y = index / size.0 as usize;
                Some(match bounds {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                })
            },
        );
    assert!(
        differing == 0,
        "{differing} pixels differ between the composed and the dense scene at {difference_bounds:?}"
    );
}

#[cfg(target_os = "linux")]
fn wide_retained_fixture(size: (u32, u32)) -> PopulatedShellFixture {
    // The demo's window, twice more beside it: wider than the screen.
    let mut tree = crate::shell::state::WorkspaceTree::bootstrap_demo();
    if let Some(strip) = tree
        .desktop
        .active_workspace_mut()
        .and_then(|workspace| workspace.strips.first_mut())
        && let Some(group) = strip.window_groups.first().cloned()
    {
        for _ in 0..2 {
            let mut copy = group.clone();
            copy.id = crate::shell::state::next_id();
            for tab in &mut copy.tabs {
                tab.id = crate::shell::state::next_id();
            }
            copy.active_tab = copy.tabs.first().map_or(0, |tab| tab.id);
            strip.window_groups.push(copy);
        }
    }
    tree.seed_doc_from_cache();
    PopulatedShellFixture::with_tree(size, Theme::dark().with_reduce_motion(true), tree)
}

#[cfg(target_os = "linux")]
#[test]
fn dense_capture_does_not_inherit_another_producers_canvas_policy() {
    const SIZE: (u32, u32) = (1280, 720);
    let mut clock = tilo_test_env::ScopedEnv::new();
    let _ambient = DenyAmbientStarts::new();
    clock.set("TILO_CLOCK", "09:41");
    let mut frame = wide_retained_fixture(SIZE);
    frame.relayout();
    let _ = frame.paint();
    frame.host_mut().set_strip_scroll_for_test(120.0);
    frame.relayout();
    let _ = frame.paint();
    // Compare the same production content on both draw-policy paths. A real
    // startup save acknowledgment must not change Notes' status between them.
    acknowledge_notes_saves(&mut frame);
    assert_notes_startup_saved(&mut frame);
    let dense = frame.paint();
    let outer = ui::retained_present::RetainedScene::begin(true);
    let nested = frame.paint();
    assert!(
        ui::retained_present::unbake_scroll(),
        "the enclosing presenter keeps its draw policy"
    );
    drop(outer);
    assert_notes_startup_saved(&mut frame);
    assert_layer_cpu_pixels_match(&nested, &dense, SIZE);
}

#[cfg(target_os = "linux")]
fn acknowledge_notes_saves(frame: &mut PopulatedShellFixture) {
    let (apps, room) = {
        let chrome = frame.chrome();
        let room = chrome
            .tree()
            .desktop
            .active_workspace()
            .expect("fixture workspace")
            .room
            .to_string();
        let apps = chrome
            .hosted_inputs()
            .in_process()
            .expect("fixture app owner")
            .clone();
        (apps, room)
    };
    assert_eq!(
        apps.borrow_mut()
            .flush_fixture_records(notes::definition().app_id, &room)
            .expect("actual Notes startup writes acknowledged"),
        3
    );
    // Durability is sampled after sync; the next sync presents that sample.
    assert_eq!(frame.wake_mounted_components(), 3);
    let _ = frame.paint_with_damage();
    assert_eq!(frame.wake_mounted_components(), 3);
    let _ = frame.paint_with_damage();
}

#[cfg(target_os = "linux")]
fn assert_notes_startup_saved(frame: &mut PopulatedShellFixture) {
    let saved = std::cell::RefCell::new(Vec::new());
    let _ = frame.mounted_mut::<()>(|widget| {
        if let Some(notes) = tilo_app::framework::testing::surface_in_root::<
            notes::NotesShell,
            notes::NotesApp,
        >(widget)
        {
            saved
                .borrow_mut()
                .push(notes.status_bar_for_test().state().save_status);
        }
        None
    });
    assert_eq!(
        *saved.borrow(),
        vec![notes::status_bar::SaveStatus::Saved; 3],
        "all three real Notes surfaces present their acknowledged startup state"
    );
}

#[cfg(target_os = "linux")]
fn acknowledged_notes_canvas(frame: &mut PopulatedShellFixture) -> FramePaint {
    let mut at_rest = frame.paint_with_damage();
    for _ in 0..8 {
        frame.relayout();
        let next = frame.paint_with_damage();
        let settled = match (&next.cache, &at_rest.cache) {
            (Some(next), Some(previous)) => next
                .layers
                .workspace
                .encoding_eq(&previous.layers.workspace),
            _ => false,
        };
        at_rest = next;
        if settled {
            break;
        }
    }
    // Startup creates real records. A writer acknowledgment between the two
    // encodings would change Saving to Saved and invalidate the pixel oracle.
    acknowledge_notes_saves(frame);
    at_rest = frame.paint_with_damage();
    assert_notes_startup_saved(frame);
    at_rest
}

#[cfg(target_os = "linux")]
#[test]
fn a_cpu_presenter_rounds_strip_layout_and_retains_the_same_screen() {
    const SIZE: (u32, u32) = (1280, 720);
    let mut clock = tilo_test_env::ScopedEnv::new();
    let _ambient = DenyAmbientStarts::new();
    clock.set("TILO_CLOCK", "09:41");
    let mut frame = wide_retained_fixture(SIZE);
    frame.host_mut().set_pixel_aligned_strip_pan(true);
    frame.set_retained_layer_width(Some(8192));
    let rest = acknowledged_notes_canvas(&mut frame);
    let offsets = rest
        .cache
        .as_ref()
        .expect("CPU canvas")
        .strips
        .iter()
        .map(|strip| strip.scroll_x)
        .collect::<Vec<_>>();
    assert_eq!(offsets.len(), 1);
    frame.host_mut().set_strip_scroll_for_test(120.5);
    frame.relayout();
    assert_eq!(
        frame.host().strip_scroll_offsets()[0].1,
        121.0,
        "the presenter receives the same pan as the laid-out input geometry"
    );
    let panned = frame.paint_with_damage();
    assert_eq!(
        panned
            .cache
            .as_ref()
            .expect("CPU canvas")
            .strips
            .iter()
            .map(|strip| strip.scroll_x)
            .collect::<Vec<_>>(),
        offsets
            .iter()
            .map(|offset| offset + 121)
            .collect::<Vec<_>>()
    );
    frame.set_retained_layer_width(None);
    acknowledge_notes_saves(&mut frame);
    let dense = frame.paint();
    assert_layer_cpu_pixels_match(&panned.scene, &dense, SIZE);
    // A policy transition restores fractional layout rather than rounding state.
    frame.host_mut().set_pixel_aligned_strip_pan(false);
    frame.relayout();
    assert_eq!(frame.host().strip_scroll_offsets()[0].1, 120.5);
}

#[test]
fn a_presenter_compositing_layers_gets_strips_in_canvas_coordinates_and_the_same_screen() {
    const SIZE: (u32, u32) = (1280, 720);
    let mut clock = tilo_test_env::ScopedEnv::new();
    let _ambient = DenyAmbientStarts::new();
    clock.set("TILO_CLOCK", "09:41");
    let mut frame = wide_retained_fixture(SIZE);
    frame.relayout();
    let _ = frame.paint();
    assert!(
        frame.paint_with_damage().cache.is_none(),
        "no layers unless a presenter composites them"
    );

    frame.set_retained_layer_width(Some(8192));
    // Settle: the first frames lay the tab bars out at their final widths.
    let at_rest = acknowledged_notes_canvas(&mut frame);
    let rest = at_rest.cache.as_ref().expect("the frame as layers");
    let offsets = rest
        .strips
        .iter()
        .map(|strip| strip.scroll_x)
        .collect::<Vec<_>>();
    assert_eq!(offsets.len(), 1, "the fixture has one retained strip");
    assert!(
        offsets[0] > 0,
        "the canvas includes left rubber-band padding"
    );
    assert!(
        rest.width > SIZE.0,
        "the fixture's strip is wider than the screen: {}",
        rest.width
    );

    frame.host_mut().set_strip_scroll_for_test(120.0);
    frame.relayout();
    let panned = frame.paint_with_damage();
    let pan = panned.cache.as_ref().expect("the frame as layers");
    assert_eq!(
        pan.strips
            .iter()
            .map(|strip| strip.scroll_x)
            .collect::<Vec<_>>(),
        offsets
            .iter()
            .map(|offset| offset + 120)
            .collect::<Vec<_>>()
    );
    assert!(
        pan.layers.backdrop.encoding_eq(&rest.layers.backdrop),
        "a pan leaves the backdrop"
    );
    assert!(
        pan.layers.chrome.encoding_eq(&rest.layers.chrome),
        "a pan leaves the chrome"
    );
    assert!(
        pan.layers.workspace.encoding_eq(&rest.layers.workspace),
        "a pan leaves the workspace in canvas coordinates"
    );
    assert!(
        panned.damage_regions.is_none(),
        "the scene moved where the layers did not"
    );
    assert!(
        frame.paint_with_damage().damage_regions.is_none(),
        "unchanged canvas offsets do not grant screen-space damage authority"
    );

    // The scene shows the strip where it is on screen, as a scene drawn
    // without layers does.
    frame.set_retained_layer_width(None);
    // Encode both paths before rasterizing either: app status bubbles use
    // monotonic linger deadlines, and CPU rendering may cross a deadline.
    // Exercise the same late acknowledgment boundary that reproduced the
    // historical 1,034-pixel failure; startup is already durable on both paths.
    acknowledge_notes_saves(&mut frame);
    let dense_scene = frame.paint();
    assert_layer_cpu_pixels_match(&panned.scene, &dense_scene, SIZE);

    frame.set_retained_layer_width(Some(8192));
    let mut independent = PopulatedShellFixture::new(SIZE, Theme::dark().with_reduce_motion(true));
    independent.relayout();
    assert!(
        independent.paint_with_damage().cache.is_none(),
        "another producer has no live capability"
    );

    frame.host_mut().set_strip_scroll_for_test(120.5);
    frame.relayout();
    let fractional = frame.paint_with_damage();
    assert!(
        fractional
            .cache
            .as_ref()
            .expect("baked layers remain eligible")
            .strips
            .is_empty()
    );
    let dense_scene = frame.paint();
    assert_layer_cpu_pixels_match(&fractional.scene, &dense_scene, SIZE);

    let mut external = fractional;
    external.force_full();
    assert!(
        external.cache.is_none(),
        "external scene changes invalidate layer authority"
    );
    frame.set_retained_layer_width(Some(64));
    assert!(
        frame.paint_with_damage().cache.is_none(),
        "oversized outputs retain the dense path"
    );
}

/// Prove `damage` repaints some, but under 1%, of the 4K output, and log
/// the pixels it rasterises: the area of its rects, overlap counted twice,
/// which only overstates the cost. A dense frame rasterises every pixel.
fn assert_repaints_under_one_percent(damage: &[FineDamageRect], label: &str) {
    let output = u64::from(NATIVE_WIDTH) * u64::from(NATIVE_HEIGHT);
    let pixels: u64 = damage
        .iter()
        .map(|rect| u64::from(rect.width) * u64::from(rect.height))
        .sum();
    let share = |value: u64| f64::from(u32::try_from(value).unwrap_or(u32::MAX));
    eprintln!(
        "TIL-579 raster pixels ({label}): {pixels} of {output} ({:.3}%); a dense frame is {output}",
        share(pixels) * 100.0 / share(output)
    );
    assert!(
        pixels > 0 && pixels * 100 < output,
        "{label}: must repaint under 1% of a 4K output, not {pixels} px: {damage:?}"
    );
}

/// Publish one peer, Ada, pointing at `at` in the active workspace, where
/// the shell paints workspace peers from.
fn point_peer_at(frame: &mut FrameBuilder, at: (f64, f64)) {
    let room = {
        let chrome = frame.chrome();
        let tree = chrome.tree();
        tree.desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == tree.desktop.active)
            .map(|workspace| workspace.room.to_string())
            .expect("the fixture has an active workspace")
    };
    frame.presence.pointers.insert(
        ui::presence::PresenceKey::Workspace { id: room },
        vec![(ui::permissions::UserId("ada".into()), at)],
    );
}

/// The pointer's dot in native pixels.
fn peer_dot(frame: &FrameBuilder, at: (f64, f64)) -> Rect {
    frame
        .layout_rect_to_scene(Rect::from_center_size(at, (24.0, 24.0)))
        .expect("the pointer is on the output")
}

/// Show a peer, move their pointer 50 px, and prove the move a sparse frame
/// of under 1% of the 4K output that covers the pointer where it was and
/// where it is, with retained pixels equal to a dense render.
fn assert_peer_move_is_sparse(frame: &mut FrameBuilder, label: &str) {
    let (from, to) = ((1_200.0, 700.0), (1_250.0, 700.0));
    let mut retained = bridge();
    let mut dense = bridge();
    point_peer_at(frame, from);
    let shown = frame.paint_with_damage();
    paint_retained_and_compare(&mut retained, &mut dense, &shown, label);
    let previous = frame.last_produced_sections.clone();
    point_peer_at(frame, to);
    let moved = frame.paint_with_damage();
    let details = sparse_refusal_details(previous.as_ref(), frame);
    let damage = moved
        .damage_regions
        .as_deref()
        .unwrap_or_else(|| panic!("{label}: a peer pointer move must be sparse: {details}"));
    assert_repaints_under_one_percent(damage, label);
    assert_damage_covers(
        peer_dot(frame, from),
        damage,
        &format!("{label}: old pointer"),
    );
    assert_damage_covers(
        peer_dot(frame, to),
        damage,
        &format!("{label}: new pointer"),
    );
    paint_retained_and_compare(&mut retained, &mut dense, &moved, label);
    assert!(
        retained.last_sparse_render_attempts() >= 1,
        "{label}: the move must exercise the production sparse render path"
    );
}

#[test]
fn a_peer_pointer_move_repaints_under_one_percent_of_a_4k_output_in_every_theme() {
    for (name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high contrast", Theme::high_contrast()),
    ] {
        let (mut frame, _clock) = settled_builder_in(theme);
        assert_peer_move_is_sparse(&mut frame, &format!("peer pointer move, {name}"));
    }
}

#[test]
fn night_light_keeps_a_peer_pointer_move_sparse_and_exact() {
    let (mut frame, mut clock) = settled_builder();
    let _settings = foreign_filter_settings(&frame);
    clock.set("TILO_SHOTS_THEME", "dark");
    frame.night_light = true;
    frame.watch_configured_theme = true;
    // What `enable_configured_theme` reads from that variable once, when
    // watching starts; the settings file stays unwatched.
    frame.shots_theme_pinned = true;
    assert!(
        frame.paint_with_damage().damage_regions.is_none(),
        "turning night light on changes every pixel"
    );
    assert_peer_move_is_sparse(&mut frame, "peer pointer move under night light");
    frame.night_light_temperature = frame.night_light_temperature.saturating_add(500);
    assert!(
        frame.paint_with_damage().damage_regions.is_none(),
        "a new night light temperature changes every pixel"
    );
}

#[test]
fn hovering_a_tab_repaints_that_tab_not_the_output() {
    let (mut frame, _clock) = settled_builder();
    let pill = frame
        .widget_registry()
        .into_iter()
        .find(|(name, _)| name.ends_with("tab:Files"))
        .map(|(_, rect)| rect)
        .expect("the Files tab is a named target");
    let mut retained = bridge();
    let mut dense = bridge();
    let settled = frame.paint_with_damage();
    paint_retained_and_compare(&mut retained, &mut dense, &settled, "before the hover");
    let previous = frame.last_produced_sections.clone();
    let mut document = automerge::AutoCommit::new();
    let mut focus = None;
    let _ = frame.dispatch_event(
        &winit::event::WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(pill.center().x, pill.center().y),
        },
        &winit::event::Modifiers::default(),
        &mut ui::widget::Edits::lend(&mut document),
        &automerge::ROOT,
        &mut focus,
    );
    let hovered = frame.paint_with_damage();
    let details = sparse_refusal_details(previous.as_ref(), &frame);
    let damage = hovered
        .damage_regions
        .as_deref()
        .unwrap_or_else(|| panic!("hovering a tab must be sparse: {details}"));
    assert_repaints_under_one_percent(damage, "tab hover");
    let pill = frame
        .layout_rect_to_scene(pill)
        .expect("the tab is on the output");
    assert_damage_covers(pill, damage, "hovered tab");
    paint_retained_and_compare(&mut retained, &mut dense, &hovered, "tab hovered");
}
