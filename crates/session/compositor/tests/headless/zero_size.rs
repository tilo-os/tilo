//! A client cannot bring the desktop down with a size: a window the compositor shows is at least
//! one pixel, whatever its client committed.

use wayland_client::protocol::wl_shm;

use tilo_compositor::StackPolicy;

use crate::harness::{TestClient, headless, pump, settle};

#[test]
fn a_window_whose_buffer_scales_away_is_not_shown_with_no_size() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = TestClient::connect(&mut compositor);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.wm_base.is_some()
    });
    client.open_window("tiny");
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.configured
    });
    // A pixel whose buffer scale divides the window away: once the scale applies, no whole pixel
    // of it reaches the desktop.
    client
        .surface
        .as_ref()
        .expect("a surface")
        .set_buffer_scale(4);
    client.show_words(1, 1, wl_shm::Format::Argb8888, 0xffff_0000);
    settle(&mut compositor, &mut client);

    for (info, (x, y, w, h)) in compositor.windows() {
        assert!(
            w > 0 && h > 0,
            "{} shows at {x},{y} with no size: {w}×{h}",
            info.app_id.unwrap_or_default()
        );
    }
    compositor.render().expect("the desktop draws");

    // The client is neither refused nor held: a buffer of a whole size shows its window.
    client
        .surface
        .as_ref()
        .expect("a surface")
        .set_buffer_scale(1);
    client.show(20, 20, [0, 255, 0, 255]);
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        compositor
            .windows()
            .iter()
            .any(|(_, (_, _, w, h))| *w == 20 && *h == 20)
    });
}
