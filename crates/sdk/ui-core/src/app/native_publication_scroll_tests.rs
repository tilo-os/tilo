// Connected native input/publication fixture; no GPU or physical scanout.
struct PublicationProbeTransport;
impl crate::collaboration::PeerTransport for PublicationProbeTransport {
    fn is_connected(&self) -> bool {
        true
    }
    fn send(&mut self, _: CollabRequest) -> Result<(), String> {
        Ok(())
    }
    fn poll(&mut self) -> Vec<CollabEvent> {
        Vec::new()
    }
    fn publish_action_directory(&mut self, _: String) -> Result<(), String> {
        Ok(())
    }
}

struct PublicationProbeRoot {
    inner: crate::widget::Scrollable,
    view_only: bool,
    syncs: Rc<Cell<usize>>,
    declarations: Rc<Cell<usize>>,
    edit_on_wheel: bool,
}
impl crate::widget::Decorator for PublicationProbeRoot {
    crate::provides!(dyn crate::resources::PublishesResources, dyn crate::widget::ResourceOperations; inner = inner);
    type Inner = crate::widget::Scrollable;
    fn inner(&self) -> &Self::Inner {
        &self.inner
    }
    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.inner
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.inner
            .accessibility()
            .map(|node| node.with_hashed_key("native.scroll-probe"))
    }
    fn view_only_scroll(&self, event: &WindowEvent) -> bool {
        self.view_only && matches!(event, WindowEvent::MouseWheel { .. })
    }
    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        self.syncs.set(self.syncs.get() + 1);
        self.inner.sync(state, path);
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.edit_on_wheel && matches!(event, WindowEvent::MouseWheel { .. }) {
            use automerge::transaction::Transactable;
            ctx.state
                .document_mut()
                .put(automerge::ROOT, "wheel-edit", true)
                .unwrap();
        }
        self.inner.handle_event(event, ctx);
    }
}

impl crate::widget::ResourceOperations for PublicationProbeRoot {
    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        let mut handlers = crate::widget::resource_operation_handlers(&self.inner);
        handlers.insert(crate::a11y_tree::SEMANTIC_OPERATION_TYPE.to_string());
        handlers
    }
}
impl crate::widget::AppWidget for PublicationProbeRoot {
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Scroll probe"
    }
    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Glyph("S")
    }
}

impl crate::resources::PublishesResources for PublicationProbeRoot {
    fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
        self.declarations.set(self.declarations.get() + 1);
        vec![
            tilo_actions::Resource::new("probe:scroll", "probe", "Scroll fixture").with_action(
                tilo_actions::ActionSpec::operation(
                    "inspect",
                    "Inspect",
                    tilo_actions::OperationKind::View,
                    crate::a11y_tree::SEMANTIC_OPERATION_TYPE,
                    &crate::a11y_tree::SemanticOperation {
                        node: "root".into(),
                        action: "focus".into(),
                        value: None,
                    },
                )
                .unwrap(),
            ),
        ]
    }
}

