use super::*;
use crate::widget::{AppModel, ModelPath};
use crate::widget::{Button, Widget};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tilo_actions::{Action, ActionError, ActionSpec, Doc, Resource};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, KeyLocation, NamedKey, PhysicalKey};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Flip {
    key: String,
}
const FLIP_TYPE: &str = "test.flip";
impl Action for Flip {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        doc.put(&ROOT, self.key.as_str(), ScalarValue::Boolean(true))?;
        Ok(())
    }
}

/// Stands in for an app's contacts/files provider.
struct PeopleProvider;
impl crate::search::SearchProvider for PeopleProvider {
    fn name(&self) -> &'static str {
        "People"
    }
    fn search(&self, query: &str) -> Vec<crate::search::SearchResult> {
        ["Ada Lovelace", "Grace Hopper"]
            .into_iter()
            .filter(|n| {
                crate::search::fold_search_text(n).contains(&crate::search::fold_search_text(query))
            })
            .map(|n| crate::search::SearchResult::new("People", n, format!("person:{n}")))
            .collect()
    }
}

struct LazyProvider;
impl crate::search::SearchProvider for LazyProvider {
    fn name(&self) -> &'static str {
        "Lazy"
    }

    fn search(&self, query: &str) -> Vec<crate::search::SearchResult> {
        if query.eq_ignore_ascii_case("restricted") {
            vec![crate::search::SearchResult::for_action(
                "Lazy",
                "Restricted lazy action",
                tilo_actions::ActionAddress::new("test.lazy:1", "flip"),
            )]
        } else {
            Vec::new()
        }
    }

    fn resolve_action_resource(&self, _address: &tilo_actions::ActionAddress) -> Option<Resource> {
        panic!("search providers must never be asked for authority")
    }
}

/// One open resource exporting a [`Flip`] action, plus a searchable
/// People provider.
struct Publisher;
struct ConsumingKeyboardRoot;
impl Widget for ConsumingKeyboardRoot {
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = tree.new_leaf(taffy::Style::default()).unwrap();
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, _: Point) {}
    fn draw(&mut self, _: &mut Scene, _: Affine, _: &mut TextRenderer, _: &Theme, _: &PresenceCtx) {
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if matches!(
            event,
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
        ) {
            ctx.state.put(&ROOT, "escaped_keyboard", true).unwrap();
            *ctx.handled = true;
        }
    }
    fn get_rect(&self) -> KurboRect {
        KurboRect::ZERO
    }
    fn name(&self) -> &'static str {
        "consuming-keyboard-root"
    }
}
impl Widget for Publisher {
    crate::provides!(
        dyn crate::resources::PublishesResources,
        dyn crate::search::capability::SearchSource
    );

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
    fn draw(
        &mut self,
        _s: &mut Scene,
        _t: Affine,
        _tr: &mut TextRenderer,
        _th: &Theme,
        _p: &PresenceCtx,
    ) {
    }
    fn name(&self) -> &'static str {
        "publisher"
    }
    fn get_rect(&self) -> KurboRect {
        KurboRect::ZERO
    }
}

impl crate::search::capability::SearchSource for Publisher {
    fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
        vec![Box::new(PeopleProvider), Box::new(LazyProvider)]
    }
    fn activate_search_result(&mut self, action_id: &str, doc: &mut AppModel, p: &ModelPath) {
        let (doc, _) = crate::doc::document(doc, p);
        doc.put(&ROOT, "navigated", ScalarValue::Str(action_id.into()))
            .unwrap();
    }
}

impl crate::resources::PublishesResources for Publisher {
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        vec![
            Resource::new("test.obj:1", "test.obj", "Widget Object").with_action(
                ActionSpec::immediate(
                    "flip",
                    "Flip it",
                    FLIP_TYPE,
                    &Flip {
                        key: "flipped".into(),
                    },
                )
                .unwrap(),
            ),
        ]
    }
    fn resource(&self, _doc: &AutoCommit, id: &str) -> Option<Resource> {
        (id == "test.lazy:1").then(|| {
            Resource::new("test.lazy:1", "test.lazy", "Lazy object").with_action(
                ActionSpec::immediate(
                    "flip",
                    "Flip lazy object",
                    FLIP_TYPE,
                    &Flip {
                        key: "lazy-flipped".into(),
                    },
                )
                .unwrap()
                .requiring(tilo_actions::PermissionRequirement::Write),
            )
        })
    }
}

