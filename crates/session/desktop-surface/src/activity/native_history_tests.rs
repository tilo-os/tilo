#[cfg(test)]
mod native_history {
    //! A native session's history: the workspace's own desk log, replayed
    //! into its room, listed by the activity pane and restored through the
    //! host that keeps the desk.

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

    /// A native session's desk and its log: "Studio" opened with Notes,
    /// Grace renaming it to "Plans", Ada then closing Notes.
    /// The pane, as a native session serves it.
    fn native_pane() -> RoomActivityPane {
        let mut pane = RoomActivityPane::new();
        pane.set_host(Rc::new(TestHost::default()));
        pane
    }

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
        let mut log = automerge::AutoCommit::new();
        assert!(crate::history_log::record(
            &mut log,
            &serde_json::to_value(&tree).unwrap(),
            "Ada",
            1_000,
        ));

        tree.workspaces[0].name = "Plans".into();
        let mut peer = log.fork();
        assert!(crate::history_log::record(
            &mut peer,
            &serde_json::to_value(&tree).unwrap(),
            "Grace",
            2_000,
        ));
        log.merge_incoming(&mut peer).unwrap();
        tree.workspaces[0].strips[0].window_groups.clear();
        assert!(crate::history_log::record(
            &mut log,
            &serde_json::to_value(&tree).unwrap(),
            "Ada",
            3_000,
        ));

        let room = tree.workspaces[0].room();
        let history = crate::history_log::room_history(log, room);
        // The desk the host keeps: what the model writes back reaches it,
        // and the session applies it on its next pass.
        let written: Rc<RefCell<Option<DesktopTree>>> = Rc::new(RefCell::new(None));
        let desk = Rc::clone(&written);
        let snapshot = ActivitySnapshot::new(tree, history.doc)
            .unwrap()
            .committing(Rc::new(move |restored: &DesktopTree| {
                *desk.borrow_mut() = Some(restored.clone());
            }));
        (snapshot, written, history.authors)
    }

    /// A native session's history is the workspace's own desk log, replayed
    /// into its room: the pane lists what changed there and who changed it.
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