#[test]
fn connected_native_long_document_wheel_publication_probe() {
    crate::i18n::I18nManager::init("en-US");
    // Keep the ordinary regression bounded while ensuring its document
    // overflows. The recorded hardware-sized benchmark explicitly selects
    // 180 paragraphs and retains its original viewport and wheel step.
    let paragraphs = std::env::var("TILO_NATIVE_SCROLL_PROBE_PARAGRAPHS")
        .map(|raw| raw.parse::<usize>().expect("integer paragraph count"))
        .unwrap_or(32);
    assert!((32..=180).contains(&paragraphs));
    let body = (0..paragraphs)
        .map(|index| {
            format!(
                "Paragraph {index}: {}\n\n",
                "Measured scrolling text with ordinary words. ".repeat(6)
            )
        })
        .collect::<String>();
    let (viewport, wheel_step) = if paragraphs == 180 {
        ((1051, 952), -1.0)
    } else {
        ((1051, 240), -0.25)
    };
    let offset = Rc::new(Cell::new((0.0, 0.0)));
    let inner = crate::widget::Scrollable::new_vertical()
        .with_persisted_offset(Rc::clone(&offset))
        .with_child(Box::new(crate::widget::BlockEditor::from_markdown(&body)));
    let root = PublicationProbeRoot {
        inner,
        view_only: false,
        syncs: Rc::new(Cell::new(0)),
        declarations: Rc::new(Cell::new(0)),
        edit_on_wheel: false,
    };
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
    app.collaboration.manager =
        CollaborationManager::with_peer_transport(PublicationProbeTransport);
    let region = Region {
        size: viewport,
        redraw: Cell::new(false),
    };
    let mut scene = Scene::new();
    app.surface_frame(&region, &mut scene);
    assert!(
        app.actions
            .last_published_actions
            .as_ref()
            .is_some_and(|actions| !actions.is_empty()),
        "native publisher must run"
    );
    app.surface_input(
        &region,
        crate::synthetic_input::cursor_moved(400.0, f64::from(viewport.1) / 2.0),
        false,
    );
    for sample in 0..10 {
        let before = offset.get();
        let started = crate::time::now();
        assert!(app.surface_input(
            &region,
            crate::synthetic_input::mouse_wheel(0.0, wheel_step),
            false
        ));
        let input_ns = started.elapsed().as_nanos();
        assert!(offset.get().1 > before.1, "real native wheel moves content");
        scene.reset();
        let started = crate::time::now();
        app.surface_frame(&region, &mut scene);
        eprintln!(
            "native-publication-wheel {}",
            serde_json::json!({
                "sample": sample, "input_ns": input_ns,
                "frame_ns": started.elapsed().as_nanos(),
                "paragraphs": paragraphs, "markdown_bytes": body.len(),
                "viewport": [viewport.0, viewport.1], "wheel_line_delta": wheel_step, "architecture": std::env::consts::ARCH,
                "profile": if cfg!(debug_assertions) { "debug" } else { "optimized" },
                "connected_publication": true, "notes_framework": false,
                "gpu_or_scanout": false,
            })
        );
    }
}

// Like the rich-text surface, this editor declares a durable semantic root.
// Publication must retain its real block controls instead of pruning them.
struct PublicationProbeEditor {
    inner: crate::widget::BlockEditor,
}
impl crate::widget::Decorator for PublicationProbeEditor {
    type Inner = crate::widget::BlockEditor;
    fn inner(&self) -> &Self::Inner {
        &self.inner
    }
    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.inner
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.inner
            .accessibility()
            .map(|node| node.with_hashed_key("native.scroll-editor"))
    }
}

#[cfg(unix)]
thread_local! {
    static SCROLL_FACTORY_ROOT: RefCell<Option<PublicationProbeRoot>> = const { RefCell::new(None) };
}

#[cfg(unix)]
fn scroll_probe_factory(
    mut context: crate::application::HostContext,
    _: crate::application::LaunchTarget,
    _: crate::application::RestoreInput,
) -> Result<crate::application::AppComponent, crate::launch::LaunchError> {
    context
        .initialize_new_record(|_| Ok(()))
        .map_err(crate::launch::LaunchError::Record)?;
    let root =
        SCROLL_FACTORY_ROOT.with_borrow_mut(|root| root.take().expect("prepared scroll root"));
    Ok(crate::application::AppComponent::new(
        context,
        Box::new(root),
        tilo_actions::ActionRegistry::new(),
    ))
}