struct CountingPublisher(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl Widget for CountingPublisher {
    crate::provides!(dyn crate::resources::PublishesResources);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
    fn draw(
        &mut self,
        _s: &mut Scene,
        _t: Affine,
        _tr: &mut TextRenderer,
        _th: &Theme,
        _p: &PresenceCtx,
    ) {
    }
    fn name(&self) -> &'static str {
        "counting-publisher"
    }
    fn get_rect(&self) -> KurboRect {
        KurboRect::ZERO
    }
}

impl crate::resources::PublishesResources for CountingPublisher {
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        vec![Resource::new("test.cache:1", "test.cache", "Cached")]
    }
}

include!("../resource_projection_tests.rs");

#[test]
fn palette_catalog_materializes_once_per_generation_under_fresh_authority() {
    // Case `control_refresh_without_action_bindings_does_not_materialize_catalog`.
    {
        use std::sync::atomic::Ordering;
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CountingPublisher(std::sync::Arc::clone(&calls))),
        );
        for _ in 0..3 {
            app.invalidate_widget_resource_cache();
            app.refresh_control_action_availability();
        }
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        app.actions.last_control_availability = Some((
            app.resource_cache_key(),
            app.actions.permission_generation.get(),
        ));
        app.refresh_control_action_availability();
        assert!(app.actions.last_control_availability.is_none());
        // Discovery still sees the complete surface on demand.
        assert!(
            app.collect_resources()
                .iter()
                .any(|r| r.id.as_str() == "test.cache:1")
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    // Case `resource_tree_is_materialized_once_per_exact_generation`.
    {
        use std::sync::atomic::Ordering;

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CountingPublisher(std::sync::Arc::clone(&calls))),
        );
        let _ = app.collect_resources();
        let _ = app.collect_resources();
        app.refresh_control_action_availability();
        app.refresh_control_action_availability();
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        app.state_mut_for_test()
            .doc
            .put(&ROOT, "cache-version", 1_i64)
            .expect("test document mutation");
        let _ = app.collect_resources();
        assert_eq!(calls.load(Ordering::Relaxed), 2);

        app.invalidate_widget_resource_cache();
        let _ = app.collect_resources();
        assert_eq!(calls.load(Ordering::Relaxed), 3);
    }

    // Case `exhausted_resource_generation_evicts_instead_of_reusing_a_snapshot`.
    {
        use std::sync::atomic::Ordering;

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(CountingPublisher(std::sync::Arc::clone(&calls))),
        );
        app.actions.resource_generation.set(u64::MAX);

        let _ = app.collect_resources();
        app.invalidate_widget_resource_cache();
        let _ = app.collect_resources();

        assert_eq!(app.actions.resource_generation.get(), u64::MAX);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            2,
            "each invalidation at the terminal generation rematerializes"
        );
    }

    // Case `exhausted_permission_generation_evicts_the_old_authority_projection`.
    {
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(Publisher));
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);
        assert_eq!(app.collect_local_resources().len(), 1);
        app.actions.permission_generation.set(u64::MAX);

        app.set_permission_ctx(PermissionContext::from_capabilities(
            collab_types::caps::Actor::person(None, "restricted"),
            Vec::new(),
        ));

        assert_eq!(app.actions.permission_generation.get(), u64::MAX);
        assert!(
            app.collect_local_resources().is_empty(),
            "the cached write action cannot survive a permission change"
        );
    }

    // Case `reopening_palette_reuses_live_resource_index_until_state_mutates`.
    {
        let mut app = App::new(State::new(), Box::new(Publisher));
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);

        app.open_action_palette();
        let first = app
            .actions
            .open_resource_search_cache
            .borrow()
            .as_ref()
            .expect("first open builds the live-resource index")
            .provider
            .clone();
        app.close_action_palette();
        app.open_action_palette();
        let second = app
            .actions
            .open_resource_search_cache
            .borrow()
            .as_ref()
            .expect("second open retains the index")
            .provider
            .clone();
        assert!(first.shares_storage_with(&second));

        app.invalidate_widget_resource_cache();
        assert!(app.actions.open_resource_search_cache.borrow().is_none());
    }

    // Case `visible_search_uses_existing_palette_and_fresh_authority`.
    {
        for revoke in [false, true] {
            let mut app = App::new_with_controlled_inputs(State::new(), Box::new(Publisher));
            app.action_registry_mut().register::<Flip>(FLIP_TYPE);
            app.enable_document_history_chrome();
            app.init_layout();
            let node = app
                .history_layout
                .unwrap()
                .search
                .expect("retained Search button");
            assert!(app.is_history_node(node), "Search is admitted local chrome");
            let button = app
                .document_history
                .search_mut()
                .expect("palette-capable host shows Search");
            assert_eq!(button.test_id(), Some("standalone-search-open"));
            assert!(button.perform_action(
                &crate::a11y::Action::Activate,
                &mut app.state.doc,
                &app.state.root
            ));
            assert_eq!(app.dispatch_history_controls(), 1);
            assert!(
                app.action_palette().is_some(),
                "real semantic button opens existing palette"
            );
            app.actions
                .action_palette
                .as_mut()
                .unwrap()
                .set_query("Flip it");
            assert!(
                app.action_palette()
                    .unwrap()
                    .result_labels()
                    .iter()
                    .any(|label| label.contains("Flip it"))
            );
            if revoke {
                app.set_permission_ctx(PermissionContext::from_capabilities(
                    collab_types::caps::Actor::person(None, "restricted"),
                    Vec::new(),
                ));
            }
            assert!(app.action_palette_handle_key(&synth_enter()));
            assert_eq!(
                app.state.doc.get(&ROOT, "flipped").unwrap().is_some(),
                !revoke,
                "activation re-resolves current permission rather than cached row authority"
            );
            assert!(
                !app.collect_resources()
                    .iter()
                    .any(|resource| resource.id.as_str() == crate::search::request::SEARCH_RESOURCE),
                "host Search never becomes an application/broker resource"
            );
            let address =
                tilo_actions::ActionAddress::new(crate::search::request::SEARCH_RESOURCE, "open");
            assert!(
                app.invoke_resource_action(
                    ResourceInvocation::new(&address, "same-local-name").broker_admitted(true)
                )
                .is_err(),
                "broker admission and matching actor names do not grant host UI locality"
            );
        }
    }

    // Case `denied_lazy_search_action_never_crosses_the_host_permission_door`.
    {
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(Publisher));
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);
        app.set_permission_ctx(PermissionContext::from_capabilities(
            collab_types::caps::Actor::person(None, "restricted"),
            Vec::new(),
        ));
        app.open_action_palette();
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query("restricted");
        // A query advances one provider page immediately; remaining providers
        // (including the host's open resources) advance on later frames.
        // Bound the drain so a pagination regression still fails this test.
        let mut progress = app
            .actions
            .action_palette
            .as_mut()
            .unwrap()
            .advance_search_page();
        for _ in 0..8 {
            if !progress.pending {
                break;
            }
            progress = app
                .actions
                .action_palette
                .as_mut()
                .unwrap()
                .advance_search_page();
        }
        assert!(!progress.pending, "the tiny provider set must be exhausted");
        assert_eq!(
            app.action_palette().unwrap().result_labels(),
            vec!["Restricted lazy action"]
        );

        assert!(app.action_palette_handle_key(&synth_enter()));
        assert!(
            app.state.doc.get(&ROOT, "lazy-flipped").unwrap().is_none(),
            "the provider hit cannot bypass the host's current permission context"
        );
        assert!(
            app.action_palette().is_some(),
            "a refused row leaves search open instead of pretending it ran"
        );
    }
}

