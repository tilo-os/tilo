//! The one widget every framework app's root is: [`RootWidget`] holds the
//! resource's root as a [`RootModel`] and forwards the whole widget contract
//! through it. Only the root itself is generic over the resource; the widget,
//! with its every default method, is compiled once for all of them.

use std::collections::HashSet;

use ui_core::Theme;
use ui_core::application_view::{ApplicationFocusRestore, ApplicationLocalView};
use ui_core::automerge::AutoCommit;
use ui_core::presence::PresenceCtx;
use ui_core::record::RecordSaveStatus;
use ui_core::resources::PublishesResources;
use ui_core::search::capability::SearchSource;
use ui_core::text_render::TextRenderer;
use ui_core::vello::Scene;
use ui_core::vello::kurbo::Affine;
use ui_core::view::ThawError;
use ui_core::widget::capability::{Request, RequestMut};
use ui_core::widget::controls::{ActionControls, AttributedControlInvocation};
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
    /// The root itself, for a test or host that reaches it by type.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
    fn provide<'a>(&'a self, request: &mut Request<'a, '_>);
    fn provide_mut<'a>(&'a mut self, request: &mut RequestMut<'a, '_>);
    fn take_structure_dirty(&mut self) -> bool;
    fn take_structure_change(&mut self) -> ui_core::widget::StructureChange;
    fn view_only_scroll(&self, event: &ui_core::winit::event::WindowEvent) -> bool;
    fn record_owners(&self) -> Vec<ui_core::history_mode::HistoryOwner>;
    fn sync(&mut self, state: &AppModel, path: &ModelPath);
    fn settle(&mut self, edits: &mut ui_core::widget::Edits<'_>, path: &ui_core::automerge::ObjId);
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    );
    fn needs_redraw(&self) -> bool;
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
}

/// The [`AppWidget`] part of a root.
pub(crate) trait RootApp {
    fn app_kind(&self) -> AppKind;
    fn app_title(&self) -> &str;
    fn app_icon(&self) -> IconDescriptor;
    fn permission_hint(&self) -> Option<AppKind>;
    fn device_shell_operation(
        &mut self,
        operation: &ui_core::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String>;
    fn request_device_shell(&mut self) -> bool;
    fn take_host_intent(&mut self) -> Option<ui_core::widget::AppHostIntent>;
    fn take_save_retry(&mut self) -> bool;
    fn record_save_status(&mut self, status: RecordSaveStatus);
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
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource>;
    fn shared_resources_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
    ) -> Option<Vec<ui_core::published::SharedResource>>;
    fn resources_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
    ) -> Vec<tilo_actions::Resource>;
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource>;
    fn resource_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
        id: &str,
    ) -> Option<tilo_actions::Resource>;
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
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation>;
    fn take_attributed_control_invocation(&mut self) -> Option<AttributedControlInvocation>;
    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &Result<ui_core::host_operation::ResourceOperationStart, tilo_actions::DoorError>,
    );
    fn has_pending_control_invocation(&self) -> bool;
}

/// The root widget of a framework app.
pub(crate) struct RootWidget(Box<dyn RootModel>);

impl RootWidget {
    pub(crate) fn new(root: Box<dyn RootModel>) -> Self {
        Self(root)
    }

    /// The root widget a host mounts. Made here, not where the resource's
    /// type is known, so its widget table is compiled once for every app.
    pub(crate) fn boxed(root: Box<dyn RootModel>) -> Box<dyn AppWidget> {
        Box::new(Self(root))
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

    fn take_structure_dirty(&mut self) -> bool {
        self.0.take_structure_dirty()
    }

    fn take_structure_change(&mut self) -> ui_core::widget::StructureChange {
        self.0.take_structure_change()
    }

    fn view_only_scroll(&self, event: &ui_core::winit::event::WindowEvent) -> bool {
        self.0.view_only_scroll(event)
    }

    fn record_owners(&self) -> Vec<ui_core::history_mode::HistoryOwner> {
        self.0.record_owners()
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.0.sync(state, path);
    }

    fn settle(&mut self, edits: &mut ui_core::widget::Edits<'_>, path: &ui_core::automerge::ObjId) {
        self.0.settle(edits, path);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.0.draw(scene, transform, text, theme, presence);
    }

    fn needs_redraw(&self) -> bool {
        self.0.needs_redraw()
    }

    fn name(&self) -> &'static str {
        "ResourceApp"
    }

    // A screenshot harness reaches the root to stage a scenario's view state
    // (`framework::stage`).
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self.0.as_any_mut())
    }

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

    fn permission_hint(&self) -> Option<AppKind> {
        self.0.permission_hint()
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

    fn take_host_intent(&mut self) -> Option<ui_core::widget::AppHostIntent> {
        self.0.take_host_intent()
    }

    fn take_save_retry(&mut self) -> bool {
        self.0.take_save_retry()
    }

    fn record_save_status(&mut self, status: RecordSaveStatus) {
        self.0.record_save_status(status);
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
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        self.0.resources(doc)
    }

    fn shared_resources_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
    ) -> Option<Vec<ui_core::published::SharedResource>> {
        self.0.shared_resources_for(participant, doc)
    }

    fn resources_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
    ) -> Vec<tilo_actions::Resource> {
        self.0.resources_for(participant, doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        self.0.resource(doc, id)
    }

    fn resource_for(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
        id: &str,
    ) -> Option<tilo_actions::Resource> {
        self.0.resource_for(participant, doc, id)
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
    fn action_control_bindings(&self) -> Vec<ui_core::widget::ControlBindingMetadata> {
        let mut bindings = Vec::new();
        Widget::for_each_child(self, &mut |child| {
            bindings.extend(ui_core::widget::controls::action_control_bindings(child));
        });
        bindings
    }

    fn has_action_control_bindings(&self) -> bool {
        // The default inventory contributes semantic bindings for this wrapper
        // and action bindings from its exposed children. Preserve that scope
        // while avoiding an allocation of the complete accessibility tree.
        let mut found = false;
        Widget::for_each_child(self, &mut |child| {
            if !found {
                found = ui_core::widget::controls::has_action_control_bindings(child);
            }
        });
        found
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
        result: &Result<ui_core::host_operation::ResourceOperationStart, tilo_actions::DoorError>,
    ) {
        self.0.on_control_invocation_result(address, result);
    }

    fn has_pending_control_invocation(&self) -> bool {
        self.0.has_pending_control_invocation()
    }
}
