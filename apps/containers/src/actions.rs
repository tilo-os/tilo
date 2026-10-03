//! Resource + action-model status for the container manager.
//!
//! The Tilo action-model recipe (`docs/action-model-recipe.md`) splits every
//! interaction into exactly one of two buckets: a *document-mutating* operation
//! becomes an [`Action`](tilo_actions::Action) — a serializable value that
//! deterministically changes the shared Automerge document, so undo and
//! multi-peer replay use the same action — while presentation / view state stays plain
//! app code. Container lifecycle commands are therefore exported as
//! [`OperationKind::Effect`](tilo_actions::OperationKind::Effect) operations,
//! while this module verifies they never masquerade as document mutations.
//!
//! ## Container data and actions
//!
//! Containers is a projection of a host resource — rootless podman. The list of
//! containers, images, volumes and distroboxes, their status, stats and logs are
//! transient host data mirrored into memory ([`crate::model::Engine`]); they are
//! **not** collaborative and never touch the document. Everything that changes
//! the world — start / stop / restart / pause / remove a container, pull / remove
//! / tag an image, create / remove a volume, `run` a new container, attach an
//! interactive shell, distrobox enter / create — is an **external** podman
//! operation (see [`crate::podman`] and the `apply_*` / `*_image` / `*_volume`
//! methods on [`crate::app::ContainersApp`]). It mutates the podman daemon, not a
//! shared CRDT, so it is exported as an `Effect`: lifecycle buttons emit inert
//! [`ControlInvocation`](tilo_actions::ControlInvocation)s and the host resolves
//! current permission/availability, records the outcome in activity history,
//! then calls the widget's operation handler. No container control shells out
//! before that authority boundary.
//!
//! ## What is in the shared document
//!
//! Containers writes **view preferences** to its Automerge document (see
//! [`crate::doc`] and [`crate::resource::ContainersAction`]):
//!
//! - **Section / selection / detail tab** are document Actions
//!   (`set-section`, `select-container`, `set-tab`) so the palette and agents
//!   share one door with the UI through [`tilo_resource::publish`].
//! - **The search box and the create-container draft are native collaborative
//!   `Text`.** `view.search` and the six `view.create.*` fields are Automerge
//!   `Text` objects on the toolkit's CRDT-native text path. The create draft is
//!   transient: submitting it shells out to `podman run` (external) and clears.
//!
//! There is no saved compose / favourites collection yet. Live engine inventory
//! (containers, images, volumes, distroboxes) stays host-local and is published
//! as live extras when hosted — never a second `containers.engine` root beside
//! `ResourceRoot`.
//!
//! ## Consequence
//!
//! Document ops and `create` live on [`tilo_resource::publish`]. Standalone /
//! `--shots` overlay engine children on that one root; hosted surfaces keep only
//! live engine rows (and an in-flight pull). Lifecycle Effects remain Effects.

