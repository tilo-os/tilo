//! Drag-and-drop between real Wayland clients and surface apps on a
//! headless compositor: files travel as `text/uri-list`, byte for byte,
//! dragged with the pointer or a finger; a drag's icon is drawn under it.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::cell::RefCell;
use std::io::{Read as _, Write as _};
use std::os::fd::AsFd as _;
use std::rc::Rc;

use tilo_compositor::{Compositor, InputEvent, StackPolicy};
use wayland_client::protocol::wl_data_device_manager::DndAction;
use wayland_client::protocol::wl_shm;

use crate::harness::{
    DropLog, DropZone, TestClient, URI_LIST, headless, input, map, pump as pump_all,
    pump_one as pump, settle,
};

const BTN_LEFT: u32 = 0x110;
/// Two files, one with an escaped space, as file managers drag them.
const FILES: &[u8] = b"file:///home/someone/a%20b.txt\r\nfile:///home/someone/c.png\r\n";

fn setup(log: &Rc<RefCell<DropLog>>) -> (Compositor, TestClient) {
    let mut compositor = headless(Box::new(StackPolicy));
    compositor.add_surface(Rc::new(RefCell::new(DropZone::new(Rc::clone(log)))));
    let client = TestClient::connect(&mut compositor);
    let mut client = map(
        client,
        &mut compositor,
        "files",
        (200, 100),
        [255, 0, 0, 255],
    );
    assert_eq!(compositor.windows()[0].1, (220, 190, 200, 100));
    client.own_data_device(&mut compositor);
    (compositor, client)
}

#[test]
fn files_dragged_out_of_a_window_drop_on_a_surface() {
    let log = Rc::new(RefCell::new(DropLog::default()));
    let (mut compositor, mut client) = setup(&log);

    input(
        &mut compositor,
        InputEvent::PointerMotion { x: 300.0, y: 240.0 },
    );
    input(
        &mut compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        },
    );
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.pressed.is_some()
    });

    // The window starts a drag of two files from its press.
    let _source = client.start_drag(
        &[URI_LIST, "text/plain;charset=utf-8"],
        FILES,
        DndAction::Move,
        client.seen.pressed.unwrap(),
        None,
    );
    settle(&mut compositor, &mut client);

    input(
        &mut compositor,
        InputEvent::PointerMotion { x: 550.0, y: 70.0 },
    );
    settle(&mut compositor, &mut client);
    let offered = log.borrow().offered.last().cloned().expect("drop offered");
    assert!(offered.contains(&URI_LIST.to_owned()), "{offered:?}");

    input(
        &mut compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: false,
        },
    );
    pump(&mut compositor, &mut client, |_, _| {
        !log.borrow().dropped.is_empty()
    });
    assert_eq!(client.seen.sent, vec![URI_LIST.to_owned()]);
    let dropped = log.borrow().dropped.clone();
    assert_eq!(dropped, vec![(URI_LIST.to_owned(), FILES.to_vec())]);
    pump(&mut compositor, &mut client, |_, client| {
        client
            .seen
            .source_events
            .iter()
            .any(|event| event == "finished")
    });
    let events = &client.seen.source_events;
    assert!(
        !events.iter().any(|event| event == "cancelled"),
        "the source moves its files, it is not told the drag failed: {events:?}"
    );
    let performed = events
        .iter()
        .position(|event| event == "performed")
        .unwrap();
    assert!(
        events[..performed]
            .iter()
            .any(|event| event.contains("Move")),
        "{events:?}"
    );
    assert_eq!(
        log.borrow().buttons,
        Vec::<(u32, bool)>::new(),
        "the drag is no click on the surface"
    );
}

#[test]
fn files_dragged_out_of_a_surface_drop_on_a_window() {
    let log = Rc::new(RefCell::new(DropLog::default()));
    let (mut compositor, mut client) = setup(&log);

    input(
        &mut compositor,
        InputEvent::PointerMotion { x: 550.0, y: 70.0 },
    );
    input(
        &mut compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        },
    );
    assert!(
        compositor
            .start_drag(vec![(URI_LIST.to_owned(), FILES.to_vec())])
            .unwrap()
    );
    // The surface it started on is offered it too, and refuses nothing.
    assert!(!log.borrow().offered.is_empty());

    input(
        &mut compositor,
        InputEvent::PointerMotion { x: 300.0, y: 240.0 },
    );
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.entered.is_some()
    });
    assert!(
        client.seen.offered.contains(&URI_LIST.to_owned()),
        "{:?}",
        client.seen.offered
    );
    assert_eq!(log.borrow().left, 1, "the drag left the surface");

    input(
        &mut compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: false,
        },
    );
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.dropped
    });
    assert_eq!(log.borrow().drag_ended, Some(true));
    assert_eq!(
        log.borrow().buttons,
        vec![(BTN_LEFT, true), (BTN_LEFT, false)],
        "the surface that started the drag sees its button come up"
    );

    let offer = client.seen.entered.clone().unwrap();
    let (mut reader, writer) = std::io::pipe().unwrap();
    offer.receive(URI_LIST.into(), writer.as_fd());
    drop(writer);
    settle(&mut compositor, &mut client);
    let mut received = Vec::new();
    reader.read_to_end(&mut received).unwrap();
    assert_eq!(received, FILES);
    offer.finish();
    settle(&mut compositor, &mut client);
}

