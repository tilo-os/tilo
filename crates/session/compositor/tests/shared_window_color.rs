//! A shared window's stream is told what the window's content is in: the
//! image description its client commits reaches the share session that
//! carries the window's frames, and unsetting it gives the configured colour
//! back.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion"
)]

#[path = "headless/harness.rs"]
#[allow(
    dead_code,
    reason = "the harness is shared with the other headless tests; this one drives part of it"
)]
mod harness;

use std::sync::Arc;

use harness::{TestClient, headless, mapped_client, pump};
use neighbour_wire::NeighbourMsg;
use neighbour_wire::colorimetry::{
    ColorPrimaries, ColorRange, Colorimetry, MatrixCoefficients, TransferCharacteristics,
};
use neighbour_wire::hdr_metadata::Hdr10Metadata;
use tilo_compositor::{Compositor, ShareStream, StackPolicy};
use tilo_window_share::config::{EncoderChoice, FlatpakShareConfig};
use tilo_window_share::flatpak_share::SharedFlatpakSession;
use wayland_protocols::wp::color_management::v1::client::wp_color_management_surface_v1::WpColorManagementSurfaceV1;
use wayland_protocols::wp::color_management::v1::client::wp_color_manager_v1::{
    Primaries, RenderIntent, TransferFunction,
};

/// A window's share session, as the compositor tells it the window's colour.
struct Tells(Arc<SharedFlatpakSession>);

impl ShareStream for Tells {
    fn set_content_color(&self, colorimetry: Colorimetry, hdr: Option<Hdr10Metadata>) {
        self.0.set_color_override(colorimetry, hdr);
    }

    fn clear_content_color(&self) {
        self.0.clear_color_override();
    }
}

/// What `session` would announce to its viewers now, as `(colour, mastering
/// volume)`: the same report a stream start carries.
fn announced(session: &SharedFlatpakSession) -> (Option<Colorimetry>, Option<Hdr10Metadata>) {
    match session.stream_start_msg() {
        NeighbourMsg::SurfaceStreamStart {
            colorimetry,
            hdr_static_metadata,
            ..
        } => (colorimetry, hdr_static_metadata),
        other => panic!("expected SurfaceStreamStart, got {other:?}"),
    }
}

/// A BT.2020 PQ description, what an HDR video player attaches.
fn hdr10_pq() -> Colorimetry {
    Colorimetry {
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferCharacteristics::SmpteSt2084,
        matrix: MatrixCoefficients::Bt2020NonConstant,
        range: ColorRange::Limited,
    }
}

/// A share of `window` that encodes nothing: the colour is decided before any
/// byte reaches an encoder, so no encoder is needed to see what it announces.
fn shared(compositor: &mut Compositor, window: u64) -> Arc<SharedFlatpakSession> {
    let config = FlatpakShareConfig {
        max_bitrate_kbps: 8000,
        target_fps: 30,
        encoder: EncoderChoice::Software,
        hdr: None,
    };
    let session = Arc::new(
        SharedFlatpakSession::start_paint(1, "colour", window, 200, 100, &config)
            .expect("a paint share needs no encoder"),
    );
    compositor.share_window(window, Arc::new(Tells(Arc::clone(&session))));
    session
}

/// The client attaches a parametric BT.2020 PQ description to its window, as
/// it would once it starts playing HDR video.
fn attach_hdr10_pq(
    compositor: &mut Compositor,
    client: &mut TestClient,
) -> WpColorManagementSurfaceV1 {
    let qh = client.queue.handle();
    let manager = client.seen.color_manager.clone().expect("colour manager");
    let creator = manager.create_parametric_creator(&qh, ());
    creator.set_primaries_named(Primaries::Bt2020);
    creator.set_tf_named(TransferFunction::St2084Pq);
    let image = creator.create(&qh, ());
    pump(compositor, &mut [client], |_, clients| {
        clients[0].seen.color_description_ready
    });

    let surface = client.surface.clone().expect("surface");
    let color_surface = manager.get_surface(&surface, &qh, ());
    color_surface.set_image_description(&image, RenderIntent::Perceptual);
    surface.commit();
    color_surface
}

#[test]
fn a_shared_windows_stream_reports_the_colour_its_client_commits_and_loses_it_when_unset() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "colour", (200, 100), [255, 0, 0, 255]);
    let window = compositor.windows().first().expect("the window").0.id;
    // Shared while its client has said nothing about its colour: the share
    // reports the colour it was set up with.
    let session = shared(&mut compositor, window);
    assert_eq!(announced(&session), (Some(Colorimetry::sdr_bt709()), None));

    let color_surface = attach_hdr10_pq(&mut compositor, &mut client);
    pump(&mut compositor, &mut [&mut client], |_, _| {
        announced(&session).0 == Some(hdr10_pq())
    });
    let (color, hdr) = announced(&session);
    assert_eq!(color, Some(hdr10_pq()), "the client's own description");
    assert_eq!(
        hdr,
        Some(Hdr10Metadata::reference_bt2020()),
        "PQ content carries the volume it was mastered for"
    );

    // The client unsets it, as a player does when it stops the film: the
    // configured colour applies again.
    color_surface.unset_image_description();
    client.surface.as_ref().expect("surface").commit();
    pump(&mut compositor, &mut [&mut client], |_, _| {
        announced(&session).0 == Some(Colorimetry::sdr_bt709())
    });
    assert_eq!(announced(&session), (Some(Colorimetry::sdr_bt709()), None));
}

#[test]
fn a_share_that_starts_after_its_client_said_its_colour_announces_it_at_once() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "colour", (200, 100), [255, 0, 0, 255]);
    let window = compositor.windows().first().expect("the window").0.id;
    attach_hdr10_pq(&mut compositor, &mut client);
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        compositor.window_color(window).is_some()
    });

    // The window was playing HDR before anyone shared it: its share must not
    // wait for the next commit to say so — a still film may not make one.
    let session = shared(&mut compositor, window);
    assert_eq!(announced(&session).0, Some(hdr10_pq()));
}
