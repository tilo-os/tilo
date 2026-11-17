//! An app's dma-bufs behind a keeper: the desktop gets the app's own buffer, never a copy, and
//! the feedback that says where to allocate travels both ways across a restart. The buffers are
//! made through `/dev/udmabuf`, so no GPU is needed; a machine without it hands over the memfd
//! behind it instead, and says so.

use std::os::fd::OwnedFd;

use wayland_client::protocol::wl_buffer::WlBuffer;

use crate::support::{ARGB8888, App, Rig, SurfaceFeedbackSeen, XRGB8888, dmabuf_plane};

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const SIZE: (i32, i32) = (64, 48);

/// A dma-buf of `SIZE` in `rgba`.
fn made(rgba: [u8; 4]) -> OwnedFd {
    let (fd, fallback) = dmabuf_plane(SIZE, rgba);
    if let Some(why) = fallback {
        eprintln!("NOTE: {why}; the plane is the memfd a udmabuf would pin");
    }
    fd
}

fn identity(fd: &OwnedFd) -> (u64, u64) {
    let stat = rustix::fs::fstat(fd).expect("the dma-buf's identity");
    (stat.st_dev, stat.st_ino)
}

fn buffer(app: &App, fd: &OwnedFd, format: u32, tag: u32) -> WlBuffer {
    app.dmabuf_from(fd, (SIZE, 0, SIZE.0 as u32 * 4), format, tag)
}

/// What the desktop shows of the only window: the first pixel and the dma-buf's identity.
fn shown(rig: &mut Rig) -> Option<([u8; 4], (u64, u64))> {
    let id = rig.desktop().windows().first()?.0.id;
    let (width, height, pixels) = rig.desktop().window_dmabuf_pixels(id)?;
    assert_eq!((width, height), SIZE);
    let first = pixels.get(..4)?.try_into().ok()?;
    Some((first, rig.desktop().window_dmabuf_id(id)?))
}

#[test]
fn an_apps_dma_buf_reaches_the_desktop_as_the_same_buffer_and_returns_after_a_restart() {
    let (red, blue) = (made(RED), made(BLUE));
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Video", Some("video"));
    rig.pump(&mut [&mut app], |_, apps| apps[0].heard.dmabuf.is_some());
    let first = buffer(&app, &red, ARGB8888, 1);
    app.commit_buffer(&first, SIZE);
    rig.pump(&mut [&mut app], |rig, _| shown(rig).is_some());
    assert_eq!(
        shown(&mut rig),
        Some((RED, identity(&red))),
        "the desktop shows the app's own dma-buf, not a copy of it"
    );

    // A second buffer replaces the first: the first comes back to the app once the desktop
    // lets go of it, the one shown never does.
    let second = buffer(&app, &blue, ARGB8888, 2);
    app.commit_buffer(&second, SIZE);
    rig.pump(&mut [&mut app], |rig, apps| {
        shown(rig).map(|(pixel, _)| pixel) == Some(BLUE) && apps[0].heard.dmabufs_released == [1]
    });
    assert_eq!(shown(&mut rig), Some((BLUE, identity(&blue))));
    // The released buffer is the app's to use again.
    app.commit_buffer(&first, SIZE);
    rig.pump(&mut [&mut app], |rig, apps| {
        shown(rig).map(|(pixel, _)| pixel) == Some(RED) && apps[0].heard.dmabufs_released == [1, 2]
    });
    assert_eq!(shown(&mut rig), Some((RED, identity(&red))));

    // The keeper holds the buffer while the desktop is away and makes it again on the next
    // one from the same planes, with the app committing nothing; it stays the app's.
    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    assert_eq!(
        app.heard.dmabufs_released,
        [1, 2],
        "the shown buffer is held"
    );
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| shown(rig).is_some());
    assert_eq!(shown(&mut rig), Some((RED, identity(&red))));

    // A desktop that takes no ARGB8888 dma-buf is not handed the held one: the keeper would
    // break its connection, and every window with it. The app hears the desktop's formats
    // and draws the next frame in one it takes.
    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    rig.attach_new_with_dmabufs(&[XRGB8888]);
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0].heard.dmabuf_formats == [XRGB8888]
    });
    rig.settle(&mut [&mut app]);
    assert_eq!(rig.keeper.describe().fault, None);
    let opaque = buffer(&app, &blue, XRGB8888, 3);
    app.commit_buffer(&opaque, SIZE);
    rig.pump(&mut [&mut app], |rig, _| shown(rig).is_some());
    assert_eq!(shown(&mut rig), Some((BLUE, identity(&blue))));
    assert!(!app.broken);
    assert_eq!(rig.keeper.describe().fault, None);
}