#[cfg(unix)]
fn bound_scroll_app(
    root: PublicationProbeRoot,
) -> (App, crate::application_test_support::ProbeHost) {
    use crate::application_test_support::{
        APP, DEVICE, author, provenance, runner_inputs, runner_permissions,
    };
    let mut host = crate::application_test_support::ProbeHost::new();
    let room = "00000000-0000-0000-0000-000000000074";
    host.records = host.owners.open(APP, room).unwrap();
    host.views = Rc::new(
        crate::application_view::InstanceViewStore::new(host.root(), DEVICE, room).unwrap(),
    );
    let definition = crate::application_test_support::definition(scroll_probe_factory);
    let target = crate::application::LaunchTarget::New;
    let context = crate::application::HostContext::prepare(
        &definition,
        crate::application::HostContextInputs {
            records: host.records.clone(),
            views: Rc::clone(&host.views),
            device_id: DEVICE.into(),
            instance_id: 74,
            permissions: runner_permissions(),
            author: author(),
            action_context: provenance(),
        },
        &target,
    )
    .unwrap();
    SCROLL_FACTORY_ROOT.with_borrow_mut(|slot| *slot = Some(root));
    let component = definition
        .instantiate(context, target, crate::application::RestoreInput::None)
        .unwrap();
    let binding = component.callback_binding();
    // The new record's first write lands only after its writer's debounce.
    // Until then the component reports it Saving, and the store's wake once
    // it lands brings a full model sync, so a loaded runner whose wheel and
    // frame straddle that wake sees a sync no scroll caused. Measure from
    // the durable record a settled window shows.
    binding
        .records
        .try_flush_record(&binding.checkout.id)
        .unwrap();
    binding
        .records
        .await_submitted(&binding.checkout.id)
        .unwrap();
    assert_eq!(
        binding.records.save_status(&binding.checkout.id).unwrap(),
        crate::record::RecordSaveStatus::Saved,
        "the bound fixture starts from a durable record"
    );
    let route = binding
        .broker_route()
        .expect("actual canonical record route");
    let history = host
        .records
        .read_checked(&binding.checkout.id, |doc| doc.clone().save())
        .unwrap();
    let manager = CollaborationManager::native_initial_named_fixture_for_test(route, history);
    let mut app =
        App::from_component_with_collaboration(component, runner_inputs(), manager).unwrap();
    let deadline = tilo_test_env::hang_deadline();
    while !app.published_action_history_ready() {
        app.poll_and_apply_collab_events();
        assert!(
            crate::time::now() < deadline,
            "original bound history must actually reach its owner"
        );
        std::thread::yield_now();
    }
    (app, host)
}

struct ViewScrollFixture {
    app: App,
    region: Region,
    scene: Scene,
    offset: Rc<Cell<(f32, f32)>>,
    syncs: Rc<Cell<usize>>,
    declarations: Rc<Cell<usize>>,
    paragraphs: usize,
    _host: Option<crate::application_test_support::ProbeHost>,
}

impl ViewScrollFixture {
    fn new(view_only: bool, edit_on_wheel: bool) -> Self {
        Self::new_as(view_only, edit_on_wheel, false)
    }

    fn new_as(view_only: bool, edit_on_wheel: bool, bound: bool) -> Self {
        crate::i18n::I18nManager::init("en-US");
        let scope = crate::wake::WakeScope::new();
        let _entry = scope.enter();
        let paragraphs = std::env::var("TILO_NATIVE_SCROLL_PROBE_PARAGRAPHS")
            .map(|raw| raw.parse::<usize>().expect("integer paragraph count"))
            .unwrap_or(32);
        assert!((32..=180).contains(&paragraphs));
        let body = (0..paragraphs)
            .map(|i| {
                format!(
                    "Paragraph {i}: {}\n\n",
                    "Measured scrolling text with ordinary words. ".repeat(6)
                )
            })
            .collect::<String>();
        let offset = Rc::new(Cell::new((0.0, 0.0)));
        let syncs = Rc::new(Cell::new(0));
        let declarations = Rc::new(Cell::new(0));
        let root = PublicationProbeRoot {
            inner: crate::widget::Scrollable::new_vertical()
                .with_persisted_offset(Rc::clone(&offset))
                .with_child(Box::new(PublicationProbeEditor {
                    inner: crate::widget::BlockEditor::from_markdown(&body),
                })),
            view_only,
            syncs: Rc::clone(&syncs),
            declarations: Rc::clone(&declarations),
            edit_on_wheel,
        };
        let (mut app, host) = if bound {
            #[cfg(unix)]
            {
                let (app, host) = bound_scroll_app(root);
                (app, Some(host))
            }
            #[cfg(not(unix))]
            panic!("the named broker fixture requires Unix sockets");
        } else {
            let mut app =
                App::from_scoped_parts_with_controlled_inputs(scope, State::new(), Box::new(root));
            app.collaboration.manager =
                CollaborationManager::with_peer_transport(PublicationProbeTransport);
            (app, None)
        };
        let region = Region {
            size: (1051, if paragraphs == 180 { 952 } else { 240 }),
            redraw: Cell::new(false),
        };
        let mut scene = Scene::new();
        app.surface_frame(&region, &mut scene);
        app.surface_input(
            &region,
            crate::synthetic_input::cursor_moved(400.0, f64::from(region.size.1) / 2.0),
            false,
        );
        app.surface_frame(&region, &mut scene);
        assert!(!app.needs_sync, "fixture starts settled");
        assert!(
            app.actions
                .last_published_actions
                .as_ref()
                .is_some_and(|actions| !actions.is_empty())
        );
        Self {
            app,
            region,
            scene,
            offset,
            syncs,
            declarations,
            paragraphs,
            _host: host,
        }
    }

