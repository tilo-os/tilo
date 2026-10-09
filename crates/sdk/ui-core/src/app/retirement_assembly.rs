//! Inner ownership during callback-bearing App assembly. No gate is held here.
#[cfg(all(any(windows, target_os = "linux"), feature = "native"))]
use super::native_runner;
use super::{
    AccessibilityRuntime, ActionRuntime, App, AppConstructionInputs, Cell, CollaborationManager,
    CollaborationRuntime, ContinuationRuntime, FloatingRuntime, HistoryRuntime, InputRuntime,
    Instant, LayoutRuntime, NodeId, PaletteSource, PermissionContext, PersistenceRuntime,
    PresenceCtx, PresenceRuntime, State, TextRenderer, Theme, Widget, WindowRuntime,
};
use crate::semantic_document_read::retirement::RootRetirementPrelude;

pub(super) struct ComponentAssemblyCustody {
    pub component: Option<crate::application::AppComponent>,
    pub scope: Option<crate::wake::WakeScope>,
    pub retirement: Option<RootRetirementPrelude>,
}
impl Drop for ComponentAssemblyCustody {
    fn drop(&mut self) {
        if let Some(retirement) = &self.retirement {
            retirement.retire();
        }
    }
}

pub(super) struct AppAssemblyCustody {
    pub retirement: Option<RootRetirementPrelude>,
    pub root: Option<Box<dyn Widget>>,
    pub wake: Option<crate::wake::WakeScope>,
    pub state: Option<State>,
    pub control: Option<crate::ControlDomain>,
    pub inputs: Option<AppConstructionInputs>,
    pub configured_theme: Option<Theme>,
    #[expect(
        clippy::option_option,
        reason = "the outer Option is the custody slot; the inner one is the host's optional screen reader"
    )]
    pub screen_reader: Option<Option<crate::screen_reader::ScreenReader>>,
    pub values: AppAssemblyValues,
}
impl AppAssemblyCustody {
    pub fn retire(&self) {
        if let Some(retirement) = &self.retirement {
            retirement.retire();
        }
    }
}
impl Drop for AppAssemblyCustody {
    fn drop(&mut self) {
        self.retire();
    }
}

// Each evaluated value enters this owned staging area immediately. A later
// initializer panic cannot drop these values before AppAssemblyCustody::drop.
// Every field is an empty-or-staged slot for the App field of the same name,
// so an optional App field is staged as `Option<Option<T>>`.
#[expect(
    clippy::option_option,
    reason = "each outer Option is a staging slot for an App field that is itself optional"
)]
#[derive(Default)]
pub(super) struct AppAssemblyValues {
    #[cfg(all(any(windows, target_os = "linux"), feature = "native"))]
    pub startup_renderer: Option<Option<native_runner::RendererPreparation>>,
    pub history: Option<Option<Box<crate::history_mode::host::HistoryHost>>>,
    #[cfg(any(test, feature = "test-support"))]
    pub controlled_installed_history_definition:
        Option<Option<&'static crate::history_mode::HistoryDefinition>>,
    pub framework_input: Option<std::rc::Rc<Cell<bool>>>,
    pub history_key_release: Option<bool>,
    pub history_focus: Option<Option<crate::widget::layout::CompositionFocus>>,
    pub text_renderer: Option<TextRenderer>,
    pub palette_source: Option<PaletteSource>,
    pub watch_system_settings: Option<bool>,
    pub layout: Option<LayoutRuntime>,
    pub bindings: Option<crate::bound_dispatch::BoundNodeMap>,
    pub component_close_reports: Option<Vec<crate::application::ComponentCloseReport>>,
    pub close_admission: Option<crate::close_admission::Admission>,
    pub needs_sync: Option<bool>,
    pub theme: Option<Theme>,
    pub debug_mode: Option<bool>,
    pub focused_node: Option<Option<NodeId>>,
    pub focusable_order: Option<Vec<NodeId>>,
    pub autofocus_first: Option<bool>,
    pub autofocus_on_structure_change: Option<bool>,
    pub appearance_reconcile: Option<bool>,
    pub theme_store_stamp: Option<Option<settings_store::SettingsStamp>>,
    pub last_theme_store_check: Option<Instant>,
    pub input: Option<InputRuntime>,
    pub focus_history: Option<HistoryRuntime>,
    pub collaboration: Option<CollaborationRuntime>,
    pub native_room_route: Option<Option<crate::collaboration::RouteSpec>>,
    pub action_publication_failure: Option<Option<String>>,
    pub presence_ctx: Option<PresenceCtx>,
    pub permission_ctx: Option<PermissionContext>,
    pub presence: Option<PresenceRuntime>,
    pub accessibility: Option<AccessibilityRuntime>,
    pub window: Option<WindowRuntime>,
    pub document_routes: Option<crate::history_mode::routes::OwnerDocumentRoutes>,
    pub committed_documents: Option<
        Vec<(
            crate::collaboration::RouteSpec,
            crate::collaboration::CommittedDocument,
        )>,
    >,
    pub persistence: Option<PersistenceRuntime>,
    pub autosave_generation: Option<u64>,
    pub actions: Option<ActionRuntime>,
    pub document_history: Option<Box<crate::standalone_history_host::StandaloneHistoryHost>>,
    pub history_chrome_enabled: Option<bool>,
    pub native_document_chrome: Option<bool>,
    pub history_layout: Option<Option<crate::standalone_history_host::HostHistoryLayout>>,
    pub history_viewport_width: Option<f64>,
    pub history_focus_pending: Option<bool>,
    pub last_published_playback: Option<Option<Vec<media::handoff::PlaybackReport>>>,
    pub last_playback_publish: Option<Option<Instant>>,
    pub continuation: Option<ContinuationRuntime>,
    pub profiler: Option<Option<crate::profiling::FrameProfiler>>,
    pub native_input_probe: Option<Option<Box<dyn Fn() -> serde_json::Value>>>,
    pub floating: Option<FloatingRuntime>,
}

