//! A window whose client describes its content as HDR shows its own colour on an SDR output, beside
//! an ordinary window that keeps its own: the described surface is drawn from its image
//! description, not from the bytes its buffer happens to hold.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion"
)]

use crate::harness::{TestClient, headless, pump};
use smithay::utils::{Logical, Point, Rectangle};
use tilo_color_pipeline::hdr_tonemap::{HdrTonemapConfig, tonemap_rgb_signal_to_srgb8};
use tilo_compositor::{Compositor, WindowInfo, WindowPolicy};
use wayland_client::protocol::wl_shm;
use wayland_protocols::wp::color_management::v1::client::wp_color_management_surface_v1::WpColorManagementSurfaceV1;
use wayland_protocols::wp::color_management::v1::client::wp_color_manager_v1::{
    Primaries, RenderIntent, TransferFunction,
};

use neighbour_wire::colorimetry::TransferCharacteristics;

/// The desktop's background, R, G, B.
const BACKGROUND: [u8; 4] = [26, 27, 38, 255];

/// Places windows side by side, 200 logical pixels wide each.
#[derive(Default)]
struct SideBySide {
    placed: i32,
}

impl WindowPolicy for SideBySide {
    fn initial_size(
        &mut self,
        _: &WindowInfo,
        _: &[Rectangle<i32, Logical>],
    ) -> Option<(i32, i32)> {
        Some((200, 100))
    }

    fn place(
        &mut self,
        _: &WindowInfo,
        _: (i32, i32),
        _: &[Rectangle<i32, Logical>],
    ) -> Point<i32, Logical> {
        let x = self.placed.saturating_mul(320);
        self.placed = self.placed.saturating_add(1);
        Point::from((x, 100))
    }
}

/// A client with a configured toplevel, ready for its first buffer.
fn window_client(compositor: &mut Compositor, app_id: &str) -> TestClient {
    let mut client = TestClient::connect(compositor);
    pump(compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.wm_base.is_some() && clients[0].seen.seat.is_some()
    });
    client.open_window(app_id);
    pump(compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.configured
    });
    client
}

/// Attach a parametric image description to `client` 's surface, as a player does with what it is
/// about to show.
fn attach_description(
    compositor: &mut Compositor,
    client: &mut TestClient,
    primaries: Primaries,
    transfer: TransferFunction,
) -> WpColorManagementSurfaceV1 {
    let qh = client.queue.handle();
    let manager = client.seen.color_manager.clone().expect("colour manager");
    let creator = manager.create_parametric_creator(&qh, ());
    creator.set_primaries_named(primaries);
    creator.set_tf_named(transfer);
    let image = creator.create(&qh, ());
    pump(compositor, &mut [client], |_, clients| {
        clients[0].seen.color_description_ready
    });

    let surface = client.surface.clone().expect("surface");
    let color_surface = manager.get_surface(&surface, &qh, ());
    color_surface.set_image_description(&image, RenderIntent::Perceptual);
    color_surface
}

/// Attach a parametric description to `client` 's surface that also says the volume the content was
/// mastered in: `min_lum` to `max_lum` cd/m².
fn attach_mastered(
    compositor: &mut Compositor,
    client: &mut TestClient,
    min_lum: u32,
    max_lum: u32,
) -> WpColorManagementSurfaceV1 {
    let qh = client.queue.handle();
    let manager = client.seen.color_manager.clone().expect("colour manager");
    let creator = manager.create_parametric_creator(&qh, ());
    creator.set_primaries_named(Primaries::Bt2020);
    creator.set_tf_named(TransferFunction::St2084Pq);
    creator.set_mastering_luminance(min_lum, max_lum);
    let image = creator.create(&qh, ());
    pump(compositor, &mut [client], |_, clients| {
        clients[0].seen.color_description_ready
    });

    let surface = client.surface.clone().expect("surface");
    let color_surface = manager.get_surface(&surface, &qh, ());
    color_surface.set_image_description(&image, RenderIntent::Perceptual);
    color_surface
}

/// Let `client` 's window show its first buffer, and wait until it maps.
fn map(compositor: &mut Compositor, client: &mut TestClient, app_id: &str) {
    pump(compositor, &mut [client], |compositor, _| {
        compositor
            .windows()
            .iter()
            .any(|(info, _)| info.app_id.as_deref() == Some(app_id))
    });
}

/// The window `app_id` opened.
fn window_of(compositor: &Compositor, app_id: &str) -> (u64, (i32, i32, i32, i32)) {
    let (info, rect) = compositor
        .windows()
        .into_iter()
        .find(|(info, _)| info.app_id.as_deref() == Some(app_id))
        .expect("the window");
    (info.id, rect)
}

/// A 10-bit `Xrgb2101010` word of linear-light samples: red in the high bits, blue in the low ones.
fn word(red: u32, green: u32, blue: u32) -> u32 {
    (red << 20) | (green << 10) | blue
}

