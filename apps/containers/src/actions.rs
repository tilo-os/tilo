//! Resource + action-model status for the container manager.
//!
//! Every interaction is exactly one of three things:
//!
//! - **A document change** ([`crate::resource::Containers`]): the
//!   new-container draft, the only thing the app's record holds. Each field
//!   is a collaborative text edited at its path, so typing is a splice that
//!   merges with a collaborator's and undoes a word at a time;
//!   `clear-create` empties the draft once a container was made from it.
//! - **An external effect on the engine**: start / stop / restart / pause /
//!   remove a container, remove an image or volume, attach a shell, enter a
//!   distrobox. These change rootless podman, not a shared document, so they
//!   are effect operations on the live rows the surface publishes
//!   ([`ResourceSurface::live_rows`](ui::framework::ResourceSurface::live_rows)).
//!   Lifecycle buttons ask the host for the same typed effects on their
//!   rows; the host resolves permission and availability, records the
//!   outcome in activity history, then the surface answers the effect. No container control
//!   shells out before that authority boundary.
//! - **Where the window is**: the section, the selected container and its
//!   detail tab are the surface's own state, never written to the document;
//!   other callers move the window with the
//!   [`ContainersEffect`](crate::resource::ContainersEffect) vocabulary.

#[cfg(test)]
mod tests {
    use automerge::{AutoCommit, ROOT, ReadDoc};
    use ui::framework::LiveRow;
    use ui::tilo_resource::Resource as _;

    use crate::app::ContainersApp;
    use crate::model::ContainerStatus;
    use crate::podman::Action as Lifecycle;
    use crate::resource::Containers;

    /// The resource as the framework publishes it beside the sample engine's
    /// rows.
    fn published(doc: &AutoCommit) -> Vec<ui::tilo_actions::Resource> {
        let state = Containers::read(doc).unwrap_or_default();
        let mut root =
            ui::tilo_resource::publish::<Containers>(Containers::KIND, Containers::LABEL, &state);
        root.children.extend(
            ContainersApp::sample()
                .engine_rows()
                .into_iter()
                .map(LiveRow::into_resource),
        );
        vec![root]
    }

    ui::action_contract_tests! {
        seed: {
            AutoCommit::new()
        },
        registry: {
            ui::tilo_actions::ActionRegistry::with(Containers::register)
        },
        resources: |doc| published(doc),
        palette: ["Open web-frontend", "Open postgres"],
    }

    /// Moving around and running lifecycle verbs write nothing: the
    /// document is the draft alone.
    #[test]
    fn navigation_and_lifecycle_verbs_leave_the_document_alone() {
        let mut host = ui::prelude::TestApp::<crate::ContainersShell>::new().unwrap();
        host.with_surface::<ContainersApp, _>(ContainersApp::stage_sample_engine)
            .unwrap();
        host.settle();
        let heads = host.peer_document().get_heads();
        let expected = host
            .with_widget::<ContainersApp, _>(|app| app.engine.containers[0].id.clone())
            .unwrap();
        host.effect_on(
            &format!("container:{expected}"),
            &crate::resource::ContainersEffect::SelectContainer {
                id: Some(expected.clone()),
            },
        )
        .unwrap();
        host.with_widget::<ContainersApp, _>(|app| {
            assert_eq!(app.view.selected.as_deref(), Some(expected.as_str()));
            // The sample engine exercises the same lifecycle implementation,
            // while the public offline row correctly refuses machine effects.
            let running = app
                .engine
                .containers
                .iter()
                .find(|c| c.status.is_running())
                .expect("sample engine has a running container")
                .id
                .clone();
            app.apply_action(Lifecycle::Stop, &running);
            assert_eq!(
                app.engine.find(&running).map(|c| c.status),
                Some(ContainerStatus::Exited)
            );
        })
        .unwrap();
        // The hosted resource owns its canonical record; the frame harness's
        // outer document is not the container draft.
        let mut doc = host.peer_document();
        assert_eq!(doc.get_heads(), heads, "nothing was written");
        let keys: Vec<String> = doc.keys(ROOT).collect();
        assert_eq!(
            keys,
            ["draft", "tilo:record", "tilo:schema_version"],
            "the canonical record contains only the app draft and framework metadata"
        );
        assert_eq!(Containers::read(&doc).unwrap(), Containers::default());
    }

    /// The rows carry only view and effect operations; offline they are
    /// unavailable.
    #[test]
    fn engine_rows_are_effects_unavailable_offline() {
        let rows: Vec<_> = ContainersApp::sample()
            .engine_rows()
            .into_iter()
            .map(LiveRow::into_resource)
            .collect();
        let open = rows
            .iter()
            .flat_map(|row| &row.actions)
            .find(|action| action.id == "select-container")
            .expect("the sample engine publishes its canonical open-row action");
        assert_eq!(open.kind, ui::tilo_actions::OperationKind::View);
        assert_eq!(
            open.contract.permission,
            ui::tilo_actions::PermissionRequirement::Read
        );
        assert!(open.contract.availability.is_available());
        let effects: Vec<_> = rows
            .iter()
            .flat_map(|row| row.actions.iter())
            .filter(|action| {
                action.kind == ui::tilo_actions::OperationKind::Effect
                    && action.contract.permission == ui::tilo_actions::PermissionRequirement::Admin
            })
            .collect();
        assert!(
            effects
                .iter()
                .any(|action| matches!(action.id.as_str(), "start" | "stop")),
            "a container offers the lifecycle verb its state allows"
        );
        assert!(
            effects
                .iter()
                .all(|action| !action.contract.availability.is_available()),
            "nothing reaches a sample engine"
        );
        assert!(
            rows.iter()
                .flat_map(|row| row.actions.iter())
                .all(|action| action.kind != ui::tilo_actions::OperationKind::Document)
        );
    }
}