pub(super) fn synth_enter() -> WindowEvent {
    let event = KeyEvent::synthetic(
        winit::event::RawKeyEvent {
            physical_key: PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified),
            state: ElementState::Pressed,
        },
        Key::Named(NamedKey::Enter),
        None,
        KeyLocation::Standard,
        false,
        None,
        Key::Named(NamedKey::Enter),
    );
    WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        event,
        is_synthetic: false,
    }
}

#[test]
fn native_directional_keys_reach_the_focused_control_before_host_navigation() {
    use crate::widget::{Column, TextInput};
    use winit::keyboard::ModifiersState;

    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(
            Column::new()
                .push(Box::new(
                    TextInput::local().with_id("editor").with_initial_value("x"),
                ))
                .push(Box::new(Button::new("done", "Done", None))),
        ),
    );
    app.init_layout();
    let root = app.layout.root_node.unwrap();
    run_layout_pass(&mut app, root, 320, 180, 1.0, false, None).unwrap();
    let editor = app.focusable_order[0];
    let done = app.focusable_order[1];
    app.set_focused_node(Some(editor));

    let mut left = synth_enter();
    if let WindowEvent::KeyboardInput { event, .. } = &mut left {
        event.logical_key = Key::Named(NamedKey::ArrowLeft);
    }
    app.input.modifiers =
        winit::event::Modifiers::from(ModifiersState::ALT | ModifiersState::SHIFT);
    let mut handled = false;
    let mut focus = None;
    app.presentation_handle_event(&left, 1.0, &mut handled, &mut focus);
    assert!(handled, "the focused editor consumes exact Alt+Shift+Left");
    if let Some(focus) = focus {
        app.set_focused_node(Some(focus));
    }
    app.handle_unconsumed_directional_key(&left, None, true, &mut handled);
    assert_eq!(app.focused_node, Some(editor));
    assert_eq!(
        app.inspect_node(editor, |widget| crate::undo::get_state(widget))
            .unwrap(),
        Some("0:b:1".into()),
        "Shift+Left extends the editor selection instead of moving host focus"
    );

    let mut down = synth_enter();
    if let WindowEvent::KeyboardInput { event, .. } = &mut down {
        event.logical_key = Key::Named(NamedKey::ArrowDown);
    }
    app.input.modifiers = winit::event::Modifiers::from(ModifiersState::ALT);
    let mut handled = false;
    let mut focus = None;
    app.presentation_handle_event(&down, 1.0, &mut handled, &mut focus);
    assert!(!handled, "single-line editor leaves Alt+Down to the host");
    app.handle_unconsumed_directional_key(&down, None, true, &mut handled);
    assert!(handled);
    assert_eq!(app.focused_node, Some(done));
}

