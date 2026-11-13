// Reads of only the changes since what a ready reader holds. Included into
// the broker tests beside `committed_read_deadline_tests.rs`, whose
// `ReadyReader` they drive against the fake broker.

fn changes_reads(captures: &Captures) -> usize {
    captures
        .committed_reads
        .iter()
        .filter(|request| {
            matches!(
                request,
                runtime_collab::committed_document::Request::OpenChanges { .. }
            )
        })
        .count()
}

fn whole_reads(captures: &Captures) -> usize {
    captures
        .committed_reads
        .iter()
        .filter(|request| {
            **request
                == runtime_collab::committed_document::Request::Open {
                    known_revision: None,
                }
        })
        .count()
}

impl ReadyReader {
    /// A ready, waiting reader whose broker answers `OpenChanges`.
    fn start_with_changes() -> Self {
        // Answers OpenChanges from the start, so the first read's
        // confirmation already uses it; no change is offered yet.
        Self::start_configured(Some(FixtureChanges {
            from: [0; 32],
            base: Vec::new(),
            bytes: Vec::new(),
        }))
    }

    /// Another collaborator's edit: the committed document moves to the
    /// revision `[revision; 32]`, and a reader holding the current one is
    /// offered only the change. Returns the change bytes and the document
    /// as the reader held it.
    fn edit_elsewhere(&mut self, body: &str, revision: u8) -> (Vec<u8>, Vec<u8>) {
        use automerge::transaction::Transactable as _;
        let held = self.graph.save();
        let before = self.graph.get_heads();
        self.graph.put(automerge::ROOT, "body", body).unwrap();
        self.graph.commit();
        let changes = self.graph.save_after(&before);
        let mut captures = self.captures.lock().unwrap();
        let from = captures.committed_revision.unwrap_or([7; 32]);
        captures.committed_history_fixture = Some(self.graph.save());
        captures.committed_revision = Some([revision; 32]);
        captures.committed_changes = Some(FixtureChanges {
            from,
            base: before.iter().map(|head| head.0).collect(),
            bytes: changes.clone(),
        });
        drop(captures);
        (changes, held)
    }

    fn next_delivered(&mut self, what: &str) -> CommittedDocument {
        tilo_test_env::wait_until(what, || {
            self.pump();
            !self.delivered.is_empty() || !self.manager.is_connected()
        });
        assert!(self.manager.is_connected(), "{what}: the reader closed");
        self.delivered.remove(0)
    }

    fn wait_ready(&mut self, what: &str) {
        tilo_test_env::wait_until(what, || {
            self.pump();
            self.ready()
        });
    }
}

/// A collaborator's edit reaches a ready reader as the change alone, which
/// applies to the document the owner holds; the reader is ready on the new
/// revision once the owner merged it and its next read confirmed it.
#[test]
fn a_remote_edit_reaches_a_ready_reader_as_only_its_changes() {
    let mut reader = ReadyReader::start_with_changes();
    for (body, revision) in [("first remote edit", 8), ("second remote edit", 9)] {
        let (changes, held) = reader.edit_elsewhere(body, revision);
        reader.wake_waits();
        let read = reader.next_delivered("the remote edit to reach the owner");
        assert!(!reader.ready(), "a delivered read withholds readiness");
        let mut owner = automerge::AutoCommit::load(&held).unwrap();
        assert!(read.applies_to(&owner));
        assert!(!read.applies_to(&automerge::AutoCommit::new()));
        read.with_mutation(|mutation| {
            assert_eq!(
                mutation,
                &collab_types::DocumentMutation::Incremental(changes.clone()),
                "only the change reaches the owner"
            );
            let collab_types::DocumentMutation::Incremental(bytes) = mutation else {
                unreachable!()
            };
            tilo_automerge::ApplyIncoming::load_incoming(&mut owner, bytes).unwrap();
        })
        .unwrap();
        assert_eq!(owner.get_heads(), reader.graph.get_heads());
        read.applied();
        reader.wait_ready("the reader to confirm the merged change");
    }
    let (asked, whole) = {
        let captures = reader.captures.lock().unwrap();
        (changes_reads(&captures), whole_reads(&captures))
    };
    let connected = reader.manager.is_connected();
    reader.finish();
    assert!(connected);
    assert_eq!(whole, 1, "only the first read was whole");
    assert!(
        asked >= 4,
        "each edit is read, and confirmed, as changes: {asked}"
    );
}

