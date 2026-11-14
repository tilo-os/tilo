//! A frame of the window, and what it hands its store: a record that did not move is taken in
//! by none.

use super::*;

impl Workspace {
    /// One frame, as a host runs it: the record presented when it moved, then synced and
    /// updated.
    pub(super) fn frame(&mut self) {
        let root = ui::automerge::ROOT;
        self.present_moved();
        self.window.sync(&self.doc, &root);
        self.window.update(&self.doc, &root);
        // Every batch a real app sends passes the UI host's check.
        if let Some(why) = self.window.view().failed() {
            panic!("the window refused what its app sent: {why}");
        }
    }

    /// Present the window the record when it moved past what its store holds, as the framework
    /// presents a surface once per change of the record.
    fn present_moved(&mut self) {
        let held = self.window.host().and_then(|host| {
            host.store().with_record(|chunks| {
                chunks
                    .first()
                    .map(|doc| doc.committed_document().get_heads())
            })
        });
        if held.is_some_and(|held| held != self.doc.get_heads()) {
            let state =
                <crate::resource::Workspace as ui::tilo_resource::Resource>::read(&self.doc)
                    .expect("the record reads");
            ResourceSurface::present(&mut self.window, &Rc::new(state));
        }
    }
}

#[test]
fn frames_over_a_record_that_did_not_move_take_nothing_in() {
    let mut workspace = Workspace::seeded("tasks");
    let host = workspace.window.host().expect("the host started");
    let merges = host.store().stats().merges;
    for _ in 0..10 {
        workspace.frame();
    }
    assert_eq!(
        host.store().stats().merges,
        merges,
        "an idle window hands the store nothing"
    );
    // A change of the record (a scenario's, committed through the door) is taken in.
    let before = workspace.doc.get_heads();
    workspace.seed("letters");
    assert_ne!(
        workspace.doc.get_heads(),
        before,
        "the scenario changed the record"
    );
    assert!(
        host.store().stats().merges > merges,
        "a moved record is taken in"
    );
    // The store holds every change of the record, none lost, and a peer replaying each one
    // gets it to the byte.
    workspace.frame();
    let held = host.store().with_record(|chunks| {
        chunks
            .first()
            .map(|doc| doc.committed_document().get_heads())
    });
    assert_eq!(
        held,
        Some(workspace.doc.get_heads()),
        "the store holds the record"
    );
    let mut registry = ui::tilo_actions::ActionRegistry::new();
    tilolang_tilo::history::register(&mut registry);
    let changes: Vec<_> = workspace
        .doc
        .get_changes(&before)
        .iter()
        .map(ui::automerge::Change::hash)
        .collect();
    assert!(!changes.is_empty());
    for change in changes {
        let verified = host.with_schemas(|| {
            ui::tilo_actions::verify_change(&mut workspace.doc, change, &registry)
        });
        assert!(
            matches!(
                verified,
                Ok(ui::tilo_actions::Verified::Reproduced
                    | ui::tilo_actions::Verified::NothingRecorded)
            ),
            "a peer replaying a change gets another one: {verified:?}"
        );
    }
}
