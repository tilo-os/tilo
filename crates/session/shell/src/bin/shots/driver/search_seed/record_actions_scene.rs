//! Searching for an action of a Notes window that holds two pages, one of
//! them since closed: both rows read as the page and the app, the closed
//! one only opening Notes on it again.
use super::super::ShellDriver;

impl ShellDriver {
    /// Notes keeps Groceries open and has closed Trip plan; the search is
    /// for their Rename actions.
    pub(in crate::driver) fn scene_palette_record_actions(&mut self) {
        use shell::shell::collab_broker::CrdtRoute;
        use shell::shell::collab_broker_capnp::{
            PublishedAction, PublishedInput, PublishedSurface,
        };
        const NOTES: &str = "so.tilo.notes";
        let Some(room) = ({
            let handle = self.chrome_handle();
            let st = handle.borrow();
            st.tree()
                .desktop
                .active_workspace()
                .map(|workspace| workspace.room().to_string())
        }) else {
            return;
        };
        let page = |peer_id: u64, record: &str, title: &str, live: bool| PublishedSurface {
            peer_id,
            app_id: NOTES.into(),
            route: CrdtRoute::record(NOTES, &room, record),
            actions: vec![PublishedAction {
                semantic_identity: None,
                resource: format!("note:{record}"),
                action: "rename".into(),
                resource_kind: "note".into(),
                resource_label: title.into(),
                action_label: format!("Rename {title}"),
                description: None,
                operation_kind: tilo_actions::OperationKind::Effect,
                params: Vec::new(),
                contract: tilo_actions::ActionContract::default(),
                input: PublishedInput::None,
                view: None,
            }],
            live,
        };
        let pages = vec![
            page(41, "groceries", "Groceries", true),
            page(42, "trip", "Trip plan", false),
        ];
        self.fb.host_mut().attach_app_action_broker({
            let broker = shell::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue();
            broker.replace_published_surfaces_for_test(pages);
            broker
        });
        // The provider snapshots published surfaces off the frame; let the
        // first query's snapshot land, as it has by any later keystroke.
        self.fb.host_mut().set_palette_visible(true);
        self.fb.host_mut().set_palette_query("rename");
        let _ = self.paint_settled();
        std::thread::sleep(std::time::Duration::from_millis(400));
        let _ = self.paint_settled();
        self.scene_palette_query("rename");
    }
}