/// A file manager that starts a drag while the window under the pointer still has no data device
/// (it binds one when its toolkit is ready) gets the offer on the next move, not never.
#[test]
fn a_drag_from_a_surface_offers_itself_to_a_window_that_binds_its_data_device_mid_drag() {
    let log = Rc::new(RefCell::new(DropLog::default()));
    let mut compositor = headless(Box::new(StackPolicy));
    compositor.add_surface(Rc::new(RefCell::new(DropZone::new(Rc::clone(&log)))));
    let mut client = map(
        TestClient::connect(&mut compositor),
        &mut compositor,
        "files",
        (200, 100),
        [255, 0, 0, 255],
    );

    input(
        &mut compositor,
        InputEvent::PointerMotion { x: 550.0, y: 70.0 },
    );
    input(
        &mut compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        },
    );
    assert!(
        compositor
            .start_drag(vec![(URI_LIST.to_owned(), FILES.to_vec())])
            .unwrap()
    );
    input(
        &mut compositor,
        InputEvent::PointerMotion { x: 300.0, y: 240.0 },
    );
    settle(&mut compositor, &mut client);
    assert!(
        client.seen.offered.is_empty(),
        "the drag is over the window, but its client listens to no data device yet"
    );

    client.own_data_device(&mut compositor);
    input(
        &mut compositor,
        InputEvent::PointerMotion { x: 310.0, y: 245.0 },
    );
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.entered.is_some()
    });
    assert!(
        client.seen.offered.contains(&URI_LIST.to_owned()),
        "{:?}",
        client.seen.offered
    );

    input(
        &mut compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: false,
        },
    );
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.dropped
    });
    let offer = client.seen.entered.clone().unwrap();
    let (mut reader, writer) = std::io::pipe().unwrap();
    offer.receive(URI_LIST.into(), writer.as_fd());
    drop(writer);
    settle(&mut compositor, &mut client);
    let mut received = Vec::new();
    reader.read_to_end(&mut received).unwrap();
    assert_eq!(received, FILES);
    offer.finish();
    settle(&mut compositor, &mut client);
    assert_eq!(log.borrow().drag_ended, Some(true));
}

/// The same for a drag another window starts: the client dragging over a window that binds its
/// data device mid-drag is offered the drag.
#[test]
fn a_drag_from_a_window_offers_itself_to_a_window_that_binds_its_data_device_mid_drag() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut target = map(
        TestClient::connect(&mut compositor),
        &mut compositor,
        "target",
        (200, 100),
        [0, 0, 255, 255],
    );
    let source = TestClient::connect(&mut compositor);
    let mut source = map(
        source,
        &mut compositor,
        "source",
        (100, 50),
        [255, 0, 0, 255],
    );
    source.own_data_device(&mut compositor);

    // The source window (270, 215, 100, 50) stands over the target's middle; pressing inside it
    // starts a drag there, and (240, 200) is the target alone.
    input(
        &mut compositor,
        InputEvent::PointerMotion { x: 300.0, y: 240.0 },
    );
    input(
        &mut compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        },
    );
    pump_all(
        &mut compositor,
        &mut [&mut source, &mut target],
        |_, clients| clients[0].seen.pressed.is_some(),
    );
    let _source = source.start_drag(
        &[URI_LIST],
        FILES,
        DndAction::Copy,
        source.seen.pressed.unwrap(),
        None,
    );
    let mut rounds = 0;
    pump_all(&mut compositor, &mut [&mut source, &mut target], |_, _| {
        rounds += 1;
        rounds >= 5
    });
    // Over the target window, which still binds no data device.
    input(
        &mut compositor,
        InputEvent::PointerMotion { x: 240.0, y: 200.0 },
    );
    let mut rounds = 0;
    pump_all(&mut compositor, &mut [&mut source, &mut target], |_, _| {
        rounds += 1;
        rounds >= 5
    });
    assert!(
        target.seen.offered.is_empty(),
        "the drag is over the window, but its client listens to no data device yet"
    );

    target.own_data_device(&mut compositor);
    input(
        &mut compositor,
        InputEvent::PointerMotion { x: 245.0, y: 205.0 },
    );
    pump_all(
        &mut compositor,
        &mut [&mut source, &mut target],
        |_, clients| clients[1].seen.entered.is_some(),
    );
    assert!(
        target.seen.offered.contains(&URI_LIST.to_owned()),
        "{:?}",
        target.seen.offered
    );

    input(
        &mut compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: false,
        },
    );
    pump_all(
        &mut compositor,
        &mut [&mut source, &mut target],
        |_, clients| clients[1].seen.dropped,
    );
    pump_all(
        &mut compositor,
        &mut [&mut source, &mut target],
        |_, clients| {
            clients[0]
                .seen
                .source_events
                .iter()
                .any(|event| event == "performed")
        },
    );
}

