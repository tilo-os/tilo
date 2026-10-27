#![allow(
    clippy::field_reassign_with_default,
    reason = "capture integration tests keep their full scenario setup explicit"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]

use super::*;
use crate::shell::state::{AppKind, Tab, TileContent, WindowGroup, WindowWidth, WorkspaceTree};
use ui::Widget;
use ui::widget::TaffyTreeExt;
use vello::kurbo::RoundedRect;
use vello::peniko::{Color, Fill};

/// A window whose document runs four times its own height, which it can
/// paint whole without scrolling: what a capture of the complete app
/// asks a window for.
#[derive(Default)]
struct LongDocument {
    rect: Rect,
}

impl LongDocument {
    fn paint(scene: &mut vello::Scene, transform: Affine, bounds: Rect) {
        scene.fill(
            Fill::NonZero,
            transform,
            Color::new([0.85, 0.85, 0.9, 1.0]),
            None,
            &bounds,
        );
    }
}

impl Widget for LongDocument {
    ui::provides!(dyn ui::widget::CompleteContentProvider);

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        taffy.fill_leaf(node_map, self)
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        _text: &mut ui::text_render::TextRenderer,
        _theme: &ui::theme::Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        Self::paint(scene, transform, self.rect);
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "LongDocument"
    }
}

impl ui::widget::CompleteContentProvider for LongDocument {
    fn complete_content_size(&self) -> Option<vello::kurbo::Size> {
        (self.rect.height() > 0.0)
            .then(|| vello::kurbo::Size::new(self.rect.width(), self.rect.height() * 4.0))
    }

    fn draw_complete_content(
        &mut self,
        scene: &mut vello::Scene,
        bounds: Rect,
        transform: Affine,
        _text: &mut ui::text_render::TextRenderer,
        _theme: &ui::theme::Theme,
        _presence: &ui::presence::PresenceCtx,
    ) -> bool {
        Self::paint(scene, transform, bounds);
        true
    }
}

impl ui::widget::AppWidget for LongDocument {
    fn app_kind(&self) -> ui::widget::AppKind {
        ui::widget::AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Long document"
    }
    fn app_icon(&self) -> ui::widget::IconDescriptor {
        ui::widget::IconDescriptor::Glyph("L")
    }
}

fn two_source_cards() -> Scene {
    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(220, 48, 64),
        None,
        &RoundedRect::from_rect(Rect::new(-300.0, 20.0, -100.0, 120.0), 12.0),
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(48, 96, 220),
        None,
        &RoundedRect::from_rect(Rect::new(900.0, 40.0, 1_100.0, 140.0), 12.0),
    );
    scene
}

/// The screenshot picker is not part of the desktop scene a screenshot is
/// cropped from: it paints over it on every frame, so its hover,
/// selection and tooltips reach the screen, and it never reaches the
/// screenshot.
#[test]
fn the_screenshot_picker_paints_live_over_the_desktop() {
    let _serial = super::freeze_tests::serial();
    let mut fb = FrameBuilder::new((800, 600));
    fb.relayout();
    assert!(
        fb.host_mut()
            .open_capture_studio(ui::widget::CaptureMode::Screenshot)
    );
    fb.relayout();
    assert!(fb.capture_choosing_screenshot());

    let mut executor = CaptureExecutor::default();
    for frame in ["first", "second"] {
        let mut presented = fb.paint();
        let desktop = presented.clone();
        let (_, appended) = executor.execute_ready(&mut fb, &mut presented);
        assert!(
            appended,
            "{frame} frame: the picker paints over the desktop"
        );
        assert!(
            presented.encoding().draw_tags.len() > desktop.encoding().draw_tags.len(),
            "{frame} frame: the presented frame is the desktop plus the picker"
        );
    }
    assert!(crate::shell::foreign::store().release_frames());
}

#[test]
fn screenshot_artifacts_keep_exact_pixels_and_exclusive_selection() {
    // Phase `saved_screenshot_round_trips_exact_pixels_for_receipt_copy`.
    {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("finished-capture.png");
        let pixels = vec![
            18, 32, 64, 255, 240, 120, 30, 255, 4, 220, 180, 96, 255, 255, 255, 0,
        ];

        write_png(&path, 2, 2, &pixels).unwrap();
        let (width, height, decoded) = read_png_rgba(&path).unwrap();

        assert_eq!((width, height), (2, 2));
        assert_eq!(decoded, pixels);
    }
    // Phase `portal_selector_reservation_is_exclusive_and_cancellable`.
    {
        let mut executor = CaptureExecutor::default();
        assert!(executor.arm_portal_screenshot());
        assert!(!executor.arm_portal_screenshot());
        executor.cancel_portal_screenshot();
        assert!(executor.arm_portal_screenshot());
    }
    // Phase `screenshot_open_requires_a_finished_file_artifact`.
    {
        let mut executor = CaptureExecutor::default();
        let mut frame_builder = FrameBuilder::new((800, 600));
        executor.open_last_screenshot();
        assert_eq!(
            executor.finish_io_blocking(&mut frame_builder),
            vec![CaptureOutcome::Failed(
                "there is no saved screenshot to open".into()
            )]
        );
    }
    // Phase `pin_uses_finished_pixels_and_does_not_create_a_live_mirror`.
    {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("finished-capture.png");
        let pixels = vec![18, 32, 64, 255, 240, 120, 30, 255];
        write_png(&path, 2, 1, &pixels).unwrap();

        let mut executor = CaptureExecutor::default();
        executor.last_screenshot.set(path);
        let mut frame_builder = FrameBuilder::new((800, 600));
        let spawn = Rect::new(280.0, 6.0, 326.0, 30.0);
        executor.pin_last_screenshot(&[spawn]);
        let outcome = executor.finish_io_blocking(&mut frame_builder);

        assert_eq!(
            outcome,
            vec![CaptureOutcome::Pinned {
                id: "pinned-capture:0".into()
            }]
        );
        assert!(
            executor.live_mirrors.is_empty(),
            "a pinned still must never enter the live refresh loop"
        );
        assert_eq!(frame_builder.host_mut().capture_mirror_count_for_tests(), 1);
    }
    // Phase `screenshot_preview_preserves_spatial_color_structure`.
    {
        let mut rgba = Vec::new();
        for row in 0..4u8 {
            for column in 0..6u8 {
                rgba.extend_from_slice(&[
                    column.saturating_mul(30),
                    row.saturating_mul(50),
                    column.saturating_add(row).saturating_mul(20),
                    255,
                ]);
            }
        }
        let preview = capture_preview(&rgba, 6, 4);
        assert_eq!((preview.columns, preview.rows), (6, 4));
        assert_eq!(preview.pixels.len(), 24);
        assert_eq!(preview.pixels[0], [0, 0, 0, 255]);
        assert_eq!(preview.pixels[5], [150, 0, 100, 255]);
        assert_eq!(preview.pixels[18], [0, 150, 60, 255]);
        assert_eq!(preview.pixels[23], [150, 150, 160, 255]);
    }
}

