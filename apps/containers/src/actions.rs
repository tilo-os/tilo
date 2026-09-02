//! Resource + action-model status for the container manager.
//!
//! The qOS action-model recipe (`docs/action-model-recipe.md`) splits every
//! interaction into exactly one of two buckets: a *document-mutating* operation
//! becomes an [`Action`](qos_actions::Action) — a serializable value that
//! deterministically changes the shared Automerge document, so undo and
//! multi-peer replay come for free — while presentation / view state stays plain
//! app code. Container lifecycle commands are therefore exported as
//! [`OperationKind::Effect`](qos_actions::OperationKind::Effect) operations,
//! while this module verifies they never masquerade as document mutations.
//!
//! ## What Containers actually is
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
//! shared CRDT, so by the recipe's rule it is *not* an action and stays as-is.
//!
//! ## What is in the shared document — and why none of it is an action
//!
//! The only thing Containers writes to its Automerge document is **view state**
//! (see [`crate::doc`]), and it falls into exactly the two non-action categories
//! the recipe names:
//!
//! - **Navigation / selection is presentation.** `view.section` (which section is
//!   showing), `view.selected` (which container is selected) and `view.tab`
//!   (which detail tab is active) are scalar strings. The recipe lists "which
//!   book is open, the current selection, an overlay being open" as presentation
//!   that must *never* be an action — "dispatching it as an action would be
//!   wrong". Containers deliberately keeps this view state in the CRDT so two
//!   peers can navigate the same manager together, but replicating a cursor does
//!   not make it a document mutation to encode.
//!
//! - **The search box and the create-container draft are native collaborative
//!   `Text`.** `view.search` and the six `view.create.*` fields (name / image /
//!   ports / env / volumes / command) are Automerge `Text` objects, edited by
//!   `splice_text_diffed` on the toolkit's CRDT-native text path — the same path
//!   the recipe says a flowing text body must take *instead of* the action
//!   executor. Two peers typing into the create form converge and undo for free
//!   through the `Text` object; re-encoding a keystroke as an action would
//!   duplicate what the text path already gives. The create draft is transient:
//!   submitting it shells out to `podman run` (external) and clears the draft.
//!
//! There is no user-authored *structural* record to hang an action off: no saved
//! run-configs or compose definitions, no favourites / pins, no custom groupings,
//! no saved-session list. The document holds one `view` map and nothing else.
//!
//! ## Consequence
//!
//! Containers publishes the transient engine objects as stable session
//! resources with typed effect operations. In demo/offline mode those same
//! operations remain enumerable but carry an explicit `engine_offline`
//! availability reason. [`take_committed_action`](ui::widget::Widget::take_committed_action)
//! remains `None`: lifecycle effects never create a document undo boundary.

