//! The one widget every framework app's root is: [`RootWidget`] holds the
//! resource's root as a [`RootModel`] and forwards the whole widget contract
//! through it. Only the root itself is generic over the resource; the widget,
//! with its every default method, is compiled once for all of them.

use std::collections::HashSet;

use ui_core::application_view::{ApplicationFocusRestore, ApplicationLocalView};
use ui_core::automerge::AutoCommit;
use ui_core::host_operation::{ResourceOperation, ResourceOperationStart, ResourceOperations};
use ui_core::resource::ResourceSaveStatus;
use ui_core::resources::PublishesResources;
use ui_core::search::capability::SearchSource;
use ui_core::view::ThawError;
use ui_core::widget::capability::{Request, RequestMut};
use ui_core::widget::controls::{ActionControls, AttributedControlInvocation};
use ui_core::widget::decorator::Decorated;
use ui_core::widget::empty_state::PaintedEmptyStates;
use ui_core::widget::{AppKind, AppModel, AppWidget, Decorator, IconDescriptor, ModelPath, Widget};

/// What the root widget asks of the root of one resource type: the view it
/// shows, the widget methods the root answers itself, and the capabilities it
/// provides.
pub(crate) trait RootModel:
    RootApp + RootPublishes + RootSearch + RootControls + 'static
{
    /// The widget shown: the view, or why the record does not read.
    fn view(&self) -> &(dyn Widget + 'static);
    fn view_mut(&mut self) -> &mut (dyn Widget + 'static);
    /// Whether the window is cleared to the theme's content colour behind
    /// what the view paints. A root that stands on nothing answers `false`.
    fn paints_ground(&self) -> bool {
        true
    }
    /// The root itself, for a test or host that reaches it by type.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
    fn provide<'a>(&'a self, request: &mut Request<'a, '_>);
    fn provide_mut<'a>(&'a mut self, request: &mut RequestMut<'a, '_>);
    fn take_structure_dirty(&mut self) -> bool;
    fn take_structure_change(&mut self) -> ui_core::widget::StructureChange;
    fn view_only_scroll(&self, event: &ui_core::winit::event::WindowEvent) -> bool;
    fn resource_owners(&self) -> Vec<ui_core::history_mode::HistoryOwner>;
    fn sync(&mut self, state: &AppModel, path: &ModelPath);
    fn settle(&mut self, edits: &mut ui_core::widget::Edits<'_>, path: &ui_core::automerge::ObjId);
    fn draw(&mut self, ctx: &mut ui_core::draw::DrawCtx<'_>);
    fn needs_redraw(&self) -> bool;
    /// The window's save status the root says in the window's bar.
    fn save_indicator(&self) -> &crate::framework::saves::SaveIndicator;
    /// When the window stops waiting for the documents its app declared, unless they open
    /// first: nothing else wakes a window whose workspace sends nothing meanwhile.
    fn gives_up_at(&self) -> Option<ui_core::time::Instant>;
    /// The save status, for a test to move its clock.
    #[cfg(test)]
    fn save_indicator_mut(&mut self) -> &mut crate::framework::saves::SaveIndicator;
    fn resource_operation_handlers(&self) -> HashSet<String>;
    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        doc: &AutoCommit,
        path: &ui_core::automerge::ObjId,
    ) -> Option<tilo_actions::InvocationUpdate>;
    fn start_operation(
        &mut self,
        op: &ui_core::host_operation::ResourceOperation<'_>,
        doc: &mut ui_core::widget::Edits<'_>,
        path: &ui_core::automerge::ObjId,
    ) -> Result<Option<ui_core::host_operation::ResourceOperationStart>, String>;

    /// [`Self::start_operation`], keeping the refusal the app gave: an
    /// action's answer that names a code (a message the mailbox holds
    /// already) reaches whoever asked as that code
    /// ([`tilo_resource::Refusal::coded`]).
    fn start_operation_refused(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &mut ui_core::widget::Edits<'_>,
        path: &ui_core::automerge::ObjId,
    ) -> Result<Option<ResourceOperationStart>, tilo_resource::Refusal>;
}

/// The [`AppWidget`] part of a root.
pub(crate) trait RootApp {
    fn app_kind(&self) -> AppKind;
    fn app_title(&self) -> &str;
    fn app_icon(&self) -> IconDescriptor;
    fn device_shell_operation(
        &mut self,
        operation: &ui_core::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String>;
    fn request_device_shell(&mut self) -> bool;
    fn take_process_intent(&mut self) -> Option<ui_core::widget::AppProcessIntent>;
    fn take_save_retry(&mut self) -> bool;
    fn resource_save_status(&mut self, status: ResourceSaveStatus) -> bool;
    fn record_refused(&mut self, refusal: &ui_core::resource::ResourceRefusal);
    fn take_notices(&mut self) -> Vec<String>;
    fn record_said(&mut self, text: &str);
    fn capture_application_view(&self) -> Result<Option<ApplicationLocalView>, ThawError>;
    fn restore_application_view(
        &mut self,
        local: &ApplicationLocalView,
        doc: &AutoCommit,
    ) -> Result<ApplicationFocusRestore, ThawError>;
    fn finish_application_view_restore(&mut self) -> Result<(), ThawError>;
}

/// The [`PublishesResources`] part of a root.
pub(crate) trait RootPublishes {
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::ResourceSpec>;
    fn shared_resources_for(
        &self,
        presence: Option<&tilo_actions::PresenceId>,
        doc: &AutoCommit,
    ) -> Option<Vec<ui_core::published::SharedResource>>;
    fn resources_for(
        &self,
        presence: Option<&tilo_actions::PresenceId>,
        doc: &AutoCommit,
    ) -> Vec<tilo_actions::ResourceSpec>;
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::ResourceSpec>;
    fn resources_past_page(&self, doc: &AutoCommit) -> Vec<tilo_actions::ResourceSpec>;
    /// How many items stand past the published pages of the record's
    /// collections: what [`Self::resources_past_page`] lists, counted without
    /// building them.
    fn past_page_len(&self, doc: &AutoCommit) -> usize {
        self.resources_past_page(doc).len()
    }
    /// The items past the published pages as their id and label, read
    /// without building a resource for each: what a name lookup scans.
    fn past_page_labels(&self, doc: &AutoCommit) -> Vec<(String, String)> {
        self.past_page_range(doc, 0, usize::MAX)
            .into_iter()
            .map(|resource| (resource.id.as_str().to_owned(), resource.label.clone()))
            .collect()
    }
    /// The items past the published pages from `offset` on, at most `take`,
    /// in [`Self::resources_past_page`]'s order and built alone, so a page pays
    /// its window and not every item a record holds.
    fn past_page_range(
        &self,
        doc: &AutoCommit,
        offset: usize,
        take: usize,
    ) -> Vec<tilo_actions::ResourceSpec> {
        self.resources_past_page(doc)
            .into_iter()
            .skip(offset)
            .take(take)
            .collect()
    }
    fn resource_pending(&self, id: &str) -> bool;
    fn resource_for(
        &self,
        presence: Option<&tilo_actions::PresenceId>,
        doc: &AutoCommit,
        id: &str,
    ) -> Option<tilo_actions::ResourceSpec>;
    /// An action of the resource `id` as this root publishes it now
    /// ([`ui_core::resources::PublishesResources::published_action`]), built from the surfaces' own
    /// statuses rather than a publication.
    fn published_action(
        &self,
        id: &str,
        spec: tilo_actions::ActionSpec,
    ) -> tilo_actions::ActionSpec;
    fn document_action_result(
        &self,
        doc: &AutoCommit,
        address: &tilo_actions::ActionAddress,
        changes: &[ui_core::automerge::Patch],
    ) -> Option<serde_json::Value>;
}

/// The [`SearchSource`] part of a root.
pub(crate) trait RootSearch {
    fn search_providers(&self) -> Vec<Box<dyn ui_core::search::SearchProvider>>;
    fn search_provider_descriptors(
        &self,
    ) -> Vec<ui_core::search::capability::SearchProviderDescriptor>;
    fn search_index_state(&self) -> Option<ui_core::search::index_publication::IndexState>;
    fn search_index_entries(&self) -> Vec<ui_core::search::index_publication::IndexEntry>;
    fn activate_search_result(
        &mut self,
        action_id: &str,
        doc: &mut ui_core::widget::Edits<'_>,
        path: &ModelPath,
    );
}

/// The [`ActionControls`] part of a root: the controls its dispatcher
/// queued, ahead of its view's.
pub(crate) trait RootControls {
    fn attach_controls(
        &mut self,
        domain: &ui_core::ControlDomain,
        attachment: ui_core::control_domain::DomainAttachment,
    );
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation>;
    fn take_attributed_control_invocation(&mut self) -> Option<AttributedControlInvocation>;
    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &Result<ui_core::host_operation::ResourceOperationStart, tilo_actions::ApiError>,
    );
    fn has_pending_control_invocation(&self) -> bool;
}

/// The root widget of a framework app, and the empty states its view
/// painted itself, described for it.
pub(crate) struct RootWidget(Box<dyn RootModel>, PaintedEmptyStates);

impl RootWidget {
    pub(crate) fn new(root: Box<dyn RootModel>) -> Self {
        Self(root, PaintedEmptyStates::default())
    }

    /// The root widget a host mounts. Made here, not where the resource's
    /// type is known, so its widget table is compiled once for every app.
    pub(crate) fn boxed(root: Box<dyn RootModel>) -> Box<dyn AppWidget> {
        Box::new(Self::new(root))
    }
}

impl Decorator for RootWidget {
    type Inner = dyn Widget;

    // The capabilities whose traits extend `Widget` are the widget's own,
    // answered through the root; the rest are the root's.
    fn provide<'a>(&'a self, request: &mut Request<'a, '_>) {
        if request.wants::<dyn PublishesResources>() {
            request.provide::<dyn PublishesResources>(self);
        } else if request.wants::<dyn SearchSource>() {
            request.provide::<dyn SearchSource>(self);
        } else if request.wants::<dyn ActionControls>() {
            request.provide::<dyn ActionControls>(self);
        } else if request.wants::<dyn Decorated>() {
            // The root stands for its view, which is no child of it: a control
            // inventory walking widgets by their children asks for it here.
            request.provide::<dyn Decorated>(self);
        } else if request.wants::<dyn ResourceOperations>() {
            request.provide::<dyn ResourceOperations>(self);
        } else {
            self.0.provide(request);
        }
    }

    fn provide_mut<'a>(&'a mut self, request: &mut RequestMut<'a, '_>) {
        if request.wants::<dyn PublishesResources>() {
            request.provide::<dyn PublishesResources>(self);
        } else if request.wants::<dyn SearchSource>() {
            request.provide::<dyn SearchSource>(self);
        } else if request.wants::<dyn ActionControls>() {
            request.provide::<dyn ActionControls>(self);
        } else if request.wants::<dyn Decorated>() {
            request.provide::<dyn Decorated>(self);
        } else if request.wants::<dyn ResourceOperations>() {
            request.provide::<dyn ResourceOperations>(self);
        } else {
            self.0.provide_mut(request);
        }
    }
    fn inner(&self) -> &(dyn Widget + 'static) {
        self.0.view()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.0.view_mut()
    }

    fn paints_ground(&self) -> bool {
        self.0.paints_ground()
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.0.take_structure_dirty()
    }

    fn take_structure_change(&mut self) -> ui_core::widget::StructureChange {
        self.0.take_structure_change()
    }

    fn view_only_scroll(&self, event: &ui_core::winit::event::WindowEvent) -> bool {
        self.0.view_only_scroll(event)
    }

    fn resource_owners(&self) -> Vec<ui_core::history_mode::HistoryOwner> {
        self.0.resource_owners()
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.0.sync(state, path);
    }

    fn settle(&mut self, edits: &mut ui_core::widget::Edits<'_>, path: &ui_core::automerge::ObjId) {
        self.0.settle(edits, path);
    }

    fn draw(&mut self, ctx: &mut ui_core::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        let root = &mut self.0;
        self.1.draw(transform, || {
            root.draw(&mut ui_core::DrawCtx::new(
                scene, transform, text, theme, env,
            ));
        });
    }

    fn needs_redraw(&self) -> bool {
        self.0.needs_redraw()
    }

    fn collect_damage(&self, regions: &mut Vec<ui_core::vello::kurbo::Rect>) {
        self.0.view().collect_damage(regions);
    }

    fn mark_drawn(&mut self) {
        self.0.view_mut().mark_drawn();
    }

    fn has_transient_feedback(&self) -> bool {
        self.0.view().has_transient_feedback()
    }

    // The save status's deadline is the sync's, and so is the end of the window's wait for
    // its declared documents; the view serves its own.
    fn serve_deadline(&mut self, now: ui_core::time::Instant) -> bool {
        if self
            .0
            .save_indicator()
            .next_wake_deadline()
            .into_iter()
            .chain(self.0.gives_up_at())
            .any(|due| due <= now)
        {
            return true;
        }
        self.0.view_mut().serve_deadline(now)
    }

    fn next_wake_deadline(&self) -> Option<ui_core::time::Instant> {
        self.0
            .view()
            .next_wake_deadline()
            .into_iter()
            .chain(self.0.save_indicator().next_wake_deadline())
            .chain(self.0.gives_up_at())
            .min()
    }

    fn handle_event(
        &mut self,
        event: &ui_core::winit::event::WindowEvent,
        ctx: &mut ui_core::widget::EventCtx<'_>,
    ) {
        self.0.view_mut().handle_event(event, ctx);
    }

    // An empty state the view draws as pixels (a surface's, a widget's
    // own paint) is described as well, its action a button that clicks
    // where it is drawn: often the only way on from a first-use screen, for
    // a screen reader as much as for anyone.
    fn accessibility(&self) -> Option<ui_core::a11y::AccessNode> {
        let view = self.0.view();
        self.1.describe(view.accessibility(), view)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<ui_core::vello::kurbo::Rect> {
        // Asked once per node without bounds of its own while the tree is
        // built, so describing the whole view here would make a build
        // quadratic in its nodes. With no empty state painted there is
        // nothing of the root's own to place.
        if self.1.is_empty() {
            return self.0.view().synthetic_accessibility_rect(path);
        }
        Decorator::accessibility(self)
            .and_then(|described| self.1.action_rect(&described, path))
            .or_else(|| self.0.view().synthetic_accessibility_rect(path))
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui_core::a11y::Action,
        state: &mut ui_core::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.0
            .view_mut()
            .perform_semantic_action(target, action, state, path)
    }

    fn name(&self) -> &'static str {
        "ResourceApp"
    }

    // A screenshot harness reaches the root to stage a scenario's view state
    // (`framework::stage`).
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self.0.as_any_mut())
    }
}