/// A screenshot taken through the main action is a file in Screenshots
/// and an image/png offer on the clipboard, with the same pixels, and
/// its receipt says it is both.
#[test]
fn a_screenshot_is_saved_into_screenshots_and_offered_on_the_clipboard() {
    use crate::compositor::selection::{
        SelectionStore, Target, clipboard_publication_pending, host_services, mirror_bytes,
        mirror_mime_types,
    };
    let store: crate::compositor::selection::SharedSelection =
        std::sync::Arc::new(std::sync::Mutex::new(SelectionStore::default()));
    let _host = ui::host::enter(&host_services(store.clone()));
    let pictures = tempfile::tempdir().unwrap();
    let mut executor = CaptureExecutor {
        pictures: Some(pictures.path().to_path_buf()),
        ..CaptureExecutor::default()
    };
    let mut frame_builder = FrameBuilder::new((800, 600));
    frame_builder.relayout();
    let (width, height) = (37_u32, 23_u32);
    let rgba: Vec<u8> = (0..width * height)
        .flat_map(|i| {
            let i = u8::try_from(i % 251).unwrap();
            [i, 255 - i, i / 2, 255]
        })
        .collect();
    let path = next_screenshot_path(executor.pictures.clone(), 0).unwrap();
    executor.write_screenshot(
        ScreenshotToWrite {
            path: path.clone(),
            width,
            height,
            rgba: rgba.clone(),
            preview: capture_preview(&rgba, width, height),
        },
        true,
        ScreenshotReceipt::Shell,
    );
    let outcomes = executor.finish_io_blocking(&mut frame_builder);

    assert_eq!(outcomes, vec![CaptureOutcome::Saved(path.clone())]);
    assert!(
        path.starts_with(pictures.path().join("Screenshots")),
        "saved in Screenshots: {}",
        path.display()
    );
    let (saved_width, saved_height, saved) = read_png_rgba(&path).unwrap();
    assert_eq!((saved_width, saved_height), (width, height));
    assert_eq!(saved, rgba, "the file holds the exact pixels");

    assert!(
        mirror_mime_types(&store, Target::Clipboard)
            .iter()
            .any(|mime| mime == "image/png"),
        "the clipboard offers image/png"
    );
    assert!(
        clipboard_publication_pending(&store),
        "and the offer goes out to every app as the selection"
    );
    let png = mirror_bytes(&store, Target::Clipboard, "image/png").unwrap();
    let copied = ui::png::decode_rgba8(&png).unwrap();
    assert_eq!((copied.width, copied.height), (width, height));
    assert_eq!(copied.rgba, rgba, "the clipboard holds the same pixels");

    assert_eq!(
        frame_builder.host().capture_screenshot_kept(),
        Some(ui::widget::ScreenshotKept::SAVED_AND_COPIED),
        "the receipt says it was copied and saved"
    );
}

/// Puts an earlier copy on the clipboard before a screenshot.
type Arrange = fn(&crate::compositor::selection::SharedSelection);

// Every target (a selection, several selections, a window, the strip, a
// screen, Print and its instant variants) ends in this one step: the
// pixels are written to the file and then copied. Whatever the clipboard
// held before, afterwards it offers the screenshot and only the
// screenshot, as a compositor-owned selection that outlives the capture
// tool and reaches apps started later.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one pass per earlier clipboard, each asserted the same way"
)]
fn every_screenshot_is_copied_to_the_clipboard_whatever_it_held_before() {
    use crate::compositor::selection::{
        SelectionStore, TEXT_MIME, Target, adopt_complete_client_selection, begin_client_selection,
        clipboard_publication_pending, compositor_owns, host_services, mirror_bytes,
        mirror_mime_types, mirror_text, record_client_bytes,
    };
    let pixels = |width: u32, height: u32, seed: u8| -> Vec<u8> {
        (0..width * height)
            .flat_map(|i| {
                let i = u8::try_from(i % 241).unwrap().wrapping_add(seed);
                [i, 255 - i, seed, 255]
            })
            .collect()
    };
    let earlier_clipboards: [(&str, Arrange); 4] = [
        ("nothing", |_| {}),
        ("a copy taken over from an app that has gone", |store| {
            let mimes = vec![TEXT_MIME.to_string(), "text/plain".to_string()];
            let generation = begin_client_selection(store, Target::Clipboard, Some(mimes));
            for mime in [TEXT_MIME, "text/plain"] {
                record_client_bytes(
                    store,
                    Target::Clipboard,
                    generation,
                    mime,
                    b"text an app copied".to_vec(),
                );
            }
            assert!(
                adopt_complete_client_selection(store, 1),
                "precondition: the compositor took the app's copy over"
            );
        }),
        ("text copied in the shell", |_| {
            ui::clipboard_copy("text copied in the shell");
        }),
        ("an earlier screenshot", |_| {
            let earlier = [9_u8, 9, 9, 255].repeat(4 * 4);
            assert_eq!(
                ui::clipboard_copy_image(4, 4, &earlier).outcome(),
                Some(true),
                "precondition: the earlier screenshot was copied"
            );
        }),
    ];
    for (index, (earlier, arrange)) in earlier_clipboards.into_iter().enumerate() {
        let store: crate::compositor::selection::SharedSelection =
            std::sync::Arc::new(std::sync::Mutex::new(SelectionStore::default()));
        let _host = ui::host::enter(&host_services(store.clone()));
        arrange(&store);
        let pictures = tempfile::tempdir().unwrap();
        let mut executor = CaptureExecutor {
            pictures: Some(pictures.path().to_path_buf()),
            ..CaptureExecutor::default()
        };
        let mut frame_builder = FrameBuilder::new((800, 600));
        frame_builder.relayout();
        let (width, height) = (29_u32, 17_u32);
        let rgba = pixels(width, height, u8::try_from(index).unwrap());
        let path = next_screenshot_path(executor.pictures.clone(), 0).unwrap();
        executor.write_screenshot(
            ScreenshotToWrite {
                path: path.clone(),
                width,
                height,
                rgba: rgba.clone(),
                preview: capture_preview(&rgba, width, height),
            },
            true,
            ScreenshotReceipt::Shell,
        );
        let outcomes = executor.finish_io_blocking(&mut frame_builder);
        assert_eq!(
            outcomes,
            vec![CaptureOutcome::Saved(path)],
            "after {earlier}"
        );

        assert_eq!(
            mirror_mime_types(&store, Target::Clipboard),
            vec!["image/png".to_string()],
            "after {earlier}: the clipboard offers the screenshot and nothing \
             else, so no app pastes what was there before"
        );
        assert_eq!(
            mirror_text(&store, Target::Clipboard),
            None,
            "after {earlier}: the earlier text is gone"
        );
        let png = mirror_bytes(&store, Target::Clipboard, "image/png")
            .unwrap_or_else(|| panic!("after {earlier}: the clipboard holds a PNG"));
        let copied = ui::png::decode_rgba8(&png).unwrap();
        assert_eq!(
            (copied.width, copied.height, copied.rgba),
            (width, height, rgba),
            "after {earlier}: the clipboard holds this screenshot's pixels"
        );
        assert!(
            compositor_owns(&store, Target::Clipboard),
            "after {earlier}: the compositor owns the copy, so it outlives the tool"
        );
        assert!(
            clipboard_publication_pending(&store),
            "after {earlier}: the copy goes out to every app as the selection"
        );
        assert_eq!(
            frame_builder.host().capture_screenshot_kept(),
            Some(ui::widget::ScreenshotKept::SAVED_AND_COPIED),
            "after {earlier}: the receipt says it was copied"
        );
    }
}