#[cfg(test)]
mod tests {
    use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
    use taffy::{NodeId, TaffyTree};
    use ui::widget::Widget;
    use winit::event::{DeviceId, ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
    use winit::keyboard::{Key, KeyLocation, NamedKey, NativeKeyCode, PhysicalKey, SmolStr};

    use crate::app::ContainersApp;
    use crate::doc::{CreateField, ViewDoc};
    use crate::model::ContainerStatus;
    use crate::podman::Action as Lifecycle;

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
            1.0,
            &Modifiers::default(),
            doc,
            &ROOT,
            &mut handled,
            &mut focus,
            NodeId::new(0),
            &taffy,
        );
    }

    fn view_obj(doc: &AutoCommit) -> ObjId {
        match doc.get(&ROOT, "view").unwrap() {
            Some((Value::Object(ObjType::Map), id)) => id,
            other => panic!("root[\"view\"] must be an Automerge Map, got {other:?}"),
        }
    }

    /// Every key Containers writes to its document is view state — navigation /
    /// selection scalars plus the native-`Text` search + create draft. Driving
    /// the real selection path (open the app palette, Enter a row) writes
    /// `view.selected`, and the document holds only the one `view` map: no saved
    /// run-configs, favourites, pins, groupings or sessions — nothing structural
    /// for an action to mutate.
    #[test]
    fn document_holds_only_view_state_and_native_text() {
        const ALLOWED: [&str; 5] = ["section", "selected", "tab", "search", "create"];

        let mut app = ContainersApp::new(false);
        let mut doc = AutoCommit::new();

        // Production find-and-jump path: the app-local palette selects a container.
        app.open_container_palette();
        let expected = app.engine.containers[0].id.clone();
        send(&mut app, &mut doc, &named_event(NamedKey::Enter));
        app.dispatch_palette_invocation_for_test(&mut doc);
        assert_eq!(
            ViewDoc::selected(&mut doc).as_deref(),
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

    /// The create-container draft is native collaborative `Text`, so it already
    /// carries the guarantee an action would otherwise provide: a keystroke driven
    /// through the production `Widget::handle_event` splices into the draft's
    /// `Text`, and an independent replica that merges the change converges
    /// byte-identically (same head hashes, not merely the same text). Nothing to
    /// encode as an action.
    #[test]
    fn create_draft_is_native_text_and_replicates_byte_identically() {
        let mut app = ContainersApp::new(false);
        let mut doc = AutoCommit::new();
        // Open the create modal on the Name field — a plain text field,
        // not the image combobox — so a keystroke lands straight on the text path.
        app.create_open = true;
        app.create_focus = CreateField::Name;

        // A base both replicas share, with the create draft materialised.
        let before = ViewDoc::create_field(&mut doc, CreateField::Name);
        assert_eq!(before, "", "draft starts empty");
        doc.commit();
        let base = doc.save();
        let mut peer = AutoCommit::load(&base).unwrap();
        assert_eq!(
            doc.get_heads(),
            peer.get_heads(),
            "precondition: replicas share a base"
        );

        // Real keystroke → create_modal_key → ViewDoc::set_create_field (a splice).
        send(&mut app, &mut doc, &char_event("w"));
        doc.commit();
        assert_eq!(
            ViewDoc::create_field(&mut doc, CreateField::Name),
            "w",
            "the keystroke spliced into the create-draft Text on the native path",
        );

        peer.merge(&mut doc).unwrap();
        assert_eq!(
            doc.get_heads(),
            peer.get_heads(),
            "byte-identical convergence after merge"
        );
        assert_eq!(
            ViewDoc::create_field(&mut peer, CreateField::Name),
            "w",
            "the peer sees the same create draft",
        );
    }

    /// A container lifecycle verb is **external**: it mutates podman's world (here,
    /// the offline engine projection), never the CRDT. Driving the real
    /// `apply_action` path stops the container in the in-memory engine while
    /// writing nothing to the document — proving lifecycle ops are not actions.
    #[test]
    fn lifecycle_verbs_are_external_not_document_mutations() {
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

    /// Offline/demo resources remain discoverable but their effects state why
    /// they cannot run; external effects never create a document undo boundary.
    #[test]
    fn exports_unavailable_effect_resources_without_document_actions() {
        let mut app = ContainersApp::new(false);
        let mut doc = AutoCommit::new();

        // Exercise the real event path (open + close the app palette) so this is
        // the live widget after interaction, not a pristine one.
        app.open_container_palette();
        send(&mut app, &mut doc, &named_event(NamedKey::Escape));
        // A stray click on the surface, too.
        send(&mut app, &mut doc, &left_press());

        let resources = Widget::resources(&app, &doc);
        assert_eq!(resources.len(), 1, "one container-engine root");
        let engine = &resources[0];
        assert_eq!(engine.kind, "containers.engine");
        let container = engine
            .children
            .iter()
            .flat_map(|resource| resource.actions.iter())
            .filter(|action| action.kind == ui::qos_actions::OperationKind::Effect)
            .collect();
        assert!(
            actions
                .iter()
                .any(|action| matches!(action.id.as_str(), "start" | "stop")),
            "container publishes the lifecycle action appropriate to its state",
        );
        assert!(actions.iter().all(|action| {
            action.kind == ui::qos_actions::OperationKind::Effect
                && !action.contract.availability.is_available()
        }));
        assert!(
            app.take_committed_action().is_none(),
            "no action is dispatched, so no action-host undo boundary is recorded",
        );
    }
}
