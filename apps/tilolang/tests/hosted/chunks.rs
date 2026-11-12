//! A hosted window over a workspace whose tasks span more than one chunk of its record: every
//! task is listed, in order, and a chunk that reaches this device after the window opened shows
//! in it without the window being opened again.

use super::*;

use super::cost::{number_of, seeded_tasks};
use tilolang_tilo::chunk::{self, ChunkNo, Chunks};

/// Tasks enough to fill the first chunk the list's run is kept in and spill into a second.
fn spanning() -> usize {
    usize::try_from(chunk::ITEMS).expect("items a chunk holds") + 40
}

/// The chunks the list's tasks are kept in, the first first: the record's own chunk holds none.
fn task_chunks(chunks: &Chunks) -> Vec<ChunkNo> {
    chunks
        .numbers()
        .filter(|number| *number != chunk::FIRST)
        .collect()
}

/// The task numbers the list shows, top to bottom.
fn listed(session: &mut Session) -> Vec<usize> {
    session
        .titles()
        .iter()
        .filter_map(|title| number_of(title))
        .collect()
}

/// The tasks the window opens on lists, in order, of `n`: Today's, the ones due today and not
/// done (every second task the workspace was seeded with is due today, every third is done).
fn today(n: usize) -> Vec<usize> {
    (0..n).filter(|i| i % 2 == 0 && i % 3 != 0).collect()
}

/// Whether `numbers` are a run of `wanted` with none left out between them.
fn a_run_of(wanted: &[usize], numbers: &[usize]) -> bool {
    let Some(start) = numbers
        .first()
        .and_then(|first| wanted.iter().position(|task| task == first))
    else {
        return false;
    };
    wanted.get(start..start + numbers.len()) == Some(numbers)
}

/// Scroll the list by `rows` of its rows, from where the row of task `from` stands, the row of
/// task `next` under it.
fn scroll_rows(session: &mut Session, (from, next): (usize, usize), rows: usize) {
    let (x, top) = shown_at(session, &format!("Item {from}"));
    let (_, below) = shown_at(session, &format!("Item {next}"));
    session.window.move_to(x, top);
    session.window.scroll(0.0, -(below - top) * rows as f64);
}

/// The host's frames and the window's until both rest, the window's store owing no work: unlike
/// [`Session::settle`], this does not wait for the store to hold the whole record, which it
/// cannot while a chunk is away. Whether they came to rest before the wait ran out.
fn rest(session: &mut Session) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut quiet = 0_u32;
    while quiet < QUIET_TURNS {
        if std::time::Instant::now() > deadline {
            return false;
        }
        let changed = session.host.turn();
        session.window.frame();
        let owed = session.search_index_pending()
            || session
                .host
                .instances()
                .next()
                .and_then(|hosted| hosted.instance().host())
                .is_some_and(|host| host.index_pending());
        if changed || owed {
            quiet = 0;
            if !changed {
                session.woken.wait(std::time::Duration::from_millis(50));
            }
        } else {
            quiet = quiet.saturating_add(1);
        }
    }
    true
}

/// The top of the list, the tasks on either side of the line between the two chunks, and its
/// end, each read where a person scrolls to: the list is every task, in the order they were
/// added, whichever chunk holds it.
fn lists_every_task_in_order(session: &mut Session, n: usize) {
    let wanted = today(n);
    let top = listed(session);
    assert!(
        top.first() == wanted.first() && top.len() > 2 && a_run_of(&wanted, &top),
        "the list starts at the first task, in order: {top:?}"
    );
    // The line between the chunks: the first chunk's last task and the second chunk's first.
    let line = usize::try_from(chunk::ITEMS).expect("items a chunk holds");
    let after = wanted
        .iter()
        .position(|task| *task >= line)
        .expect("tasks past the line");
    scroll_rows(session, (wanted[0], wanted[1]), after - 4);
    session.settle();
    let across = listed(session);
    assert!(
        across.contains(&wanted[after - 1])
            && across.contains(&wanted[after])
            && a_run_of(&wanted, &across),
        "the tasks on either side of the chunks' line show next to each other, in order: \
         {across:?}"
    );
    session.window.scroll(0.0, -200_000.0);
    session.settle();
    let end = listed(session);
    assert!(
        end.last() == wanted.last() && a_run_of(&wanted, &end),
        "the list ends at the last task, in order: {end:?}"
    );
}

