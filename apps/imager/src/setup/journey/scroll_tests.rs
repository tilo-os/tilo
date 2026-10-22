use super::*;
use crate::resource::GuidedInstallerShell;
use ui::framework::testing::TestApp;

fn inventory() -> super::super::flatpak_transfer::Inventory {
    use super::super::flatpak_transfer::{Installation, InstalledRef, Inventory};
    Inventory {
        refs: (0..71)
            .map(|index| InstalledRef {
                reference: format!("app/org.example.Application{index}/x86_64/stable"),
                origin: "example".into(),
                installation: Installation::User,
                name: format!("Application {index}"),
            })
            .collect(),
        remotes: Vec::new(),
    }
}

#[test]
fn expanded_belongings_wheel_retains_rows_and_footer() {
    // An explicit source selects read-only native discovery; the ordinary
    // regression uses deterministic metadata. Both exercise the same rows,
    // dispatch, counters and assertions, never the person's running desktop.
    let inventory = if let Some(home) = std::env::var_os("TILO_IMAGER_SCROLL_INVENTORY_HOME") {
        eprintln!("scroll inventory: native Flatpak discovery");
        super::super::flatpak_transfer::discover(
            &super::super::flatpak_transfer::NativeRunner,
            std::path::Path::new(&home),
        )
        .unwrap()
    } else {
        eprintln!("scroll inventory: deterministic regression metadata");
        inventory()
    };
    exercise_inventory(inventory);
}

// Mount the production journey directly for layout counters. The resource
// host can asynchronously refresh its own chrome, which is not journey
// composition. Semantic dispatch is checked separately through TestApp below.
struct JourneyHarness(ui::headless::FrameHarness);

impl JourneyHarness {
    fn harness(&mut self) -> &mut ui::headless::FrameHarness {
        &mut self.0
    }

    fn with_widget<T: 'static, O>(&mut self, visit: impl FnOnce(&mut T) -> O) -> Option<O> {
        self.0.widget_mut::<T>().map(visit)
    }

    fn settle(&mut self) {
        self.0.relayout();
    }
}

fn prepare(preview: &mut InstallerPreview, inventory: super::super::flatpak_transfer::Inventory) {
    preview.live = true;
    preview.restart = restart_controller::Restart::default();
    preview.journey.page = Page::Belongings;
    preview.journey.destination = Some(Destination::Another);
    preview.journey.migrate = Some(true);
    preview.journey.categories = [false, true, false];
    preview.journey.details = true;
    preview.belongings.flatpak = Some(inventory);
    preview.dirty = true;
}

