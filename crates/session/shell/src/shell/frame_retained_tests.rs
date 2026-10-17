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
    DenyAmbientStarts, PopulatedShellFixture, dispatch_populated_fixture_click, settled_pan_report,
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
        &ui::synthetic_input::cursor_moved(
            f64::from(NATIVE_WIDTH) - 50.0,
            f64::from(NATIVE_HEIGHT) - 50.0,
        ),
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
    // The section's bounds also hold the strip rail and ribbon, which are painted before
    // and after opening alike and so are no damage; what opening changes is the panel and
    // its shadow.
    let opened_extent = Rect::new(
        panel.x0 - shadow_reach,
        panel.y0 - shadow_reach,
        panel.x1 + shadow_reach,
        panel.y1 + shadow_reach,
    );
    assert_damage_covers(
        opened_extent,
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
fn workspace_root_rebuild_frames_carry_the_new_scene() {
    let mut frame = PopulatedShellFixture::new((NATIVE_WIDTH, NATIVE_HEIGHT), Theme::dark());
    frame.relayout();
    let warm = frame.paint_with_damage();
    assert!(
        !warm.scene.encoding().is_empty(),
        "the desktop before the switch records source commands"
    );
    frame.with_chrome(|chrome| {
        chrome.add_workspace("frames after rebuild");
    });
    let mut document = AutoCommit::new();
    let document_root = automerge::ROOT;
    frame.pump_apps(&mut document, &document_root);
    frame.relayout();
    let rebuilt = frame.paint_with_damage();
    assert!(
        !rebuilt.scene.encoding().is_empty(),
        "the rebuilt workspace root reaches the screen"
    );
    // The rest of the switch's frame run draws the same desktop. A presenter
    // keeps every pixel it already shows, on one display or several.
    for index in 0..3 {
        let following = frame.paint_with_damage();
        assert!(
            !following.scene.encoding().is_empty(),
            "switch frame {index} carries the desktop on every presenter"
        );
        assert_eq!(
            following.damage_regions.as_deref(),
            Some(&[][..]),
            "switch frame {index} repaints nothing the rebuilt frame drew"
        );
    }
}

fn assert_populated_tree_inventory(fixture: &PopulatedShellFixture) {
    assert_eq!(fixture.viewport(), (3840.0, 2160.0));
    assert_eq!(
        fixture.host().fixture_provider_ids(),
        vec!["actions", "records", "file-picker"]
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
        panned: crate::shell::overlay_host::PannedSections::default(),
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
        layers_generation: None,
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

#[cfg(target_os = "linux")]
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
        crate::shell::render::paint::damage_is_profitable(
            dirty_pixels,
            u64::from(NATIVE_WIDTH) * u64::from(NATIVE_HEIGHT),
        ),
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
    foreign_damage_fixture_on(window_width, surface, (NATIVE_WIDTH, NATIVE_HEIGHT))
}

/// [`foreign_damage_fixture`] on a desktop of `output` pixels.
fn foreign_damage_fixture_on(
    window_width: ui::widget::WindowWidth,
    surface: u64,
    output: (u32, u32),
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
    let mut fixture =
        PopulatedShellFixture::with_tree(output, Theme::dark().with_reduce_motion(true), tree);
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
    // Importing a GPU frame goes through the Linux dmabuf capture path.
    #[cfg(not(target_os = "linux"))]
    assert!(
        importing_bridge.is_none(),
        "importing a GPU frame needs Linux"
    );
    #[cfg(target_os = "linux")]
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
        return;
    }
    store.publish_rgba(
        surface,
        &texel.repeat(size.0 as usize * size.1 as usize),
        size.0,
        size.1,
    );
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

fn foreign_filter_settings(frame: &FrameBuilder) -> crate::shell::test_support::InstalledSettings {
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
    crate::shell::test_support::install_settings(std::sync::Arc::new(source))
}

// Exact client damage is published through the Linux capture seam
// (`publish_damaged_rgba_for_test`).
#[cfg(target_os = "linux")]
#[test]
fn wide_foreign_commit_damage_tracks_exact_buffers_and_matches_dense_gpu_pixels() {
    for (imported, density) in [(false, 1), (false, 2), (true, 1), (true, 2)] {
        let _ambient = DenyAmbientStarts::new();
        let mut clock = tilo_test_env::ScopedEnv::new();
        clock.set("TILO_CLOCK", "09:41");
        let surface = 8_493_206 + u64::from(imported) * 2 + u64::from(density == 2);
        // A little short of native height, so the tile stays at most 2048 px
        // tall: the renderer's 8192 px image atlas keeps a window's previous
        // frame resident while its next one arrives, and two 2x frames of a
        // taller tile cannot be stacked in it, so the new one would be
        // omitted (an atlas budget, not the damage tracking under test).
        let (mut fixture, logical_size) = foreign_damage_fixture_on(
            ui::widget::WindowWidth::Wide,
            surface,
            (NATIVE_WIDTH, NATIVE_HEIGHT - 32),
        );
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
            crate::shell::render::paint::damage_is_profitable(
                dirty_pixels,
                u64::from(NATIVE_WIDTH) * u64::from(NATIVE_HEIGHT),
            ),
            "a wide tile's two small edits must exercise sparse admission"
        );
        // A fresh dense oracle: its image atlas keeps the previous frame
        // resident for two passes, and at density 2 this 3456x2056 tile's
        // 6912x4112 buffer cannot share the 8192 px atlas with its own
        // predecessor, so a reused oracle would omit the new frame and
        // compare against a blank tile rather than against the truth.
        oracle = bridge();
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

/// A client that damages one text line (a typed character in a terminal)
/// repaints only that line of the output: the frame's damage is the line's
/// rows across the tile, the retained render takes the sparse path and writes
/// no pixel outside them, and the result matches a dense render of the scene.
#[cfg(target_os = "linux")]
#[test]
fn one_line_of_client_damage_repaints_only_that_line() {
    let _ambient = DenyAmbientStarts::new();
    let mut clock = tilo_test_env::ScopedEnv::new();
    clock.set("TILO_CLOCK", "09:41");
    let surface = 8_493_301;
    let (mut fixture, size) = foreign_damage_fixture(ui::widget::WindowWidth::Narrow, surface);
    let mut retained = bridge();
    let mut oracle = bridge();
    let mut pixels = [32, 32, 32, 255].repeat(size.0 as usize * size.1 as usize);
    let store = crate::shell::foreign::store();
    store.publish_rgba(surface, &pixels, size.0, size.1);
    let first = fixture.paint_with_damage();
    paint_retained_and_compare(&mut retained, &mut oracle, &first, "the empty grid");
    // One line of a text grid, 20 buffer rows, across the whole buffer.
    let line = Rect::new(0.0, 100.0, f64::from(size.0), 120.0);
    for y in line.y0 as usize..line.y1 as usize {
        for x in 0..size.0 as usize {
            let index = (y * size.0 as usize + x) * 4;
            pixels[index..index + 4].copy_from_slice(&[224, 224, 224, 255]);
        }
    }
    store.publish_damaged_rgba_for_test(surface, &pixels, size, vec![line]);
    let typed = fixture.paint_with_damage();
    let mounted = fixture.foreign_surface_rect(surface).expect("mounted");
    let placement = fixture
        .layout_rect_to_scene(mounted)
        .expect("scene placement");
    let scale_y = placement.height() / f64::from(size.1);
    let line_top = placement.y0 + line.y0 * scale_y;
    let line_bottom = placement.y0 + line.y1 * scale_y;
    // A filtered image's damage reaches one pixel past the changed texels,
    // and is repainted in whole 16-pixel tiles, Vello's fine tile.
    let tile = 16.0;
    let (rows_top, rows_bottom) = (
        ((line_top - 1.0) / tile).floor() * tile,
        ((line_bottom + 1.0) / tile).ceil() * tile,
    );
    let (columns_left, columns_right) = (
        ((placement.x0 - 1.0) / tile).floor() * tile,
        ((placement.x1 + 1.0) / tile).ceil() * tile,
    );
    let damage = typed
        .damage_regions
        .as_ref()
        .expect("a one-line commit keeps bounded damage");
    assert!(!damage.is_empty(), "the typed line must be repainted");
    for rect in damage {
        assert!(
            f64::from(rect.y) >= rows_top && f64::from(rect.y + rect.height) <= rows_bottom,
            "damage {rect:?} reaches outside the tile rows {rows_top}..{rows_bottom} of the \
             typed line {line_top}..{line_bottom}"
        );
        assert!(
            f64::from(rect.x) >= columns_left && f64::from(rect.x + rect.width) <= columns_right,
            "damage {rect:?} reaches outside the tile {placement:?}"
        );
    }
    let damaged: u64 = damage
        .iter()
        .map(|rect| u64::from(rect.width) * u64::from(rect.height))
        .sum();
    let line_pixels = ((rows_bottom - rows_top) * (columns_right - columns_left)) as u64;
    assert!(
        damaged <= line_pixels,
        "{damaged} damaged pixels for a line of at most {line_pixels}"
    );
    paint_retained_and_compare(&mut retained, &mut oracle, &typed, "one typed line");
    assert!(
        retained.last_sparse_render_attempts() >= 1,
        "a one-line commit renders through the sparse path, not the whole output"
    );
    let stats = retained
        .last_sparse_render_stats()
        .expect("the sparse render reports what it wrote");
    assert!(
        stats.written_pixels <= damaged,
        "the sparse render wrote {} pixels for {damaged} damaged",
        stats.written_pixels
    );
    assert_foreign_pixel(
        &mut oracle,
        &typed,
        Point::new(placement.center().x, f64::midpoint(line_top, line_bottom)),
        [224, 224, 224],
    );
    store.forget(surface);
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
    wide_retained_fixture_in(size, Theme::dark())
}

#[cfg(target_os = "linux")]
fn wide_retained_fixture_in(size: (u32, u32), theme: Theme) -> PopulatedShellFixture {
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
    PopulatedShellFixture::with_tree(size, theme.with_reduce_motion(true), tree)
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
    acknowledge_notes_saves_of(frame, 3);
}

/// Acknowledge the startup writes of the fixture's `surfaces` Notes windows.
#[cfg(target_os = "linux")]
fn acknowledge_notes_saves_of(frame: &mut PopulatedShellFixture, surfaces: usize) {
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
        surfaces
    );
    // Durability is sampled after sync; the next sync presents that sample.
    assert_eq!(frame.wake_mounted_components(), surfaces);
    let _ = frame.paint_with_damage();
    assert_eq!(frame.wake_mounted_components(), surfaces);
    let _ = frame.paint_with_damage();
}

#[cfg(target_os = "linux")]
fn assert_notes_startup_saved(frame: &mut PopulatedShellFixture) {
    assert_notes_startup_saved_of(frame, 3);
}

/// All `surfaces` real Notes windows present their acknowledged startup
/// state.
#[cfg(target_os = "linux")]
fn assert_notes_startup_saved_of(frame: &mut PopulatedShellFixture, surfaces: usize) {
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
        vec![notes::status_bar::SaveStatus::Saved; surfaces],
        "all {surfaces} real Notes surfaces present their acknowledged startup state"
    );
}

/// The strips layers of two frames hold the same drawings: what no strip
/// carries, and every strip in canvas coordinates.
#[cfg(target_os = "linux")]
fn strips_drawn_alike(a: &FrameCache, b: &FrameCache) -> bool {
    a.layers.workspace.encoding_eq(&b.layers.workspace)
        && a.layers.strips.len() == b.layers.strips.len()
        && a.layers
            .strips
            .iter()
            .zip(&b.layers.strips)
            .all(|(a, b)| a.encoding_eq(b))
}

#[cfg(target_os = "linux")]
fn acknowledged_notes_canvas(frame: &mut PopulatedShellFixture) -> FramePaint {
    acknowledged_notes_canvas_of(frame, (3, 3))
}

/// [`acknowledged_notes_canvas`] for a fixture whose Notes windows write
/// `records` startup records, `surfaces` of them shown.
#[cfg(target_os = "linux")]
fn acknowledged_notes_canvas_of(
    frame: &mut PopulatedShellFixture,
    (records, surfaces): (usize, usize),
) -> FramePaint {
    let mut at_rest = frame.paint_with_damage();
    for _ in 0..8 {
        frame.relayout();
        let next = frame.paint_with_damage();
        let settled = match (&next.cache, &at_rest.cache) {
            (Some(next), Some(previous)) => strips_drawn_alike(next, previous),
            _ => false,
        };
        at_rest = next;
        if settled {
            break;
        }
    }
    // Startup creates real records. A writer acknowledgment between the two
    // encodings would change Saving to Saved and invalidate the pixel oracle.
    acknowledge_notes_saves_of(frame, records);
    at_rest = frame.paint_with_damage();
    assert_notes_startup_saved_of(frame, surfaces);
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
    frame.host_mut().set_strip_pan_quantum(Some(1.0));
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
    // The frame a pan rests on keeps the chrome the pan began with; the
    // strip indicators show from the next one. Compare that frame, which
    // draws them as the dense scene does, not the one before it.
    let shown = frame.paint_with_damage();
    frame.set_retained_layer_width(None);
    acknowledge_notes_saves(&mut frame);
    let dense = frame.paint();
    assert_layer_cpu_pixels_match(&shown.scene, &dense, SIZE);
    // A policy transition restores fractional layout rather than rounding state.
    frame.host_mut().set_strip_pan_quantum(None);
    frame.relayout();
    assert_eq!(frame.host().strip_scroll_offsets()[0].1, 120.5);
}

#[cfg(target_os = "linux")]
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
        strips_drawn_alike(pan, rest),
        "a pan leaves the strips in canvas coordinates"
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
    // without layers does. The frame a pan rests on keeps the chrome the pan
    // began with and the strip indicators show from the next one, so compare
    // that next frame: it draws them as the dense scene does.
    let shown = frame.paint_with_damage();
    frame.set_retained_layer_width(None);
    // Encode both paths before rasterizing either: app status bubbles use
    // monotonic linger deadlines, and CPU rendering may cross a deadline.
    // Exercise the same late acknowledgment boundary that reproduced the
    // historical 1,034-pixel failure; startup is already durable on both paths.
    acknowledge_notes_saves(&mut frame);
    let dense_scene = frame.paint();
    assert_layer_cpu_pixels_match(&shown.scene, &dense_scene, SIZE);

    assert_layers_yield_to_the_dense_path_when_not_exact(&mut frame, SIZE);
}