/// A window over a workspace whose tasks fill one chunk and spill into the next lists every
/// task in order: the instance reads both chunks it was handed, and the list does not stop where
/// the first chunk ends.
#[test]
fn a_list_whose_tasks_span_two_chunks_shows_every_task_in_order() {
    let _motion = ui::motion::testing::settled();
    let n = spanning();
    let chunks = seeded_tasks(n);
    assert_eq!(
        task_chunks(&chunks).len(),
        2,
        "the tasks span two chunks: {:?}",
        chunks.numbers().collect::<Vec<_>>()
    );
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = Session::tasks_over_in(&route, chunks, None);
    assert!(session.index_whole(), "the window read the whole workspace");
    lists_every_task_in_order(&mut session, n);
}

/// A window opened while a chunk of the workspace has not reached this device lists the tasks it
/// has and rests; when a peer sends the chunk, the window lists the tasks it holds too, in their
/// place, without being opened again.
#[test]
fn a_chunk_that_arrives_after_the_window_opened_shows_without_opening_it_again() {
    let _motion = ui::motion::testing::settled();
    let n = spanning();
    let whole = seeded_tasks(n);
    let [first, away] = task_chunks(&whole)[..] else {
        panic!("the tasks span two chunks");
    };
    // This device holds the record and the first chunk of tasks; the second is a peer's.
    let mut here = Chunks::of(whole.first().expect("the record").clone());
    here.insert(first, whole.mirror(first).expect("the first chunk").clone());
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = Session::tasks_over_launched(&route, here, None);
    assert!(
        rest(&mut session),
        "the window rests over the tasks it has while the chunk is away"
    );
    assert!(
        !session.index_whole(),
        "the store knows a chunk of the workspace is not here yet"
    );
    let top = listed(&mut session);
    assert!(
        top.first() == today(n).first() && a_run_of(&today(n), &top),
        "the window lists the tasks it has, in order: {top:?}"
    );

    // The peer's chunk arrives on the chunk's route, as the broker delivers it.
    let chunk_route = RouteSpec::record(
        route.app_id.clone(),
        route.cluster_id,
        chunk::record_id(route.record.as_deref().unwrap_or(""), away),
    );
    let snapshot = DocumentMutation::Snapshot(
        whole
            .mirror(away)
            .expect("the peer's chunk")
            .committed_document()
            .save(),
    );
    session
        .inbox()
        .borrow_mut()
        .push_back(delivered(&chunk_route, "bea", &snapshot));
    session.settle();
    assert!(
        session.index_whole(),
        "the chunk that arrived is read: the store holds the whole workspace"
    );
    lists_every_task_in_order(&mut session, n);
}

/// `mutation`, sent by `me` on `route`, as the document envelope the broker delivers to the
/// route's other replicas.
fn delivered(route: &RouteSpec, me: &str, mutation: &DocumentMutation) -> CollabEvent {
    let record = route.record.as_deref().unwrap_or("");
    let workspace = route.cluster_id.to_string();
    let mut envelope = collab_types::SignedOp::new_unsigned(
        mutation.encode().expect("the change encodes"),
        collab_types::document_artifact_id(&route.app_id, &workspace, record),
        collab_types::Ed25519PublicKey::from_bytes([0; 32]),
        collab_types::document_attestation_id(&route.app_id, &workspace, record),
        collab_types::OpKind::Change,
        std::time::SystemTime::UNIX_EPOCH,
        0,
    );
    envelope.signature = collab_types::Signature::from_bytes([0; 64]);
    CollabEvent::Document {
        envelope: Box::new(envelope),
        from_user_id: me.to_owned(),
        decrypted_payload: None,
    }
}