pub(super) struct ValidationRetirement<'a>(pub &'a Option<RootRetirementPrelude>, pub bool);
impl ValidationRetirement<'_> {
    pub fn disarm(mut self) {
        self.1 = false;
    }
}
impl Drop for ValidationRetirement<'_> {
    fn drop(&mut self) {
        if self.1 {
            if let Some(retirement) = self.0 {
                retirement.retire();
            }
        }
    }
}

#[expect(
    clippy::unreachable,
    reason = "both callers stage the root, state and inputs before assembly; a missing slot is a construction bug that retires first"
)]
pub(super) fn assemble(mut assembly: AppAssemblyCustody) -> App {
    let _entry = assembly.wake.as_ref().map_or_else(
        crate::wake::WakeScope::enter_detached,
        crate::wake::WakeScope::enter,
    );
    let (Some(inputs), Some(root)) = (assembly.inputs.as_mut(), assembly.root.as_mut()) else {
        assembly.retire();
        unreachable!("App assembly owns its root and inputs");
    };
    let configured_theme = assembly
        .configured_theme
        .insert(inputs.configured_theme.resolve_in_place());
    let control = assembly.control.insert(
        inputs
            .control_domain
            .clone()
            .unwrap_or_else(crate::control_custody::ControlDomain::outer),
    );
    crate::widget::controls::install_control_custody(
        root.as_mut(),
        control,
        crate::control_custody::CustodyAttachment::Initial,
    );
    control.finish_initial_installation();
    #[cfg(all(any(windows, target_os = "linux"), feature = "native"))]
    {
        assembly.values.startup_renderer = Some(None);
    }
    assembly.values.history = Some(None);
    #[cfg(any(test, feature = "test-support"))]
    {
        assembly.values.controlled_installed_history_definition = Some(None);
    }
    assembly.values.framework_input = Some(std::rc::Rc::new(Cell::new(false)));
    assembly.values.history_key_release = Some(false);
    assembly.values.history_focus = Some(None);
    assembly.values.text_renderer = Some(inputs.text_renderer.resolve_in_place());
    assembly.values.palette_source = Some(inputs.palette_source);
    assembly.values.watch_system_settings = Some(inputs.watch_system_settings);
    assembly.values.layout = Some(LayoutRuntime::new());
    assembly.values.bindings = Some(crate::bound_dispatch::BoundNodeMap::new());
    assembly.values.component_close_reports = Some(vec![]);
    assembly.values.close_admission = Some(crate::close_admission::Admission::default());
    assembly.values.needs_sync = Some(true);
    assembly.values.theme = Some(configured_theme.clone());
    assembly.values.debug_mode = Some((inputs.debug_mode)());
    assembly.values.focused_node = Some(None);
    assembly.values.focusable_order = Some(Vec::new());
    assembly.values.autofocus_first = Some(false);
    assembly.values.autofocus_on_structure_change = Some(false);
    assembly.values.appearance_reconcile = Some(false);
    assembly.values.theme_store_stamp = Some((inputs.theme_store_stamp)());
    assembly.values.last_theme_store_check = Some(crate::time::now());
    assembly.values.input = Some(InputRuntime::new());
    assembly.values.focus_history = Some(HistoryRuntime::new());
    let local_author = inputs.local_author.resolve_in_place();
    assembly.values.collaboration = Some(CollaborationRuntime::new(
        std::mem::replace(&mut inputs.collab_manager, CollaborationManager::disabled()),
        local_author,
    ));
    assembly.values.native_room_route = Some(None);
    assembly.values.action_publication_failure = Some(None);
    assembly.values.presence_ctx = Some(inputs.presence_ctx.resolve_in_place());
    assembly.values.permission_ctx = Some(PermissionContext::permissive());
    assembly.values.presence = Some(PresenceRuntime::new());
    assembly.screen_reader = Some((inputs.screen_reader)());
    let (Some(accessibility_theme), Some(screen_reader)) = (
        assembly.configured_theme.take(),
        assembly.screen_reader.take(),
    ) else {
        assembly.retire();
        unreachable!("the theme and screen reader were staged above");
    };
    assembly.values.accessibility = Some(AccessibilityRuntime::new(
        accessibility_theme,
        screen_reader,
    ));
    assembly.values.window = Some(WindowRuntime::new());
    assembly.values.document_routes =
        Some(crate::history_mode::routes::OwnerDocumentRoutes::default());
    assembly.values.committed_documents = Some(Vec::new());
    assembly.values.persistence = Some(PersistenceRuntime::new());
    assembly.values.autosave_generation = Some(0);
    assembly.values.actions = Some(ActionRuntime::new());
    assembly.values.document_history = Some(Box::new(
        crate::standalone_history_host::StandaloneHistoryHost::new(),
    ));
    assembly.values.history_chrome_enabled = Some(false);
    assembly.values.native_document_chrome = Some(true);
    assembly.values.history_layout = Some(None);
    assembly.values.history_viewport_width = Some(1000.0);
    assembly.values.history_focus_pending = Some(false);
    assembly.values.last_published_playback = Some(None);
    assembly.values.last_playback_publish = Some(None);
    let Some(inputs) = assembly.inputs.as_ref() else {
        assembly.retire();
        unreachable!("App assembly owns its inputs");
    };
    assembly.values.continuation = Some(ContinuationRuntime::new((inputs
        .pending_continuation_restore)(
    )));
    assembly.values.profiler = Some((inputs.profiler)());
    assembly.values.native_input_probe = Some(None);
    assembly.values.floating = Some(FloatingRuntime::new((inputs.floating_surface_client)()));
    // No callbacks or allocation remain in the transfer. Missing internal
    // slots retire before any extracted value can be dropped.
    let AppAssemblyValues {
        #[cfg(all(any(windows, target_os = "linux"), feature = "native"))]
            startup_renderer: Some(startup_renderer),
        history: Some(history),
        #[cfg(any(test, feature = "test-support"))]
            controlled_installed_history_definition: Some(controlled_installed_history_definition),
        framework_input: Some(framework_input),
        history_key_release: Some(history_key_release),
        history_focus: Some(history_focus),
        text_renderer: Some(text_renderer),
        palette_source: Some(palette_source),
        watch_system_settings: Some(watch_system_settings),
        layout: Some(layout),
        bindings: Some(bindings),
        component_close_reports: Some(component_close_reports),
        close_admission: Some(close_admission),
        needs_sync: Some(needs_sync),
        theme: Some(theme),
        debug_mode: Some(debug_mode),
        focused_node: Some(focused_node),
        focusable_order: Some(focusable_order),
        autofocus_first: Some(autofocus_first),
        autofocus_on_structure_change: Some(autofocus_on_structure_change),
        appearance_reconcile: Some(appearance_reconcile),
        theme_store_stamp: Some(theme_store_stamp),
        last_theme_store_check: Some(last_theme_store_check),
        input: Some(input),
        focus_history: Some(focus_history),
        collaboration: Some(collaboration),
        native_room_route: Some(native_room_route),
        action_publication_failure: Some(action_publication_failure),
        presence_ctx: Some(presence_ctx),
        permission_ctx: Some(permission_ctx),
        presence: Some(presence),
        accessibility: Some(accessibility),
        window: Some(window),
        document_routes: Some(document_routes),
        committed_documents: Some(committed_documents),
        persistence: Some(persistence),
        autosave_generation: Some(autosave_generation),
        actions: Some(actions),
        document_history: Some(document_history),
        history_chrome_enabled: Some(history_chrome_enabled),
        native_document_chrome: Some(native_document_chrome),
        history_layout: Some(history_layout),
        history_viewport_width: Some(history_viewport_width),
        history_focus_pending: Some(history_focus_pending),
        last_published_playback: Some(last_published_playback),
        last_playback_publish: Some(last_playback_publish),
        continuation: Some(continuation),
        profiler: Some(profiler),
        native_input_probe: Some(native_input_probe),
        floating: Some(floating),
    } = std::mem::take(&mut assembly.values)
    else {
        assembly.retire();
        unreachable!("all App construction values are staged");
    };
    let (Some(root_widget), Some(state), Some(control_domain), Some(inputs)) = (
        assembly.root.take(),
        assembly.state.take(),
        assembly.control.take(),
        assembly.inputs.take(),
    ) else {
        assembly.retire();
        unreachable!("App assembly owns all inputs");
    };
    let wake_scope = assembly.wake.take();
    let retirement = assembly.retirement.take();
    let mut app = App {
        retirement,
        #[cfg(all(any(windows, target_os = "linux"), feature = "native"))]
        startup_renderer,
        control_domain,
        wake_scope,
        state,
        root_widget,
        history,
        #[cfg(any(test, feature = "test-support"))]
        controlled_installed_history_definition,
        framework_input,
        history_key_release,
        history_focus,
        text_renderer,
        palette_source,
        watch_system_settings,
        layout,
        bindings,
        component_close_reports,
        close_admission,
        needs_sync,
        theme,
        debug_mode,
        focused_node,
        focusable_order,
        autofocus_first,
        autofocus_on_structure_change,
        appearance_reconcile,
        theme_store_stamp,
        last_theme_store_check,
        theme_schedule: None,
        input,
        focus_history,
        collaboration,
        native_room_route,
        presence_ctx,
        permission_ctx,
        presence,
        accessibility,
        window,
        committed_documents,
        persistence,
        autosave_generation,
        document_routes,
        actions,
        document_history,
        history_chrome_enabled,
        native_document_chrome,
        history_layout,
        history_viewport_width,
        history_focus_pending,
        continuation,
        action_publication_failure,
        last_published_playback,
        last_playback_publish,
        profiler,
        native_input_probe,
        floating,
    };

    app.accessibility.window.your_colour = (inputs.your_colour)();
    app.accessibility.a11y.high_contrast = (inputs.high_contrast)();
    app.accessibility.a11y.reduce_motion = match app.palette_source {
        PaletteSource::Host => app.accessibility.base_theme.reduce_motion,
        PaletteSource::Owned { .. } => (inputs.reduce_motion)(),
    };
    app.accessibility.a11y.screen_reader = app.accessibility.screen_reader.is_some();
    app.accessibility.text_scale = match app.palette_source {
        PaletteSource::Host => app.text_renderer.text_scale(),
        PaletteSource::Owned { .. } => (inputs.text_scale)(),
    };
    app.accessibility.ui_scale = match app.palette_source {
        PaletteSource::Host => app.accessibility.base_theme.ui_scale,
        PaletteSource::Owned { .. } => (inputs.ui_scale)(),
    };
    app.rebuild_theme();

    // The scope is also what sends `Arrived`, so an app that never set
    // one would never catch up on anything written before it opened.
    // No room means no scope.
    if let Some(scope) = (inputs.startup_presence_scope)() {
        app.set_presence_broadcast_scope(Some(scope));
    }
    app.presence_ctx.set_surface_id(inputs.surface_id.resolve());
    // Apply launcher-provided continuation before the first widget sync or
    // redraw.  Waiting for an input event left a restored app blank on a
    // quiet freshly-created window.
    app.apply_pending_continuation_restore();
    // The first report is level-triggered.  Sending it here makes a
    // launched app visible to continuity capture even if it receives no
    // input events before the next peer asks for a checkpoint.
    app.publish_continuation_metadata();
    app.publish_playback_handoff();
    app
}