#[test]
fn palette_dispatch_routes_through_the_app_and_its_visible_controls() {
    // Case `tab_focused_host_buttons_keep_enter_and_space_out_of_consuming_app`.
    {
        for open_search in [false, true] {
            for key in [NamedKey::Enter, NamedKey::Space] {
                let mut app =
                    App::new_with_controlled_inputs(State::new(), Box::new(ConsumingKeyboardRoot));
                app.enable_document_history_chrome();
                app.init_layout();
                let layout = app.history_layout.unwrap();
                let target = if open_search {
                    layout.search.unwrap()
                } else {
                    layout.toolbar.unwrap()
                };
                app.set_focused_node(None);
                let mut tab = synth_enter();
                if let WindowEvent::KeyboardInput { event, .. } = &mut tab {
                    event.logical_key = Key::Named(NamedKey::Tab);
                }
                // Exercise the real native Tab event route, rather than assigning
                // the target directly or activating the Button as an AT action.
                for _ in 0..app.focusable_order.len() {
                    assert!(app.handle_tab_navigation(&tab, None));
                    if app.focused_node == Some(target) {
                        break;
                    }
                }
                assert_eq!(app.focused_node, Some(target), "Tab reaches host control");
                let ime = WindowEvent::Ime(winit::event::Ime::Commit("composed text".into()));
                let mut handled = false;
                let mut focus = None;
                assert!(app.presentation_handle_event(&ime, 1.0, &mut handled, &mut focus));
                assert!(
                    app.state
                        .doc
                        .get(&ROOT, "escaped_keyboard")
                        .unwrap()
                        .is_none(),
                    "IME input focused on host chrome must never reach app content"
                );
                let mut event = synth_enter();
                if let WindowEvent::KeyboardInput { event, .. } = &mut event {
                    event.logical_key = Key::Named(key);
                }
                let mut handled = false;
                let mut focus = None;
                assert!(app.presentation_handle_event(&event, 1.0, &mut handled, &mut focus));
                assert!(
                    app.state
                        .doc
                        .get(&ROOT, "escaped_keyboard")
                        .unwrap()
                        .is_none(),
                    "focused chrome input must never reach consuming app content"
                );
                assert!(!handled, "Button leaves activation to the native fallback");
                app.handle_unconsumed_activation_key(&event, &mut handled);
                assert!(handled);
                assert_eq!(app.dispatch_control_invocations(), 1);
                assert_eq!(app.action_palette().is_some(), open_search);
                assert_eq!(app.document_history.is_open(), !open_search);
                assert!(
                    app.state
                        .doc
                        .get(&ROOT, "escaped_keyboard")
                        .unwrap()
                        .is_none()
                );
                // Positive control: the same fixture consumes and mutates when
                // its own app node receives this event with chrome unfocused.
                app.set_focused_node(Some(layout.app));
                let mut handled = false;
                app.presentation_handle_event(&event, 1.0, &mut handled, &mut focus);
                assert!(handled);
                assert_eq!(
                    app.state
                        .doc
                        .get(&ROOT, "escaped_keyboard")
                        .unwrap()
                        .and_then(|(value, _)| value.as_bool()),
                    Some(true)
                );
                app.state.doc.delete(&ROOT, "escaped_keyboard").unwrap();
                let mut handled = false;
                app.presentation_handle_event(&ime, 1.0, &mut handled, &mut focus);
                assert!(
                    handled,
                    "app-focused IME reaches the same consuming fixture"
                );
                assert_eq!(
                    app.state
                        .doc
                        .get(&ROOT, "escaped_keyboard")
                        .unwrap()
                        .and_then(|(value, _)| value.as_bool()),
                    Some(true)
                );
            }
        }
    }

    // Case `ctrl_k_scopes_to_resources_then_dispatch_mutates_doc_and_records_undo`.
    {
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(Publisher));
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);

        let res = app.collect_resources();
        assert_eq!(res.len(), 1, "one open resource");
        assert_eq!(res[0].kind, "test.obj");

        // Scopes the palette to those resources AND folds in the widget's
        // searchable content — one surface, both kinds.
        app.open_action_palette();
        let ap = app.action_palette().expect("palette is open");
        assert!(ap.is_open());
        let labels = ap.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Flip it")),
            "palette lists the widget's action: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l == "Ada Lovelace"),
            "palette folds in the widget's searchable content: {labels:?}"
        );

        assert!(app.state.doc.get(&ROOT, "flipped").unwrap().is_none());
        assert!(app.state.peek_last_undo().is_none());
        app.needs_sync = false;
        app.layout.layout_dirty = false;

        // Enter selects the top row (the action) and dispatches it.
        let consumed = app.action_palette_handle_key(&synth_enter());
        assert!(consumed, "the open palette consumed the Enter key");

        let flipped = app
            .state
            .doc
            .get(&ROOT, "flipped")
            .unwrap()
            .and_then(|(v, _)| v.as_bool());
        assert_eq!(
            flipped,
            Some(true),
            "dispatched action set the flag in the CRDT"
        );
        assert!(
            app.state.peek_last_undo().is_some(),
            "the dispatched action is undoable"
        );
        assert!(app.needs_sync, "dispatch marks the app for re-sync");
        assert!(
            app.layout.layout_dirty,
            "dispatch marks the app for relayout"
        );
        assert!(
            app.action_palette().is_none(),
            "palette closed after selection"
        );
    }

    // Case `synthesized_semantic_action_routes_back_to_its_visible_control`.
    {
        let mut app = App::new(
            State::new(),
            Box::new(Button::new("complete", "Complete", Some("completed"))),
        );
        app.init_layout();
        let resource = app
            .collect_resources()
            .into_iter()
            .find(|resource| {
                resource.kind == "tilo.ui.control"
                    && resource.label == "Complete"
                    && resource
                        .actions
                        .iter()
                        .any(|action| action.id == "activate")
            })
            .expect("collaborative button publishes a semantic action");
        let address = tilo_actions::ActionAddress::new(resource.id.as_str(), "activate");
        let route = app
            .preferred_action_ui_route(&address)
            .expect("semantic action resolves to visible UI");

        assert_eq!(route.control_id, "complete");
        assert!(route.bounds.is_some());
        assert!(!route.disabled);
    }

    // Case `exact_widget_palette_request_resolves_only_declared_actions`.
    {
        let mut app = App::new(State::new(), Box::new(Publisher));
        app.action_registry_mut().register::<Flip>(FLIP_TYPE);
        assert!(
            app.open_action_palette_at(&tilo_actions::ActionAddress::new("test.obj:1", "flip"))
        );
        assert!(
            !app.open_action_palette_at(&tilo_actions::ActionAddress::new(
                "test.obj:1",
                "not-declared"
            )),
            "a stale or invented widget request is refused"
        );
        assert!(
            app.action_palette().is_none(),
            "a refused exact request does not leave an unrelated palette open"
        );
    }

    // Case `ctrl_k_content_row_navigates_through_the_app`:
    // A content row does NOT dispatch — it hands its id back through
    // [`Widget::activate_search_result`] instead.
    {
        let mut app = App::new(State::new(), Box::new(Publisher));
        app.open_action_palette();
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query("ada");
        let labels = app.action_palette().unwrap().result_labels();
        assert!(
            labels.iter().any(|l| l == "Ada Lovelace"),
            "content hit present: {labels:?}"
        );
        assert!(
            !labels.iter().any(|l| l.contains("Flip it")),
            "the action row is filtered out by the query: {labels:?}"
        );

        assert!(app.state.doc.get(&ROOT, "navigated").unwrap().is_none());
        app.needs_sync = false;
        app.layout.layout_dirty = false;

        let consumed = app.action_palette_handle_key(&synth_enter());
        assert!(consumed, "the open palette consumed the Enter key");

        let nav = app
            .state
            .doc
            .get(&ROOT, "navigated")
            .unwrap()
            .and_then(|(v, _)| v.as_str().map(str::to_string));
        assert_eq!(
            nav,
            Some("person:Ada Lovelace".to_string()),
            "the content row's id reached the app to navigate"
        );
        assert!(
            app.action_palette().is_none(),
            "navigation closes the palette"
        );
        assert!(
            app.needs_sync && app.layout.layout_dirty,
            "navigation marks a redraw"
        );
    }

    // Case `projected_view_preserves_pending_edit_and_eventual_attribution`.
    {
        let mut app = App::new_with_controlled_inputs(
            State::new(),
            Box::new(Button::new("focus-me", "Focus me", None)),
        );
        app.init_layout();
        app.set_focused_node(None);
        assert!(!app.root_widget.is_focused());
        let before = app.state.doc.get_heads();
        app.state.doc.put(ROOT, "draft", "unfinished").unwrap();
        let pending = app.state.doc.pending_ops();
        app.open_action_palette();
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query("focus");
        assert!(app.action_palette_handle_key(&synth_enter()));
        assert!(app.root_widget.is_focused());
        assert_eq!(app.state.doc.pending_ops(), pending);
        assert_eq!(app.state.doc.base_heads(), before);
        assert!(app.state.peek_last_undo().is_none());

        app.state.doc.put(ROOT, "draft", "finished").unwrap();
        let context = tilo_actions::ActionContext::for_resource(
            "Finish draft",
            "test.draft",
            "note",
            "Draft",
        );
        assert!(app.state.commit_action_authored_with_context(
            None,
            None,
            Some(app.collaboration.local_author.clone()),
            Some(context),
        ));
        let changes = app.state.doc.get_changes(&before);
        assert_eq!(changes.len(), 1, "view must not split the pending edit");
        let context = changes[0]
            .message()
            .and_then(|message| tilo_actions::ActionContext::from_change_message(message))
            .unwrap();
        assert_eq!(context.action_label, "Finish draft");
    }

    // Case `ctrl_k_invokes_a_projected_semantic_view_control`.
    {
        let button = Button::new("drive-me", "Drive me", None);
        let mut app = App::new_with_controlled_inputs(State::new(), Box::new(button));
        app.init_layout();
        app.set_focused_node(None);
        assert!(!app.root_widget.is_focused());
        app.open_action_palette();
        app.actions
            .action_palette
            .as_mut()
            .unwrap()
            .set_query("focus");

        assert!(app.action_palette_handle_key(&synth_enter()));
        assert!(
            app.root_widget.is_focused(),
            "Ctrl+K reaches the real retained control"
        );
    }
}

