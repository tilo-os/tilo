//! A window's own artwork: a program that draws no icons of its own in the
//! shell can hand one over, and that is what its window shows.

use super::*;

/// The icon a window's facts carry.
fn window_icon(compositor: &Compositor, id: u64) -> Option<std::sync::Arc<WindowIcon>> {
    compositor
        .windows()
        .into_iter()
        .find(|(info, _)| info.id == id)
        .and_then(|(info, _)| info.icon)
}

/// The id of the window whose program is `app_id`.
fn window_id(compositor: &Compositor, app_id: &str) -> u64 {
    compositor
        .windows()
        .into_iter()
        .find(|(info, _)| info.app_id.as_deref() == Some(app_id))
        .map(|(info, _)| info.id)
        .expect("the window is mapped")
}

/// Let both sides exchange what they queued, without waiting for anything:
/// what the compositor made of the requests is known afterwards.
fn quiet(compositor: &mut Compositor, client: &mut TestClient) {
    for _ in 0..4 {
        client.exchange();
        compositor
            .dispatch(Duration::from_millis(2))
            .expect("dispatch");
        client.exchange();
    }
}

#[test]
fn a_window_that_sets_its_own_icon_carries_it_in_its_facts() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(
        &mut compositor,
        "org.example.UnlistedGame",
        (200, 160),
        [20, 30, 40, 255],
    );
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.icon_manager.is_some()
    });
    let mut sizes = client.seen.icon_sizes.clone();
    sizes.sort_unstable();
    assert_eq!(
        sizes,
        vec![16, 32, 48],
        "clients hear the sizes a tab pill paints at"
    );

    let id = window_id(&compositor, "org.example.UnlistedGame");
    assert!(
        window_icon(&compositor, id).is_none(),
        "a window that sets no icon shows nothing of its own"
    );

    let qh = client.queue.handle();
    let manager = client.seen.icon_manager.clone().expect("manager");
    let small = client.icon_buffer(16, [32, 128, 192, 255]);
    let large = client.icon_buffer(32, [32, 128, 192, 255]);
    let icon = manager.create_icon(&qh, ());
    icon.add_buffer(&small, 1);
    icon.add_buffer(&large, 1);
    manager.set_icon(client.toplevel.as_ref().expect("toplevel"), Some(&icon));
    quiet(&mut compositor, &mut client);
    assert!(
        window_icon(&compositor, id).is_none(),
        "the icon is double buffered: nothing changes before the commit"
    );

    client.surface.as_ref().expect("surface").commit();
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        window_icon(compositor, id).is_some()
    });
    let own = window_icon(&compositor, id).expect("the window's own icon");
    let IconArtwork::Pixels(images) = own.artwork() else {
        panic!("the pixels the window handed over");
    };
    let edges: Vec<u32> = images.iter().map(|image| image.width).collect();
    assert_eq!(edges, vec![16, 32], "every size the window offered is kept");
    assert_eq!(
        &images[1].pixels[..4],
        [32, 128, 192, 255],
        "the pixels are the window's, as premultiplied RGBA"
    );

    manager.set_icon(client.toplevel.as_ref().expect("toplevel"), None);
    client.surface.as_ref().expect("surface").commit();
    quiet(&mut compositor, &mut client);
    assert!(
        window_icon(&compositor, id).is_none(),
        "unsetting the icon returns the window to its program's artwork"
    );
}

#[test]
fn a_client_that_frees_its_icon_and_its_buffer_keeps_the_icon() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(
        &mut compositor,
        "org.example.Browser",
        (200, 160),
        [20, 30, 40, 255],
    );
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.icon_manager.is_some()
    });
    let id = window_id(&compositor, "org.example.Browser");

    let qh = client.queue.handle();
    let manager = client.seen.icon_manager.clone().expect("manager");
    let buffer = client.icon_buffer(32, [16, 32, 48, 255]);
    let icon = manager.create_icon(&qh, ());
    icon.add_buffer(&buffer, 1);
    manager.set_icon(client.toplevel.as_ref().expect("toplevel"), Some(&icon));
    // A browser frees the icon object, then its buffer, then commits.
    icon.destroy();
    buffer.destroy();
    client.surface.as_ref().expect("surface").commit();
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        window_icon(compositor, id).is_some()
    });
    let own = window_icon(&compositor, id).expect("the icon outlives its protocol objects");
    let IconArtwork::Pixels(images) = own.artwork() else {
        panic!("the pixels the window handed over");
    };
    assert_eq!(&images[0].pixels[..4], [16, 32, 48, 255]);

    client.open_window("org.example.Browser.Second");
    client.show(100, 80, [1, 2, 3, 255]);
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        compositor.windows().len() == 2
    });
}

#[test]
fn a_window_that_names_a_theme_icon_shows_that_name() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(
        &mut compositor,
        "org.example.Launcher",
        (200, 160),
        [20, 30, 40, 255],
    );
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.icon_manager.is_some()
    });
    let id = window_id(&compositor, "org.example.Launcher");

    let qh = client.queue.handle();
    let manager = client.seen.icon_manager.clone().expect("manager");
    let icon = manager.create_icon(&qh, ());
    icon.set_name("org.example.Launcher".to_owned());
    manager.set_icon(client.toplevel.as_ref().expect("toplevel"), Some(&icon));
    client.surface.as_ref().expect("surface").commit();
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        window_icon(compositor, id).is_some()
    });
    let own = window_icon(&compositor, id).expect("the window's own icon");
    assert_eq!(
        own.artwork(),
        &IconArtwork::Named("org.example.Launcher".to_owned()),
        "a window that hands over no pixels names what to draw"
    );
}

#[test]
fn an_icon_with_nothing_in_it_clears_the_one_a_window_had() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(
        &mut compositor,
        "org.example.Game",
        (200, 160),
        [20, 30, 40, 255],
    );
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.icon_manager.is_some()
    });
    let id = window_id(&compositor, "org.example.Game");

    let qh = client.queue.handle();
    let manager = client.seen.icon_manager.clone().expect("manager");
    let buffer = client.icon_buffer(32, [16, 32, 48, 255]);
    let icon = manager.create_icon(&qh, ());
    icon.add_buffer(&buffer, 1);
    manager.set_icon(client.toplevel.as_ref().expect("toplevel"), Some(&icon));
    client.surface.as_ref().expect("surface").commit();
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        window_icon(compositor, id).is_some()
    });

    // An icon with neither a name nor pixels resets the window's icon, as a
    // null icon does.
    let empty = manager.create_icon(&qh, ());
    manager.set_icon(client.toplevel.as_ref().expect("toplevel"), Some(&empty));
    client.surface.as_ref().expect("surface").commit();
    quiet(&mut compositor, &mut client);
    assert!(
        window_icon(&compositor, id).is_none(),
        "an empty icon returns the window to its program's artwork"
    );
}
