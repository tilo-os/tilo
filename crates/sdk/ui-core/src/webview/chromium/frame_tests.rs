use super::*;

#[test]
fn repeated_full_size_packets_keep_one_frame_but_events_still_wake() {
    use std::sync::atomic::AtomicUsize;

    let shared = Arc::new(Shared::default());
    let wakes = Arc::new(AtomicUsize::new(0));
    let observed = wakes.clone();
    *shared.waker.lock().unwrap() = Some(Arc::new(move || {
        observed.fetch_add(1, Ordering::Relaxed);
    }));
    let routes = Arc::new(Mutex::new(HashMap::from([(1, Arc::downgrade(&shared))])));
    let frame = protocol::WireFrame {
        bgra: vec![255; 868 * 772 * 4],
        width: 868,
        height: 772,
    };
    let mut packet = Vec::new();
    protocol::write_tab_frame(&mut packet, 1, &frame).unwrap();
    let mut retained = None;
    for _ in 0..40 {
        let Some(protocol::Packet::TabFrame(tab, decoded)) =
            protocol::read_packet(&mut std::io::Cursor::new(&packet)).unwrap()
        else {
            panic!("expected routed full-size frame");
        };
        deliver_frame(&routes, tab, decoded);
        let pixels = shared.frames.lock().unwrap().as_ref().unwrap().rgba.clone();
        if let Some(first) = &retained {
            assert!(Arc::ptr_eq(first, &pixels));
        } else {
            retained = Some(pixels);
        }
    }
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
    shared.event(WebViewEvent::TitleChanged("Updated title".into()));
    assert_eq!(wakes.load(Ordering::Relaxed), 2);
    assert!(matches!(
        shared.events.lock().unwrap().front(),
        Some(WebViewEvent::TitleChanged(title)) if title == "Updated title"
    ));
}

#[test]
fn a_client_tabs_frame_passes_through_the_owner_as_the_host_sent_it() {
    let outbox = Arc::new(shared::Outbox::default());
    let shared = Arc::new(Shared {
        remote: Some(outbox.clone()),
        ..Shared::default()
    });
    let routes = Arc::new(Mutex::new(HashMap::from([(2, Arc::downgrade(&shared))])));
    let mut packet = Vec::new();
    protocol::write_tab_frame(
        &mut packet,
        2,
        &protocol::WireFrame {
            bgra: vec![10, 20, 30, 255, 40, 50, 60, 255],
            width: 2,
            height: 1,
        },
    )
    .unwrap();
    let Some(protocol::Packet::TabFrame(tab, frame)) =
        protocol::read_packet(&mut std::io::Cursor::new(&packet)).unwrap()
    else {
        panic!("expected a routed frame");
    };
    let received = frame.bgra.as_ptr();
    deliver_frame(&routes, tab, frame);
    let (forwarded_tab, forwarded) = outbox.take_frame().expect("the client's frame is queued");
    assert_eq!(forwarded_tab, 2);
    assert_eq!(
        forwarded.bgra.as_ptr(),
        received,
        "forwarded in the buffer it was read into, neither copied nor converted"
    );
    let mut written = Vec::new();
    protocol::write_tab_frame(&mut written, forwarded_tab, &forwarded).unwrap();
    assert_eq!(written, packet, "the client gets the packet the host sent");
    assert!(
        shared.frames.lock().unwrap().is_none(),
        "the owner draws none of it"
    );
}

