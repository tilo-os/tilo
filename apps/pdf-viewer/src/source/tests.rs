//! Source-layer tests: the worker queue bounds, coalesces and isolates sessions, and private saves stay private.

use super::*;

#[test]
fn evicted_page_results_notify_only_their_session_once() {
    let shared = queue();
    let first = test_handle(Arc::clone(&shared), 1);
    let second = test_handle(Arc::clone(&shared), 2);
    // Share one allocation, but account each queued image's full size.
    let image = ui::raster::rgba8(vec![0; 4 * 1024 * 1024], 1024, 1024);
    for page in 0..12 {
        push_event(
            &shared,
            1,
            RasterEvent::Rendered {
                request: RenderRequest {
                    document_id: 1,
                    page,
                    target_width: 1024,
                },
                image: Some(image.clone()),
            },
        );
    }
    let state = shared.state.lock().expect("queue lock");
    assert!(state.event_bytes <= MAX_PENDING_RESULT_BYTES);
    assert_eq!(state.evicted_sessions, HashSet::from([1]));
    drop(state);
    assert!(
        second.drain_events().is_empty(),
        "another viewer is unaffected"
    );
    let events = first.drain_events();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, RasterEvent::VisualWorkEvicted,))
            .count(),
        1,
        "loss is coalesced without losing its completion signal"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RasterEvent::Rendered { image: Some(_), .. },)),
        "the remaining images still reach the viewer"
    );
    assert!(
        first.drain_events().is_empty(),
        "the notification was consumed"
    );
}

fn test_handle(queue: Arc<WorkerQueue>, id: u64) -> RasterHandle {
    RasterHandle {
        session: Arc::new(SessionGuard {
            queue,
            id,
            closed: std::sync::atomic::AtomicBool::new(false),
        }),
    }
}

fn tile(document_id: u64, level: i32, col: u32, row: u32) -> TileRequest {
    TileRequest {
        document_id,
        page: 0,
        level,
        col,
        row,
    }
}

fn queue() -> Arc<WorkerQueue> {
    Arc::new(WorkerQueue {
        state: Mutex::new(QueueState::default()),
        ready: Condvar::new(),
    })
}

fn open_request(id: u64, path: &str) -> OpenRequest {
    OpenRequest {
        id,
        path: path.into(),
    }
}

fn open(id: u64, path: &str) -> RasterCommand {
    RasterCommand::OpenPath(open_request(id, path))
}

fn text(document_id: u64, page: usize) -> TextRequest {
    TextRequest { document_id, page }
}

#[test]
fn inputs_and_results_are_bounded_before_allocation() {
    let file = tempfile::NamedTempFile::new().expect("temporary source");
    std::fs::write(file.path(), b"12345").expect("write source");
    let error = read_file_bounded(file.path(), 4).expect_err("source exceeds cap");
    assert!(error.to_string().contains("opens files up to"));

    // (width, height, valid)
    for (w, h, valid) in [
        (612.0, 792.0, true),
        (f64::NAN, 792.0, false),
        (0.0, 792.0, false),
        (MAX_PAGE_EDGE_POINTS + 1.0, 792.0, false),
    ] {
        assert_eq!(valid_page_size(w, h), valid, "page size {w}x{h}");
    }

    let bucket = RasterBackend::bucket_for_page(4_096, (612.0, 20_000.0))
        .expect("a tall but valid page still gets a bounded raster");
    let pixels = f64::from(bucket) * f64::from(bucket) * 20_000.0 / 612.0;
    assert!(pixels <= MAX_RASTER_PIXELS * 1.01);

    let counter = std::sync::atomic::AtomicU64::new(u64::MAX - 1);
    assert_eq!(next_open_request_id(&counter), Ok(u64::MAX - 1));
    assert_eq!(
        next_open_request_id(&counter),
        Err(RasterSubmitError::Exhausted)
    );

    // Result payloads are byte bounded.
    let request = RenderRequest {
        document_id: 1,
        page: 0,
        target_width: 4_096,
    };
    let oversized_image = ui::raster::rgba8(vec![0; MAX_PENDING_RESULT_BYTES + 1], 4_097, 2_048);
    assert!(matches!(
        bound_page_event(RasterEvent::Rendered {
            request,
            image: Some(oversized_image),
        }),
        RasterEvent::RenderFailed { .. }
    ));
    let oversized_text = Arc::new(PageText {
        text: "x".repeat(MAX_PENDING_RESULT_BYTES + 1),
        chars: Vec::new(),
        layer: None,
        map: None,
    });
    assert!(matches!(
        bound_page_event(RasterEvent::TextExtracted {
            request: text(1, 0),
            text: oversized_text,
        }),
        RasterEvent::TextFailed { .. }
    ));
}