/// An owner that lacks the change's base (it never merged what the reader
/// holds) refuses it, and the reader reads the document whole instead of
/// confirming a document the owner does not show.
#[test]
fn changes_the_owner_cannot_apply_are_read_whole() {
    let mut reader = ReadyReader::start_with_changes();
    let _ = reader.edit_elsewhere("an edit the owner cannot place", 8);
    reader.wake_waits();
    let read = reader.next_delivered("the change to reach the owner");
    assert!(!read.applies_to(&automerge::AutoCommit::new()));
    read.refuse();
    assert_eq!(
        reader.apply_next_read("the whole read after the refusal"),
        1
    );
    let whole = whole_reads(&reader.captures.lock().unwrap());
    let ready = reader.ready();
    reader.finish();
    assert_eq!(whole, 2, "the reader asks for the whole document again");
    assert!(ready, "and is ready once that whole read is merged");
}

/// A revocation while a collaborator keeps editing: the reader's first read
/// after it is refused, and the reader closes with nothing delivered.
#[test]
fn a_revocation_while_a_collaborator_edits_closes_on_the_first_read() {
    let mut reader = ReadyReader::start_with_changes();
    for (body, revision) in [("edit one", 8), ("edit two", 9)] {
        let _ = reader.edit_elsewhere(body, revision);
        reader.wake_waits();
        let read = reader.next_delivered("an edit before the revocation");
        read.applied();
        reader.wait_ready("the reader to confirm the edit");
    }
    let (opens, asked) = {
        let mut captures = reader.captures.lock().unwrap();
        captures.committed_open_failure = Some("committed read refused: peer revoked".into());
        (open_count(&captures), changes_reads(&captures))
    };
    let _ = reader.edit_elsewhere("edit after the revocation", 10);
    reader.wake_waits();
    reader.wait_for_close("the revoked reader's first read to close it");
    let (refused_opens, refused_changes) = {
        let captures = reader.captures.lock().unwrap();
        (
            open_count(&captures) - opens,
            changes_reads(&captures) - asked,
        )
    };
    let ready = reader.ready();
    let delivered = reader.delivered.len();
    reader.finish();
    assert!(!ready, "a revoked reader keeps no readiness");
    assert_eq!(delivered, 0, "a revoked reader is handed nothing");
    assert_eq!(
        (refused_changes, refused_opens),
        (2, 2),
        "the wait's OpenChanges after the revocation is refused, and so is the Open after it"
    );
}

/// The changes' acknowledgement misses its deadline: the reader renews its
/// channel and reads the document whole there, never the missed changes.
#[test]
fn a_reader_renewed_after_missed_changes_reads_whole() {
    let mut reader = ReadyReader::start_with_changes();
    let joins = reader.joins();
    let _ = reader.edit_elsewhere("an edit whose changes are missed", 8);
    let (entered, observed) = mpsc::channel();
    let (release, gate) = tokio::sync::oneshot::channel();
    reader.captures.lock().unwrap().committed_ack_gate = Some((entered, gate));
    reader.wake_waits();
    tilo_test_env::recv(
        "the broker to withhold the changes' acknowledgement",
        &observed,
    );
    reader.wait_for_join_or_close(joins + 1, "the missed changes to renew the channel");
    let connected = reader.manager.is_connected();
    let applied = if connected {
        reader.apply_next_read("the whole read on the renewed channel")
    } else {
        0
    };
    let ready = reader.ready();
    let whole = whole_reads(&reader.captures.lock().unwrap());
    drop(release);
    reader.finish();
    assert!(
        connected,
        "a missed acknowledgement renews instead of closing"
    );
    assert_eq!(
        applied, 1,
        "the whole document reaches the owner, no change"
    );
    assert!(ready);
    assert_eq!(whole, 2, "the renewed channel starts with a whole read");
}