#[test]
fn an_hdr_window_keeps_its_own_colour_beside_an_sdr_one() {
    let mut compositor = headless(Box::new(SideBySide::default()));
    // What an HDR video player commits: a 10-bit buffer of linear light,
    // described as such.
    let mut player = window_client(&mut compositor, "player");
    let _described = attach_description(
        &mut compositor,
        &mut player,
        Primaries::Srgb,
        TransferFunction::ExtLinear,
    );
    player.show_words(200, 100, wl_shm::Format::Xrgb2101010, word(512, 256, 768));
    map(&mut compositor, &mut player, "player");
    // An ordinary window beside it, which also says its content is sRGB.
    let mut editor = window_client(&mut compositor, "editor");
    let _sdr = attach_description(
        &mut compositor,
        &mut editor,
        Primaries::Srgb,
        TransferFunction::ExtSrgb,
    );
    editor.show(200, 100, [255, 0, 0, 255]);
    map(&mut compositor, &mut editor, "editor");

    let (player_window, player_rect) = window_of(&compositor, "player");
    let (editor_window, editor_rect) = window_of(&compositor, "editor");
    assert_eq!(player_rect, (0, 100, 200, 100));
    assert_eq!(editor_rect, (320, 100, 200, 100));

    // The compositor keeps what the client said the surface carries.
    let (colour, _) = compositor
        .window_color(player_window)
        .expect("the described colour");
    assert_eq!(colour.transfer, TransferCharacteristics::Linear);
    let (editor_colour, _) = compositor
        .window_color(editor_window)
        .expect("the described colour");
    assert_eq!(editor_colour.transfer, TransferCharacteristics::Srgb);

    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();

    // Linear light 0.5, 0.25, 0.75 is drawn as that light, not as the bytes that carry it.
    let signal = [512.0 / 1023.0, 256.0 / 1023.0, 768.0 / 1023.0];
    let expected = tonemap_rgb_signal_to_srgb8(signal, colour, HdrTonemapConfig::default());
    let shown = frame.pixel(100, 150).expect("inside the player");
    assert_eq!(
        &shown[..3],
        &expected,
        "the described surface shows its signal"
    );
    assert_eq!(shown[3], 255);
    assert_ne!(
        expected,
        [128, 64, 191],
        "the buffer's bytes would show as something else, so the description was used"
    );

    // The ordinary window keeps its own colour, and the desktop is still the desktop.
    assert_eq!(
        frame.pixel(420, 150),
        Some([255, 0, 0, 255]),
        "the SDR window is untouched"
    );
    assert_eq!(frame.pixel(10, 400), Some(BACKGROUND));
}

#[test]
fn a_ten_bit_window_that_says_nothing_shows_the_bytes_it_holds() {
    let mut compositor = headless(Box::new(SideBySide::default()));
    let mut plain = window_client(&mut compositor, "plain");
    plain.show_words(200, 100, wl_shm::Format::Xrgb2101010, word(512, 256, 768));
    map(&mut compositor, &mut plain, "plain");
    assert_eq!(
        compositor.window_color(window_of(&compositor, "plain").0),
        None
    );

    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let shown = frame.pixel(100, 150).expect("inside the window");
    let bytes = [128_u32, 64, 192];
    for (channel, expected) in shown[..3].iter().zip(bytes) {
        assert!(
            i32::from(*channel).abs_diff(i32::try_from(expected).unwrap()) <= 3,
            "the buffer is drawn as it is: {shown:?}"
        );
    }
}

#[test]
fn a_film_is_tone_mapped_with_the_volume_it_was_mastered_for() {
    let mut compositor = headless(Box::new(SideBySide::default()));
    // The same PQ light, mastered two ways: a bright cinema master and the HDR10 baseline. Each
    // player says which one its content is.
    let mut cinema = window_client(&mut compositor, "cinema");
    let _cinema_colour = attach_mastered(&mut compositor, &mut cinema, 500, 4000);
    cinema.show_words(200, 100, wl_shm::Format::Xrgb2101010, word(768, 768, 768));
    map(&mut compositor, &mut cinema, "cinema");
    let mut baseline = window_client(&mut compositor, "baseline");
    let _baseline_colour = attach_mastered(&mut compositor, &mut baseline, 500, 1000);
    baseline.show_words(200, 100, wl_shm::Format::Xrgb2101010, word(768, 768, 768));
    map(&mut compositor, &mut baseline, "baseline");

    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();

    // Each window's rendition is the tone map of its own mastering volume.
    let shown = |app_id: &str, at: (i32, i32)| {
        let (id, _) = window_of(&compositor, app_id);
        let (colour, metadata) = compositor.window_color(id).expect("the described colour");
        let metadata = metadata.expect("the volume the content was mastered in");
        let expected = tonemap_rgb_signal_to_srgb8(
            [768.0 / 1023.0; 3],
            colour,
            HdrTonemapConfig::from_metadata(Some(metadata)),
        );
        let pixel = frame
            .pixel(u32::try_from(at.0).unwrap(), u32::try_from(at.1).unwrap())
            .expect("inside the window");
        assert_eq!(&pixel[..3], &expected, "{app_id} shows its own volume");
        (pixel[..3] != [0, 0, 0]).then_some(pixel).expect("drawn")
    };
    let cinema_shown = shown("cinema", (100, 150));
    let baseline_shown = shown("baseline", (420, 150));
    assert_ne!(
        cinema_shown, baseline_shown,
        "a master made for 4000 cd/m² compresses differently from one made for 1000"
    );
    let (cinema_id, _) = window_of(&compositor, "cinema");
    let (_, cinema_volume) = compositor.window_color(cinema_id).expect("colour");
    assert_eq!(
        cinema_volume
            .expect("metadata")
            .mastering
            .max_display_mastering_luminance,
        40_000_000,
        "4000 cd/m² in 1/10000 units"
    );
}