fn exercise_inventory(inventory: super::super::flatpak_transfer::Inventory) {
    let private = tempfile::tempdir().unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("HOME", private.path());
    for (key, directory) in [
        ("XDG_STATE_HOME", "state"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_RUNTIME_DIR", "runtime"),
    ] {
        let directory = private.path().join(directory);
        std::fs::create_dir(&directory).unwrap();
        environment.set(key, directory);
    }
    environment.remove("DISPLAY");
    environment.remove("WAYLAND_DISPLAY");
    environment.set(
        "DBUS_SESSION_BUS_ADDRESS",
        format!("unix:path={}/no-session-bus", private.path().display()),
    );
    let action_inventory = inventory.clone();
    let mut app = JourneyHarness(ui::headless::FrameHarness::new_with_theme(
        ui::State::new(),
        Box::new(InstallerPreview::default()),
        (1180, 820),
        Theme::dark(),
    ));
    app.with_widget::<InstallerPreview, _>(|preview| prepare(preview, inventory))
        .unwrap();
    app.settle();
    let row_count = app
        .with_widget::<InstallerPreview, _>(|preview| preview.flatpak_apps().len())
        .unwrap();
    assert!(
        row_count >= 71,
        "the actual rendered journey must retain every application row"
    );
    let (body, footer, first) = app
        .with_widget::<InstallerPreview, _>(|preview| {
            let (body, overflowing) = visual_tests::body_scroll_geometry(preview).unwrap();
            assert!(overflowing);
            let footer = visual_tests::control(preview, "installer.next")
                .unwrap()
                .get_rect();
            let first = visual_tests::control(preview, "installer.flatpak.app.0")
                .unwrap()
                .get_rect();
            (body, footer, first)
        })
        .unwrap();
    capture(&mut app, "top");
    app.harness().click_at(body.x0 + 4.0, body.y0 + 4.0);
    let nodes = app.harness().layout_node_ids();
    let rebuilt = app.harness().rebuilt_frames();
    let frames = app.harness().relayout_frames();
    let (cold_hits, cold_misses) = app.harness().text_cache_stats();
    assert!(
        cold_hits > 0 && cold_misses > 0,
        "the production renderer must have measured actual labels"
    );
    let (_, cold_shapes, _) = app.harness().take_text_shape_stats();
    assert!(
        cold_shapes > 0,
        "the production text shaper must have run during warmup"
    );
    app.harness().begin_profiling();
    let mut measured_frames = 0;
    let mut measured_leaves = 0;
    for _ in 0..6 {
        app.harness().scroll(0.0, -1.0);
        let layout = app.harness().last_relayout();
        eprintln!("journey scroll layout: {layout:?}");
        measured_frames += usize::from(layout.measured);
        measured_leaves += layout.marked_leaves;
    }
    let samples = app.harness().take_profile_samples();
    assert!(
        !samples.is_empty(),
        "profile must observe production frames"
    );
    eprintln!(
        "journey scroll production component frames={}, total_us={}, layout_us={}, draw_us={}",
        samples.len(),
        samples.iter().map(|frame| frame.total_us).sum::<u64>(),
        samples
            .iter()
            .map(|frame| frame.build_layout_us)
            .sum::<u64>(),
        samples.iter().map(|frame| frame.draw_us).sum::<u64>(),
    );
    assert!(app.harness().relayout_frames() > frames);
    assert_eq!(app.harness().rebuilt_frames(), rebuilt);
    assert_eq!(app.harness().layout_node_ids(), nodes);
    let (hits, misses) = app.harness().text_cache_stats();
    eprintln!(
        "journey scroll rows={row_count}, text cache before=({cold_hits},{cold_misses}), after=({hits},{misses})"
    );
    assert_eq!(misses, cold_misses, "wheel must retain text measurements");
    let (shape_hits, shape_misses, shape_ns) = app.harness().take_text_shape_stats();
    eprintln!(
        "journey scroll cold_shapes={cold_shapes}, shape_hits={shape_hits}, shape_misses={shape_misses}, shape_ns={shape_ns}"
    );
    assert_eq!(
        shape_misses, 0,
        "wheel must reuse shaped application labels"
    );
    assert_eq!(
        (measured_frames, measured_leaves),
        (0, 0),
        "wheel frames must translate already-solved rows rather than invalidate their measurements"
    );
    app.with_widget::<InstallerPreview, _>(|preview| {
        let moved = visual_tests::control(preview, "installer.flatpak.app.0")
            .unwrap()
            .get_rect();
        assert!(moved.y0 < first.y0, "wheel must actually move content");
        assert_eq!(
            visual_tests::control(preview, "installer.next")
                .unwrap()
                .get_rect(),
            footer
        );
        assert!(body.y1 <= footer.y0);
        assert!(
            visual_tests::control(preview, &format!("installer.flatpak.app.{}", row_count - 1))
                .is_some()
        );
    })
    .unwrap();
    app.harness().key("End");
    app.with_widget::<InstallerPreview, _>(|preview| {
        let last =
            visual_tests::control(preview, &format!("installer.flatpak.app.{}", row_count - 1))
                .unwrap()
                .get_rect();
        assert!(
            last.y0 >= body.y0 && last.y1 <= body.y1,
            "keyboard scroll must reveal the last application above the footer: {last:?} {body:?}"
        );
        assert_eq!(
            visual_tests::control(preview, "installer.next")
                .unwrap()
                .get_rect(),
            footer
        );
    })
    .unwrap();
    capture(&mut app, "bottom");
    drop(app);
    let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer-preview"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| prepare(preview, action_inventory))
        .unwrap();
    app.settle();
    let choice = app
        .with_widget::<InstallerPreview, _>(|preview| preview.flatpak_apps()[0].id.clone())
        .unwrap();
    app.effect(&InstallerPreviewEffect::SetPreviewFlatpakApp {
        app: choice,
        included: false,
    })
    .unwrap();
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert_eq!(
            preview.flatpak_apps().len(),
            row_count,
            "selection must not remove rows"
        );
        assert!(!preview.flatpak_apps()[0].included);
        let checkbox = visual_tests::control(preview, "installer.flatpak.app.0")
            .unwrap()
            .accessibility()
            .unwrap();
        assert_eq!(
            checkbox.state.checked,
            Some(false),
            "offscreen semantic edits still update accessibility"
        );
    })
    .unwrap();
}

fn capture(app: &mut JourneyHarness, phase: &str) {
    if let Some(directory) = std::env::var_os("TILO_UI_SCREENSHOT_DIR") {
        let directory = std::path::PathBuf::from(directory).join("installer-journey");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("belongings-scroll-{phase}.png"));
        app.harness().render_png(&path);
        eprintln!("private headless scroll evidence: {}", path.display());
    }
}
