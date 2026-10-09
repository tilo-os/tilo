use super::*;
use crate::compositor::frame::FramePaint;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Fill;

fn solid(color: Color) -> Scene {
    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        color,
        None,
        &Rect::new(0.0, 0.0, 64.0, 48.0),
    );
    scene
}

fn unchanged(scene: Scene, revision: u64, previous: u64) -> FramePaint {
    let mut frame = FramePaint::full(scene);
    frame.revision = revision;
    frame.previous_revision = Some(previous);
    frame.damage_regions = Some(Vec::new());
    frame
}

#[test]
fn completed_empty_damage_reuses_pixels_and_honors_invalidation() {
    let mut backend = HeadlessBackend::new(64, 48).expect("fixture renderer");
    backend.set_watched(true);
    assert!(
        backend.take_presenter().is_none(),
        "headless completion must remain synchronous"
    );
    let red = Color::from_rgb8(200, 20, 30);
    let blue = Color::from_rgb8(20, 30, 200);
    backend.submit_frame_with_damage(&unchanged(solid(red), 1, 99), OutputId(0));
    let first = backend.last_frame().to_vec();
    assert_eq!(
        first.len(),
        12_288,
        "the first empty-damage frame must still render"
    );
    assert_eq!(&first[..4], &[200, 20, 30, 255]);
    // Qualify the first ordinary readback before it can be frozen.
    backend.submit_frame_with_damage(&unchanged(solid(red), 2, 1), OutputId(0));
    assert_eq!(backend.last_frame(), first);
    let allocation = backend.last_frame().as_ptr();
    backend.submit_frame_with_damage(&unchanged(solid(red), 3, 2), OutputId(0));
    assert_eq!(backend.last_frame(), first);
    assert_eq!(
        backend.last_frame().as_ptr(),
        allocation,
        "a proved unchanged frame retains the completed image allocation"
    );
    backend.submit_frame_with_damage(&unchanged(solid(red), 4, 3), OutputId(0));
    assert_eq!(
        backend.last_frame().as_ptr(),
        allocation,
        "a skipped render still acknowledges its new revision"
    );

    // Missing a produced frame breaks authority even when the next list is empty.
    backend.submit_frame_with_damage(&unchanged(solid(blue), 6, 5), OutputId(0));
    assert_ne!(backend.last_frame(), first);
    assert_eq!(&backend.last_frame()[..4], &[20, 30, 200, 255]);
    backend.submit_frame(&solid(red), OutputId(0));
    backend.submit_frame_with_damage(&unchanged(solid(blue), 7, 6), OutputId(0));
    assert_eq!(
        &backend.last_frame()[..4],
        &[20, 30, 200, 255],
        "unversioned submission invalidates the retained predecessor"
    );

    backend.set_watched(false);
    assert!(
        !backend.supports_retained_scene_damage(),
        "unwatched frames keep the cheap producer path"
    );
    backend.submit_frame_with_damage(&unchanged(solid(red), 8, 7), OutputId(0));
    assert!(backend.last_frame().is_empty());
    backend.set_watched(true);
    backend.submit_frame_with_damage(&unchanged(solid(red), 9, 8), OutputId(0));
    assert_eq!(
        backend.last_frame(),
        first,
        "rewatching cannot retain an unrendered frame"
    );
    backend.set_watched(false);
    backend.set_watched(true);
    backend.submit_frame_with_damage(&unchanged(solid(blue), 10, 9), OutputId(0));
    assert_eq!(
        &backend.last_frame()[..4],
        &[20, 30, 200, 255],
        "reattaching invalidates authority even without an intervening submit"
    );

    let mut external = unchanged(solid(red), 11, 10);
    external.force_full();
    backend.submit_frame_with_damage(&external, OutputId(0));
    assert_eq!(
        backend.last_frame(),
        first,
        "external scene mutation must render"
    );
    let mut replacement = HeadlessBackend::new(32, 24).expect("replacement size renderer");
    replacement.set_watched(true);
    replacement.submit_frame_with_damage(&unchanged(solid(blue), 12, 11), OutputId(0));
    assert_eq!(
        replacement.last_frame().len(),
        3_072,
        "replacing the output size starts without a completed frame"
    );
}