#[test]
fn host_commands_present_host_services_and_enter_the_palette_door() {
    use crate::host_chrome::model::{CommandGroup, HostCommand};
    use crate::platform::HostShortcut;
    let find = |commands: &[HostCommand], group: CommandGroup, action: &str| {
        commands
            .iter()
            .find(|command| command.group == group && command.binding.address().action == action)
            .cloned()
            .unwrap_or_else(|| panic!("{group:?} {action} is presented"))
    };
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(Publisher));
    app.action_registry_mut().register::<Flip>(FLIP_TYPE);
    for title in ["first", "second"] {
        app.state.doc.put(ROOT, "title", title).unwrap();
        app.state.commit_action(None, None);
    }
    app.enable_document_history_chrome();
    app.init_layout();
    let platform = app.input.platform;

    let commands = app.host_commands(true);
    assert!(find(&commands, CommandGroup::Undo, "undo").enabled);
    assert!(!find(&commands, CommandGroup::Undo, "redo").enabled);
    let search = find(&commands, CommandGroup::Search, "open");
    assert!(search.enabled);
    assert_eq!(
        search.shortcut,
        Some(HostShortcut::Search.shortcut(platform))
    );
    let flip = find(&commands, CommandGroup::AppActions, "flip");
    assert_eq!(flip.label, "Flip it");
    let show = find(&commands, CommandGroup::History, "open");
    assert_eq!(show.label, "Show History");
    assert!(show.enabled);
    assert_eq!(
        show.shortcut,
        Some(HostShortcut::History.shortcut(platform))
    );
    assert!(
        !commands
            .iter()
            .any(|command| command.binding.address().action == "compare"),
        "nothing to compare before History opens"
    );
    let recent: Vec<_> = commands
        .iter()
        .filter(|command| command.group == CommandGroup::RecentChanges)
        .cloned()
        .collect();
    assert_eq!(recent.len(), 2);
    assert!(
        recent
            .iter()
            .all(|command| command.enabled && command.label.contains(" · "))
    );
    assert!(
        commands
            .iter()
            .all(|command| command.binding.address().resource != "tilo.history"),
        "the broker History door is the History menu, not a second copy"
    );

    // An app action enters the same door as the palette and is undoable
    // from the Edit command.
    assert!(app.invoke_host_command(&flip.invocation().unwrap()));
    assert!(app.state.doc.get(ROOT, "flipped").unwrap().is_some());

    // Show History opens the pane; the item becomes Hide History, and
    // Compare waits for two selected changes.
    assert!(app.invoke_host_command(&show.invocation().unwrap()));
    assert!(app.document_history.is_open());
    let commands = app.host_commands(false);
    let hide = find(&commands, CommandGroup::History, "hide");
    assert_eq!(hide.label, "Hide History");
    assert!(!find(&commands, CommandGroup::History, "compare").enabled);
    assert!(app.invoke_host_command(&hide.invocation().unwrap()));
    assert!(!app.document_history.is_open());

    // A recent change opens History at that change: the newest is the flip.
    let newest = app
        .host_commands(true)
        .into_iter()
        .find(|command| command.group == CommandGroup::RecentChanges)
        .unwrap();
    assert!(app.invoke_host_command(&newest.invocation().unwrap()));
    let snapshot = app.document_history_snapshot().unwrap().unwrap();
    assert_eq!(
        snapshot.selection,
        vec![snapshot.entries.last().unwrap().id]
    );

    // The keyboard shortcut is the same command.
    assert!(app.invoke_host_shortcut(HostShortcut::History));
    assert!(!app.document_history.is_open());

    // Undo through the Edit command reverts the flip.
    let undo = find(&app.host_commands(false), CommandGroup::Undo, "undo");
    assert!(app.invoke_host_command(&undo.invocation().unwrap()));
    assert!(app.state.doc.get(ROOT, "flipped").unwrap().is_none());

    // While the palette is open its own shortcut closes it, so the Search
    // command steps aside.
    app.open_action_palette();
    assert!(!find(&app.host_commands(false), CommandGroup::Search, "open").enabled);
}