/// The workspace's whole copy of a record reaches an owner that cannot read it
/// (a version this app does not read). The owner keeps its own record: nothing
/// waits on the capture, the route publishes again, and a later change, whose
/// base the owner holds, still lands. The unreadable bytes are never asked for
/// again.
#[test]
fn a_whole_capture_the_owner_cannot_read_leaves_the_route_going() {
    use automerge::transaction::Transactable as _;
    let mut reader = ReadyReader::start();
    // The record moves on, and the broker hands a reader that holds the one
    // before it the whole copy of the new one.
    {
        let mut captures = reader.captures.lock().unwrap();
        captures.committed_history_fixture = Some(vec![0xff; 16]);
        captures.committed_revision = Some([8; 32]);
    }
    reader.wake_waits();
    let read = reader.next_delivered("the whole capture to reach the owner");
    assert!(read.is_whole(), "the record arrives whole");
    assert!(
        !reader.ready(),
        "a delivered capture withholds readiness while its owner has it"
    );
    let mut owner = reader.graph.clone();
    let merged = read.take_into(
        &mut owner,
        |_document: &mut automerge::AutoCommit, mutation| {
            let collab_types::DocumentMutation::Snapshot(bytes) = mutation else {
                return Err("changes where the whole document was expected".to_string());
            };
            automerge::AutoCommit::load(bytes).map_err(|error| error.to_string())
        },
    );
    assert!(
        merged.is_none(),
        "a copy of a version this app does not read is not taken in"
    );
    assert!(
        read.was_refused(),
        "refusing the whole copy ends the reader's wait on it"
    );
    // Nothing waits on the capture: the route publishes the record the owner
    // holds again, and the delivery turn it held is free.
    reader.wait_ready("the route to take its owner's record again");
    assert!(
        reader.state.document_reads.publishable(&reader.key),
        "a copy it cannot read never closes the route"
    );
    // A change the owner can place, because it holds its base: its own heads,
    // not the bytes it could not read.
    let before = owner.get_heads();
    owner
        .put(automerge::ROOT, "body", "an edit after the unreadable copy")
        .unwrap();
    owner.commit();
    {
        let mut captures = reader.captures.lock().unwrap();
        captures.committed_history_fixture = Some(owner.save());
        captures.committed_revision = Some([9; 32]);
        captures.committed_changes = Some(FixtureChanges {
            from: [8; 32],
            base: before.iter().map(|head| head.0).collect(),
            bytes: owner.save_after(&before),
        });
    }
    reader.wake_waits();
    let later = reader.next_delivered("the change after the unreadable copy");
    assert!(
        !later.is_whole(),
        "the reader went on from the revision it read"
    );
    assert!(
        later.applies_to(&owner),
        "the change bases on the record the owner holds"
    );
    assert!(
        later
            .take_into(&mut owner, |document, mutation| {
                let collab_types::DocumentMutation::Incremental(bytes) = mutation else {
                    return Err("the whole document where changes were expected".to_string());
                };
                tilo_automerge::ApplyIncoming::load_incoming(document, bytes)
                    .map_err(|error| error.to_string())
            })
            .is_some(),
        "a change it can place still lands"
    );
    reader.wait_ready("the reader to confirm the taken change");
    let whole = whole_reads(&reader.captures.lock().unwrap());
    let connected = reader.manager.is_connected();
    let unapplied = reader.delivered.len();
    reader.finish();
    assert!(connected, "a capture it cannot read never stops the reader");
    assert_eq!(
        whole, 1,
        "the whole document is asked for once, never the bytes it could not read"
    );
    assert_eq!(unapplied, 0, "nothing else is handed over");
}