#[test]
fn unchanged_frames_do_not_wake_or_replace_the_painted_pixels() {
    use std::sync::atomic::AtomicUsize;

    let shared = Arc::new(Shared::default());
    let wakes = Arc::new(AtomicUsize::new(0));
    let observed = wakes.clone();
    *shared.waker.lock().unwrap() = Some(Arc::new(move || {
        observed.fetch_add(1, Ordering::Relaxed);
    }));
    let pixels = Arc::new(vec![255; 8]);
    let frame = |width, height, rgba, trace_id| super::super::Frame {
        trace_id,
        damage: None,
        rgba,
        width,
        height,
    };
    present_frame(&shared, 1, frame(2, 1, pixels.clone(), Some(1)));
    present_frame(&shared, 1, frame(2, 1, Arc::new(vec![255; 8]), Some(2)));
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
    let retained = shared.frames.lock().unwrap().clone().unwrap();
    assert!(Arc::ptr_eq(&retained.rgba, &pixels));
    assert_eq!(retained.trace_id, Some(1));

    let changed = Arc::new(vec![0, 255, 255, 255, 255, 255, 255, 255]);
    present_frame(&shared, 1, frame(2, 1, changed.clone(), Some(3)));
    assert_eq!(wakes.load(Ordering::Relaxed), 2);
    assert!(Arc::ptr_eq(
        &shared.frames.lock().unwrap().as_ref().unwrap().rgba,
        &changed,
    ));
    present_frame(&shared, 1, frame(1, 2, changed, Some(4)));
    assert_eq!(wakes.load(Ordering::Relaxed), 3);
    let resized = shared.frames.lock().unwrap().clone().unwrap();
    assert_eq!(
        (resized.width, resized.height, resized.trace_id),
        (1, 2, Some(4))
    );

    // A new view needs its first frame and wake even when its pixels match
    // the document whose retained frame was explicitly cleared.
    *shared.frames.lock().unwrap() = None;
    present_frame(&shared, 1, frame(1, 2, resized.rgba.clone(), Some(5)));
    assert_eq!(wakes.load(Ordering::Relaxed), 4);
    assert_eq!(
        shared.frames.lock().unwrap().as_ref().unwrap().trace_id,
        Some(5)
    );
}

fn wire_damage(rects: Vec<crate::webview::PixelRect>, bgra: [u8; 4]) -> damage::WireDamage {
    let area: u32 = rects.iter().map(|rect| rect.width * rect.height).sum();
    damage::WireDamage {
        width: 64,
        height: 48,
        rects,
        pixels: bgra.repeat(area as usize),
    }
}

fn caret() -> crate::webview::PixelRect {
    crate::webview::PixelRect {
        x: 20,
        y: 10,
        width: 2,
        height: 16,
    }
}

fn queued(tx: &CommandQueue) -> Vec<Value> {
    tx.state
        .lock()
        .unwrap()
        .0
        .iter()
        .map(|(command, _)| command.clone())
        .collect()
}

#[test]
fn a_caret_blink_changes_only_its_rect_of_the_shown_frame() {
    use std::sync::atomic::AtomicUsize;

    let shared = Arc::new(Shared::default());
    let wakes = Arc::new(AtomicUsize::new(0));
    let observed = wakes.clone();
    *shared.waker.lock().unwrap() = Some(Arc::new(move || {
        observed.fetch_add(1, Ordering::Relaxed);
    }));
    let routes = Arc::new(Mutex::new(HashMap::from([(3, Arc::downgrade(&shared))])));
    let tx = CommandQueue::default();
    deliver_frame(
        &routes,
        3,
        protocol::WireFrame {
            bgra: vec![255; 64 * 48 * 4],
            width: 64,
            height: 48,
        },
    );
    let before = shared.frames.lock().unwrap().clone().unwrap();

    let mut packet = Vec::new();
    damage::write(
        &mut packet,
        3,
        &wire_damage(vec![caret()], [10, 20, 30, 255]),
    )
    .unwrap();
    assert!(
        packet.len() * 20 < 64 * 48 * 4,
        "{} bytes for the caret, not the frame",
        packet.len()
    );
    let Some(protocol::Packet::TabDamage(tab, sent)) =
        protocol::read_packet(&mut std::io::Cursor::new(&packet)).unwrap()
    else {
        panic!("expected a damage packet");
    };
    deliver_damage(&routes, tab, sent, &tx);

    let after = shared.frames.lock().unwrap().clone().unwrap();
    assert_eq!(wakes.load(Ordering::Relaxed), 2, "the view wakes for it");
    let pixel = |frame: &super::super::Frame, x: usize, y: usize| {
        let at = (y * 64 + x) * 4;
        frame.rgba[at..at + 4].to_vec()
    };
    assert_eq!(
        pixel(&after, 20, 10),
        [30, 20, 10, 255],
        "converted to RGBA"
    );
    assert_eq!(pixel(&after, 21, 25), [30, 20, 10, 255]);
    for (x, y) in [(19, 10), (22, 10), (20, 9), (20, 26), (0, 0), (63, 47)] {
        assert_eq!(pixel(&after, x, y), [255; 4], "({x}, {y}) is as it was");
    }
    assert_eq!(
        pixel(&before, 20, 10),
        [255; 4],
        "the frame shown before is kept"
    );
    let generation = before.damage.unwrap().generation;
    assert_eq!(
        after.damage.unwrap().since(generation),
        Some(vec![caret()]),
        "the view is told where"
    );
    assert!(queued(&tx).is_empty(), "nothing asked of the host");

    // The same rect again changes nothing and wakes nothing.
    deliver_damage(
        &routes,
        3,
        wire_damage(vec![caret()], [10, 20, 30, 255]),
        &tx,
    );
    assert_eq!(wakes.load(Ordering::Relaxed), 2);
}