#[test]
fn the_desktops_dma_buf_feedback_reaches_the_app_and_is_kept_across_a_restart() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Video", Some("video"));
    app.show(&mut rig, (100, 80), GREEN);
    rig.pump(&mut [&mut app], |_, apps| apps[0].heard.dmabuf_done > 0);
    assert_eq!(
        app.heard.dmabuf_formats,
        [ARGB8888, XRGB8888],
        "the default feedback names the desktop's formats"
    );
    let dmabuf = app.heard.dmabuf.clone().unwrap();
    let surface = app.surface.clone().unwrap();
    let qh = app.queue.handle();
    let _feedback = dmabuf.get_surface_feedback(&surface, &qh, SurfaceFeedbackSeen);
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0].heard.surface_feedback_done > 0
    });
    assert_eq!(app.heard.surface_formats, [ARGB8888, XRGB8888]);

    // While the desktop is away, the surface's feedback is what the last desktop said.
    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    let seen = app.heard.surface_feedback_done;
    let _away = dmabuf.get_surface_feedback(&surface, &qh, SurfaceFeedbackSeen);
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0].heard.surface_feedback_done > seen
    });
    assert_eq!(app.heard.surface_formats, [ARGB8888, XRGB8888]);

    // A desktop that says otherwise is passed on to the objects the app already holds.
    let (defaults, surfaces) = (app.heard.dmabuf_done, app.heard.surface_feedback_done);
    rig.attach_new_with_dmabufs(&[XRGB8888]);
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0].heard.dmabuf_done > defaults && apps[0].heard.surface_feedback_done > surfaces
    });
    assert_eq!(app.heard.dmabuf_formats, [XRGB8888]);
    assert_eq!(app.heard.surface_formats, [XRGB8888]);
    assert!(!app.broken);
    assert_eq!(rig.keeper.describe().fault, None);
}

#[test]
fn a_malformed_dma_buf_ends_only_the_app_that_sent_it() {
    let red = made(RED);
    let mut rig = Rig::new(None);
    let mut good = App::connect(&mut rig);
    good.open_window(&mut rig, "Good", Some("good"));
    good.show(&mut rig, (100, 80), GREEN);
    let mut bad = App::connect(&mut rig);
    bad.open_window(&mut rig, "Bad", Some("bad"));
    rig.pump(&mut [&mut good, &mut bad], |_, apps| {
        apps[1].heard.dmabuf.is_some()
    });

    // Rows wider than the buffer holds: the planes reach past its end.
    let past_end = bad.dmabuf_from(&red, (SIZE, 0, SIZE.0 as u32 * 4 * 64), ARGB8888, 1);
    bad.commit_buffer(&past_end, SIZE);
    rig.pump(&mut [&mut good, &mut bad], |_, apps| apps[1].broken);
    rig.settle(&mut [&mut good, &mut bad]);
    assert!(!good.broken, "the other app keeps running");
    assert_eq!(
        rig.keeper.describe().fault,
        None,
        "the desktop never saw it"
    );
    assert_eq!(rig.windows().len(), 1, "the other app's window stays");

    // A plane that is no buffer at all (a pipe has no size) is refused the same way, before the
    // desktop's renderer could try to map it.
    let mut odd = App::connect(&mut rig);
    odd.open_window(&mut rig, "Odd", Some("odd"));
    rig.pump(&mut [&mut good, &mut odd], |_, apps| {
        apps[1].heard.dmabuf.is_some()
    });
    let (pipe, _writer) = std::io::pipe().expect("a pipe");
    let not_a_buffer = odd.dmabuf_from(
        &OwnedFd::from(pipe),
        (SIZE, 0, SIZE.0 as u32 * 4),
        ARGB8888,
        2,
    );
    odd.commit_buffer(&not_a_buffer, SIZE);
    rig.pump(&mut [&mut good, &mut odd], |_, apps| apps[1].broken);
    rig.desktop().render().expect("a frame");
    rig.settle(&mut [&mut good, &mut odd]);
    assert!(!good.broken);
    assert_eq!(rig.keeper.describe().fault, None);
    assert_eq!(rig.windows().len(), 1);
}
