//! `wp_color_management_v1` against a headless compositor: a client is told
//! the HDR descriptions it may build, builds and attaches one, and the
//! compositor keeps it on the window's surface; a client that asks for what
//! the compositor said it does not support is disconnected alone.

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

use std::time::Duration;

use harness::{TestClient, headless, mapped_client, pump};
use tilo_compositor::StackPolicy;

use neighbour_wire::colorimetry::{ColorPrimaries, TransferCharacteristics};
use wayland_protocols::wp::color_management::v1::client::wp_color_manager_v1::{
    Feature, Primaries, RenderIntent, TransferFunction,
};

#[test]
fn client_negotiation_advertises_bt2020_pq_and_parametric_feature() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = TestClient::connect(&mut compositor);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.color_manager_done && clients[0].seen.compositor.is_some()
    });

    let seen = &client.seen;
    assert!(
        seen.color_primaries.contains(&(Primaries::Srgb as u32)),
        "sRGB primaries not advertised: {:?}",
        seen.color_primaries
    );
    assert!(
        seen.color_primaries.contains(&(Primaries::Bt2020 as u32)),
        "BT.2020 primaries not advertised: {:?}",
        seen.color_primaries
    );
    assert!(
        seen.color_transfers
            .contains(&(TransferFunction::St2084Pq as u32)),
        "PQ transfer not advertised: {:?}",
        seen.color_transfers
    );
    assert!(
        seen.color_transfers
            .contains(&(TransferFunction::Hlg as u32)),
        "HLG transfer not advertised: {:?}",
        seen.color_transfers
    );
    assert!(
        seen.color_features.contains(&(Feature::Parametric as u32)),
        "Parametric feature not advertised: {:?}",
        seen.color_features
    );
    assert!(
        seen.color_features
            .contains(&(Feature::SetPrimaries as u32)),
        "SetPrimaries feature not advertised: {:?}",
        seen.color_features
    );
}

#[test]
fn unsupported_icc_new_id_disconnects_only_the_client_without_panicking_dispatch() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = TestClient::connect(&mut compositor);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.color_manager_done
    });

    let manager = client.seen.color_manager.clone().expect("colour manager");
    manager.create_icc_creator(&client.queue.handle(), ());
    client
        .connection
        .flush()
        .expect("flush unsupported request");

    // This exact dispatch used to abort the compositor in wayland-backend's
    // post-callback new-id bookkeeping. A protocol error must be confined to
    // this client instead.
    compositor
        .dispatch(Duration::from_millis(2))
        .expect("unsupported ICC request must not panic server dispatch");
    // Prove the error stayed client-scoped: another ordinary client is
    // served after the first one was killed.
    let mut survivor = TestClient::connect(&mut compositor);
    pump(&mut compositor, &mut [&mut survivor], |_, clients| {
        clients[0].seen.color_manager_done
    });
}

#[test]
fn windows_scrgb_description_becomes_ready() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = TestClient::connect(&mut compositor);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.color_manager_done
    });
    let seen = &client.seen;
    assert!(
        seen.color_features
            .contains(&(Feature::WindowsScrgb as u32)),
        "windows_scrgb feature not advertised: {:?}",
        seen.color_features
    );
    assert!(
        seen.color_transfers
            .contains(&(TransferFunction::ExtLinear as u32)),
        "ext_linear transfer not advertised: {:?}",
        seen.color_transfers
    );

    let manager = client.seen.color_manager.clone().expect("colour manager");
    manager.create_windows_scrgb(&client.queue.handle(), ());
    client.connection.flush().expect("flush scRGB request");
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.color_description_ready
    });
}

#[test]
fn client_attaches_bt2020_pq_image_description_and_the_window_records_it() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "colour", (200, 100), [255, 0, 0, 255]);
    let qh = client.queue.handle();
    let manager = client.seen.color_manager.clone().expect("colour manager");

    // A parametric BT.2020 + PQ description, ready to attach.
    let creator = manager.create_parametric_creator(&qh, ());
    creator.set_primaries_named(Primaries::Bt2020);
    creator.set_tf_named(TransferFunction::St2084Pq);
    let image = creator.create(&qh, ());
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.color_description_ready
    });

    let surface = client.surface.clone().expect("surface");
    let color_surface = manager.get_surface(&surface, &qh, ());
    color_surface.set_image_description(&image, RenderIntent::Perceptual);
    surface.commit();

    let id = compositor.windows().first().expect("the window").0.id;
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        compositor.window_color(id).is_some()
    });
    let (color, hdr) = compositor.window_color(id).expect("the attached colour");
    assert_eq!(color.primaries, ColorPrimaries::Bt2020);
    assert_eq!(color.transfer, TransferCharacteristics::SmpteSt2084);
    assert!(
        hdr.is_some(),
        "PQ content carries the mastering volume it was made for"
    );
}