impl ResourceOperations for RootWidget {
    fn resource_operation_handlers(&self) -> HashSet<String> {
        self.0.resource_operation_handlers()
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        doc: &AutoCommit,
        path: &ui_core::automerge::ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        self.0
            .poll_resource_operation(invocation, cancellation_requested, doc, path)
    }

    fn start_operation(
        &mut self,
        op: &ui_core::host_operation::ResourceOperation<'_>,
        doc: &mut ui_core::widget::Edits<'_>,
        path: &ui_core::automerge::ObjId,
    ) -> Result<Option<ui_core::host_operation::ResourceOperationStart>, String> {
        self.0.start_operation(op, doc, path)
    }

    fn start_operation_refused(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &mut ui_core::widget::Edits<'_>,
        path: &ui_core::automerge::ObjId,
    ) -> Result<Option<ResourceOperationStart>, tilo_resource::Refusal> {
        self.0.start_operation_refused(op, doc, path)
    }
}

impl AppWidget for RootWidget {
    fn app_kind(&self) -> AppKind {
        self.0.app_kind()
    }

    fn app_title(&self) -> &str {
        self.0.app_title()
    }

    fn app_icon(&self) -> IconDescriptor {
        self.0.app_icon()
    }

    fn device_shell_operation(
        &mut self,
        operation: &ui_core::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String> {
        self.0.device_shell_operation(operation)
    }

    fn request_device_shell(&mut self) -> bool {
        self.0.request_device_shell()
    }

    fn take_process_intent(&mut self) -> Option<ui_core::widget::AppProcessIntent> {
        self.0.take_process_intent()
    }

    fn take_save_retry(&mut self) -> bool {
        self.0.take_save_retry()
    }

    /// Every Tilo app has a compact layout that works at phone width, and a
    /// person resizing its window is stopped there rather than squeezing it:
    /// at what the app's layout states it needs
    /// ([`ui_core::widget::minimum_window_width`], a `NavShell`'s compact
    /// layout), never below phone width.
    fn min_window_width(&self) -> Option<f64> {
        let phone = f64::from(ui_core::tokens::Metrics::BASE.window_min_width);
        Some(ui_core::widget::minimum_window_width(self).map_or(phone, |width| width.max(phone)))
    }

    fn resource_save_status(&mut self, status: ResourceSaveStatus) -> bool {
        self.0.resource_save_status(status)
    }

    fn record_refused(&mut self, refusal: &ui_core::resource::ResourceRefusal) {
        self.0.record_refused(refusal);
    }

    fn take_notices(&mut self) -> Vec<String> {
        self.0.take_notices()
    }

    fn record_said(&mut self, text: &str) {
        self.0.record_said(text);
    }

    fn capture_application_view(&self) -> Result<Option<ApplicationLocalView>, ThawError> {
        self.0.capture_application_view()
    }

    fn restore_application_view(
        &mut self,
        local: &ApplicationLocalView,
        doc: &AutoCommit,
    ) -> Result<ApplicationFocusRestore, ThawError> {
        self.0.restore_application_view(local, doc)
    }

    fn finish_application_view_restore(&mut self) -> Result<(), ThawError> {
        self.0.finish_application_view_restore()
    }
}

impl PublishesResources for RootWidget {
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        self.0.resources(doc)
    }