/// Pinning the last screenshot decodes it on the capture worker: while
/// its bytes do not arrive, every frame returns without the pin, and the
/// pin lands on the first frame after they do.
#[test]
fn reading_the_last_screenshot_never_holds_a_frame() {
    let directory = tempfile::tempdir().unwrap();
    let finished = directory.path().join("finished.png");
    write_png(&finished, 2, 1, &[18, 32, 64, 255, 240, 120, 30, 255]).unwrap();
    let bytes = std::fs::read(&finished).unwrap();
    let hydrating =
        crate::shell::disk_sync_guard::blocking_source(directory.path(), "hydrating.png");

    let mut executor = CaptureExecutor::default();
    executor.last_screenshot.set(hydrating.clone());
    let mut frame_builder = FrameBuilder::new((800, 600));
    frame_builder.relayout();
    executor.pin_last_screenshot(&[Rect::new(280.0, 6.0, 326.0, 30.0)]);
    for _ in 0..3 {
        let mut scene = frame_builder.paint();
        let (outcomes, _) = executor.execute_ready(&mut frame_builder, &mut scene);
        assert!(
            outcomes.is_empty(),
            "nothing to report while the read blocks"
        );
    }
    assert!(
        executor.io.is_busy(),
        "the decode still waits for its bytes"
    );

    std::fs::write(&hydrating, bytes).unwrap();
    tilo_test_env::wait_until("the decode to finish", || executor.has_finished_io());
    let mut scene = frame_builder.paint();
    let (outcomes, _) = executor.execute_ready(&mut frame_builder, &mut scene);
    assert_eq!(
        outcomes,
        vec![CaptureOutcome::Pinned {
            id: "pinned-capture:0".into()
        }]
    );
    assert_eq!(frame_builder.host_mut().capture_mirror_count_for_tests(), 1);
}

/// "Save to this workspace's files" copies onto the workspace mount,
/// which may be a peer's: the copy runs on the capture worker. While
/// the worker is held up, frames return and nothing lands in the
/// workspace; once it runs, the copy is reported on a later frame.
#[test]
fn saving_a_screenshot_into_the_workspace_never_holds_a_frame() {
    let directory = tempfile::tempdir().unwrap();
    let shot = directory.path().join("Tilo Screenshot.png");
    write_png(&shot, 2, 1, &[18, 32, 64, 255, 240, 120, 30, 255]).unwrap();
    let workspace = directory.path().join("workspace");

    let mut executor = CaptureExecutor::default();
    executor.last_screenshot.set(shot.clone());
    let (release, held) = std::sync::mpsc::channel::<()>();
    executor.io.submit(move || {
        let _ = held.recv();
        CaptureDone::new(|_, _| None)
    });
    let mut frame_builder = FrameBuilder::new((800, 600));
    frame_builder.relayout();
    executor.send_last_screenshot(ScreenshotSendTarget::Workspace, workspace.clone());
    for _ in 0..3 {
        let mut scene = frame_builder.paint();
        let (outcomes, _) = executor.execute_ready(&mut frame_builder, &mut scene);
        assert!(
            outcomes.is_empty(),
            "nothing to report while the copy waits"
        );
    }
    let landed = workspace
        .join(crate::shell::screenshot_send::WORKSPACE_SCREENSHOTS_DIR)
        .join("Tilo Screenshot.png");
    assert!(
        !landed.exists(),
        "the copy did not run on the frame's thread"
    );

    release.send(()).unwrap();
    let outcomes = tilo_test_env::wait_for("the copy to finish", || {
        let mut scene = frame_builder.paint();
        let outcomes = executor.execute_ready(&mut frame_builder, &mut scene).0;
        (!outcomes.is_empty()).then_some(outcomes)
    });
    assert_eq!(
        outcomes,
        vec![CaptureOutcome::ScreenshotSent(Ok(
            crate::shell::screenshot_send::SendReceipt::Saved(landed.clone())
        ))]
    );
    assert_eq!(
        std::fs::read(&landed).unwrap(),
        std::fs::read(&shot).unwrap()
    );
}