#[cfg(test)]
mod tests {
    use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
    use taffy::{NodeId, TaffyTree};
    use tilo_automerge::ApplyIncoming;
    use ui::widget::Widget;
    use winit::event::{DeviceId, ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
    use winit::keyboard::{Key, KeyLocation, NamedKey, NativeKeyCode, PhysicalKey, SmolStr};

    use crate::app::ContainersApp;
    use crate::doc::{CreateField, ViewDoc};
    use crate::model::ContainerStatus;
    use crate::podman::Action as Lifecycle;

    // Document view Actions register on Containers; engine lifecycle Effects
    // are checked on the published standalone surface.
    ui::action_contract_tests! {
        seed: AutoCommit::new(),
        registry: {
            use ui::tilo_resource::Resource as _;
            let mut registry = ui::tilo_actions::ActionRegistry::new();
            crate::resource::Containers::register(&mut registry);
            registry
        },
        resources: |doc| ui::resources::resources(&ContainersApp::new(false), doc),
        palette: ["Open web-frontend", "Open postgres"],
    }

    fn device_id() -> DeviceId {
        // winit exposes no public DeviceId constructor; tests only need a value.
        unsafe { std::mem::zeroed() }
    }

    /// A character keypress through the real winit event path (the same trick the
    /// app's own widget tests use for winit's `pub(crate)` `KeyEventExtra`).
    fn char_event(c: &str) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(SmolStr::new(c));
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(SmolStr::new(c));
        WindowEvent::KeyboardInput {
            device_id: device_id(),
            event: ev,
            is_synthetic: false,
        }
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

    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
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

    fn view_obj(doc: &AutoCommit) -> ObjId {
        match doc.get(&ROOT, "view").unwrap() {
            Some((Value::Object(ObjType::Map), id)) => id,
            other => panic!("root[\"view\"] must be an Automerge Map, got {other:?}"),
        }
    }

    /// The document holds only view state and native create-draft text that replicates byte-identically; lifecycle verbs are external effects exported without document actions.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn document_holds_view_state_while_lifecycle_verbs_stay_external() {
        // --- phase: document_holds_only_view_state_and_native_text ---
        {
            const ALLOWED: [&str; 5] = ["section", "selected", "tab", "search", "create"];

            let mut app = ContainersApp::new(false);
            let mut doc = AutoCommit::new();

            // Production find-and-jump path: the app-local palette selects a container.
            app.open_container_palette();
            let expected = app.engine.containers[0].id.clone();
            send(&mut app, &mut doc, &named_event(NamedKey::Enter));
            app.dispatch_palette_invocation_for_test(&mut doc);
            assert_eq!(
                ViewDoc::selected(&doc).as_deref(),
                Some(expected.as_str()),
                "activating a palette row selects that container (navigation)",
            );

            // The whole document is a single `view` map — nothing else.
            let root_keys: Vec<String> = doc.keys(&ROOT).collect();
            assert_eq!(
                root_keys,
                vec!["view".to_string()],
                "document is only the view map: {root_keys:?}"
            );

            // Every key inside `view` is a known view-state key; classify each.
            let view = view_obj(&doc);
            for key in doc.keys(&view) {
                assert!(
                    ALLOWED.contains(&key.as_str()),
                    "unexpected structural key in document: {key}"
                );
            }
            // No user-authored record collection exists to hang an action off.
            for forbidden in [
                "favourites",
                "favorites",
                "pins",
                "run_configs",
                "groups",
                "sessions",
            ] {
                assert!(
                    doc.get(&view, forbidden).unwrap().is_none(),
                    "document must not carry a `{forbidden}` record collection",
                );
            }

            // Selection is a presentation scalar; the create draft is a native-Text map.
            match doc.get(&view, "selected").unwrap() {
                Some((Value::Scalar(s), _)) => {
                    assert!(
                        matches!(s.as_ref(), automerge::ScalarValue::Str(v) if v.as_str() == expected)
                    );
                }
                other => panic!("view.selected must be a scalar string, got {other:?}"),
            }
            match doc.get(&view, "create").unwrap() {
                Some((Value::Object(ObjType::Map), _)) | None => {}
                other => panic!("view.create must be a native-Text map, got {other:?}"),
            }
        }

        // --- phase: create_draft_is_native_text_and_replicates_byte_identically ---
        {
            let mut app = ContainersApp::new(false);
            let mut doc = AutoCommit::new();
            // Open the create modal on the Name field — a plain text field,
            // not the image combobox — so a keystroke lands straight on the text path.
            app.create_open = true;
            app.create_focus = CreateField::Name;

            // A base both replicas share before any create draft exists.
            let before = ViewDoc::create_field(&doc, CreateField::Name);
            assert_eq!(before, "", "draft starts empty");
            doc.commit();
            let base = doc.save();
            let mut peer = AutoCommit::load(&base).unwrap();
            assert_eq!(
                doc.get_heads(),
                peer.get_heads(),
                "precondition: replicas share a base"
            );

            // Real keystroke → create_modal_key → SetCreateField (a splice).
            send(&mut app, &mut doc, &char_event("w"));
            doc.commit();
            assert_eq!(
                ViewDoc::create_field(&doc, CreateField::Name),
                "w",
                "the keystroke spliced into the create-draft Text on the native path",
            );

            peer.merge_incoming(&mut doc).unwrap();
            assert_eq!(
                doc.get_heads(),
                peer.get_heads(),
                "byte-identical convergence after merge"
            );
            assert_eq!(
                ViewDoc::create_field(&peer, CreateField::Name),
                "w",
                "the peer sees the same create draft",
            );
        }

        // --- phase: lifecycle_verbs_are_external_not_document_mutations ---
        {
            let mut app = ContainersApp::new(false);
            let mut doc = AutoCommit::new();
            let heads_before = doc.get_heads();

            let running = app
                .engine
                .containers
                .iter()
                .find(|c| c.status.is_running())
                .expect("sample engine has a running container")
                .clone();

            // Production lifecycle path (offline mirror of `podman stop`).
            app.apply_action(Lifecycle::Stop, &running.id);

            let after = app
                .engine
                .find(&running.id)
                .expect("container still present");
            assert_eq!(
                after.status,
                ContainerStatus::Exited,
                "the engine reflects the stop"
            );
            assert_eq!(
                doc.get_heads(),
                heads_before,
                "a lifecycle verb writes nothing to the shared document",
            );
        }

        // --- phase: exports_unavailable_effect_resources_without_document_actions ---
        {
            let mut app = ContainersApp::new(false);
            let mut doc = AutoCommit::new();

            // Exercise the real event path (open + close the app palette) so this is
            // the live widget after interaction, not a pristine one.
            app.open_container_palette();
            send(&mut app, &mut doc, &named_event(NamedKey::Escape));
            // A stray click on the surface, too.
            send(&mut app, &mut doc, &left_press());

            let resources = ui::resources::resources(&app, &doc);
            assert_eq!(resources.len(), 1, "one container-engine root");
            let engine = &resources[0];
            assert_eq!(engine.kind, "containers.engine");
            let actions: Vec<_> = engine
                .children
                .iter()
                .flat_map(|resource| resource.actions.iter())
                .filter(|action| action.kind == ui::tilo_actions::OperationKind::Effect)
                .collect();
            assert!(
                actions
                    .iter()
                    .any(|action| matches!(action.id.as_str(), "start" | "stop")),
                "container publishes the lifecycle action appropriate to its state",
            );
            assert!(actions.iter().all(|action| {
                action.kind == ui::tilo_actions::OperationKind::Effect
                    && !action.contract.availability.is_available()
            }));
            assert!(
                ui::undo::take_committed_action(&mut app).is_none(),
                "no action is dispatched, so no action-host undo boundary is recorded",
            );
        }
    }
}