#[test]
fn a_tile_batch_replaces_stale_tiles_and_keeps_its_priority_order() {
    let queue = queue();
    let handle = test_handle(Arc::clone(&queue), 1);
    let other = test_handle(Arc::clone(&queue), 2);
    let batch = |document_id, tiles| TileBatch { document_id, tiles };
    // The first viewport, then a scroll: the new batch names what is on
    // screen now, nearest the centre first.
    handle
        .request_tiles(batch(
            7,
            vec![tile(7, 12, 0, 0), tile(7, 12, 1, 0), tile(7, 12, 2, 0)],
        ))
        .expect("first batch");
    handle
        .request_tiles(batch(
            7,
            vec![tile(7, 12, 1, 5), tile(7, 12, 1, 0), tile(7, 12, 1, 5)],
        ))
        .expect("second batch");
    other
        .request_tiles(batch(9, vec![tile(9, 8, 0, 0)]))
        .expect("another window");
    // A whole-page placeholder is served before any tile.
    handle
        .request_render(RenderRequest {
            document_id: 7,
            page: 0,
            target_width: 1024,
        })
        .expect("placeholder");
    let mut state = queue.state.lock().unwrap();
    let order: Vec<RasterCommand> = std::iter::from_fn(|| next_page_work(&mut state))
        .map(|queued| queued.command)
        .collect();
    assert!(matches!(order[0], RasterCommand::Render(_)));
    // Stale tiles (0,0) and (2,0) were dropped, the duplicate collapsed,
    // and the two windows are served in turn.
    assert_eq!(
        order[1..].to_vec(),
        vec![
            RasterCommand::Tile(tile(7, 12, 1, 5)),
            RasterCommand::Tile(tile(9, 8, 0, 0)),
            RasterCommand::Tile(tile(7, 12, 1, 0)),
        ]
    );
    drop(state);
    // An empty batch cancels whatever is still queued.
    handle
        .request_tiles(batch(7, vec![tile(7, 12, 3, 3)]))
        .expect("third batch");
    handle.request_tiles(batch(7, Vec::new())).expect("cancel");
    let mut state = queue.state.lock().unwrap();
    assert!(next_page_work(&mut state).is_none());
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn owner_queue_coalesces_requests_and_keeps_only_the_latest_open_and_plan() {
    // Build the endpoint without starting its worker so the test can
    // inspect exactly what a busy owner will receive.
    let handle = test_handle(queue(), 1);
    let first = open_request(1, "first.pdf");
    let latest = open_request(2, "latest.pdf");
    handle
        .enqueue(RasterCommand::OpenPath(first.clone()))
        .expect("queue first open");
    for (width, why) in [
        (101, "queue first render"),
        (110, "coalesce nearby zoom bucket"),
    ] {
        handle
            .request_render(RenderRequest {
                document_id: first.id,
                page: 3,
                target_width: width,
            })
            .expect(why);
    }
    handle
        .request_text(text(first.id, 3))
        .expect("queue stale text");
    handle
        .enqueue(RasterCommand::OpenPath(latest.clone()))
        .expect("replace queued open");
    let state = handle.session.queue.state.lock().expect("queue lock");
    assert_ne!(first.id, latest.id);
    assert_eq!(state.open.get(&1), Some(&latest));
    assert_eq!(state.open_events.len(), 1);
    assert!(matches!(
        state.open_events.front(),
        Some(QueuedEvent {
            session_id: 1,
            event: RasterEvent::OpenSuperseded { request }
        }) if *request == first
    ));
    assert!(state.renders.is_empty(), "new open clears stale paints");
    assert!(state.texts.is_empty(), "new open clears stale text");
    drop(state);

    // Nearby page requests coalesce without an open in between.
    let handle = test_handle(queue(), 1);
    let request = RenderRequest {
        document_id: 2,
        page: 3,
        target_width: 101,
    };
    handle.request_render(request).expect("queue first render");
    handle
        .request_render(RenderRequest {
            target_width: 110,
            ..request
        })
        .expect("coalesce nearby zoom bucket");
    handle.request_text(text(2, 3)).expect("queue text");
    handle.request_text(text(2, 3)).expect("coalesce text");
    let state = handle.session.queue.state.lock().expect("queue lock");
    assert_eq!(state.renders.len(), 1);
    assert!(matches!(
        &state.renders[0].command,
        RasterCommand::Render(RenderRequest { target_width, .. })
            if *target_width == RasterBackend::bucket_for(101)
    ));
    assert_eq!(state.texts.len(), 1);
    drop(state);

    // Link reads share the page queue, coalesce, and are cleared by a plan edit.
    let handle = test_handle(queue(), 1);
    let request = LinksRequest {
        document_id: 2,
        page: 0,
    };
    handle.request_links(request).expect("queue links");
    handle.request_links(request).expect("coalesce links");
    handle
        .request_text(text(2, 0))
        .expect("text for the same page is distinct work");
    let state = handle.session.queue.state.lock().expect("queue lock");
    assert_eq!(state.texts.len(), 2);
    assert!(matches!(
        &state.texts[0].command,
        RasterCommand::Links(queued) if *queued == request
    ));
    drop(state);
    // Results for one page coalesce; text and links for it do not.
    let links = |n: usize| RasterEvent::LinksRead {
        request,
        links: Arc::new(
            (0..n)
                .map(|_| PageLink {
                    rect: PtRect::new(0.0, 0.0, 1.0, 1.0),
                    target: LinkTarget::Page { page: 0, at: None },
                })
                .collect(),
        ),
    };
    assert!(same_page_result(&links(1), &links(2)));
    assert!(!same_page_result(
        &links(1),
        &RasterEvent::TextFailed {
            request: text(2, 0),
            error: String::new(),
        }
    ));
    handle
        .reconcile_page_plan(ReconcileRequest {
            document_id: 2,
            plan: Vec::new(),
        })
        .expect("queue plan");
    let state = handle.session.queue.state.lock().expect("queue lock");
    assert!(state.texts.is_empty(), "a plan edit drops stale link reads");
    drop(state);

    // Only the latest collaborative page plan is kept.
    let handle = test_handle(queue(), 1);
    let plan = |id: &str| ReconcileRequest {
        document_id: 7,
        plan: vec![PagePlanEntry {
            id: id.into(),
            source_page: 1,
            imported: None,
            rotation: 0,
        }],
    };
    handle
        .reconcile_page_plan(plan("first"))
        .expect("queue first plan");
    handle
        .reconcile_page_plan(plan("latest"))
        .expect("replace queued plan");
    let state = handle.session.queue.state.lock().expect("queue lock");
    assert_eq!(state.reconcile_order.len(), 1);
    assert_eq!(state.reconciles.get(&1), Some(&plan("latest")));
    drop(state);
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn sessions_are_isolated_and_settlements_bounded_per_session() {
    // An unavailable worker surfaces its startup error.
    let failed = Arc::new(WorkerQueue {
        state: Mutex::new(QueueState {
            closing_sessions: HashSet::from([1]),
            start_error: Some("thread creation denied".to_string()),
            ..QueueState::default()
        }),
        ready: Condvar::new(),
    });
    assert_eq!(
        test_handle(failed, 1).request_text(text(1, 0)),
        Err(RasterSubmitError::Unavailable(
            "thread creation denied".to_string()
        ))
    );

    // Closing one session does not block another.
    let shared = queue();
    let first = test_handle(Arc::clone(&shared), 1);
    let second = test_handle(Arc::clone(&shared), 2);
    first
        .enqueue(open(1, "first.pdf"))
        .expect("first session queues open");
    second
        .enqueue(open(2, "second.pdf"))
        .expect("second session queues open");
    first.shutdown();
    second
        .request_text(text(2, 0))
        .expect("second session remains usable");
    let state = shared.state.lock().expect("queue lock");
    assert!(!state.open.contains_key(&1));
    assert!(state.open.contains_key(&2));
    assert!(state.texts.iter().any(|queued| queued.session_id == 2));
    drop(state);

    // A dropped session purges its undrained open settlements.
    let shared = queue();
    {
        let first = test_handle(Arc::clone(&shared), 1);
        for id in 1..=8 {
            first
                .enqueue(open(id, &format!("{id}.pdf")))
                .expect("queue transient open");
        }
        assert!(!first.drain_events().is_empty());
        // Queue one more settlement and intentionally do not drain it.
        first
            .enqueue(open(9, "undrained.pdf"))
            .expect("queue undrained open");
    }
    test_handle(Arc::clone(&shared), 2)
        .enqueue(open(10, "next.pdf"))
        .expect("dropped session cannot consume another session's capacity");
    let state = shared.state.lock().expect("queue lock");
    assert!(
        state
            .open_events
            .iter()
            .all(|queued| queued.session_id != 1)
    );
    drop(state);

    // Open settlements are bounded per session without losing the current completion.
    let shared = queue();
    let handle = test_handle(Arc::clone(&shared), 1);
    for id in 1..=63 {
        handle
            .enqueue(open(id, &format!("{id}.pdf")))
            .expect("settlement capacity reserves the current open");
    }
    assert!(matches!(
        handle.enqueue(open(64, "overflow.pdf")),
        Err(RasterSubmitError::QueueFull)
    ));
    let current = {
        let mut state = shared.state.lock().expect("queue lock");
        assert_eq!(
            session_settlement_count(&state, 1),
            MAX_PENDING_OPEN_SETTLEMENTS.saturating_sub(2)
        );
        let current = state.open.get(&1).cloned().expect("current open retained");
        let _ = state.open_events.pop_front();
        current
    };
    push_event(
        &shared,
        1,
        RasterEvent::OpenFailed {
            request: current.clone(),
            error: "fixture failure".to_string(),
        },
    );
    let events = handle.drain_events();
    assert!(matches!(
        events.last(),
        Some(RasterEvent::OpenFailed { request, .. }) if request == &current
    ));

    // A full hidden session cannot block another session's settlement.
    let shared = queue();
    shared.state.lock().expect("queue lock").open_events.extend(
        (0..MAX_PENDING_OPEN_SETTLEMENTS).map(|id| QueuedEvent {
            session_id: 1,
            event: RasterEvent::OpenFailed {
                request: open_request(id as u64, "hidden.pdf"),
                error: "fixture".into(),
            },
        }),
    );
    let visible = test_handle(Arc::clone(&shared), 2);
    visible
        .enqueue(open(900, "visible.pdf"))
        .expect("another session remains admissible");
    push_event(
        &shared,
        2,
        RasterEvent::OpenFailed {
            request: open_request(900, "visible.pdf"),
            error: "fixture".into(),
        },
    );
    assert!(matches!(
        visible.drain_events().as_slice(),
        [RasterEvent::OpenFailed { request, .. }] if request.id == 900
    ));
}

#[cfg(unix)]
#[test]
fn private_save_directory_does_not_follow_a_predictable_sibling_symlink() {
    use std::os::unix::fs::symlink;

    let parent = tempfile::tempdir().expect("parent");
    let destination = parent.path().join("export.pdf");
    let victim = parent.path().join("victim.pdf");
    std::fs::write(&victim, b"keep").expect("victim");
    let predictable = parent.path().join(".export.pdf.tilo-7.saving");
    symlink(&victim, &predictable).expect("attacker symlink");

    let private = private_save(&destination, 7, b"safe export".to_vec());
    assert_ne!(private.temporary, predictable);
    assert!(
        !private.temporary.exists(),
        "nothing is written before the commit"
    );
    flush_private_save(&private, &destination).expect("atomic publish");

    assert_eq!(std::fs::read(&victim).expect("victim contents"), b"keep");
    assert_eq!(
        std::fs::read(&destination).expect("destination contents"),
        b"safe export"
    );
}