/// A presenter that composites layers still gets the dense path when it has no
/// live capability, when a pan lands between pixels, when the scene is changed
/// externally, and when the output is wider than the layer budget.
#[cfg(target_os = "linux")]
fn assert_layers_yield_to_the_dense_path_when_not_exact(
    frame: &mut PopulatedShellFixture,
    size: (u32, u32),
) {
    frame.set_retained_layer_width(Some(8192));
    let mut independent = PopulatedShellFixture::new(size, Theme::dark().with_reduce_motion(true));
    independent.relayout();
    assert!(
        independent.paint_with_damage().cache.is_none(),
        "another producer has no live capability"
    );

    frame.host_mut().set_strip_scroll_for_test(120.5);
    frame.relayout();
    let fractional = frame.paint_with_damage();
    assert!(
        fractional.cache.is_none(),
        "a pan between pixels keeps its exact screen-space drawing, not layers"
    );
    // As above: the indicators show from the frame after the pan's rest.
    let fractional = frame.paint_with_damage();
    let dense_scene = frame.paint();
    assert_layer_cpu_pixels_match(&fractional.scene, &dense_scene, size);

    frame.host_mut().set_strip_scroll_for_test(121.0);
    frame.relayout();
    let mut external = frame.paint_with_damage();
    assert!(
        external.cache.is_some(),
        "a whole-pixel pan comes as layers"
    );
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

/// A GPU target of `size` on its own device, as the DRM presenter renders.
#[cfg(target_os = "linux")]
fn gpu_target(size: (u32, u32)) -> crate::shell::render::paint::RetainedGpuTarget {
    let _env = tilo_test_env::lock();
    let mut context = ui::gpu::RenderContext::new();
    let index = pollster::FutureExt::block_on(context.device(None))
        .expect("strip layer tests require a real adapter");
    crate::shell::render::paint::RetainedGpuTarget::new(&context.devices[index], size.0, size.1)
        .expect("GPU target")
}

/// Where `apart` (from [`pixels_apart`]) lies on a `size` output, as a map
/// of 32×32-pixel cells (`#` holds a pixel apart), for a failure message.
#[cfg(target_os = "linux")]
fn apart_map(apart: &[(usize, usize, u8)], size: (u32, u32)) -> String {
    let columns = size.0.div_ceil(32) as usize;
    let rows = size.1.div_ceil(32) as usize;
    let mut cells = vec![b'.'; columns * rows];
    for &(x, y, _) in apart {
        cells[(y / 32) * columns + x / 32] = b'#';
    }
    cells
        .chunks(columns)
        .map(|row| String::from_utf8_lossy(row).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The pixels (x, y, largest channel step) where `a` and `b`, RGBA rows
/// `width` wide, are more than one step apart.
#[cfg(target_os = "linux")]
fn pixels_apart(a: &[u8], b: &[u8], width: u32) -> Vec<(usize, usize, u8)> {
    const BLOCK_BYTES: usize = 1024;
    assert_eq!(a.len(), b.len(), "both targets have one size");
    // Equal blocks have zero channel distance. Avoid the unoptimized
    // per-channel iterator work for those pixels without changing the oracle.
    let mut apart = Vec::new();
    for (block, (a, b)) in a.chunks(BLOCK_BYTES).zip(b.chunks(BLOCK_BYTES)).enumerate() {
        if a == b {
            continue;
        }
        for (pixel, (a, b)) in a.chunks_exact(4).zip(b.chunks_exact(4)).enumerate() {
            let step = a
                .iter()
                .zip(b)
                .map(|(x, y)| x.abs_diff(*y))
                .max()
                .unwrap_or(0);
            if step > 1 {
                let index = block * (BLOCK_BYTES / 4) + pixel;
                apart.push((index % width as usize, index / width as usize, step));
            }
        }
    }
    apart
}

#[test]
#[cfg(target_os = "linux")]
fn pixel_oracle_detects_the_entire_real_render_change_across_blocks() {
    let size = (128, 128);
    let mut target = gpu_target(size);
    target.set_base_color(Color::TRANSPARENT);
    let before = render_whole(&mut target, &Scene::new(), 1, "empty control");
    let after = render_whole(&mut target, &rect_scene(), 2, "changed control");
    assert!(pixels_apart(&before, &before, size.0).is_empty());
    let apart = pixels_apart(&before, &after, size.0);
    let positions = apart.iter().map(|&(x, y, _)| (x, y)).collect::<Vec<_>>();
    let expected = (20..120)
        .flat_map(|y| (10..100).map(move |x| (x, y)))
        .collect::<Vec<_>>();
    assert_eq!(
        positions, expected,
        "every changed rendered pixel is observed"
    );
    assert!(
        apart.iter().all(|&(_, _, step)| step == 255),
        "opaque scene pixels change alpha by 255"
    );
    for alpha in [1_u8, 2] {
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Theme::dark()
                .tokens
                .colors
                .surfaces
                .scrim
                .with_alpha(f32::from(alpha) / 255.0),
            None,
            &Rect::new(10.0, 20.0, 100.0, 120.0),
        );
        let shown = render_whole(&mut target, &scene, u64::from(alpha) + 2, "alpha control");
        let distance = shown
            .iter()
            .zip(&before)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert_eq!(
            distance, alpha,
            "the real render pins the RGBA channel distance"
        );
        let apart = pixels_apart(&before, &shown, size.0);
        if alpha == 1 {
            assert!(
                apart.is_empty(),
                "one channel step retains the existing tolerance"
            );
        } else {
            let positions = apart.iter().map(|&(x, y, _)| (x, y)).collect::<Vec<_>>();
            assert_eq!(
                positions, expected,
                "two channel steps expose every changed pixel"
            );
            assert!(apart.iter().all(|&(_, _, step)| step == alpha));
        }
    }
}

/// How many pixels of the band copies of `work` differ by more than one
/// step between `before`, moved as the copies move it, and `after`, and the
/// most steps any of them differs by: what moving a full render of one
/// frame gives against a full render of the next. The most is over every
/// copied pixel, a step of rounding included: once vello rasterises a shape
/// alike at every offset, that rounding is all whole renders at two pans
/// differ in, and a copy can show it.
#[cfg(target_os = "linux")]
fn moved_render_apart(
    work: &crate::shell::render::strip_layers::LayerWork,
    before: &[u8],
    after: &[u8],
    width: u32,
) -> (usize, u8) {
    let crate::shell::render::strip_layers::WorkspaceWork::Pan { copies, .. } = &work.workspace
    else {
        return (0, 0);
    };
    let pixel = |pixels: &[u8], x: u32, y: u32| {
        let at = (y as usize * width as usize + x as usize) * 4;
        pixels[at..at + 4].to_vec()
    };
    let mut apart = (0, 0_u8);
    for copy in copies {
        for y in copy.y..copy.y + copy.height {
            for dx in 0..copy.width {
                let moved = pixel(before, copy.src_x + dx, y);
                let fresh = pixel(after, copy.dst_x + dx, y);
                let step = moved
                    .iter()
                    .zip(&fresh)
                    .map(|(x, y)| x.abs_diff(*y))
                    .max()
                    .unwrap_or(0);
                if step > 1 {
                    apart.0 += 1;
                }
                apart.1 = apart.1.max(step);
            }
        }
    }
    apart
}

/// Vello does not rasterise a shape identically at every offset: two whole
/// renders of the strips at adjacent pans, one moved onto the other, differ
/// at a few edge pixels by up to ~6 % coverage (measured at most 25
/// such pixels per step and 15 channel steps at 1280x720, in all three
/// themes). A pixel a pan copies keeps the value it was rasterised with, so
/// against a whole render at the new pan it can show that same difference;
/// the copied pixels accumulate up to ~80 of them on screen. Anything
/// structural (a band copied to the wrong place, a stale or missing band, a
/// wrong composite) is thousands of pixels.
#[cfg(target_os = "linux")]
const OFFSET_PIXELS: usize = 128;

/// The most channel steps vello's offset variance moves an edge pixel
/// ([`OFFSET_PIXELS`] measured up to 15), with room for a theme's sharper
/// contrast. A shape a fraction of a pixel off, or missing, differs by far
/// more (127 steps for chrome half a pixel off at 150 %).
#[cfg(target_os = "linux")]
const OFFSET_STEPS: u8 = 24;

/// Present `painted` as the DRM direct path does: from its layers when it
/// came as layers, else from its scene with its damage. The pixels the
/// layers rasterised, for a layered frame.
#[cfg(target_os = "linux")]
fn present(
    target: &mut crate::shell::render::paint::RetainedGpuTarget,
    painted: &FramePaint,
    label: &str,
) -> Option<u64> {
    let deadline = Instant::now() + tilo_test_env::hang_bound();
    match painted.cache.as_ref() {
        Some(cache) if target.layers_eligible(cache) => {
            target
                .render_layers_until(cache, painted.revision, deadline, |_, _, _| {})
                .unwrap_or_else(|error| panic!("{label}: layers: {error}"));
            Some(
                target
                    .last_layer_raster_pixels()
                    .expect("a layered frame records its work")
                    .0,
            )
        }
        _ => {
            target
                .render_until(
                    &painted.scene,
                    SceneDamage {
                        revision: painted.revision,
                        previous_revision: painted.previous_revision,
                        regions: painted.damage_regions.as_deref(),
                    },
                    deadline,
                )
                .unwrap_or_else(|error| panic!("{label}: scene: {error}"));
            None
        }
    }
}

/// `scene` rendered whole into `target`, read back.
#[cfg(target_os = "linux")]
fn render_whole(
    target: &mut crate::shell::render::paint::RetainedGpuTarget,
    scene: &Scene,
    revision: u64,
    label: &str,
) -> Vec<u8> {
    target
        .render_until(
            scene,
            SceneDamage {
                revision,
                previous_revision: None,
                regions: None,
            },
            Instant::now() + tilo_test_env::hang_bound(),
        )
        .unwrap_or_else(|error| panic!("{label}: whole render: {error}"));
    target.read_rgba_for_test()
}

/// A desktop a pan is presented on: its panel in device pixels, the scale
/// the panel shows it at, and how many strips it has (the top one pans).
#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct PanDesktop {
    name: &'static str,
    panel: (u32, u32),
    scale: f64,
    strips: usize,
}

#[cfg(target_os = "linux")]
impl PanDesktop {
    /// The Notes startup records the fixture's windows write, and how many
    /// Notes windows it shows: on two strips each of the six groups holds a
    /// Notes tab, the first strip's groups show different tabs and the
    /// second's all show their first, Notes.
    fn notes(&self) -> (usize, usize) {
        if self.strips > 1 { (6, 4) } else { (3, 3) }
    }
}

/// The wide demo strip in `theme`, alone or with a second strip below it, on
/// one panel of `desktop.panel` device pixels shown at `desktop.scale`.
#[cfg(target_os = "linux")]
fn pan_fixture(desktop: PanDesktop, theme: Theme) -> PopulatedShellFixture {
    let mut frame = if desktop.strips > 1 {
        PopulatedShellFixture::with_tree(
            desktop.panel,
            theme.with_reduce_motion(true),
            two_strip_tree(),
        )
    } else {
        wide_retained_fixture_in(desktop.panel, theme)
    };
    if (desktop.scale - 1.0).abs() > f64::EPSILON {
        let (width, height) = (f64::from(desktop.panel.0), f64::from(desktop.panel.1));
        frame.set_output_geometry(vec![crate::shell::output_scene::OutputGeometry {
            id: 0,
            monitor: String::new(),
            x: 0.0,
            y: 0.0,
            width: width / desktop.scale,
            height: height / desktop.scale,
            scale: desktop.scale,
            scene: Rect::new(0.0, 0.0, width, height),
        }]);
    }
    frame
}

/// The demo desktop with three windows on its first strip and a copy of that
/// strip below it: the docked wheel rides the first over the second, and the
/// two strips' drawings meet in the gap between them.
#[cfg(target_os = "linux")]
fn two_strip_tree() -> crate::shell::state::WorkspaceTree {
    use crate::shell::state::{StripId, WindowWidth};
    let mut tree = crate::shell::state::WorkspaceTree::bootstrap_demo();
    if let Some(workspace) = tree.desktop.active_workspace_mut()
        && let Some(first) = workspace.strips.first_mut()
        && let Some(template) = first.window_groups.first().cloned()
    {
        let mut groups = Vec::new();
        for index in 0..3 {
            let mut copy = template.clone();
            copy.id = crate::shell::state::next_id();
            for tab in &mut copy.tabs {
                tab.id = crate::shell::state::next_id();
            }
            copy.active_tab = copy.tabs[index % copy.tabs.len()].id;
            copy.width = if index % 2 == 0 {
                WindowWidth::Wide
            } else {
                WindowWidth::Normal
            };
            groups.push(copy);
        }
        first.window_groups = groups;
        let mut second = first.clone();
        second.id = StripId(0x5ea1_ffff);
        second.name = "Second".into();
        for group in &mut second.window_groups {
            group.id = crate::shell::state::next_id();
            for tab in &mut group.tabs {
                tab.id = crate::shell::state::next_id();
            }
            group.active_tab = group.tabs[0].id;
        }
        workspace.strips.push(second);
    }
    tree.seed_doc_from_cache();
    tree
}

/// `shown`, the composited output of `painted`, shows what the desktop drawn
/// without layers shows: nothing the layers leave out or misplace (a strip's
/// shadow in its neighbour's row, the wheel over the strip below, chrome a
/// fraction of a pixel off) is lost. That desktop draws every edge at its
/// screen coordinates, the layers at canvas coordinates moved by whole
/// pixels, so vello's offset variance shows along edges all over the output,
/// never more than it does between two whole renders ([`OFFSET_STEPS`]);
/// anything misplaced or missing is far more.
#[cfg(target_os = "linux")]
fn assert_nothing_lost(
    frame: &mut FrameBuilder,
    dense: &mut crate::shell::render::paint::RetainedGpuTarget,
    (shown, painted): (&[u8], &FramePaint),
    size: (u32, u32),
    label: &str,
) {
    let screen = frame.paint();
    let unlayered = render_whole(dense, &screen, 0, label);
    let lost = pixels_apart(shown, &unlayered, size.0);
    let lost_max = lost.iter().map(|&(_, _, step)| step).max().unwrap_or(0);
    eprintln!(
        "strip layers {label}: {} pixels more than one step from the desktop drawn without \
         layers (max {lost_max})",
        lost.len()
    );
    let output = size.0 as usize * size.1 as usize;
    let edge_noise = lost_max <= OFFSET_STEPS && lost.len() <= output / 100;
    if !edge_noise {
        eprintln!(
            "strip layers {label}: strips {:?}, wheel {:?}, pans {:?}\n{}",
            painted.cache.as_ref().map(|cache| &cache.strips),
            painted
                .cache
                .as_ref()
                .and_then(|cache| cache.layers.wheel.as_ref().map(|wheel| (
                    wheel.x,
                    wheel.y,
                    wheel.width,
                    wheel.height
                ))),
            frame.host().strip_scroll_offsets(),
            apart_map(&lost, size)
        );
    }
    assert!(
        edge_noise,
        "{label}: {} pixels differ from the desktop drawn without layers by more than one \
         step, up to {lost_max} steps: more than vello's offset variance (first {:?})",
        lost.len(),
        &lost[..lost.len().min(16)]
    );
}

/// The strips of `painted`, drawn as layers, rendered whole on their own at
/// its pan: what a pan's band copies move.
#[cfg(target_os = "linux")]
fn render_strips_alone(
    target: &mut crate::shell::render::paint::RetainedGpuTarget,
    painted: &FramePaint,
    label: &str,
) -> Option<Vec<u8>> {
    let cache = painted.cache.as_ref()?;
    let mut scene = Scene::new();
    crate::shell::render::strip_layers::append_panned(&mut scene, &cache.layers, &cache.strips);
    Some(render_whole(target, &scene, painted.revision, label))
}

/// A strip pan presented from retained layers, frame by frame, shows what a
/// full render of each frame shows, and what the desktop drawn without
/// layers shows, in every theme; and once the layers are up, a pan frame
/// rasterises only what it uncovered.
#[cfg(target_os = "linux")]
fn assert_pan_composites_like_full_renders(desktop: PanDesktop) {
    let size = desktop.panel;
    let mut clock = tilo_test_env::ScopedEnv::new();
    let _ambient = DenyAmbientStarts::new();
    clock.set("TILO_CLOCK", "09:41");
    let output = u64::from(size.0) * u64::from(size.1);
    for (theme_name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high contrast", Theme::high_contrast()),
    ] {
        let name = format!("{}, {theme_name}", desktop.name);
        let mut frame = pan_fixture(desktop, theme);
        frame.set_strip_pan_presenter(false, true);
        let _ = acknowledged_notes_canvas_of(&mut frame, desktop.notes());
        let mut layered = gpu_target(size);
        let mut dense = gpu_target(size);
        // The strips alone, rendered whole at each frame's pan: what a pan's
        // band copies move, and what vello draws at the new offset instead.
        let mut strips_only = gpu_target(size);
        let mut composited = 0;
        let mut thin = 0;
        let mut worst = 0;
        let mut worst_step = 0_u8;
        let mut control_step = 0_u8;
        let mut previous_strips: Option<Vec<u8>> = None;
        frame
            .host_mut()
            .update_active_strip_pan(0.0, true, settled_pan_report(0));
        // Fractional travel, as a touchpad reports it: the pans land on
        // whole device pixels, so the layers move without resampling.
        let mut travel = 0.0;
        for step in 0..24 {
            travel += if step < 16 { 23.7 } else { -31.3 };
            frame
                .host_mut()
                .update_active_strip_pan(travel, false, settled_pan_report(step + 1));
            frame.relayout();
            let painted = frame.paint_with_damage();
            let label = format!("{name}, pan step {step}");
            if let Some(raster) = present(&mut layered, &painted, &label) {
                composited += 1;
                thin += usize::from(raster < output / 4);
            }
            let shown = layered.read_rgba_for_test();
            let full = render_whole(&mut dense, &painted.scene, painted.revision, &label);
            let apart = pixels_apart(&shown, &full, size.0);
            let max = apart.iter().map(|&(_, _, step)| step).max().unwrap_or(0);
            // Every fourth frame, against the desktop drawn without layers.
            if step % 4 == 3 {
                assert_nothing_lost(&mut frame, &mut dense, (&shown, &painted), size, &label);
            }
            let strips = render_strips_alone(&mut strips_only, &painted, &label);
            let control = match (
                layered.last_layer_work_for_test(),
                previous_strips.as_deref(),
                strips.as_deref(),
            ) {
                (Some(work), Some(before), Some(after)) => {
                    moved_render_apart(work, before, after, size.0)
                }
                _ => (0, 0),
            };
            previous_strips = strips;
            eprintln!(
                "strip layers {label}: {} pixels more than one step from a full render (max {max}); \
                 whole strip renders at the previous pan, moved: {control:?}; raster {:?}",
                apart.len(),
                layered.last_layer_raster_pixels()
            );
            worst = worst.max(apart.len());
            worst_step = worst_step.max(max);
            control_step = control_step.max(control.1);
            assert!(
                apart.len() <= OFFSET_PIXELS,
                "{label}: {} pixels differ from a full render by more than one step (max {max}, \
                 first {:?})",
                apart.len(),
                &apart[..apart.len().min(16)]
            );
        }
        eprintln!(
            "strip layers {name}: at most {worst} pixels per frame more than one step from a whole \
             render, at most {worst_step} steps; whole renders at adjacent pans differ by up to \
             {control_step} steps"
        );
        assert!(
            worst_step <= control_step.saturating_add(1),
            "{name}: the composited pan differs from a whole render by {worst_step} steps, more \
             than vello's own whole renders at adjacent pans ({control_step})"
        );
        assert!(
            composited >= 20 && thin >= 16,
            "{name}: the pan comes as layers ({composited} of 24 frames), and once they are up a \
             pan frame rasterises only what it uncovered ({thin} frames)"
        );
        assert_rests_in_screen_space(&mut frame, &name);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_strip_pan_composited_from_layers_matches_a_full_render_in_every_theme() {
    assert_pan_composites_like_full_renders(PanDesktop {
        name: "100 %",
        panel: (1280, 720),
        scale: 1.0,
        strips: 1,
    });
}

/// Pulling the strip past its start rides the docked wheel in from the
/// screen edge, and its disc keeps going left of its slot to the edge of the
/// screen. Composited from layers, the wheel's sprite used to stop at the
/// slot, so the rubber band showed the disc cut off in a hard vertical edge.
#[cfg(target_os = "linux")]
#[test]
fn a_rubber_band_past_the_strip_start_composited_from_layers_matches_a_full_render() {
    let desktop = PanDesktop {
        name: "left overpull",
        panel: (1280, 720),
        scale: 1.0,
        strips: 1,
    };
    let size = desktop.panel;
    let mut clock = tilo_test_env::ScopedEnv::new();
    let _ambient = DenyAmbientStarts::new();
    clock.set("TILO_CLOCK", "09:41");
    let mut frame = pan_fixture(desktop, Theme::dark());
    frame.set_strip_pan_presenter(false, true);
    let _ = acknowledged_notes_canvas_of(&mut frame, desktop.notes());
    let mut layered = gpu_target(size);
    let mut dense = gpu_target(size);
    frame
        .host_mut()
        .update_active_strip_pan(0.0, true, settled_pan_report(0));
    let mut travel = 0.0;
    let mut composited = 0;
    for step in 0..12 {
        travel -= 37.3;
        frame
            .host_mut()
            .update_active_strip_pan(travel, false, settled_pan_report(step + 1));
        frame.relayout();
        let painted = frame.paint_with_damage();
        let label = format!("left overpull, step {step}");
        if present(&mut layered, &painted, &label).is_some() {
            composited += 1;
        }
        let shown = layered.read_rgba_for_test();
        let full = render_whole(&mut dense, &painted.scene, painted.revision, &label);
        let apart = pixels_apart(&shown, &full, size.0);
        assert!(
            apart.len() <= OFFSET_PIXELS,
            "{label}: {} pixels differ from a full render (first {:?}); the wheel layer \
             does not hold the disc the rubber band exposes",
            apart.len(),
            &apart[..apart.len().min(16)]
        );
    }
    assert!(
        frame
            .host()
            .strip_scroll_offsets()
            .iter()
            .any(|&(_, scroll, _)| scroll < -100.0),
        "fixture: the pan pulled the strip well past its start: {:?}",
        frame.host().strip_scroll_offsets()
    );
    assert!(
        composited >= 8,
        "the rubber band came as layers ({composited} of 12)"
    );
}

/// A 1920×1080 panel at 150 %: the desktop is laid out at 1280×720 and a
/// device pixel is two thirds of a layout pixel.
#[cfg(target_os = "linux")]
#[test]
fn a_strip_pan_at_150_percent_composited_from_layers_matches_a_full_render() {
    assert_pan_composites_like_full_renders(PanDesktop {
        name: "150 %",
        panel: (1920, 1080),
        scale: 1.5,
        strips: 1,
    });
}

/// A 1600×900 panel at 125 %: the desktop is laid out at 1280×720 and a
/// device pixel is four fifths of a layout pixel.
#[cfg(target_os = "linux")]
#[test]
fn a_strip_pan_at_125_percent_composited_from_layers_matches_a_full_render() {
    assert_pan_composites_like_full_renders(PanDesktop {
        name: "125 %",
        panel: (1600, 900),
        scale: 1.25,
        strips: 1,
    });
}

/// A 1920×1080 panel at 200 % (laid out at 960×540): the output the
/// tolerance is measured on, as at 150 %.
#[cfg(target_os = "linux")]
#[test]
fn a_strip_pan_at_200_percent_composited_from_layers_matches_a_full_render() {
    assert_pan_composites_like_full_renders(PanDesktop {
        name: "200 %",
        panel: (1920, 1080),
        scale: 2.0,
        strips: 1,
    });
}

/// Two strips, the docked wheel riding the upper one over the lower: the
/// upper strip pans, its shadows meet the lower strip's, and the wheel moves
/// over the lower strip's windows.
#[cfg(target_os = "linux")]
#[test]
fn a_pan_over_two_strips_composited_from_layers_matches_a_full_render() {
    assert_pan_composites_like_full_renders(PanDesktop {
        name: "two strips",
        panel: (1280, 720),
        scale: 1.0,
        strips: 2,
    });
}

/// Two strips on a 1920×1080 panel at 150 %: the wheel and the strips
/// step by a device pixel, two thirds of a layout pixel.
#[cfg(target_os = "linux")]
#[test]
fn a_pan_over_two_strips_at_150_percent_composited_from_layers_matches_a_full_render() {
    assert_pan_composites_like_full_renders(PanDesktop {
        name: "two strips at 150 %",
        panel: (1920, 1080),
        scale: 1.5,
        strips: 2,
    });
}

/// Two strips on a 1920×1080 panel at 200 %.
#[cfg(target_os = "linux")]
#[test]
fn a_pan_over_two_strips_at_200_percent_composited_from_layers_matches_a_full_render() {
    assert_pan_composites_like_full_renders(PanDesktop {
        name: "two strips at 200 %",
        panel: (1920, 1080),
        scale: 2.0,
        strips: 2,
    });
}

/// [`present`], except that a frame proven unchanged is kept rather than
/// rendered, as the DRM presenter keeps it.
#[cfg(target_os = "linux")]
fn present_or_keep(
    target: &mut crate::shell::render::paint::RetainedGpuTarget,
    painted: &FramePaint,
    label: &str,
) -> Option<u64> {
    let unchanged = SceneDamage {
        revision: painted.revision,
        previous_revision: painted.previous_revision,
        regions: painted.damage_regions.as_deref(),
    };
    if painted.cache.is_none() && target.keep_unchanged(unchanged) {
        return None;
    }
    present(target, painted, label)
}

/// Warm `target`'s layers from the desktop `frame` draws at rest, as the
/// session does once the desktop rests: the pixels it rasterised.
#[cfg(target_os = "linux")]
fn warm_at_rest(
    frame: &mut FrameBuilder,
    target: &mut crate::shell::render::paint::RetainedGpuTarget,
    keep_going: &dyn Fn() -> bool,
    label: &str,
) -> (u64, FrameCache) {
    let layers = frame
        .layers_at_rest()
        .unwrap_or_else(|| panic!("{label}: the desktop at rest is drawn as layers"));
    let pixels = target
        .warm_layers(
            &layers,
            Instant::now() + tilo_test_env::hang_bound(),
            keep_going,
        )
        .unwrap_or_else(|error| panic!("{label}: warm: {error}"))
        .unwrap_or_else(|| panic!("{label}: the target presents these layers"));
    (pixels, layers)
}

/// Let the desktop rest as long as the session waits before it warms the
/// layers (a second), then draw until it is at rest: two frames in a row
/// that change nothing, with nothing animating. A window off screen asks
/// for no frames while it animates (its spawn animation runs on the clock),
/// so frames drawn back to back after the fixture starts can find it still
/// moving: a draw of the strips in canvas coordinates, which draws it,
/// would then differ from the next.
#[cfg(target_os = "linux")]
fn rest_as_the_session_does(frame: &mut FrameBuilder) {
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let mut quiet = 0;
    for _ in 0..400 {
        frame.relayout();
        let painted = frame.paint_with_damage();
        let unchanged = painted.damage_regions.as_ref().is_some_and(Vec::is_empty);
        quiet = if unchanged && !frame.needs_animation_frame() {
            quiet + 1
        } else {
            0
        };
        if quiet >= 2 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("the desktop did not come to rest");
}

/// The first frame of a pan from a rest whose layers were warmed (as
/// `warmed`) rasterises neither the backdrop nor the chrome, and of the
/// strips only what the pan uncovered (and where two strips' drawings meet):
/// it copies the rest. A window that changed off screen since the warm (an
/// app the warm drew for the first time presenting its content) changes
/// nothing the screen shows, so it rasterises nothing.
#[cfg(target_os = "linux")]
fn assert_first_frame_from_warmed_rest(
    layered: &crate::shell::render::paint::RetainedGpuTarget,
    (warmed, painted): (&FrameCache, &FramePaint),
    output: u64,
    label: &str,
) {
    let cache = painted
        .cache
        .as_ref()
        .unwrap_or_else(|| panic!("{label}: a pan comes as layers"));
    let work = layered
        .last_layer_work_for_test()
        .unwrap_or_else(|| panic!("{label}: a layered frame records its work"));
    let raster = work.raster_pixels(layered.size());
    let apart = layers_apart(warmed, cache);
    assert!(
        !work.backdrop
            && !work.chrome
            && work.workspace != crate::shell::render::strip_layers::WorkspaceWork::Whole,
        "{label}: the first frame from a warmed rest rasterises neither the backdrop nor the \
         chrome nor the strips' layer whole ({work:?}); the layers warmed and the pan's differ \
         in\n{apart}"
    );
    assert!(
        raster < output / 16,
        "{label}: the first frame rasterises only what the pan uncovered ({raster} px of \
         {output}, {work:?}); the layers warmed and the pan's differ in\n{apart}"
    );
}

/// Where two drawings of the desktop as layers differ, for a failure
/// message: each layer, and per strip where its blit is and the first
/// place each encoded stream differs.
#[cfg(target_os = "linux")]
fn layers_apart(old: &FrameCache, new: &FrameCache) -> String {
    use std::fmt::Write as _;
    fn first_apart<T: PartialEq>(a: &[T], b: &[T]) -> Option<usize> {
        a.iter()
            .zip(b)
            .position(|(a, b)| a != b)
            .or_else(|| (a.len() != b.len()).then(|| a.len().min(b.len())))
    }
    fn streams(a: &Scene, b: &Scene) -> String {
        let (a, b) = (a.encoding(), b.encoding());
        format!(
            "path_tags {:?}/{}/{} path_data {:?} draw_tags {:?} draw_data {:?} transforms {:?} \
             ({:?} vs {:?}) styles {}/{} n_paths {}/{} patches {}/{}",
            first_apart(&a.path_tags, &b.path_tags),
            a.path_tags.len(),
            b.path_tags.len(),
            first_apart(&a.path_data, &b.path_data),
            first_apart(&a.draw_tags, &b.draw_tags),
            first_apart(&a.draw_data, &b.draw_data),
            first_apart(&a.transforms, &b.transforms),
            first_apart(&a.transforms, &b.transforms).and_then(|at| a.transforms.get(at)),
            first_apart(&a.transforms, &b.transforms).and_then(|at| b.transforms.get(at)),
            a.styles.len(),
            b.styles.len(),
            a.n_paths,
            b.n_paths,
            a.resources.patches.len(),
            b.resources.patches.len(),
        )
    }
    let mut text = format!(
        "width {}/{} backdrop same {} chrome same {} workspace same {} wheel {:?}/{:?}\n",
        old.width,
        new.width,
        old.layers.backdrop.encoding_eq(&new.layers.backdrop),
        old.layers.chrome.encoding_eq(&new.layers.chrome),
        old.layers.workspace.encoding_eq(&new.layers.workspace),
        old.layers
            .wheel
            .as_ref()
            .map(|wheel| (wheel.x, wheel.y, wheel.width, wheel.height)),
        new.layers
            .wheel
            .as_ref()
            .map(|wheel| (wheel.x, wheel.y, wheel.width, wheel.height)),
    );
    for (index, (a, b)) in old.layers.strips.iter().zip(&new.layers.strips).enumerate() {
        let _ = writeln!(
            text,
            "strip {index}: {:?} -> {:?} same {}: {}",
            old.strips.get(index),
            new.strips.get(index),
            a.encoding_eq(b),
            streams(a, b)
        );
    }
    text
}

/// A strip pan from a desktop at rest whose layers the presenter warmed
/// while it rested composites its first frame from them: that frame
/// rasterises only what the pan uncovered, not every layer, and each frame
/// shows what a full render of it shows, and what the desktop drawn without
/// layers shows, in every theme. Warming draws no frame: the damage between
/// the frames around it stays sparse, and layers already warm are left
/// alone. A warm that stops for a frame keeps what it finished and the pan
/// is still right.
#[cfg(target_os = "linux")]
fn assert_pan_from_warmed_rest(desktop: PanDesktop) {
    let size = desktop.panel;
    let mut clock = tilo_test_env::ScopedEnv::new();
    let _ambient = DenyAmbientStarts::new();
    clock.set("TILO_CLOCK", "09:41");
    let output = u64::from(size.0) * u64::from(size.1);
    for (theme_name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high contrast", Theme::high_contrast()),
    ] {
        for stopped in [false, true] {
            let name = format!(
                "{}, {theme_name}{}",
                desktop.name,
                if stopped { ", warm stopped" } else { "" }
            );
            let mut frame = pan_fixture(desktop, theme.clone());
            frame.set_strip_pan_presenter(false, true);
            let _ = acknowledged_notes_canvas_of(&mut frame, desktop.notes());
            rest_as_the_session_does(&mut frame);
            let mut layered = gpu_target(size);
            let mut dense = gpu_target(size);
            for _ in 0..4 {
                frame.relayout();
                let painted = frame.paint_with_damage();
                assert!(
                    painted.cache.is_none(),
                    "{name}: a desktop at rest is drawn in screen space"
                );
                let _ = present_or_keep(&mut layered, &painted, &name);
            }
            let budget = std::cell::Cell::new(if stopped { 1_u32 } else { u32::MAX });
            let keep_going = || {
                let left = budget.get();
                budget.set(left.saturating_sub(1));
                left > 0
            };
            let (warmed, warm_layers) = warm_at_rest(&mut frame, &mut layered, &keep_going, &name);
            assert!(!frame.layers_stale(), "{name}: the layers caught up");
            if stopped {
                assert!(
                    warmed < 3 * output,
                    "{name}: a warm stopped for a frame leaves the rest ({warmed} px)"
                );
            } else {
                assert!(
                    warmed >= 3 * output,
                    "{name}: a first warm rasterises every layer ({warmed} px)"
                );
                assert_eq!(
                    warm_at_rest(&mut frame, &mut layered, &|| true, &name).0,
                    0,
                    "{name}: layers already warm are left alone"
                );
            }
            frame.relayout();
            let rest = frame.paint_with_damage();
            assert!(
                rest.damage_regions.as_ref().is_some_and(Vec::is_empty),
                "{name}: drawing the layers made no frame: the next frame at rest is unchanged"
            );
            let _ = present_or_keep(&mut layered, &rest, &name);
            frame
                .host_mut()
                .update_active_strip_pan(0.0, true, settled_pan_report(0));
            let mut travel = 0.0;
            for step in 0..6 {
                travel += 23.7;
                frame.host_mut().update_active_strip_pan(
                    travel,
                    false,
                    settled_pan_report(step + 1),
                );
                frame.relayout();
                let painted = frame.paint_with_damage();
                let label = format!("{name}, pan step {step}");
                let raster = present(&mut layered, &painted, &label)
                    .unwrap_or_else(|| panic!("{label}: a pan comes as layers"));
                eprintln!(
                    "strip layers {label}: raster {raster} px, composited {:?}",
                    layered.last_layer_raster_pixels()
                );
                if step == 0 && !stopped {
                    assert_first_frame_from_warmed_rest(
                        &layered,
                        (&warm_layers, &painted),
                        output,
                        &label,
                    );
                }
                assert_shows_a_full_render(&layered, &mut dense, &painted, &label);
                if step == 0 {
                    let shown = layered.read_rgba_for_test();
                    assert_nothing_lost(&mut frame, &mut dense, (&shown, &painted), size, &label);
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_pan_from_a_warmed_rest_copies_its_first_frame_in_every_theme() {
    assert_pan_from_warmed_rest(PanDesktop {
        name: "100 %",
        panel: (1280, 720),
        scale: 1.0,
        strips: 1,
    });
}

#[cfg(target_os = "linux")]
#[test]
fn a_pan_from_a_warmed_rest_at_150_percent_copies_its_first_frame() {
    assert_pan_from_warmed_rest(PanDesktop {
        name: "150 %",
        panel: (1920, 1080),
        scale: 1.5,
        strips: 1,
    });
}

#[cfg(target_os = "linux")]
#[test]
fn a_pan_from_a_warmed_rest_at_200_percent_copies_its_first_frame() {
    assert_pan_from_warmed_rest(PanDesktop {
        name: "200 %",
        panel: (1920, 1080),
        scale: 2.0,
        strips: 1,
    });
}

#[cfg(target_os = "linux")]
#[test]
fn a_pan_over_two_strips_from_a_warmed_rest_copies_its_first_frame() {
    assert_pan_from_warmed_rest(PanDesktop {
        name: "two strips",
        panel: (1280, 720),
        scale: 1.0,
        strips: 2,
    });
}

/// A warm at rest after only the chrome changed (a collaborator's pointer
/// appeared over the desktop) rasterises the chrome tiles that changed, not
/// the whole chrome layer, and leaves the other layers alone; the pan from
/// that rest rasterises no chrome and shows what a full render of it shows,
/// in every theme.
#[cfg(target_os = "linux")]
#[test]
fn a_warm_after_the_chrome_changed_rasterises_only_the_tiles_that_changed() {
    const SIZE: (u32, u32) = (1280, 720);
    let desktop = PanDesktop {
        name: "100 %",
        panel: SIZE,
        scale: 1.0,
        strips: 1,
    };
    let mut clock = tilo_test_env::ScopedEnv::new();
    let _ambient = DenyAmbientStarts::new();
    clock.set("TILO_CLOCK", "09:41");
    let output = u64::from(SIZE.0) * u64::from(SIZE.1);
    for (theme_name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high contrast", Theme::high_contrast()),
    ] {
        let mut frame = pan_fixture(desktop, theme);
        frame.set_strip_pan_presenter(false, true);
        let _ = acknowledged_notes_canvas_of(&mut frame, desktop.notes());
        rest_as_the_session_does(&mut frame);
        let mut layered = gpu_target(SIZE);
        let mut dense = gpu_target(SIZE);
        let (first, _) = warm_at_rest(&mut frame, &mut layered, &|| true, theme_name);
        assert!(
            first >= 3 * output,
            "{theme_name}: a first warm rasterises every layer ({first} px)"
        );
        point_peer_at(&mut frame, (400.0, 300.0));
        rest_as_the_session_does(&mut frame);
        assert!(
            frame.layers_stale(),
            "{theme_name}: the pointer changed the desktop at rest"
        );
        let (rewarmed, layers) = warm_at_rest(&mut frame, &mut layered, &|| true, theme_name);
        eprintln!("strip layers {theme_name}: rewarm after the pointer {rewarmed} px");
        let changed = layers
            .chrome_damage
            .as_ref()
            .unwrap_or_else(|| panic!("{theme_name}: the chrome's change is bounded"));
        assert!(
            !changed.rects.is_empty(),
            "{theme_name}: the pointer's tiles changed"
        );
        assert!(
            rewarmed > 0 && rewarmed < output / 16,
            "{theme_name}: the warm rasterises the pointer's tiles, not a layer ({rewarmed} px of \
             {output})"
        );
        frame
            .host_mut()
            .update_active_strip_pan(0.0, true, settled_pan_report(0));
        frame
            .host_mut()
            .update_active_strip_pan(23.7, false, settled_pan_report(1));
        frame.relayout();
        let painted = frame.paint_with_damage();
        let label = format!("{theme_name}, pan after the rewarm");
        let raster = present(&mut layered, &painted, &label)
            .unwrap_or_else(|| panic!("{label}: a pan comes as layers"));
        let work = layered
            .last_layer_work_for_test()
            .unwrap_or_else(|| panic!("{label}: a layered frame records its work"));
        assert!(
            !work.backdrop && !work.chrome,
            "{label}: the warmed backdrop and chrome are what the pan shows ({work:?}, {raster} px)"
        );
        assert_shows_a_full_render(&layered, &mut dense, &painted, &label);
        let shown = layered.read_rgba_for_test();
        assert_nothing_lost(&mut frame, &mut dense, (&shown, &painted), SIZE, &label);
    }
}

/// When a pan ends, no frame at rest is rendered whole: the first one drawn
/// in screen space again also comes as layers and is composited from those
/// the presenter holds (nothing changed, so nothing is rasterised), the
/// frames after it are sparse, and the presenter's layers stay what the
/// screen shows. A second pan from that rest composites only what moves. In
/// every theme, every frame shows what a full render of it shows.
#[cfg(target_os = "linux")]
#[test]
fn the_desktop_comes_to_rest_after_a_pan_without_a_dense_frame() {
    const SIZE: (u32, u32) = (1280, 720);
    let desktop = PanDesktop {
        name: "100 %",
        panel: SIZE,
        scale: 1.0,
        strips: 1,
    };
    let mut clock = tilo_test_env::ScopedEnv::new();
    let _ambient = DenyAmbientStarts::new();
    clock.set("TILO_CLOCK", "09:41");
    let output = u64::from(SIZE.0) * u64::from(SIZE.1);
    for (theme_name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high contrast", Theme::high_contrast()),
    ] {
        let mut frame = pan_fixture(desktop, theme);
        frame.set_strip_pan_presenter(false, true);
        let _ = acknowledged_notes_canvas_of(&mut frame, desktop.notes());
        let mut layered = gpu_target(SIZE);
        let mut dense = gpu_target(SIZE);
        frame.relayout();
        let rest = frame.paint_with_damage();
        let _ = present_or_keep(&mut layered, &rest, theme_name);
        frame
            .host_mut()
            .update_active_strip_pan(0.0, true, settled_pan_report(0));
        let mut travel = 0.0;
        for step in 0..6 {
            travel += 23.7;
            frame
                .host_mut()
                .update_active_strip_pan(travel, false, settled_pan_report(step + 1));
            frame.relayout();
            let painted = frame.paint_with_damage();
            let label = format!("{theme_name}, pan step {step}");
            let _ = present(&mut layered, &painted, &label);
            assert_shows_a_full_render(&layered, &mut dense, &painted, &label);
        }
        frame.host_mut().end_active_strip_pan(Instant::now(), false);
        // What the last frame that came as layers rasterised.
        let mut last_layered = None;
        for step in 0..10 {
            frame.relayout();
            let painted = frame.paint_with_damage();
            let label = format!("{theme_name}, at rest {step}");
            assert!(
                painted.cache.is_some() || painted.damage_regions.is_some(),
                "{label}: a frame at rest after a pan is not rendered whole"
            );
            if painted.cache.is_some() {
                last_layered = present(&mut layered, &painted, &label);
            } else {
                let _ = present_or_keep(&mut layered, &painted, &label);
            }
            assert_shows_a_full_render(&layered, &mut dense, &painted, &label);
        }
        assert_eq!(
            last_layered,
            Some(0),
            "{theme_name}: the first frame drawn in screen space after the pan is composited \
             from layers that already show it"
        );
        frame.relayout();
        assert!(
            frame.paint_with_damage().cache.is_none(),
            "{theme_name}: a strip at rest is drawn in screen space"
        );
        if frame.layers_stale() {
            // The desktop changed at rest: the session warms the layers.
            let _ = warm_at_rest(&mut frame, &mut layered, &|| true, theme_name);
        }
        frame
            .host_mut()
            .update_active_strip_pan(0.0, true, settled_pan_report(0));
        frame
            .host_mut()
            .update_active_strip_pan(23.7, false, settled_pan_report(1));
        frame.relayout();
        let painted = frame.paint_with_damage();
        let label = format!("{theme_name}, second pan");
        let raster = present(&mut layered, &painted, &label)
            .unwrap_or_else(|| panic!("{label}: a pan comes as layers"));
        eprintln!(
            "strip layers {label}: raster {raster} px, composited {:?}",
            layered.last_layer_raster_pixels()
        );
        assert!(
            raster < output / 4,
            "{label}: a second pan from rest rasterises what it uncovered ({raster} px)"
        );
        assert_shows_a_full_render(&layered, &mut dense, &painted, &label);
    }
}

/// `target` shows what a full render of `painted` shows, within vello's
/// offset variance ([`OFFSET_PIXELS`], [`OFFSET_STEPS`]).
#[cfg(target_os = "linux")]
fn assert_shows_a_full_render(
    target: &crate::shell::render::paint::RetainedGpuTarget,
    dense: &mut crate::shell::render::paint::RetainedGpuTarget,
    painted: &FramePaint,
    label: &str,
) {
    let shown = target.read_rgba_for_test();
    let full = render_whole(dense, &painted.scene, painted.revision, label);
    let apart = pixels_apart(&shown, &full, target.size().0);
    let max = apart.iter().map(|&(_, _, step)| step).max().unwrap_or(0);
    assert!(
        apart.len() <= OFFSET_PIXELS && max <= OFFSET_STEPS,
        "{label}: {} pixels differ from a full render by more than one step (max {max}, first \
         {:?})",
        apart.len(),
        &apart[..apart.len().min(16)]
    );
}

/// Once the pan ends and its linger passes, the producer draws the strip in
/// screen space again, with screen-space damage.
#[cfg(target_os = "linux")]
fn assert_rests_in_screen_space(frame: &mut FrameBuilder, name: &str) {
    frame.host_mut().end_active_strip_pan(Instant::now(), false);
    for _ in 0..8 {
        frame.relayout();
        let _ = frame.paint_with_damage();
    }
    frame.relayout();
    assert!(
        frame.paint_with_damage().cache.is_none(),
        "{name}: a strip at rest is drawn in screen space"
    );
}

/// A layered pan steps by one device pixel, in layout pixels.
#[test]
fn layered_pans_step_by_one_device_pixel() {
    assert_eq!(pan_step(1.0), Some(1.0));
    assert_eq!(pan_step(2.0), Some(0.5));
    assert_eq!(pan_step(1.5), Some(1.0 / 1.5));
    assert_eq!(pan_step(1.25), Some(0.8));
    assert_eq!(pan_step(0.0), None);
    assert_eq!(pan_step(f64::NAN), None);
}

/// On a 150 % panel the presenter gets pans on whole device pixels: the
/// drawn pan is a multiple of two thirds of a layout pixel, each strip's
/// destination is its pan rect magnified
/// and rounded out to whole pixels, and its pan is the canvas shift in
/// device pixels.
#[cfg(target_os = "linux")]
#[test]
fn a_scaled_display_gets_strip_pans_on_device_pixels() {
    use crate::shell::state::StripId;
    let mut clock = tilo_test_env::ScopedEnv::new();
    let _ambient = DenyAmbientStarts::new();
    clock.set("TILO_CLOCK", "09:41");
    let desktop = PanDesktop {
        name: "150 %",
        panel: (1920, 1080),
        scale: 1.5,
        strips: 1,
    };
    let mut frame = pan_fixture(desktop, Theme::dark());
    frame.set_strip_pan_presenter(false, true);
    for _ in 0..4 {
        frame.relayout();
        let _ = frame.paint_with_damage();
    }
    frame
        .host_mut()
        .update_active_strip_pan(0.0, true, settled_pan_report(0));
    let mut travel = 0.0;
    for step in 0..6 {
        travel += 17.3;
        frame
            .host_mut()
            .update_active_strip_pan(travel, false, settled_pan_report(step + 1));
        frame.relayout();
        let painted = frame.paint_with_damage();
        let (id, scroll, max_scroll) = frame.host().strip_scroll_offsets()[0];
        let device = scroll * desktop.scale;
        assert!(
            (device - device.round()).abs() < 1e-5,
            "step {step}: the drawn pan {scroll} is {device} device pixels"
        );
        let cache = painted
            .cache
            .as_ref()
            .unwrap_or_else(|| panic!("step {step}: a pan on a scaled display comes as layers"));
        assert_eq!(cache.strips.len(), 1, "step {step}: one strip");
        let strip = cache.strips[0];
        let pans = frame
            .host()
            .strip_pan_rect(StripId(id))
            .expect("the strip pans");
        let expected = |value: f64| value * desktop.scale;
        assert_eq!(
            (strip.dest_x, strip.dest_y),
            (
                expected(pans.x0).floor() as u32,
                expected(pans.y0).floor() as u32
            ),
            "step {step}: the destination is the pan rect magnified, rounded out"
        );
        assert_eq!(
            strip.dest_y + strip.dest_h,
            (expected(pans.y1).ceil() as u32).min(desktop.panel.1),
            "step {step}"
        );
        let pad = frame.host().strip_canvas_pad(StripId(id));
        assert!(ui::widget::pan_within_canvas(scroll, max_scroll, pad));
        assert_eq!(
            f64::from(strip.scroll_x),
            ((scroll + pad) * desktop.scale).round(),
            "step {step}: the strip is read at its canvas shift in device pixels"
        );
    }
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
        "#3892 raster pixels ({label}): {pixels} of {output} ({:.3}%); a dense frame is {output}",
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
        vec![(
            ui::permissions::UserId("ada".into()),
            (at.0 as f32, at.1 as f32),
        )],
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

/// The overview fading in over two displays side by side: after the chord's
/// own frame every frame is sparse and repaints one display, and the pixels
/// a presenter keeps from those repairs are a dense render's, in every theme.
/// This is what the multi-display presenter shows for the fade, with no
/// overview-specific path of its own.
#[test]
fn overview_fade_repaints_one_display_and_matches_dense_in_every_theme() {
    use crate::shell::output_scene::OutputGeometry;

    for (name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high contrast", Theme::high_contrast()),
    ] {
        let (mut frame, _clock) = settled_builder_in(theme);
        frame.set_output_geometry(vec![
            OutputGeometry::unscaled(0.0, 0.0, 1920.0, 2160.0).with_id(0),
            OutputGeometry::unscaled(1920.0, 0.0, 1920.0, 2160.0).with_id(1),
        ]);
        frame.relayout();
        // The fade itself is under test, not the reduced-motion snap.
        frame.theme.reduce_motion = false;
        let mut frames = vec![frame.paint_with_damage(), frame.paint_with_damage()];
        frame.with_chrome(|chrome| {
            chrome.overlays.overview = true;
            chrome.bump();
        });
        frame.host_mut().latch_overview_transition();
        // Produce the fade first: rendering 4K oracles between samples
        // would outlast it.
        for _ in 0..3 {
            frames.push(frame.paint_with_damage());
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
        frames.push(frame.paint_with_damage());
        let mut retained = bridge();
        let mut dense = bridge();
        let mut repainted = false;
        for (index, painted) in frames.iter().enumerate() {
            let label = format!("overview fade, {name}, frame {index}");
            assert!(!painted.scene.encoding().is_empty(), "{label}: no desktop");
            // Two settled frames, then the chord's own frame, which may
            // change chrome beyond the overview once.
            if index > 2 {
                let damage = painted
                    .damage_regions
                    .as_deref()
                    .unwrap_or_else(|| panic!("{label}: must be sparse, not both displays"));
                let first = damage.iter().all(|rect| rect.x + rect.width <= 1920);
                let second = damage.iter().all(|rect| rect.x >= 1920);
                assert!(first || second, "{label}: {damage:?} spans both displays");
                repainted |= !damage.is_empty();
            }
            paint_retained_and_compare(&mut retained, &mut dense, painted, &label);
        }
        assert!(
            repainted,
            "overview fade, {name}: the fade repaints its display"
        );
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
        &ui::synthetic_input::cursor_moved(pill.center().x, pill.center().y),
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

// #4166: overlays, in-process app widgets and tooltips repaint what they
// change, not the output or the tile.

/// A theme by name.
type ThemeCase = (&'static str, fn() -> Theme);

const THEMES: [ThemeCase; 3] = [
    ("dark", Theme::dark),
    ("light", Theme::light),
    ("high contrast", Theme::high_contrast),
];

thread_local! {
    /// Keyboard focus as the compositor's event loop keeps it between
    /// events: a click places it, the next key press reads it.
    static FOCUS: std::cell::Cell<Option<NodeId>> = const { std::cell::Cell::new(None) };
}

/// [`settled_builder_in`], with no keyboard focus carried over from
/// another desktop.
fn desktop_in(theme: Theme) -> (PopulatedShellFixture, tilo_test_env::ScopedEnv) {
    FOCUS.with(|focus| focus.set(None));
    settled_builder_in(theme)
}

fn dispatch(frame: &mut FrameBuilder, event: &WindowEvent) {
    let mut document = automerge::AutoCommit::new();
    let mut focus = FOCUS.with(std::cell::Cell::get);
    let _ = frame.dispatch_event(
        event,
        &winit::event::Modifiers::default(),
        &mut ui::widget::Edits::lend(&mut document),
        &automerge::ROOT,
        &mut focus,
    );
    FOCUS.with(|cell| cell.set(focus));
}

fn point_at(frame: &mut FrameBuilder, at: Point) {
    dispatch(frame, &ui::synthetic_input::cursor_moved(at.x, at.y));
}

fn press(frame: &mut FrameBuilder, button: winit::event::MouseButton) {
    for state in [
        winit::event::ElementState::Pressed,
        winit::event::ElementState::Released,
    ] {
        dispatch(frame, &ui::synthetic_input::mouse_input(state, button));
    }
}

fn type_text(frame: &mut FrameBuilder, text: &str) {
    for c in text.chars() {
        dispatch(
            frame,
            &ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
                Key::Character(c.to_string().into()),
                Some(c.to_string().into()),
                winit::event::ElementState::Pressed,
            )),
        );
    }
}

/// Why each damage-tracked section of the latest frame could or could not
/// be compared part by part with `previous` (foreign images not paired).
fn part_refusals(previous: Option<&ProducedSections>, frame: &FrameBuilder) -> String {
    let (Some(old), Some(new)) = (previous, frame.last_produced_sections.as_ref()) else {
        return "missing source sections".into();
    };
    let compare = |old_scene: &Scene,
                   old_parts: &Option<ui::scene_parts::SectionParts>,
                   new_scene: &Scene,
                   new_parts: &Option<ui::scene_parts::SectionParts>| {
        match (old_parts, new_parts) {
            (Some(old_parts), Some(new_parts)) => format!(
                "{:?}",
                ui::scene_parts::damage(old_scene, old_parts, new_scene, new_parts)
            ),
            _ => "no parts".into(),
        }
    };
    // Where the workspace's pixels actually differ, when its commands do.
    let workspace_pixels = if old.workspace.encoding_eq(&new.workspace) {
        "same".to_owned()
    } else {
        let mut oracle = bridge();
        let before = render_dense(&mut oracle, &old.workspace);
        let after = render_dense(&mut oracle, &new.workspace);
        let mut bounds: Option<(usize, usize, usize, usize)> = None;
        for (index, (a, b)) in before.chunks(4).zip(after.chunks(4)).enumerate() {
            if a != b {
                let (x, y) = (index % NATIVE_WIDTH as usize, index / NATIVE_WIDTH as usize);
                bounds = Some(bounds.map_or((x, y, x, y), |(x0, y0, x1, y1)| {
                    (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
                }));
            }
        }
        // And which named widgets are there.
        let names = bounds.map_or_else(Vec::new, |(x0, y0, x1, y1)| {
            let px = |value: usize| f64::from(u32::try_from(value).unwrap_or(u32::MAX));
            let changed = Rect::new(px(x0), px(y0), px(x1) + 1.0, px(y1) + 1.0);
            frame
                .widget_registry()
                .into_iter()
                .filter(|(_, rect)| {
                    frame
                        .layout_rect_to_scene(*rect)
                        .is_some_and(|rect| !rect.intersect(changed).is_zero_area())
                })
                .map(|(name, rect)| format!("{name}@{rect:?}"))
                .collect()
        });
        format!("{bounds:?} under {names:?}")
    };
    format!(
        "workspace={} (pixels differ in {workspace_pixels}) overlays={} above={} | {}",
        compare(
            &old.workspace,
            &old.parts.workspace,
            &new.workspace,
            &new.parts.workspace
        ),
        compare(
            &old.workspace_overlays,
            &old.parts.workspace_overlays,
            &new.workspace_overlays,
            &new.parts.workspace_overlays
        ),
        compare(
            &old.above_quick_settings,
            &old.parts.above_quick_settings,
            &new.above_quick_settings,
            &new.parts.above_quick_settings
        ),
        sparse_refusal_details(previous, frame)
    )
}

/// What each in-process app handed the compositor with its last frame.
fn embedded_damage(frame: &FrameBuilder) -> String {
    frame
        .node_map
        .values()
        .filter_map(|&widget| {
            // SAFETY: node_map pointers are owned by the live widget tree and
            // remain valid while the frame is borrowed.
            let widget = unsafe { &*widget };
            let app = widget.as_any()?.downcast_ref::<ui::app::EmbeddedApp>()?;
            Some(format!(
                "[{:?}: {}]",
                app.get_rect(),
                app.damage_report_for_test()
            ))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The pixels a frame rasterises: the area of its damage rects (overlap
/// counted twice, which only overstates), or the whole output when dense.
fn raster_pixels(paint: &FramePaint) -> u64 {
    paint.damage_regions.as_deref().map_or_else(
        || u64::from(NATIVE_WIDTH) * u64::from(NATIVE_HEIGHT),
        |damage| {
            damage
                .iter()
                .map(|rect| u64::from(rect.width) * u64::from(rect.height))
                .sum()
        },
    )
}

/// Paint the settled desktop, make `change` and paint again. The change's
/// frame must be sparse, rasterise under `max_percent` % of the 4K output
/// and leave the pixels a dense render of it shows. Logs the raster pixels
/// as the issue's evidence and returns the damage.
fn assert_change_repaints_under(
    frame: &mut FrameBuilder,
    label: &str,
    max_percent: u64,
    change: impl FnOnce(&mut FrameBuilder),
) -> Vec<FineDamageRect> {
    let mut retained = bridge();
    let mut dense = bridge();
    let settled = frame.paint_with_damage();
    paint_retained_and_compare(
        &mut retained,
        &mut dense,
        &settled,
        &format!("{label}: before"),
    );
    let previous = frame.last_produced_sections.clone();
    change(frame);
    let changed = frame.paint_with_damage();
    let output = u64::from(NATIVE_WIDTH) * u64::from(NATIVE_HEIGHT);
    let pixels = raster_pixels(&changed);
    eprintln!(
        "#4166 raster pixels ({label}): {pixels} of {output} ({}); a dense frame is {output}",
        if changed.damage_regions.is_some() {
            "sparse"
        } else {
            "dense"
        }
    );
    let damage = changed.damage_regions.clone().unwrap_or_else(|| {
        panic!(
            "{label}: must be sparse: {}",
            part_refusals(previous.as_ref(), frame)
        )
    });
    assert!(
        pixels > 0 && pixels * 100 < output * max_percent,
        "{label}: must repaint under {max_percent}% of a 4K output, not {pixels} px: {damage:?}; {}; {}",
        part_refusals(previous.as_ref(), frame),
        embedded_damage(frame)
    );
    paint_retained_and_compare(&mut retained, &mut dense, &changed, label);
    // Damage the presenter finds unprofitable renders whole.
    assert!(
        !crate::shell::render::paint::damage_is_profitable(pixels, output)
            || retained.last_sparse_render_attempts() >= 1,
        "{label}: the change must exercise the production sparse render path"
    );
    damage
}

#[test]
fn typing_in_the_open_palette_repaints_the_palette_not_the_output() {
    for (name, theme) in THEMES {
        let (mut frame, _clock) = desktop_in(theme());
        frame.host_mut().open_palette_for_test();
        for _ in 0..3 {
            frame.relayout();
            let _ = frame.paint_with_damage();
        }
        let card = frame
            .widget_registry()
            .into_iter()
            .find(|(name, _)| name.ends_with("command_palette_input"))
            .map(|(_, rect)| rect)
            .expect("the open palette is a named target");
        let label = format!("typing in the open palette, {name}");
        // The card grows with its results: at 4K a 720 px card's old and
        // new extent with its shadow is about 5.4 % of the output.
        let damage = assert_change_repaints_under(&mut frame, &label, 6, |frame| {
            type_text(frame, "f");
            frame.relayout();
        });
        let card = frame
            .layout_rect_to_scene(card)
            .expect("the palette is on the output");
        assert_damage_covers(card, &damage, &label);
    }
}

#[test]
fn nested_damage_regions_are_rasterised_once() {
    let region = |x, y, width, height| FineDamageRect {
        x,
        y,
        width,
        height,
    };
    let card = region(100, 100, 300, 200);
    let grown = region(100, 100, 300, 400);
    let blur = region(120, 120, 200, 100);
    let elsewhere = region(900, 0, 10, 10);
    assert_eq!(
        without_contained_regions(&[card, grown, blur, elsewhere, grown]),
        vec![grown, elsewhere]
    );
}

#[test]
fn a_notification_appearing_repaints_its_card() {
    for (name, theme) in THEMES {
        let (mut frame, _clock) = desktop_in(theme());
        let label = format!("a notification appearing, {name}");
        let _ = assert_change_repaints_under(&mut frame, &label, 5, |frame| {
            frame.with_chrome(|chrome| {
                chrome.push_notification("Build finished", "tilo-ui passed its tests");
                chrome.bump();
            });
            frame.relayout();
        });
    }
}

#[test]
fn opening_a_tab_menu_repaints_the_menu() {
    for (name, theme) in THEMES {
        let (mut frame, _clock) = desktop_in(theme());
        let label = format!("opening a tab menu, {name}");
        // The menu key on the focused tab: the menu, and nothing a pointer
        // does to the bar on its way there.
        let _ = assert_change_repaints_under(&mut frame, &label, 5, |frame| {
            assert!(
                frame.host_mut().open_focused_tab_context_menu(),
                "the focused tab has a menu"
            );
            frame.relayout();
        });
        assert!(
            frame.host().overlay_shown_for_test(
                crate::shell::overlay_host::layers::LayerId::TabContextMenu
            ),
            "{label}: the menu is open"
        );
    }
}

/// Acknowledge whatever Notes wrote while starting (the 4K desktop mounts
/// the Notes surfaces it shows) and let the apps present it, so a later
/// save state does not change under the change being measured.
#[cfg(target_os = "linux")]
fn settle_notes(frame: &mut FrameBuilder) {
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
    let flushed = apps
        .borrow_mut()
        .flush_fixture_records(notes::definition().app_id, &room)
        .expect("Notes startup writes acknowledged");
    assert!(flushed > 0, "the desktop mounted Notes");
    for _ in 0..3 {
        frame.wake_mounted_components();
        frame.relayout();
        let _ = frame.paint_with_damage();
    }
}

/// `read` the Notes surface fully on screen, with its app's rect.
#[cfg(target_os = "linux")]
fn with_notes_on_screen<R>(
    frame: &FrameBuilder,
    read: impl FnOnce(Rect, &mut notes::NotesApp) -> R,
) -> R {
    let screen = Rect::new(0.0, 0.0, frame.viewport.0, frame.viewport.1);
    let mut seen = Vec::new();
    let found = frame.node_map.values().find_map(|&widget| {
        // SAFETY: node_map pointers are owned by the live widget tree and
        // remain valid while the frame is exclusively borrowed.
        let widget = unsafe { &mut *widget };
        let app = widget
            .as_any_mut()?
            .downcast_mut::<ui::app::EmbeddedApp>()?;
        let rect = app.get_rect();
        seen.push(rect);
        if rect.area() <= 0.0 || screen.intersect(rect) != rect {
            return None;
        }
        let notes = tilo_app::framework::testing::surface_in_root::<
            notes::NotesShell,
            notes::NotesApp,
        >(app.component_root_mut()?)?;
        Some((rect, std::ptr::from_mut(notes)))
    });
    let Some((rect, notes)) = found else {
        panic!("no Notes surface on screen; embedded apps at {seen:?}");
    };
    // SAFETY: the Notes surface lives in the widget tree `frame` borrows
    // exclusively for this call.
    read(rect, unsafe { &mut *notes })
}

#[cfg(target_os = "linux")]
fn settle(frame: &mut FrameBuilder, frames: usize) {
    for _ in 0..frames {
        frame.relayout();
        let _ = frame.paint_with_damage();
    }
}

/// Open a note in the Notes surface on screen, through its first-note call
/// to action when the notebook is empty, and return the centre of the
/// note's title in layout coordinates.
#[cfg(target_os = "linux")]
fn open_a_note(frame: &mut FrameBuilder, label: &str) -> Point {
    settle_notes(frame);
    let action = with_notes_on_screen(&*frame, |rect, notes| {
        notes
            .main_panel_mut()
            .empty_action_center_for_test()
            .map(|at| Point::new(rect.x0 + at.x, rect.y0 + at.y))
    });
    if let Some(action) = action {
        point_at(frame, action);
        press(frame, winit::event::MouseButton::Left);
        settle(frame, 4);
        settle_notes(frame);
    }
    let (opened, title) = with_notes_on_screen(&*frame, |rect, notes| {
        let title = notes.main_title_center_for_test();
        (
            notes.selected_for_test().is_some(),
            Point::new(rect.x0 + title.x, rect.y0 + title.y),
        )
    });
    assert!(
        opened,
        "{label}: Notes shows a note (first-note action {action:?})"
    );
    title
}

#[cfg(target_os = "linux")]
#[test]
fn typing_in_notes_repaints_what_changed_not_the_tile() {
    for (name, theme) in THEMES {
        let (mut frame, _clock) = desktop_in(theme());
        let label = format!("naming a note, {name}");
        let title = open_a_note(&mut frame, &label);
        if !with_notes_on_screen(&frame, |_, notes| notes.main_title_editing_for_test()) {
            point_at(&mut frame, title);
            press(&mut frame, winit::event::MouseButton::Left);
            settle(&mut frame, 2);
        }
        assert!(
            with_notes_on_screen(&frame, |_, notes| notes.main_title_editing_for_test()),
            "{label}: the note's title is being named"
        );
        type_text(&mut frame, "Plan");
        settle(&mut frame, 3);
        // The title is a part of the page of its own: a letter
        // repaints the title's band, not the page panel or the tile.
        let _ = assert_change_repaints_under(&mut frame, &label, 5, |frame| {
            type_text(frame, "s");
            frame.relayout();
        });

        // A key repaints its line, its caret and the save bubble whose state
        // it flips. The flip used to rebuild Notes' widget tree, so
        // every key repainted the tile (7,022,592 of 8,294,400 px).
        let label = format!("typing in a note, {name}");
        let body = with_notes_on_screen(&frame, |rect, notes| {
            let panel = notes.main_panel_mut();
            let title = panel.title_rect_for_test();
            let line = Point::new(title.x0 + 24.0, panel.body_top_for_test() + 12.0);
            Point::new(rect.x0 + line.x, rect.y0 + line.y)
        });
        point_at(&mut frame, body);
        press(&mut frame, winit::event::MouseButton::Left);
        settle(&mut frame, 3);
        type_text(&mut frame, "The quick brown fox ");
        settle(&mut frame, 3);
        let before = with_notes_on_screen(&frame, |_, notes| notes.editor_text_for_test());
        for letter in ["j", "u", "m", "p", "s"] {
            let _ = assert_change_repaints_under(&mut frame, &label, 5, |frame| {
                type_text(frame, letter);
                frame.relayout();
            });
        }
        let after = with_notes_on_screen(&frame, |_, notes| notes.editor_text_for_test());
        assert!(
            after.len() > before.len() && after.contains("fox jumps"),
            "{label}: the keys reached the note's text: {before:?} -> {after:?}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_typing_update_in_one_window_is_bounded_by_the_window_and_admitted_sparse() {
    let (mut frame, _clock) = desktop_in(Theme::dark());
    let label = "typing in a note, bounded by its window";
    let title = open_a_note(&mut frame, label);
    if !with_notes_on_screen(&frame, |_, notes| notes.main_title_editing_for_test()) {
        point_at(&mut frame, title);
        press(&mut frame, winit::event::MouseButton::Left);
        settle(&mut frame, 2);
    }
    let body = with_notes_on_screen(&frame, |rect, notes| {
        let panel = notes.main_panel_mut();
        let title = panel.title_rect_for_test();
        let line = Point::new(title.x0 + 24.0, panel.body_top_for_test() + 12.0);
        Point::new(rect.x0 + line.x, rect.y0 + line.y)
    });
    point_at(&mut frame, body);
    press(&mut frame, winit::event::MouseButton::Left);
    settle(&mut frame, 3);
    type_text(&mut frame, "The quick brown fox ");
    settle(&mut frame, 3);
    let layout_rect = with_notes_on_screen(&frame, |rect, _| rect);
    let window_rect = frame
        .layout_rect_to_scene(layout_rect)
        .expect("the window is on the output");
    let damage = assert_change_repaints_under(&mut frame, label, 100, |frame| {
        type_text(frame, "j");
        frame.relayout();
    });
    // The window's own tile, plus the tile rounding and shadow around it, and nothing else.
    let fringe = 64.0;
    for rect in &damage {
        let region = Rect::new(
            f64::from(rect.x),
            f64::from(rect.y),
            f64::from(rect.x + rect.width),
            f64::from(rect.y + rect.height),
        );
        assert!(
            region.x0 >= window_rect.x0 - fringe
                && region.y0 >= window_rect.y0 - fringe
                && region.x1 <= window_rect.x1 + fringe
                && region.y1 <= window_rect.y1 + fringe,
            "{label}: damage {region:?} leaves the window {window_rect:?}"
        );
    }
    // Notes rebuilds its widget tree per key and reports its whole tile, so the damage is the
    // window (87% of this 4K fixture, which Notes nearly fills); the presenter's gate decides
    // admission and `assert_change_repaints_under` compared the pixels with a dense render.
}

#[test]
fn a_blurred_surface_joins_only_the_damage_its_blur_can_see() {
    use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};
    let (frame, _clock) = desktop_in(Theme::dark());
    let blurred = |bounds: Rect| RecordedBackdrop {
        image: ImageData {
            data: Blob::new(Arc::new(vec![0, 0, 0, 255])),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::AlphaPremultiplied,
            width: 1,
            height: 1,
        },
        bounds,
        blur_px: 20.0,
        layer: 0,
        below: Scene::new(),
        ground: Color::BLACK,
    };
    let caret = FineDamageRect {
        x: 1_000,
        y: 1_000,
        width: 16,
        height: 32,
    };
    let near = blurred(Rect::new(1_020.0, 1_000.0, 1_400.0, 1_300.0));
    let far = blurred(Rect::new(2_000.0, 100.0, 3_000.0, 600.0));
    let nowhere = blurred(Rect::new(9_000.0, 9_000.0, 9_500.0, 9_500.0));
    let regions = frame
        .with_backdrop_damage(Some(vec![caret]), &[near.clone(), far, nowhere])
        .expect("a blurred surface far from the damage keeps it sparse");
    let near_bounds = frame
        .native_damage_rect(Some(near.bounds))
        .expect("on the output");
    assert_eq!(
        regions,
        vec![caret, near_bounds],
        "only the surface whose blur reaches the caret is repainted, and one on no display is not"
    );
    assert_eq!(
        frame.with_backdrop_damage(Some(Vec::new()), std::slice::from_ref(&near)),
        Some(Vec::new()),
        "an unchanged frame stays unchanged"
    );
    assert_eq!(frame.with_backdrop_damage(None, &[near]), None);
}

/// Moving focus to the other strip changes no geometry: the old and new
/// focused windows' edges, the strip ribbon and the strip rail. The frame
/// repaints those, not the stretch of desktop between the ribbon and the
/// rail, and shows what a dense render shows, in every theme.
#[cfg(target_os = "linux")]
#[test]
fn moving_to_another_strip_repaints_the_focus_and_the_strip_chrome() {
    for (name, theme) in THEMES {
        let mut clock = tilo_test_env::ScopedEnv::new();
        clock.set("TILO_CLOCK", "09:41");
        let mut frame = PopulatedShellFixture::with_tree(
            (NATIVE_WIDTH, NATIVE_HEIGHT),
            theme().with_reduce_motion(true),
            two_strip_tree(),
        );
        for _ in 0..8 {
            frame.relayout();
            let _ = frame.paint_with_damage();
        }
        // The six Notes windows' startup writes, acknowledged before the
        // move: Saving turning Saved is not what the move repaints.
        acknowledge_notes_saves_of(&mut frame, 6);
        frame.relayout();
        let _ = frame.paint_with_damage();
        for (direction, way) in [(1, "down"), (-1, "up")] {
            let label = format!("moving {way} a strip, {name}");
            // Two windows' edges (their corners and thin bands along their
            // sides, in 16-pixel tiles), the ribbon and the rail: 5.2-6.2 %
            // of a 4K output. Edge bands as deep as the corners made it
            // about 7.6 % (strip_pan_bench), and the whole stretch between
            // the ribbon and the rail was another 26 %.
            let damage = assert_change_repaints_under(&mut frame, &label, 7, |frame| {
                let mut moved = false;
                frame.with_chrome(|chrome| moved = chrome.switch_strip(direction));
                assert!(moved, "{label}: the desktop has another strip to move to");
                frame.relayout();
            });
            eprintln!("{label}: damage {damage:?}");
        }
    }
}

#[cfg(target_os = "linux")]
#[path = "frame_scroll_copy_tests.rs"]
mod scroll_copy;
