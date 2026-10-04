//! Resource + action-model status for the container manager.
//!
//! Every interaction is exactly one of three things:
//!
//! - **A document action** ([`crate::resource::ContainersAction`]): the
//!   new-container draft, the only thing the app's document holds. Each
//!   field is a collaborative text published as a child of the resource, so
//!   typing is a splice that merges with a collaborator's and undoes a word
//!   at a time; `clear-create` empties the draft once a container was made
//!   from it.
//! - **An external effect on the engine**: start / stop / restart / pause /
//!   remove a container, remove an image or volume, attach a shell, enter a
//!   distrobox. These change rootless podman, not a shared document, so they
//!   are effect operations on the live rows the surface publishes
//!   ([`ResourceSurface::live_rows`](ui::framework::ResourceSurface::live_rows)).
//!   Lifecycle buttons emit inert
//!   [`ControlInvocation`](ui::tilo_actions::ControlInvocation)s; the host
//!   resolves permission and availability, records the outcome in activity
//!   history, then calls the widget's operation handler. No container control
//!   shells out before that authority boundary.
//! - **Where the window is**: the section, the selected container and its
//!   detail tab are the surface's own state, never written to the document;
//!   other callers move the window with the
//!   [`ContainersEffect`](crate::resource::ContainersEffect) vocabulary.

#[cfg(test)]
mod tests {
    use automerge::{AutoCommit, ROOT, ReadDoc};
    use taffy::{NodeId, TaffyTree};
    use ui::framework::LiveRow;
    use ui::tilo_resource::Resource as _;
    use ui::widget::Widget;
    use winit::event::{DeviceId, ElementState, KeyEvent, Modifiers, WindowEvent};
    use winit::keyboard::{Key, KeyLocation, NamedKey, NativeKeyCode, PhysicalKey};

    use crate::app::ContainersApp;
    use crate::model::ContainerStatus;
    use crate::podman::Action as Lifecycle;
    use crate::resource::{Containers, ContainersState};

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
            let mut doc = AutoCommit::new();
            Containers::initialize(&mut doc).unwrap();
            doc
        },
        registry: {
            let mut registry = ui::tilo_actions::ActionRegistry::new();
            Containers::register(&mut registry);
            registry
        },
        resources: |doc| published(doc),
        palette: ["Open web-frontend", "Open postgres"],
    }

    fn device_id() -> DeviceId {
        // winit exposes no public DeviceId constructor; tests only need a value.
        unsafe { std::mem::zeroed() }
    }

    fn named_event(named: NamedKey) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(named);
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: device_id(),
            event: ev,
            is_synthetic: false,
        }
    }

    /// Pump one event through the production `Widget::handle_event`.
    fn send(app: &mut ContainersApp, doc: &mut AutoCommit, ev: &WindowEvent) {
        let taffy = TaffyTree::<()>::new();
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: doc,
                path: &ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: NodeId::new(0),
                taffy: &taffy,
            },
        );
    }

    /// Moving around and running lifecycle verbs write nothing: the
    /// document is the draft alone.
    #[test]
    fn navigation_and_lifecycle_verbs_leave_the_document_alone() {
        let mut app = ContainersApp::sample();
        let mut doc = AutoCommit::new();
        Containers::initialize(&mut doc).unwrap();
        doc.commit();
        let heads = doc.get_heads();

        // The app-local palette selects a container.
        app.open_container_palette();
        let expected = app.engine.containers[0].id.clone();
        send(&mut app, &mut doc, &named_event(NamedKey::Enter));
        app.dispatch_palette_invocation_for_test(&mut doc);
        assert_eq!(app.view.selected.as_deref(), Some(expected.as_str()));

        // An offline lifecycle verb changes the sample engine.
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

        Widget::sync(&mut app, &mut doc, &ROOT);
        assert_eq!(doc.get_heads(), heads, "nothing was written");
        let keys: Vec<String> = doc.keys(ROOT).collect();
        assert_eq!(keys, ["create"], "the document is the draft");
        assert_eq!(Containers::read(&doc).unwrap(), ContainersState::default());
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
        let effects: Vec<_> = rows
            .iter()
            .flat_map(|row| row.actions.iter())
            .filter(|action| action.kind == ui::tilo_actions::OperationKind::Effect)
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