/// "Send…" to a device reads the screenshot on the capture worker, as
/// the copy into the workspace does: while the worker is held up,
/// frames return with nothing to offer. The outcome then carries what
/// the worker read, and the loop's offer reads no file: the screenshot
/// may be gone from disk by then and the offer still holds its bytes.
#[test]
fn sending_a_screenshot_to_a_device_never_reads_it_on_the_loop() {
    struct Phone {
        offered: Vec<Vec<u8>>,
    }
    impl crate::shell::screenshot_send::ScreenshotSendBridge for Phone {
        fn devices(&self) -> Vec<ui::widget::ScreenshotSendDevice> {
            vec![ui::widget::ScreenshotSendDevice {
                id: "device-phone".into(),
                label: "Ada's phone".into(),
            }]
        }
        fn offer_to_device(
            &mut self,
            id: &str,
            screenshot: crate::shell::device_fabric_runtime::PreparedNearbyDrop,
        ) -> Result<(), String> {
            if id != "device-phone" {
                return Err(format!("offered to {id}"));
            }
            let crate::shell::device_fabric_runtime::PreparedNearbyDrop::Inline { bytes, .. } =
                screenshot
            else {
                return Err("a small screenshot is offered inline".into());
            };
            self.offered.push(bytes);
            Ok(())
        }
        fn save_to_workspace(&mut self, _path: &std::path::Path) -> Result<PathBuf, String> {
            Err("a device send never saves to the workspace".into())
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let shot = directory.path().join("Tilo Screenshot.png");
    write_png(&shot, 2, 1, &[18, 32, 64, 255, 240, 120, 30, 255]).unwrap();
    let bytes = std::fs::read(&shot).unwrap();

    let mut executor = CaptureExecutor::default();
    executor.last_screenshot.set(shot.clone());
    let (release, held) = std::sync::mpsc::channel::<()>();
    executor.io.submit(move || {
        let _ = held.recv();
        CaptureDone::new(|_, _| None)
    });
    let mut frame_builder = FrameBuilder::new((800, 600));
    frame_builder.relayout();
    executor.send_last_screenshot(
        ScreenshotSendTarget::Device("device-phone".into()),
        directory.path().join("workspace"),
    );
    for _ in 0..3 {
        let mut scene = frame_builder.paint();
        let (outcomes, _) = executor.execute_ready(&mut frame_builder, &mut scene);
        assert!(outcomes.is_empty(), "nothing to offer while the read waits");
    }

    release.send(()).unwrap();
    let outcomes = tilo_test_env::wait_for("the read to finish", || {
        let mut scene = frame_builder.paint();
        let outcomes = executor.execute_ready(&mut frame_builder, &mut scene).0;
        (!outcomes.is_empty()).then_some(outcomes)
    });
    let [CaptureOutcome::ScreenshotSendRequested { device, screenshot }] = outcomes.as_slice()
    else {
        panic!("a device send is handed to the loop: {outcomes:?}");
    };
    assert_eq!(device, "device-phone");

    std::fs::remove_file(&shot).unwrap();
    let mut phone = Phone {
        offered: Vec::new(),
    };
    let receipt =
        crate::shell::screenshot_send::offer(device, screenshot.take(), &mut phone).unwrap();
    assert_eq!(
        receipt,
        crate::shell::screenshot_send::SendReceipt::Offered {
            device: "Ada's phone".into()
        }
    );
    assert_eq!(
        phone.offered,
        vec![bytes],
        "the worker's read is what is offered"
    );
}

#[test]
fn pip_targets_retire_with_their_app_and_keep_virtual_bounds() {
    // Phase `in_process_pip_retires_when_its_app_activity_disappears`.
    {
        let mut executor = CaptureExecutor::default();
        executor.live_mirrors.push(LiveMirror {
            id: "browser.media-pip".into(),
            title: "Video".into(),
            intent: CaptureIntent {
                mode: CaptureMode::Pip,
                source: ui::widget::SourceMode::Region,
                destination: CaptureDestination::FloatingWindow,
                regions: vec![Rect::new(100.0, 80.0, 500.0, 305.0)],
                anchors: Vec::new(),
                include_pointer: false,
                audio: AudioCaptureMode::None,
                pip_layout: PipLayout::Auto,
                targets: Vec::new(),
                recording_command: None,
                replay_command: None,
                screenshot_command: None,
            },
            renderer: None,
            preferred_size: Some(Size::new(400.0, 225.0)),
            tracking_source: None,
            close_when_app_inactive: true,
            spawn_from: None,
        });
        let mut frame_builder = FrameBuilder::new((800, 600));
        let mut clean = frame_builder.paint();

        let (outcomes, _capture_overlays_appended) =
            executor.execute_ready(&mut frame_builder, &mut clean);
        assert_eq!(
            outcomes,
            vec![CaptureOutcome::FloatingClosed {
                id: "browser.media-pip".into()
            }]
        );
        assert!(executor.live_mirrors.is_empty());
    }
    // Phase `offscreen_target_retains_virtual_bounds_for_pip_handoff`.
    {
        let virtual_rect = Rect::new(-1_300.0, 120.0, -300.0, 820.0);
        let intent = CaptureIntent {
            mode: CaptureMode::Pip,
            source: ui::widget::SourceMode::Window,
            destination: CaptureDestination::FloatingWindow,
            regions: Vec::new(),
            anchors: Vec::new(),
            include_pointer: false,
            audio: AudioCaptureMode::None,
            pip_layout: PipLayout::Auto,
            targets: vec![ui::widget::CaptureTarget {
                id: "notes".into(),
                title: "Notes".into(),
                app_id: "so.tilo.notes".into(),
                visible_rect: Rect::ZERO,
                virtual_rect,
                complete_rect: Some(Rect::new(-1_300.0, 120.0, 10.0, 1_308.0)),
                ..Default::default()
            }],
            recording_command: None,
            replay_command: None,
            screenshot_command: None,
        };
        assert_eq!(intent_spawn_bounds(&intent), Some(virtual_rect));
    }
}

#[test]
fn spatial_capture_geometry_frames_regions_without_desktop_gaps() {
    // Phase `spatial_multi_capture_leaves_unselected_gap_transparent`.
    fn spatial_multi_capture_leaves_unselected_gap_transparent() {
        let source = two_source_cards();
        let regions = [
            Rect::new(-300.0, 20.0, -100.0, 120.0),
            Rect::new(900.0, 40.0, 1_100.0, 140.0),
        ];
        let (scene, width, height) = compose_scene(&source, &regions, PipLayout::Auto).unwrap();
        let Ok(mut renderer) = OffscreenRenderer::try_new(width, height) else {
            eprintln!("skipping pixel assertion: no offscreen GPU adapter");
            return;
        };
        let rgba = renderer.render_rgba(&scene);
        let alpha = |x: u32, y: u32| rgba[((y * width + x) * 4 + 3) as usize];
        assert!(alpha(100, 50) > 0, "selected left card is present");
        assert_eq!(
            alpha(700, 50),
            0,
            "desktop pixels between selected regions must not leak into export"
        );
        assert!(alpha(1_300, 70) > 0, "selected right card is present");
    }
    // Phase `spatial_capture_can_frame_content_beyond_both_screen_edges`.
    {
        let source = two_source_cards();
        let regions = [
            Rect::new(-300.0, 20.0, -100.0, 120.0),
            Rect::new(900.0, 40.0, 1_100.0, 140.0),
        ];
        let (scene, width, height) = compose_scene(&source, &regions, PipLayout::Auto).unwrap();
        assert_eq!((width, height), (1_400, 120));
        assert!(!scene.encoding().is_empty());
    }
    // Phase `grid_removes_the_unused_virtual_desktop_gap`.
    {
        let source = two_source_cards();
        let regions = [
            Rect::new(-300.0, 20.0, -100.0, 120.0),
            Rect::new(900.0, 40.0, 1_100.0, 140.0),
        ];
        let (_, width, height) = compose_scene(&source, &regions, PipLayout::Grid).unwrap();
        assert_eq!((width, height), (416, 100));
    }
    // Phase `multi_region_handoff_geometry_preserves_source_and_grid_endpoints`.
    {
        let regions = [
            Rect::new(250.0, 180.0, 710.0, 410.0),
            Rect::new(760.0, 310.0, 1_170.0, 590.0),
        ];
        let source_bounds = union_rects(&regions).unwrap();
        let first_source = normalized_rect(regions[0], source_bounds);
        assert_eq!(first_source.x0, 0.0);
        assert_eq!(first_source.y0, 0.0);
        assert!(first_source.x1 < 0.51);

        let (width, height, placements) = grid_geometry(&regions);
        let target_bounds = Rect::new(0.0, 0.0, width, height);
        let first_target = normalized_rect(placements[0], target_bounds);
        let second_target = normalized_rect(placements[1], target_bounds);
        assert!(first_target.x1 < second_target.x0, "grid gap must survive");
        assert!(first_target.height() < 1.0, "wide source is letterboxed");
        assert_eq!(second_target.y0, 0.0);
        assert_eq!(second_target.y1, 1.0);
    }
    // Phase `oversized_capture_scales_down_without_changing_aspect_ratio`.
    {
        let (_, _, scale) = output_dimensions(16_384.0, 8_192.0).unwrap();
        assert_eq!(scale, 0.5);
        assert_eq!(output_dimensions(16_384.0, 8_192.0).unwrap().0, 8_192);
        assert_eq!(output_dimensions(16_384.0, 8_192.0).unwrap().1, 4_096);
    }
    // Phase `spatial_multi_capture_leaves_unselected_gap_transparent` (defined above).
    spatial_multi_capture_leaves_unselected_gap_transparent();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn app_capture_follows_its_live_surface_and_never_fakes_success() {
    // Phase `full_app_capture_paints_a_wholly_offscreen_live_group`.
    fn full_app_capture_paints_a_wholly_offscreen_live_group() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let (strip_id, target_group, target_tab) = {
            let workspace = tree
                .desktop
                .active_workspace_mut()
                .expect("bootstrap has an active workspace");
            let strip = workspace.strips.first_mut().expect("bootstrap has a strip");
            let target = strip
                .window_groups
                .first_mut()
                .expect("bootstrap has a window group");
            target.tabs.truncate(1);
            target.active_tab = target.tabs[0].id;
            target.width = WindowWidth::Fraction(1_000);
            let target_group = target.id;
            let target_tab = target.active_tab;
            for (group_id, tab_id, label) in
                [(70_001, 80_001, "Middle"), (70_002, 80_002, "Visible")]
            {
                strip.window_groups.push(WindowGroup {
                    id: group_id,
                    tabs: vec![Tab {
                        id: tab_id,
                        title: label.into(),
                        tile: TileContent::Stub {
                            kind: AppKind::Settings,
                            label: label.into(),
                        },
                        app_title: None,
                    }],
                    active_tab: tab_id,
                    width: WindowWidth::Fraction(1_000),
                    maximized: false,
                    focus_history: Vec::new(),
                });
            }
            (strip.id, target_group, target_tab)
        };
        // Keep the strip parked on the target group. The desktop eases its
        // active group into view on a wall-clock animation, so naming the
        // last group active here would let a slow run finish that ease
        // before the explicit pan below and leave nothing to scroll.
        tree.desktop.set_active_group(strip_id, target_group);

        let mut frame_builder = FrameBuilder::with_tree(tree, (800, 600));
        let _ = frame_builder.paint();
        frame_builder
            .host_mut()
            .group_at_for_tests(strip_id, 0)
            .and_then(|group| group.active_host_mut())
            .expect("target group's window")
            .replace_content(ui::widget::TileContent::App(Box::new(
                LongDocument::default(),
            )));
        frame_builder.relayout();
        let _ = frame_builder.paint();
        assert!(
            frame_builder
                .host_mut()
                .group_at_for_tests(strip_id, 0)
                .is_some_and(|group| group.complete_capture_rect().is_some()),
            "a long document publishes its complete capture geometry"
        );
        // Focusing the last group already eases the strip toward it on
        // the wall clock, so whether this jump still moves the strip
        // depends on how long the frames above took. Jump to the end of
        // that ease and check where the strip rests instead.
        let _ = frame_builder
            .host_mut()
            .group_row_mut_for_tests(strip_id)
            .expect("live strip row")
            .scroll_group_into_view(70_002);
        frame_builder.relayout();
        let onscreen = frame_builder
            .host_mut()
            .group_row_mut_for_tests(strip_id)
            .expect("live strip row")
            .onscreen_group_ids();
        assert!(
            onscreen.contains(&70_002) && !onscreen.contains(&target_group),
            "the strip must rest on the last group with the target offscreen: {onscreen:?}"
        );
        let clean_screen = frame_builder.paint();
        let target_group_ref = frame_builder
            .host_mut()
            .group_at_for_tests(strip_id, 0)
            .expect("offscreen target group");
        let live_rect = target_group_ref.get_rect();
        let live_content = target_group_ref
            .active_host()
            .expect("the target window's content")
            .get_rect();
        let target_rect = target_group_ref
            .complete_capture_rect()
            .expect("a long document has complete capture geometry");
        assert!(
            target_rect.height() > live_rect.height(),
            "semantic app capture must extend below its live window"
        );
        assert_eq!(
            target_rect
                .intersect(Rect::new(0.0, 0.0, 800.0, 600.0))
                .area(),
            0.0,
            "precondition: the selected app is wholly outside the physical output; got {target_rect:?}"
        );

        let intent = CaptureIntent {
            mode: ui::widget::CaptureMode::Screenshot,
            source: ui::widget::SourceMode::Window,
            destination: CaptureDestination::File,
            regions: Vec::new(),
            anchors: Vec::new(),
            include_pointer: false,
            audio: AudioCaptureMode::None,
            pip_layout: PipLayout::Auto,
            targets: vec![ui::widget::CaptureTarget {
                id: format!("group:{target_group}:tab:{target_tab}"),
                title: "Notes".into(),
                app_id: "so.tilo.notes".into(),
                visible_rect: Rect::ZERO,
                // Deliberately stale: the live draw must win over picker-time
                // geometry after strip motion.
                virtual_rect: Rect::new(10_000.0, 0.0, 10_100.0, 100.0),
                complete_rect: Some(Rect::new(10_000.0, 0.0, 10_100.0, 200.0)),
                ..Default::default()
            }],
            recording_command: None,
            replay_command: None,
            screenshot_command: None,
        };
        let (source, regions) = capture_source(&mut frame_builder, &clean_screen, &intent);
        assert_eq!(regions, vec![target_rect]);
        assert!(
            !source.encoding().is_empty(),
            "the offscreen app must paint"
        );
        let (capture, width, height) = compose_scene(&source, &regions, PipLayout::Auto).unwrap();
        assert_eq!(width, target_rect.width().ceil() as u32);
        assert_eq!(height, target_rect.height().ceil() as u32);
        let mut live_intent = intent.clone();
        live_intent.mode = CaptureMode::Pip;
        live_intent.destination = CaptureDestination::FloatingWindow;
        let (_, live_regions) = capture_source(&mut frame_builder, &clean_screen, &live_intent);
        assert_eq!(
            live_regions,
            vec![live_content],
            "PiP shows the offscreen window's live content, without its frame, rather than flattening the complete document"
        );
        let Ok(mut renderer) = OffscreenRenderer::try_new(width, height) else {
            eprintln!("skipping pixel assertion: no offscreen GPU adapter");
            return;
        };
        let rgba = render_capture_rgba(&mut renderer, &capture);
        assert!(
            rgba.chunks_exact(4).any(|pixel| pixel[3] != 0),
            "the compositor returned dimensions but no captured app pixels"
        );
    }
    // Phase `full_app_capture_paints_a_wholly_offscreen_live_group` (defined above).
    full_app_capture_paints_a_wholly_offscreen_live_group();
    // Phase `vanished_full_app_never_falls_back_to_a_blank_success`.
    {
        let mut frame_builder = FrameBuilder::new((800, 600));
        let clean_screen = frame_builder.paint();
        let intent = CaptureIntent {
            mode: ui::widget::CaptureMode::Screenshot,
            source: ui::widget::SourceMode::Window,
            destination: CaptureDestination::File,
            regions: Vec::new(),
            anchors: Vec::new(),
            include_pointer: false,
            audio: AudioCaptureMode::None,
            pip_layout: PipLayout::Auto,
            targets: vec![ui::widget::CaptureTarget {
                id: "group:999999:tab:999999".into(),
                title: "Gone".into(),
                app_id: "so.tilo.gone".into(),
                visible_rect: Rect::ZERO,
                virtual_rect: Rect::new(1_000.0, 100.0, 1_800.0, 700.0),
                complete_rect: None,
                ..Default::default()
            }],
            recording_command: None,
            replay_command: None,
            screenshot_command: None,
        };
        let (_, regions) = capture_source(&mut frame_builder, &clean_screen, &intent);
        assert!(regions.is_empty());
        assert!(
            compose_scene(&Scene::new(), &regions, PipLayout::Auto).is_err(),
            "a vanished target must report failure instead of saving transparent pixels"
        );
    }
    // Phase `tracked_app_region_follows_its_surface_without_changing_local_content`.
    {
        let local_video = Rect::new(80.0, 120.0, 560.0, 390.0);
        let first_app = Rect::new(200.0, 60.0, 1_000.0, 660.0);
        let moved_app = Rect::new(640.0, 90.0, 1_440.0, 690.0);
        let first = app_local_region_on_surface(local_video, first_app).unwrap();
        let moved = app_local_region_on_surface(local_video, moved_app).unwrap();
        assert_eq!(first, Rect::new(280.0, 180.0, 760.0, 450.0));
        assert_eq!(moved, Rect::new(720.0, 210.0, 1_200.0, 480.0));
        assert_eq!(first.size(), moved.size());
    }
    // Phase `semantic_region_update_preserves_element_identity`.
    {
        let updated = updated_local_tracking_source(
            Some(FloatingSource::AppElement {
                app_id: "so.tilo.browser".into(),
                element_id: "media:7".into(),
                region: Rect::new(10.0, 20.0, 410.0, 245.0),
            }),
            "so.tilo.browser".into(),
            Rect::new(24.0, 36.0, 624.0, 374.0),
        );
        assert_eq!(
            updated,
            FloatingSource::AppElement {
                app_id: "so.tilo.browser".into(),
                element_id: "media:7".into(),
                region: Rect::new(24.0, 36.0, 624.0, 374.0),
            }
        );
    }
}

#[test]
fn a_pip_of_a_window_keeps_showing_that_window_after_scrolling_and_switching_strips() {
    let mut tree = WorkspaceTree::bootstrap_demo();
    let (strip_id, target_group, target_tab) = {
        let workspace = tree
            .desktop
            .active_workspace_mut()
            .expect("bootstrap has an active workspace");
        let strip = workspace.strips.first_mut().expect("bootstrap has a strip");
        let target = strip
            .window_groups
            .first_mut()
            .expect("bootstrap has a window group");
        target.tabs.truncate(1);
        target.active_tab = target.tabs[0].id;
        target.width = WindowWidth::Fraction(1_000);
        let target_group = target.id;
        let target_tab = target.active_tab;
        for (group_id, tab_id, label) in [(71_001, 81_001, "Middle"), (71_002, 81_002, "Last")] {
            strip.window_groups.push(WindowGroup {
                id: group_id,
                tabs: vec![Tab {
                    id: tab_id,
                    title: label.into(),
                    tile: TileContent::Stub {
                        kind: AppKind::Settings,
                        label: label.into(),
                    },
                    app_title: None,
                }],
                active_tab: tab_id,
                width: WindowWidth::Fraction(1_000),
                maximized: false,
                focus_history: Vec::new(),
            });
        }
        (strip.id, target_group, target_tab)
    };
    tree.desktop.set_active_group(strip_id, target_group);
    let mut frame_builder = FrameBuilder::with_tree(tree, (800, 600));
    let _ = frame_builder.paint();
    // A picture in picture shows the window's content, without its frame.
    let window_now = |frame_builder: &mut FrameBuilder| {
        frame_builder
            .host_mut()
            .group_at_for_tests(strip_id, 0)
            .expect("the picked window")
            .active_host()
            .expect("the picked window's content")
            .get_rect()
    };
    let picked = window_now(&mut frame_builder);
    let intent = CaptureIntent {
        mode: CaptureMode::Pip,
        source: SourceMode::Window,
        destination: CaptureDestination::FloatingWindow,
        regions: vec![picked],
        anchors: Vec::new(),
        include_pointer: false,
        audio: AudioCaptureMode::None,
        pip_layout: PipLayout::Auto,
        targets: vec![ui::widget::CaptureTarget {
            id: format!("group:{target_group}:tab:{target_tab}"),
            title: "Settings".into(),
            visible_rect: picked,
            virtual_rect: picked,
            focused: true,
            on_active_strip: true,
            ..Default::default()
        }],
        recording_command: None,
        replay_command: None,
        screenshot_command: None,
    };
    let clean = frame_builder.paint();
    let (_, regions) = capture_source(&mut frame_builder, &clean, &intent);
    assert_eq!(regions, vec![picked]);

    // The person scrolls the strip until the window is off screen: the
    // picture reads the window where it is now, not the old screen rect.
    let _ = frame_builder
        .host_mut()
        .group_row_mut_for_tests(strip_id)
        .expect("live strip row")
        .scroll_group_into_view(71_002);
    frame_builder.relayout();
    let clean = frame_builder.paint();
    let scrolled = window_now(&mut frame_builder);
    assert!(
        (scrolled.x0 - picked.x0).abs() > 1.0,
        "precondition: the strip moved the window: {picked:?} -> {scrolled:?}"
    );
    let (source, regions) = capture_source(&mut frame_builder, &clean, &intent);
    assert_eq!(regions, vec![scrolled], "the picture follows its window");
    assert!(!source.encoding().is_empty(), "the window paints");

    // The person switches to another strip: still that window.
    frame_builder.with_chrome(|state| {
        state.create_strip("Elsewhere").expect("a second strip");
    });
    let _ = frame_builder.paint();
    frame_builder.relayout();
    let clean = frame_builder.paint();
    let (source, regions) = capture_source(&mut frame_builder, &clean, &intent);
    assert_eq!(regions, vec![window_now(&mut frame_builder)]);
    assert!(!source.encoding().is_empty(), "the window still paints");
}

/// Every target the capture tool offers captures, on high-density displays
/// beside a 1x monitor: native windows, host (Wayland) windows whose
/// frames are dmabuf GPU frames or shm pixels, a window scrolled off
/// screen, a selection inside a window and one across windows, each
/// screen and the strip. On the person's laptop a capture failed with
/// "the selected source has no paintable pixels"; this pins that every
/// kind of source resolves to pixels, and that a host window's pixels
/// are its own.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one arrangement per pass, every target the tool offers in each"
)]
fn every_target_captures_host_windows_off_screen_windows_at_scale_on_two_displays() {
    const SHM: u64 = 0xCA97_5001;
    const DMABUF: u64 = 0xCA97_5002;
    const SHM_TEXEL: [u8; 4] = [0xE0, 0x18, 0xA0, 0xFF];
    const DMABUF_TEXEL: [u8; 4] = [0x18, 0xC0, 0x40, 0xFF];
    let mut gpu = ui::gpu::RenderContext::new();
    let gpu_device = pollster::block_on(gpu.device(None));
    for arrangement in ["laptop-beside-1080p", "2880x1800@1.9565,1920x1080@1"] {
        let outputs = crate::compositor::backend::headless::resolve_output_arrangement(arrangement)
            .expect("an output arrangement");
        let mut tree = WorkspaceTree::bootstrap_demo();
        for group in tree
            .desktop
            .workspaces
            .iter_mut()
            .flat_map(|ws| ws.strips.iter_mut())
            .flat_map(|strip| strip.window_groups.iter_mut())
        {
            // Each window the width of a display, so some are off screen.
            group.width = WindowWidth::Fraction(1_000);
        }
        let mut fb = FrameBuilder::with_tree(
            tree,
            crate::compositor::backend::combined_geometry(&outputs),
        );
        fb.set_output_geometry(crate::compositor::backend::output_geometry(&outputs));
        let store = crate::shell::foreign::store();
        let mut tabs = std::collections::HashMap::new();
        for (surface, app) in [
            (SHM, "org.example.capture.shm"),
            (DMABUF, "org.example.capture.gpu"),
        ] {
            store.forget(surface);
            store.note_adopted(surface);
            let tab = {
                let chrome = fb.chrome();
                let mut rendered = chrome.tree_mut();
                crate::compositor::tab_binding::adopt_toplevel_at(
                    &mut rendered,
                    surface,
                    Some(app.to_string()),
                    crate::compositor::tab_binding::Placement::NewGroup,
                )
            };
            tabs.insert(surface, tab);
            fb.with_chrome(crate::shell::chrome_state::ChromeState::bump);
        }
        {
            // Each window the width of a display, so some are off screen.
            let chrome = fb.chrome();
            let mut rendered = chrome.tree_mut();
            for group in rendered
                .desktop
                .workspaces
                .iter_mut()
                .flat_map(|ws| ws.strips.iter_mut())
                .flat_map(|strip| strip.window_groups.iter_mut())
            {
                group.width = WindowWidth::Fraction(1_000);
            }
        }
        fb.with_chrome(crate::shell::chrome_state::ChromeState::bump);
        let target_of = |targets: &[ui::widget::CaptureTarget], surface: u64| {
            let suffix = format!(":tab:{}", tabs[&surface]);
            targets.iter().find(|t| t.id.ends_with(&suffix)).cloned()
        };
        let settle = |fb: &mut FrameBuilder| {
            for _ in 0..4 {
                let mut doc = automerge::AutoCommit::new();
                fb.pump_apps(&mut doc, &automerge::ROOT);
                fb.relayout();
                let _ = fb.paint();
            }
        };
        settle(&mut fb);
        let size_of = |fb: &mut FrameBuilder, surface: u64| -> (u32, u32) {
            store
                .viewport(surface)
                .or_else(|| {
                    fb.foreign_surface_rect(surface)
                        .map(|rect| (rect.width().round() as u32, rect.height().round() as u32))
                })
                .unwrap_or((800, 600))
        };
        let shm_size = size_of(&mut fb, SHM);
        store.publish_rgba(
            SHM,
            &SHM_TEXEL.repeat((shm_size.0 * shm_size.1) as usize),
            shm_size.0,
            shm_size.1,
        );
        if let Some(id) = gpu_device {
            let handle = gpu.devices.get(id).expect("the device just made");
            // As the presenting renderer does: client frames are imported
            // on its device, and a capture renderer on another device
            // reads them back through the external image source.
            let generation =
                crate::shell::gpu_frames::publish_present_device(&handle.device, &handle.queue);
            let size = size_of(&mut fb, DMABUF);
            let frame = crate::shell::gpu_frames::frame_for_test(
                &handle.device,
                &handle.queue,
                generation,
                size,
                DMABUF_TEXEL,
                DMABUF,
            );
            assert!(
                matches!(
                    store.publish_gpu_for_test(DMABUF, frame),
                    Some(crate::shell::foreign::CaptureOutcome::Dmabuf { .. })
                ),
                "{arrangement}: the GPU frame takes the dmabuf path"
            );
        } else {
            eprintln!("no wgpu adapter; the dmabuf window still holds its place");
        }
        // Until the host windows show their frames, past the opening
        // card's hand-over.
        let deadline = tilo_test_env::hang_deadline();
        while std::time::Instant::now() < deadline
            && !(store.is_presented(SHM) && (gpu_device.is_none() || store.is_presented(DMABUF)))
        {
            settle(&mut fb);
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        settle(&mut fb);
        let clean = fb.paint();
        let targets = fb.host().capture_targets();
        assert!(
            targets.len() >= 3,
            "{arrangement}: the demo windows and both host windows: {targets:?}"
        );
        assert!(
            targets.iter().any(|t| t.visible_rect.area() <= 0.0),
            "{arrangement}: precondition: a window is off screen"
        );
        let intent = |source: SourceMode,
                      regions: Vec<Rect>,
                      anchors: Vec<ui::widget::RegionAnchor>,
                      targets: Vec<ui::widget::CaptureTarget>| CaptureIntent {
            mode: CaptureMode::Screenshot,
            source,
            destination: CaptureDestination::FileAndClipboard,
            regions,
            anchors,
            include_pointer: false,
            audio: AudioCaptureMode::None,
            pip_layout: PipLayout::Auto,
            targets,
            recording_command: None,
            replay_command: None,
            screenshot_command: None,
        };
        let captures = |fb: &mut FrameBuilder, what: &str, intent: &CaptureIntent| {
            let (source, regions) = capture_source(fb, &clean, intent);
            assert!(
                !regions.is_empty() && regions.iter().all(|r| r.area() > 0.0),
                "{arrangement}: {what} resolves to pixels: {regions:?}"
            );
            assert!(
                !source.encoding().is_empty(),
                "{arrangement}: {what} paints"
            );
            regions
        };
        // Every window, whole: on screen, half on, off screen, host ones.
        for target in &targets {
            captures(
                &mut fb,
                &format!("window {}", target.id),
                &intent(
                    SourceMode::Window,
                    vec![target.visible_rect],
                    Vec::new(),
                    vec![target.clone()],
                ),
            );
        }
        let strip: Vec<_> = targets
            .iter()
            .filter(|t| t.on_active_strip)
            .cloned()
            .collect();
        captures(
            &mut fb,
            "the strip",
            &intent(SourceMode::Strip, Vec::new(), Vec::new(), strip),
        );
        let displays: Vec<Rect> = outputs
            .iter()
            .map(crate::compositor::backend::Output::logical_rect)
            .collect();
        for display in &displays {
            captures(
                &mut fb,
                "a screen",
                &intent(SourceMode::Display, vec![*display], Vec::new(), Vec::new()),
            );
        }
        let seam = displays[0].x1;
        captures(
            &mut fb,
            "a selection across both screens",
            &intent(
                SourceMode::Region,
                vec![Rect::new(seam - 200.0, 200.0, seam + 200.0, 500.0)],
                Vec::new(),
                Vec::new(),
            ),
        );
        for surface in [SHM, DMABUF] {
            let target = target_of(&targets, surface).expect("the host window is a target");
            let local = Rect::new(40.0, 80.0, 240.0, 240.0);
            captures(
                &mut fb,
                "a selection inside a host window",
                &intent(
                    SourceMode::Region,
                    vec![local + target.virtual_rect.origin().to_vec2()],
                    vec![ui::widget::RegionAnchor {
                        target: target.id.clone(),
                        local,
                    }],
                    vec![target],
                ),
            );
        }
        let host_targets: Vec<(u64, [u8; 4], ui::widget::CaptureTarget)> =
            [(SHM, SHM_TEXEL), (DMABUF, DMABUF_TEXEL)]
                .into_iter()
                .filter(|(surface, _)| *surface == SHM || gpu_device.is_some())
                .filter_map(|(surface, texel)| {
                    target_of(&targets, surface).map(|t| (surface, texel, t))
                })
                .collect();
        assert_eq!(
            host_targets.len(),
            if gpu_device.is_some() { 2 } else { 1 },
            "{arrangement}: both host windows are capture targets"
        );
        for (surface, texel, target) in host_targets {
            let mut pip = intent(SourceMode::Window, Vec::new(), Vec::new(), vec![target]);
            pip.mode = CaptureMode::Pip;
            pip.destination = CaptureDestination::FloatingWindow;
            let shot = CaptureExecutor::render_screenshot(&mut fb, &clean, &pip);
            let Ok((rgba, width, height)) = shot else {
                eprintln!("{arrangement}: no capture renderer: {shot:?}");
                continue;
            };
            let centre = OffscreenRenderer::sample(&rgba, width, width / 2, height / 2);
            let near = |a: u8, b: u8| a.abs_diff(b) <= 6;
            assert!(
                near(centre.0, texel[0]) && near(centre.1, texel[1]) && near(centre.2, texel[2]),
                "{arrangement}: host window {surface:#x} captured as its own pixels; centre {centre:?}"
            );
        }
        for surface in [SHM, DMABUF] {
            store.forget(surface);
        }
    }
}

#[test]
fn recording_pauses_and_replay_buffers_stay_bounded() {
    // Phase `paused_recording_never_schedules_an_encoder_frame`.
    assert!(!recording_frame_due(true, Duration::from_secs(30)));
    assert!(!recording_frame_due(false, Duration::from_millis(32)));
    assert!(recording_frame_due(false, Duration::from_millis(33)));
    // Phase `replay_ring_is_bounded_and_always_begins_on_a_keyframe`.
    {
        let mut frames: VecDeque<_> = (0..REPLAY_FRAMES + 17)
            .map(|index| BufferedReplayFrame {
                access_unit: std::sync::Arc::from(vec![index as u8]),
                keyframe: index % REPLAY_KEYFRAME_INTERVAL as usize == 0,
            })
            .collect();
        let mut bytes = frames.len();
        trim_replay_video_frames(&mut frames, &mut bytes);
        assert!(frames.len() <= REPLAY_FRAMES);
        assert!(frames.front().is_some_and(|frame| frame.keyframe));
        assert_eq!(bytes, frames.len());
    }
    // Phase `replay_audio_tail_pads_warmup_and_keeps_the_newest_samples`.
    {
        let pcm: VecDeque<u8> = (0..12).collect();
        let padded = replay_audio_tail(&pcm, 1);
        assert_eq!(padded.len(), AUDIO_BYTES_PER_VIDEO_FRAME);
        assert_eq!(&padded[padded.len() - 12..], &(0..12).collect::<Vec<_>>());

        let pcm: VecDeque<u8> = (0..AUDIO_BYTES_PER_VIDEO_FRAME + 8)
            .map(|value| value as u8)
            .collect();
        let tail = replay_audio_tail(&pcm, 1);
        assert_eq!(tail[0], 8);
        assert_eq!(tail.len(), AUDIO_BYTES_PER_VIDEO_FRAME);
    }
}
