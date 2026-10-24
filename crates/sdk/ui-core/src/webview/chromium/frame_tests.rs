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