    fn shared_resources_for(
        &self,
        presence: Option<&tilo_actions::PresenceId>,
        doc: &AutoCommit,
    ) -> Option<Vec<ui_core::published::SharedResource>> {
        self.0.shared_resources_for(presence, doc)
    }

    fn resources_for(
        &self,
        presence: Option<&tilo_actions::PresenceId>,
        doc: &AutoCommit,
    ) -> Vec<tilo_actions::ResourceSpec> {
        self.0.resources_for(presence, doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::ResourceSpec> {
        self.0.resource(doc, id)
    }

    fn resources_past_page(&self, doc: &AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        self.0.resources_past_page(doc)
    }

    fn past_page_len(&self, doc: &AutoCommit) -> usize {
        self.0.past_page_len(doc)
    }

    fn past_page_labels(&self, doc: &AutoCommit) -> Vec<(String, String)> {
        self.0.past_page_labels(doc)
    }

    fn past_page_range(
        &self,
        doc: &AutoCommit,
        offset: usize,
        take: usize,
    ) -> Vec<tilo_actions::ResourceSpec> {
        self.0.past_page_range(doc, offset, take)
    }

    fn resource_pending(&self, id: &str) -> bool {
        self.0.resource_pending(id)
    }

    fn resource_for(
        &self,
        presence: Option<&tilo_actions::PresenceId>,
        doc: &AutoCommit,
        id: &str,
    ) -> Option<tilo_actions::ResourceSpec> {
        self.0.resource_for(presence, doc, id)
    }

    fn published_action(
        &self,
        id: &str,
        spec: tilo_actions::ActionSpec,
    ) -> tilo_actions::ActionSpec {
        self.0.published_action(id, spec)
    }

    fn document_action_result(
        &self,
        doc: &AutoCommit,
        address: &tilo_actions::ActionAddress,
        changes: &[ui_core::automerge::Patch],
    ) -> Option<serde_json::Value> {
        self.0.document_action_result(doc, address, changes)
    }
}

impl SearchSource for RootWidget {
    fn search_providers(&self) -> Vec<Box<dyn ui_core::search::SearchProvider>> {
        self.0.search_providers()
    }

    fn search_provider_descriptors(
        &self,
    ) -> Vec<ui_core::search::capability::SearchProviderDescriptor> {
        self.0.search_provider_descriptors()
    }

    fn search_index_state(&self) -> Option<ui_core::search::index_publication::IndexState> {
        self.0.search_index_state()
    }

    fn search_index_entries(&self) -> Vec<ui_core::search::index_publication::IndexEntry> {
        self.0.search_index_entries()
    }

    fn activate_search_result(
        &mut self,
        action_id: &str,
        doc: &mut ui_core::widget::Edits<'_>,
        path: &ModelPath,
    ) {
        self.0.activate_search_result(action_id, doc, path);
    }
}

impl ActionControls for RootWidget {
    /// The root keeps its view's controls for itself when the session keeps
    /// its record: a press there is submitted over the app's connection, and
    /// a host's local document API never runs it
    /// (`docs/designs/app-language/one-app-model.md` §4).
    fn attach_controls(
        &mut self,
        domain: &ui_core::ControlDomain,
        attachment: ui_core::control_domain::DomainAttachment,
    ) {
        self.0.attach_controls(domain, attachment);
    }

    fn action_control_bindings(&self) -> Vec<ui_core::widget::ControlBindingMetadata> {
        ui_core::widget::controls::children_action_control_bindings(self)
    }

    fn has_action_control_bindings(&self) -> bool {
        ui_core::widget::controls::children_have_action_control_bindings(self)
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.0.take_control_invocation()
    }

    fn take_attributed_control_invocation(&mut self) -> Option<AttributedControlInvocation> {
        self.0.take_attributed_control_invocation()
    }

    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &Result<ui_core::host_operation::ResourceOperationStart, tilo_actions::ApiError>,
    ) {
        self.0.on_control_invocation_result(address, result);
    }

    fn has_pending_control_invocation(&self) -> bool {
        self.0.has_pending_control_invocation()
    }
}
