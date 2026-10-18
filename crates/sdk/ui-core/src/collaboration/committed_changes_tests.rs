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
        (1, 1),
        "one read after the revocation, its OpenChanges, refused"
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
