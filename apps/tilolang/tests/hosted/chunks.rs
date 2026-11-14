//! A hosted window over a workspace whose tasks span more than one chunk of its record: every
//! task is listed, in order, and a chunk that reaches this device after the window opened shows
//! in it without the window being opened again.

use super::*;

use super::cost::{number_of, own, seeded_tasks};
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
fn a_list_whose_tasks_span_two_chunks_shows_every_task_in_order(n: usize, seeded: &Chunks) {
    let _motion = ui::motion::testing::settled();
    let chunks = own(seeded);
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
fn a_chunk_that_arrives_after_the_window_opened_shows_without_opening_it_again(
    n: usize,
    whole: &Chunks,
) {
    let _motion = ui::motion::testing::settled();
    let [first, away] = task_chunks(whole)[..] else {
        panic!("the tasks span two chunks");
    };
    // This device holds the record and the first chunk of tasks; the second is a peer's.
    let mut here = Chunks::of(whole.first().expect("the record").clone());
    here.insert(first, whole.mirror(first).expect("the first chunk").clone());
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = Session::tasks_over_launched(&route, here, None);
    // The window rests over the tasks it has while the chunk is away: a chunk the source cannot
    // give is not one the store owes.
    session.settle();
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
pub(super) fn delivered(route: &RouteSpec, me: &str, mutation: &DocumentMutation) -> CollabEvent {
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

/// The workspace whose tasks span two chunks is seeded once for both windows: the one handed
/// both chunks opens a copy of its own, and the one a chunk reaches later holds its own copies
/// of the record and the first chunk and is sent a snapshot of the second.
#[test]
fn tasks_that_span_two_chunks_are_listed_whole_with_both_here_and_when_one_arrives_later() {
    let n = spanning();
    let whole = seeded_tasks(n);
    a_list_whose_tasks_span_two_chunks_shows_every_task_in_order(n, &whole);
    a_chunk_that_arrives_after_the_window_opened_shows_without_opening_it_again(n, &whole);
}

/// A window the live `App` runs: the chunk that arrives after it opened is caught up on a
/// worker, which wakes the window's frame, and the list shows the tasks the chunk holds once it
/// lands. No part of the catch-up is derived on the window's own frame.
#[test]
fn a_chunk_that_arrives_after_the_live_window_opened_is_caught_up_off_its_frame() {
    let _motion = ui::motion::testing::settled();
    let n = spanning();
    let whole = seeded_tasks(n);
    let [first, away] = task_chunks(&whole)[..] else {
        panic!("the tasks span two chunks");
    };
    let mut here = Chunks::of(whole.first().expect("the record").clone());
    here.insert(first, whole.mirror(first).expect("the first chunk").clone());
    let route = RouteSpec::app(TASKS, uuid::Uuid::new_v4());
    let mut session = Session::brokered(&route, TASKS, "ada", "en-US", here, None, Window::live);
    session.settle();
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
        session.catch_ups() > 0,
        "the chunk that arrived was caught up into the window's index"
    );
    assert_eq!(
        session.catch_ups_in_frame(),
        0,
        "on a worker, which wakes the window's frame, not on the frame itself"
    );
    assert!(
        session.index_whole(),
        "the window reads the whole workspace"
    );
    // The frame the worker woke ran the views that read the new rows, and the window draws the
    // tasks the caught-up index answers.
    assert!(
        !session.titles().is_empty(),
        "the window draws the tasks once the catch-up landed"
    );
}