    fn wheel(&mut self) -> u128 {
        let before = self.offset.get().1;
        let started = crate::time::now();
        assert!(self.app.surface_input(
            &self.region,
            crate::synthetic_input::mouse_wheel(
                0.0,
                if self.paragraphs == 180 { -1.0 } else { -0.25 }
            ),
            false
        ));
        let elapsed = started.elapsed().as_nanos();
        assert!(
            self.offset.get().1 > before,
            "production wheel moves content"
        );
        elapsed
    }

    fn frame(&mut self) -> u128 {
        self.scene.reset();
        let started = crate::time::now();
        self.app.surface_frame(&self.region, &mut self.scene);
        started.elapsed().as_nanos()
    }
}

/// A wheel only the scroller took is view-only whether the root declares it
/// up front or the scroller reports its pan: either way neither the document
/// sync nor the resource projection runs again. Settings declared nothing,
/// so every notch republished its whole tree.
#[test]
fn view_only_native_scroll_keeps_publication_and_updates_accessibility_geometry() {
    for view_only in [false, true] {
        let mut fixture = ViewScrollFixture::new(view_only, false);
        let before_doc = crate::doc::settle_mark(&fixture.app.state.doc);
        fixture.app.refresh_semantic_tree();
        let before_geometry = fixture
            .app
            .semantic_tree()
            .iter()
            .filter_map(|node| node.bounds.map(|bounds| (node.id.clone(), bounds)))
            .collect::<Vec<_>>();
        let syncs = fixture.syncs.get();
        let declarations = fixture.declarations.get();
        let directory = fixture.app.actions.last_published_actions.clone();
        for sample in 0..10 {
            let input_ns = fixture.wheel();
            assert!(
                !fixture.app.needs_sync,
                "view-only wheel must not request document sync"
            );
            let frame_ns = fixture.frame();
            eprintln!(
                "native-view-only-wheel {}",
                serde_json::json!({
                    "view_only":view_only,"sample":sample,"input_ns":input_ns,"frame_ns":frame_ns,
                    "paragraphs":fixture.paragraphs,"viewport":[fixture.region.size.0,fixture.region.size.1],"architecture":std::env::consts::ARCH,
                    "profile":if cfg!(debug_assertions) {"test-debug"} else {"optimized"},
                    "connected_publication":true,"notes_framework":false,"gpu_or_scanout":false,
                    "sync_calls":fixture.syncs.get()-syncs,"declaration_calls":fixture.declarations.get()-declarations,
                })
            );
        }
        assert_eq!(
            crate::doc::settle_mark(&fixture.app.state.doc),
            before_doc,
            "view input leaves the canonical document unchanged"
        );
        assert_eq!(
            fixture.app.actions.last_published_actions, directory,
            "the action surface remains available"
        );
        assert_eq!(
            fixture.syncs.get(),
            syncs,
            "no repeated document synchronization"
        );
        assert_eq!(
            fixture.declarations.get(),
            declarations,
            "no repeated resource projection"
        );
        fixture.app.refresh_semantic_tree();
        assert!(
            before_geometry.iter().any(|(id, bounds)| fixture
                .app
                .semantic_tree()
                .node(id)
                .is_some_and(|node| node.bounds.is_some_and(|after| after != *bounds))),
            "fresh semantic geometry follows scrolling even while publication is cached"
        );
    }
}

#[test]
fn view_only_native_scroll_refuses_pending_and_dispatch_edits() {
    use automerge::{ReadDoc, transaction::Transactable};
    for (edit_during_dispatch, declared) in
        [(false, true), (true, true), (false, false), (true, false)]
    {
        let mut fixture = ViewScrollFixture::new(declared, edit_during_dispatch);
        if !edit_during_dispatch {
            fixture
                .app
                .state
                .doc
                .put(automerge::ROOT, "pending-edit", true)
                .unwrap();
            assert!(fixture.app.state.doc.pending_ops() > 0);
        }
        let syncs = fixture.syncs.get();
        let declarations = fixture.declarations.get();
        fixture.wheel();
        assert!(
            fixture.app.needs_sync,
            "actual or pending edits must retain synchronization"
        );
        fixture.frame();
        assert!(fixture.syncs.get() > syncs);
        assert!(fixture.declarations.get() > declarations);
        let key = if edit_during_dispatch {
            "wheel-edit"
        } else {
            "pending-edit"
        };
        assert!(
            fixture
                .app
                .state
                .doc
                .get(automerge::ROOT, key)
                .unwrap()
                .is_some(),
            "the real edit survives the fallback"
        );
    }
}