#[test]
fn a_drag_shows_its_icon_under_the_pointer_until_it_drops() {
    let log = Rc::new(RefCell::new(DropLog::default()));
    let (mut compositor, mut client) = setup(&log);
    input(
        &mut compositor,
        InputEvent::PointerMotion { x: 300.0, y: 240.0 },
    );
    input(
        &mut compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        },
    );
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.pressed.is_some()
    });

    // A 10x10 blue icon, its centre on the pointer.
    let qh = client.queue.handle();
    let icon = client
        .seen
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh, ());
    let fd = rustix::fs::memfd_create("tilo-dnd-icon", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
    let mut file = std::fs::File::from(fd);
    file.write_all(&[255, 0, 0, 255].repeat(100)).unwrap();
    let pool = client
        .seen
        .shm
        .as_ref()
        .unwrap()
        .create_pool(file.as_fd(), 400, &qh, ());
    let buffer = pool.create_buffer(0, 10, 10, 40, wl_shm::Format::Argb8888, &qh, ());
    let serial = client.seen.pressed.unwrap();
    let _source = client.start_drag(&[URI_LIST], FILES, DndAction::Copy, serial, Some(&icon));
    icon.attach(Some(&buffer), 0, 0);
    icon.offset(-5, -5);
    icon.damage_buffer(0, 0, 10, 10);
    icon.commit();
    settle(&mut compositor, &mut client);

    input(
        &mut compositor,
        InputEvent::PointerMotion { x: 100.0, y: 400.0 },
    );
    settle(&mut compositor, &mut client);
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let blue = |x, y| frame.pixel(x, y) == Some([0, 0, 255, 255]);
    assert!(
        blue(96, 396) && blue(100, 400) && blue(104, 404),
        "the icon"
    );
    assert!(!blue(106, 406) && !blue(94, 394), "only the icon");

    input(
        &mut compositor,
        InputEvent::PointerButton {
            button: BTN_LEFT,
            pressed: false,
        },
    );
    settle(&mut compositor, &mut client);
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_ne!(
        frame.pixel(100, 400),
        Some([0, 0, 255, 255]),
        "gone with the drop"
    );
}

#[test]
fn files_dragged_out_of_a_window_with_a_finger_drop_on_a_surface() {
    let log = Rc::new(RefCell::new(DropLog::default()));
    let (mut compositor, mut client) = setup(&log);
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.touch.is_some()
    });
    input(
        &mut compositor,
        InputEvent::TouchDown {
            slot: 0,
            x: 300.0,
            y: 240.0,
        },
    );
    input(&mut compositor, InputEvent::TouchFrame);
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.touched.is_some()
    });
    let serial = client.seen.touched.unwrap();
    let _source = client.start_drag(&[URI_LIST], FILES, DndAction::Copy, serial, None);
    settle(&mut compositor, &mut client);

    input(
        &mut compositor,
        InputEvent::TouchMotion {
            slot: 0,
            x: 550.0,
            y: 70.0,
        },
    );
    settle(&mut compositor, &mut client);
    assert!(
        !log.borrow().offered.is_empty(),
        "the finger carries the drag"
    );
    input(&mut compositor, InputEvent::TouchUp { slot: 0 });
    pump(&mut compositor, &mut client, |_, _| {
        !log.borrow().dropped.is_empty()
    });
    assert_eq!(
        log.borrow().dropped.clone(),
        vec![(URI_LIST.to_owned(), FILES.to_vec())]
    );
}