#[test]
fn damage_for_a_frame_the_tab_lacks_asks_the_host_for_it_whole_once() {
    let shared = Arc::new(Shared::default());
    let routes = Arc::new(Mutex::new(HashMap::from([(4, Arc::downgrade(&shared))])));
    let tx = CommandQueue::default();
    deliver_damage(&routes, 4, wire_damage(vec![caret()], [0, 0, 0, 255]), &tx);
    deliver_damage(&routes, 4, wire_damage(vec![caret()], [0, 0, 0, 255]), &tx);
    assert!(shared.frames.lock().unwrap().is_none(), "nothing to show");
    assert_eq!(
        queued(&tx),
        [serde_json::json!({"op":"resend_frame","tab":4})],
        "asked once"
    );
    // The frame whole, then damage for a frame of other dimensions.
    deliver_frame(
        &routes,
        4,
        protocol::WireFrame {
            bgra: vec![0; 32 * 48 * 4],
            width: 32,
            height: 48,
        },
    );
    assert!(shared.frames.lock().unwrap().is_some());
    deliver_damage(&routes, 4, wire_damage(vec![caret()], [0, 0, 0, 255]), &tx);
    assert_eq!(queued(&tx).len(), 2, "asked again");
}

#[test]
fn a_client_tabs_damage_folds_into_what_the_owner_queued_for_it() {
    let outbox = Arc::new(shared::Outbox::default());
    let shared = Arc::new(Shared {
        remote: Some(outbox.clone()),
        ..Shared::default()
    });
    let routes = Arc::new(Mutex::new(HashMap::from([(2, Arc::downgrade(&shared))])));
    let tx = CommandQueue::default();

    // Onto a whole frame not yet sent, the rect is written into it.
    deliver_frame(
        &routes,
        2,
        protocol::WireFrame {
            bgra: vec![0; 64 * 48 * 4],
            width: 64,
            height: 48,
        },
    );
    deliver_damage(&routes, 2, wire_damage(vec![caret()], [9, 8, 7, 255]), &tx);
    let (_, whole) = outbox.take_frame().expect("the frame, whole");
    let at = (10 * 64 + 20) * 4;
    assert_eq!(&whole.bgra[at..at + 4], &[9, 8, 7, 255], "as sent: BGRA");
    assert!(
        shared.frames.lock().unwrap().is_none(),
        "the owner draws none of it"
    );

    // Two changes not yet sent go as one, in order.
    let other = crate::webview::PixelRect {
        x: 40,
        y: 30,
        width: 4,
        height: 4,
    };
    deliver_damage(&routes, 2, wire_damage(vec![caret()], [1, 1, 1, 255]), &tx);
    deliver_damage(&routes, 2, wire_damage(vec![other], [2, 2, 2, 255]), &tx);
    let (tab, folded) = outbox.take_damage().expect("one damage packet");
    assert_eq!(tab, 2);
    assert_eq!(folded.rects, [caret(), other]);
    assert!(queued(&tx).is_empty());

    // More change than a whole frame: the host sends it whole, and what
    // changed meanwhile waits for it.
    let all = crate::webview::PixelRect {
        x: 0,
        y: 0,
        width: 64,
        height: 30,
    };
    deliver_damage(&routes, 2, wire_damage(vec![all], [3, 3, 3, 255]), &tx);
    deliver_damage(&routes, 2, wire_damage(vec![all], [4, 4, 4, 255]), &tx);
    deliver_damage(&routes, 2, wire_damage(vec![caret()], [5, 5, 5, 255]), &tx);
    assert_eq!(
        queued(&tx),
        [serde_json::json!({"op":"resend_frame","tab":2})]
    );
    assert!(
        outbox.take_damage().is_none(),
        "nothing the client could apply"
    );
    deliver_frame(
        &routes,
        2,
        protocol::WireFrame {
            bgra: vec![0; 64 * 48 * 4],
            width: 64,
            height: 48,
        },
    );
    assert!(outbox.take_frame().is_some());
    deliver_damage(&routes, 2, wire_damage(vec![caret()], [6, 6, 6, 255]), &tx);
    assert!(outbox.take_damage().is_some(), "changes flow again");
}