#[cfg(unix)]
#[test]
fn bound_view_only_native_scroll_carries_owner_without_hiding_record_edits() {
    // Declared by the root, or reported by the scroller it pans: a hosted
    // app (Settings) declares nothing.
    for (edit, declared) in [(false, true), (true, true), (false, false), (true, false)] {
        let mut fixture = ViewScrollFixture::new_as(declared, edit, true);
        let binding = fixture
            .app
            .root_widget
            .callback_binding()
            .expect("real bound owner");
        let before = binding
            .read_document(&fixture.app.state.doc, |doc, _| {
                crate::doc::settle_mark(doc)
            })
            .unwrap();
        assert_eq!(before.1, 0, "bound fixture is committed");
        let syncs = fixture.syncs.get();
        let declarations = fixture.declarations.get();
        fixture.wheel();
        // A saved record is never sampled again on a timer: however long a
        // loaded runner takes between the wheel and its frame, only the
        // wheel syncs.
        let component = &mut fixture
            .app
            .root_widget
            .as_any_mut()
            .and_then(|root| root.downcast_mut::<crate::widget::BoundAppWidget>())
            .expect("a bound component")
            .component;
        assert_eq!(
            component.sampled_record_save(),
            Some(&crate::record::RecordSaveStatus::Saved)
        );
        let after = binding
            .read_document(&fixture.app.state.doc, |doc, _| {
                crate::doc::settle_mark(doc)
            })
            .unwrap();
        if edit {
            assert_ne!(after, before, "wheel edited the actual bound record");
            assert!(fixture.app.needs_sync);
        } else {
            assert_eq!(after, before);
            assert!(!fixture.app.needs_sync);
        }
        fixture.frame();
        if edit {
            assert!(fixture.syncs.get() > syncs);
            assert!(fixture.declarations.get() > declarations);
        } else {
            assert_eq!(fixture.syncs.get(), syncs);
            assert_eq!(fixture.declarations.get(), declarations);
        }
    }
}

#[test]
fn view_only_native_scroll_keeps_modified_input_and_model_work_on_full_path() {
    for model_pending in [false, true] {
        let mut fixture = ViewScrollFixture::new(true, false);
        if model_pending {
            fixture.app.needs_sync = true;
        } else {
            // Alt, not Ctrl: Ctrl+wheel zooms the window instead of scrolling.
            fixture
                .app
                .set_surface_modifiers(winit::event::Modifiers::from(
                    winit::keyboard::ModifiersState::ALT,
                ));
        }
        let syncs = fixture.syncs.get();
        let declarations = fixture.declarations.get();
        fixture.wheel();
        fixture.frame();
        assert!(fixture.syncs.get() > syncs);
        assert!(fixture.declarations.get() > declarations);
    }
}

#[test]
fn native_long_document_frame_phase_probe() {
    let mut fixture = ViewScrollFixture::new(true, false);
    fixture.app.profiler = Some(crate::profiling::FrameProfiler::for_capture());
    for sample in 0..3 {
        fixture.wheel();
        crate::profiling::frame_probe::enable();
        let elapsed = fixture.frame();
        let draw_phases = crate::profiling::frame_probe::take();
        let profiler = fixture.app.profiler.as_mut().unwrap();
        profiler.commit_frame(u64::try_from(elapsed / 1000).unwrap());
        let timings = profiler.take_capture();
        assert_eq!(timings.len(), 1);
        eprintln!(
            "native-frame-phase {}",
            serde_json::json!({
                "sample":sample,"paragraphs":fixture.paragraphs,
                "viewport":[fixture.region.size.0,fixture.region.size.1],
                "profile":"test-debug","gpu_or_scanout":false,
                "timings":timings[0], "draw_phases_ns_calls":draw_phases,
            })
        );
    }
}