#[test]
fn a_surface_surface_drags_files_with_the_finger_holding_it() {
    let log = Rc::new(RefCell::new(DropLog::default()));
    let (mut compositor, mut client) = setup(&log);
    input(
        &mut compositor,
        InputEvent::TouchDown {
            slot: 3,
            x: 550.0,
            y: 70.0,
        },
    );
    assert!(
        compositor
            .start_drag(vec![(URI_LIST.to_owned(), FILES.to_vec())])
            .unwrap()
    );
    input(
        &mut compositor,
        InputEvent::TouchMotion {
            slot: 3,
            x: 300.0,
            y: 240.0,
        },
    );
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.entered.is_some()
    });
    input(&mut compositor, InputEvent::TouchUp { slot: 3 });
    pump(&mut compositor, &mut client, |_, client| {
        client.seen.dropped
    });
    assert_eq!(log.borrow().drag_ended, Some(true));
    assert_eq!(
        log.borrow().buttons,
        vec![(BTN_LEFT, true), (BTN_LEFT, false)],
        "the surface the finger held sees it lift"
    );
}

/// Words dragged out of a window, as an editor drags a selection.
const WORDS: &[u8] = b"carried across";
const TEXT: &str = "text/plain;charset=utf-8";

tilo_test_env::scenario! {
    /// A drag that leaves for another device, and one that arrives from one.
    fn a_drag_is_carried_between_devices;

    phase a_drag_carried_to_another_device_is_read_and_ends_here_dropped_nowhere;
    {
        let log = Rc::new(RefCell::new(DropLog::default()));
        let (mut compositor, mut client) = setup(&log);
        assert_eq!(compositor.dragged_formats(), None, "no drag runs yet");

        input(
            &mut compositor,
            InputEvent::PointerMotion { x: 300.0, y: 240.0 },
        );
        input(
            &mut compositor,
            InputEvent::PointerButton {
                button: BTN_LEFT,
                pressed: true,
            },
        );
        pump(&mut compositor, &mut client, |_, client| {
            client.seen.pressed.is_some()
        });
        let _source = client.start_drag(&[TEXT], WORDS, DndAction::Copy, client.seen.pressed.unwrap(), None);
        settle(&mut compositor, &mut client);
        input(
            &mut compositor,
            InputEvent::PointerMotion { x: 310.0, y: 245.0 },
        );
        settle(&mut compositor, &mut client);
        assert_eq!(compositor.dragged_formats(), Some(vec![TEXT.to_owned()]));

        // What crosses is read from the source while the drag still runs.
        let mut pull = compositor.pull_drag(TEXT).unwrap();
        let mut read = None;
        pump(&mut compositor, &mut client, |_, _| {
            if let tilo_compositor::clipboard_history::Pulled::Ready(bytes) = pull.drain() {
                read = Some(bytes);
            }
            read.is_some()
        });
        assert_eq!(read.as_deref(), Some(WORDS));

        // The drag went on on the other device: here it ends over the window it was over, dropped
        // nowhere, and its source hears it was cancelled.
        assert!(compositor.cancel_drag().unwrap());
        pump(&mut compositor, &mut client, |_, client| {
            client
                .seen
                .source_events
                .iter()
                .any(|event| event == "cancelled")
        });
        assert!(!client.seen.dropped, "the window under it took no drop");
        assert_eq!(compositor.dragged_formats(), None);
        input(
            &mut compositor,
            InputEvent::PointerButton {
                button: BTN_LEFT,
                pressed: false,
            },
        );
        settle(&mut compositor, &mut client);
        assert!(!client.seen.dropped, "the release after it drops nothing");
    }

    phase a_drag_carried_here_from_another_device_drops_where_the_release_comes;
    {
        let log = Rc::new(RefCell::new(DropLog::default()));
        let (mut compositor, mut client) = setup(&log);
        // The pointer arrived from the other device with the button held there: nothing here was
        // pressed.
        input(
            &mut compositor,
            InputEvent::PointerMotion { x: 100.0, y: 100.0 },
        );
        assert!(
            compositor
                .carry_drag_here(vec![(URI_LIST.to_owned(), WORDS.to_vec())])
                .unwrap()
        );
        input(
            &mut compositor,
            InputEvent::PointerMotion { x: 300.0, y: 240.0 },
        );
        pump(&mut compositor, &mut client, |_, client| {
            client.seen.entered.is_some()
        });
        input(
            &mut compositor,
            InputEvent::PointerButton {
                button: BTN_LEFT,
                pressed: false,
            },
        );
        pump(&mut compositor, &mut client, |_, client| {
            client.seen.dropped
        });
        let offer = client.seen.entered.clone().unwrap();
        let (mut reader, writer) = std::io::pipe().unwrap();
        offer.receive(URI_LIST.into(), writer.as_fd());
        drop(writer);
        settle(&mut compositor, &mut client);
        let mut received = Vec::new();
        reader.read_to_end(&mut received).unwrap();
        assert_eq!(received, WORDS);
        assert_eq!(
            log.borrow().buttons,
            Vec::<(u32, bool)>::new(),
            "no app here started it, so none hears its button"
        );
    }
}