#[test]
fn deterministic_theme_scenes_match_dense_and_materials_force_rendering() {
    let mut backend = HeadlessBackend::new(64, 48).expect("fixture renderer");
    backend.set_watched(true);
    let mut revision = 1u64;
    for theme in [
        ui::theme::Theme::dark(),
        ui::theme::Theme::light(),
        ui::theme::Theme::high_contrast(),
    ] {
        let scene = solid(theme.tokens.colors.surfaces.base);
        backend.submit_frame(&scene, OutputId(0));
        let oracle = backend.last_frame().to_vec();
        let mut initial = FramePaint::full(scene.clone());
        initial.revision = revision;
        backend.submit_frame_with_damage(&initial, OutputId(0));
        assert_eq!(
            backend.last_frame(),
            oracle,
            "theme boundary retains the dense pixel oracle"
        );
        let qualified = revision.saturating_add(1);
        backend
            .submit_frame_with_damage(&unchanged(scene.clone(), qualified, revision), OutputId(0));
        assert_eq!(backend.last_frame(), oracle);
        let allocation = backend.last_frame().as_ptr();
        let next = qualified.saturating_add(1);
        backend.submit_frame_with_damage(&unchanged(scene.clone(), next, qualified), OutputId(0));
        assert_eq!(backend.last_frame(), oracle);
        assert_eq!(backend.last_frame().as_ptr(), allocation);
        let mut material = unchanged(scene, next.saturating_add(1), next);
        material.backdrop = Some(std::sync::Arc::new(ui::backdrop::BackdropFrame::default()));
        backend.submit_frame_with_damage(&material, OutputId(0));
        assert_eq!(backend.last_frame(), oracle);
        assert_ne!(
            backend.last_frame().as_ptr(),
            allocation,
            "material frames conservatively render instead of retaining an earlier target"
        );
        revision = next.saturating_add(2);
    }
}

#[test]
fn production_producer_authorizes_only_matching_completed_headless_frames() {
    use crate::shell::test_support::PopulatedShellFixture;
    let mut clock = tilo_test_env::ScopedEnv::new();
    clock.set("TILO_CLOCK", "09:41");
    let mut frame = PopulatedShellFixture::new(
        (320, 240),
        ui::theme::Theme::dark().with_reduce_motion(true),
    );
    let mut backend = HeadlessBackend::new(320, 240).expect("fixture renderer");
    backend.set_watched(true);
    assert!(
        backend.supports_retained_scene_damage(),
        "the native loop must request the production damage proof"
    );
    let mut reused = false;
    let mut consecutive_empty = 0u32;
    for _ in 0..8 {
        frame.relayout();
        let paint = frame.paint_with_damage();
        let allocation = backend.last_frame().as_ptr();
        let prior = backend.last_frame().to_vec();
        let empty =
            paint.damage_regions.as_ref().is_some_and(Vec::is_empty) && paint.backdrop.is_none();
        backend.submit_frame_with_damage(&paint, OutputId(0));
        consecutive_empty = if empty {
            consecutive_empty.saturating_add(1)
        } else {
            0
        };
        if consecutive_empty >= 2 && !prior.is_empty() {
            assert_eq!(backend.last_frame(), prior);
            assert_eq!(
                backend.last_frame().as_ptr(),
                allocation,
                "the real producer's quiet frame must avoid a render/readback"
            );
            reused = true;
            break;
        }
    }
    drop(frame);
    assert!(
        reused,
        "the pinned settled production fixture must yield an unchanged frame"
    );
}

#[test]
fn hdr_metadata_cannot_reuse_the_completed_sdr_image() {
    use crate::shell::hdr_surface::{HdrLayer, HdrPlane, SurfaceSignal};
    use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};
    let mut backend = HeadlessBackend::new(64, 48).expect("fixture renderer");
    backend.set_watched(true);
    let color = Color::from_rgb8(200, 20, 30);
    backend.submit_frame_with_damage(&unchanged(solid(color), 1, 99), OutputId(0));
    backend.submit_frame_with_damage(&unchanged(solid(color), 2, 1), OutputId(0));
    backend.submit_frame_with_damage(&unchanged(solid(color), 3, 2), OutputId(0));
    let allocation = backend.last_frame().as_ptr();
    let signal = SurfaceSignal::for_colorimetry(
        crate::collab::colorimetry::Colorimetry::hdr10(),
        crate::shell::hdr_tonemap::HdrTonemapConfig::default(),
    )
    .expect("HDR signal");
    let plane = HdrPlane::from_signal_rgb(1, 1, [[0.5; 3]], signal).expect("HDR plane");
    let mut frame = unchanged(solid(color), 4, 3);
    frame.hdr_layers.push(HdrLayer {
        plane: std::sync::Arc::new(plane),
        image: ImageData {
            data: Blob::new(std::sync::Arc::new(vec![0, 0, 0, 255])),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::AlphaPremultiplied,
            width: 1,
            height: 1,
        },
        image_to_scene: Affine::IDENTITY,
        scene_to_image: Affine::IDENTITY,
        bounds: Rect::new(0.0, 0.0, 1.0, 1.0),
        channel: 0,
    });
    backend.submit_frame_with_damage(&frame, OutputId(0));
    assert_eq!(&backend.last_frame()[..4], &[200, 20, 30, 255]);
    assert_ne!(
        backend.last_frame().as_ptr(),
        allocation,
        "HDR metadata must remain on the ordinary SDR rendering path"
    );
}
