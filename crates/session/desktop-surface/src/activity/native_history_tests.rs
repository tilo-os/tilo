#[cfg(test)]
mod native_history {
    //! A native session's history: the device's own document of the desk,
    //! listed by the activity pane and restored through the host that keeps
    //! the desk.

    use super::*;
    use crate::activity::testing::TestHost;
    use tilo_automerge::ApplyIncoming as _;

    /// The history a native session serves, what the host keeps of what the
    /// model writes back, and who the changes are by.
    type NativeHistory = (
        crate::activity::ActivitySnapshot,
        Rc<RefCell<Option<tilo_desktop_model::DesktopTree>>>,
        std::collections::BTreeMap<String, String>,
    );

    /// The pane, as a native session serves it.
    fn native_pane() -> RoomActivityPane {
        let mut pane = RoomActivityPane::new();
        pane.set_host(Rc::new(TestHost::default()));
        pane
    }

    /// Write the desk as it stands into `doc` and stamp it at `time`: one
    /// change of the desk, as a session records each pass.
    fn stamp(doc: &mut automerge::AutoCommit, tree: &tilo_desktop_model::DesktopTree, time: i64) {
        tilo_desktop_model::desktop_doc::seed(doc, tree);
        doc.commit_with(automerge::transaction::CommitOptions::default().with_time(time));
    }

    /// A native session's desk and its document: "Studio" opened with Notes,
    /// Grace renaming it to "Plans", Ada then closing Notes.
    fn native_history() -> NativeHistory {
        use crate::activity::ActivitySnapshot;
        use tilo_desktop_model::{DesktopTree, IdMinter, Tab, TileContent};

        let mut minter = IdMinter::new(7);
        let notes = Tab {
            id: minter.mint(),
            title: "Notes".into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.notes".into(),
                label: "Notes".into(),
                note: String::new(),
            },
            app_title: None,
        };
        let mut tree = crate::seed_tree(&mut minter, "Studio", vec![notes]);

        // The one document the desk is kept as: its changes are what the desk
        // did, who did it and when.
        let ada = automerge::ActorId::random();
        let grace = automerge::ActorId::random();
        let mut doc = automerge::AutoCommit::new();
        doc.set_actor(ada.clone());
        stamp(&mut doc, &tree, 1_000);

        tree.workspaces[0].name = "Plans".into();
        let mut peer = doc.fork();
        peer.set_actor(grace.clone());
        stamp(&mut peer, &tree, 2_000);
        doc.merge_incoming(&mut peer).unwrap();

        tree.workspaces[0].strips[0].window_groups.clear();
        stamp(&mut doc, &tree, 3_000);

        // The desk the host keeps: what the model writes back reaches it,
        // and the session applies it on its next pass.
        let written: Rc<RefCell<Option<DesktopTree>>> = Rc::new(RefCell::new(None));
        let desk = Rc::clone(&written);
        let snapshot = ActivitySnapshot::of_desktop(tree, doc).committing(Rc::new(
            move |restored: &DesktopTree| {
                *desk.borrow_mut() = Some(restored.clone());
            },
        ));
        let authors = std::collections::BTreeMap::from([
            (ada.to_hex_string(), "Ada".to_owned()),
            (grace.to_hex_string(), "Grace".to_owned()),
        ]);
        (snapshot, written, authors)
    }

    /// A native session's history is the desk's own document: the pane lists
    /// what changed there and who changed it.
    #[test]
    fn the_native_history_lists_what_changed_in_the_workspace() {
        let (mut snapshot, _written, authors) = native_history();
        let mut pane = native_pane();
        crate::history_log::register_authors(pane.authors_mut(), &authors, &|_| None);
        pane.set_view(ActivityView::Explore);
        pane.refresh(&mut snapshot);

        let rows: Vec<String> = pane
            .entries()
            .iter()
            .map(|entry| entry.summary.clone())
            .collect();
        assert!(
            rows.iter().any(|summary| summary.contains("Plans")),
            "the rename is what changed in the workspace: {rows:?}"
        );
        assert!(
            pane.entries()
                .iter()
                .any(|entry| entry.author.name == "Grace"),
            "who changed it is named: {:?}",
            pane.entries()
                .iter()
                .map(|entry| entry.author.name.clone())
                .collect::<Vec<_>>()
        );
    }

    /// Restoring a version writes the desk back through the host that keeps
    /// it, and the restore is itself a change of the workspace.
    #[test]
    fn a_version_restored_from_the_native_history_is_written_back() {
        use tilo_actions::remote::workspace_activity as wire;
        use tilo_desktop_model::DesktopTree;

        let (mut snapshot, written, authors) = native_history();
        let mut pane = native_pane();
        crate::history_log::register_authors(pane.authors_mut(), &authors, &|_| None);
        pane.set_view(ActivityView::Explore);
        pane.refresh(&mut snapshot);

        // The rows are oldest first: the newest is Ada closing Notes, so the
        // one before it is the version with the window still open.
        let listed = pane.entries().len();
        let before_close = listed
            .checked_sub(2)
            .and_then(|older| pane.entries().get(older))
            .map(|entry| entry.id)
            .expect("a version before the close");
        let arguments =
            serde_json::json!(wire::Range::one(&wire::entry_hex(before_close.as_bytes())))
                .as_object()
                .cloned()
                .expect("a range");
        let outcome = pane
            .serve_action(wire::RESTORE, &arguments, &mut snapshot, None)
            .expect("the history takes the restore");
        assert!(outcome.reverted, "the restore wrote");
        let restored: DesktopTree = written
            .borrow_mut()
            .take()
            .expect("the desk is written back");
        assert_eq!(
            restored
                .active_workspace()
                .map(|workspace| workspace.name.as_str()),
            Some("Plans"),
            "the workspace is back at the version restored"
        );
        assert_eq!(
            restored.workspaces[0].strips[0].window_groups.len(),
            1,
            "the window Ada closed is back"
        );
        assert!(
            pane.entries().len() > listed,
            "the restore is a new change on top, not a rewind"
        );
    }
}
