//! How many bytes a queued item costs: the meter queue admission charges,
//! and one measuring function per kind of command, request and event the
//! bridge carries. `None` throughout means a size computation overflowed,
//! which queue admission treats exactly like durable overload.

use super::super::handoff::HandoffRequest;
use super::DocumentMutation;
use super::{
    BrokerActionSurface, CollabEvent, CollabRequest, Collaborator, ExportedAction, ExportedInput,
    PresencePayload, SdkCommand, SignedOp,
};

#[derive(Default)]
pub(crate) struct ByteMeter(Option<usize>);

impl ByteMeter {
    pub(super) fn add(&mut self, bytes: usize) {
        self.0 = self.0.and_then(|total| total.checked_add(bytes));
    }

    pub(super) fn add_product(&mut self, count: usize, bytes_each: usize) {
        self.0 = self.0.and_then(|total| {
            count
                .checked_mul(bytes_each)
                .and_then(|bytes| total.checked_add(bytes))
        });
    }

    pub(super) fn string_capacity(&mut self, value: &String) {
        self.add(value.capacity());
    }

    pub(super) fn bytes(&mut self, value: &Vec<u8>) {
        self.add(value.capacity());
    }

    pub(super) fn finish(self) -> Option<usize> {
        self.0
    }
}

fn json_bytes(value: &serde_json::Value, meter: &mut ByteMeter) {
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
        serde_json::Value::String(value) => meter.string_capacity(value),
        serde_json::Value::Array(values) => {
            meter.add_product(values.capacity(), std::mem::size_of::<serde_json::Value>());
            for value in values {
                json_bytes(value, meter);
            }
        }
        serde_json::Value::Object(values) => json_map_bytes(values, meter),
    }
}

fn json_map_bytes(values: &serde_json::Map<String, serde_json::Value>, meter: &mut ByteMeter) {
    // `serde_json::Map` is a BTreeMap unless preserve_order is
    // enabled. Charge a deliberately conservative node allowance so
    // either representation remains within this budget.
    meter.add_product(values.len(), 512);
    for (key, value) in values {
        meter.string_capacity(key);
        json_bytes(value, meter);
    }
}

/// Count a serializable opaque nested value: the length of its JSON text,
/// written into one reused buffer. Direct `String`/`Vec` owners still use
/// capacities above; this covers nested action/context representations
/// whose concrete shape is intentionally owned by `tilo-actions`. Writing
/// to a `Vec<u8>` reuses the serializer every value is already sent with,
/// instead of compiling a second one per type for a counting writer.
pub(super) fn serialized_bytes<T: serde::Serialize>(value: &T, meter: &mut ByteMeter) {
    match runtime_collab::wire_json_len(value) {
        usize::MAX => meter.0 = None,
        bytes => meter.add(bytes),
    }
}

fn exported_action_bytes(action: &ExportedAction, meter: &mut ByteMeter) {
    meter.string_capacity(&action.resource);
    if let Some(identity) = &action.semantic_identity {
        meter.string_capacity(&identity.kind);
        meter.string_capacity(&identity.key);
    }
    meter.string_capacity(&action.action);
    meter.string_capacity(&action.resource_kind);
    meter.string_capacity(&action.resource_label);
    meter.string_capacity(&action.action_label);
    if let Some(description) = &action.description {
        meter.string_capacity(description);
    }
    serialized_bytes(&action.params, meter);
    serialized_bytes(&action.contract, meter);
    if let ExportedInput::Text {
        prompt,
        placeholder,
        ..
    } = &action.input
    {
        meter.string_capacity(prompt);
        meter.string_capacity(placeholder);
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive request meter must account for every bounded queue variant without a permissive fallback"
)]
fn request_bytes(request: &CollabRequest) -> Option<usize> {
    let mut meter = ByteMeter(Some(std::mem::size_of::<CollabRequest>()));
    match request {
        CollabRequest::Hello {
            app_id,
            record,
            record_kind,
            handle,
            ..
        } => {
            meter.string_capacity(app_id);
            if let Some(record) = record {
                meter.string_capacity(record);
            }
            if let Some(kind) = record_kind {
                meter.string_capacity(kind);
            }
            if let Some(handle) = handle {
                meter.string_capacity(handle);
            }
        }
        CollabRequest::Document {
            mutation,
            authorization,
        } => {
            match mutation {
                DocumentMutation::Snapshot(data) | DocumentMutation::Incremental(data) => {
                    meter.bytes(data)
                }
            }
            if let Some(authorization) = authorization {
                meter.0 = meter
                    .0
                    .and_then(|bytes| bytes.checked_add(authorization.retained_bytes()?));
            }
        }
        CollabRequest::SignedOperation { envelope } => meter.bytes(&envelope.payload),
        CollabRequest::Presence { key, payload } => {
            serialized_bytes(key, &mut meter);
            presence_payload_bytes(payload, &mut meter);
        }
        CollabRequest::FetchScrollback { .. }
        | CollabRequest::OpenWorkspaceControl { .. }
        | CollabRequest::CloseWorkspaceControl { .. }
        | CollabRequest::RequestDocumentSnapshot => {}
        CollabRequest::LaunchInstalledApplication {
            app_id,
            package_root,
            ..
        } => {
            meter.string_capacity(app_id);
            if let Some(root) = package_root {
                meter.string_capacity(root);
            }
        }
        CollabRequest::RemoteAction { message } => serialized_bytes(message, &mut meter),
        CollabRequest::SelectedDiscovery { command, .. } => serialized_bytes(command, &mut meter),
        CollabRequest::OpenSelected { command, .. } => serialized_bytes(command, &mut meter),
        CollabRequest::OpenSelectedV2 { command, .. } => serialized_bytes(command, &mut meter),
        CollabRequest::LocalFileClipboard { command, .. } => serialized_bytes(command, &mut meter),
        CollabRequest::ExternalModeration { command, .. } => serialized_bytes(command, &mut meter),
        CollabRequest::OfferContent { hash, bytes } => {
            meter.string_capacity(hash);
            meter.bytes(bytes);
        }
        CollabRequest::WantContent { hash } => meter.string_capacity(hash),
        CollabRequest::PublishActions { app_id, actions } => {
            meter.string_capacity(app_id);
            meter.add_product(actions.capacity(), std::mem::size_of::<ExportedAction>());
            for action in actions {
                exported_action_bytes(action, &mut meter);
            }
        }
        CollabRequest::PublishActionSurface { app_id, delta } => {
            meter.string_capacity(app_id);
            meter.add_product(
                delta.shapes.capacity(),
                std::mem::size_of::<(runtime_collab::ShapeId, ExportedAction)>(),
            );
            for (_, shape) in &delta.shapes {
                exported_action_bytes(shape, &mut meter);
            }
            meter.add_product(
                delta.templates.capacity(),
                std::mem::size_of::<(runtime_collab::ShapeId, Vec<runtime_collab::ShapeId>)>(),
            );
            for (_, members) in &delta.templates {
                meter.add_product(members.capacity(), runtime_collab::SHAPE_ID_BYTES);
            }
            meter.add_product(
                delta.resources.capacity(),
                std::mem::size_of::<runtime_collab::SurfaceResource>(),
            );
            for resource in &delta.resources {
                meter.string_capacity(&resource.resource);
                meter.string_capacity(&resource.fields.resource_kind);
                meter.string_capacity(&resource.fields.resource_label);
                serialized_bytes(&resource.fields.semantic_identity, &mut meter);
            }
            meter.add_product(delta.removed.capacity(), std::mem::size_of::<String>());
            for resource in &delta.removed {
                meter.string_capacity(resource);
            }
        }
        CollabRequest::ReportContinuation {
            app_id, metadata, ..
        } => {
            meter.string_capacity(app_id);
            meter.bytes(metadata);
        }
        CollabRequest::ReportPlayback {
            app_id,
            reports_json,
        } => {
            meter.string_capacity(app_id);
            meter.bytes(reports_json);
        }
        CollabRequest::PublishSearchIndex { app_id, delta_json } => {
            meter.string_capacity(app_id);
            meter.bytes(delta_json);
        }
        CollabRequest::NativeMediaCommand { command, .. } => serialized_bytes(command, &mut meter),
        CollabRequest::RealtimeCommand {
            app_id, command, ..
        } => {
            meter.string_capacity(app_id);
            serialized_bytes(command, &mut meter);
        }
        CollabRequest::NearbyTransferCommand {
            app_id, command, ..
        } => {
            meter.string_capacity(app_id);
            serialized_bytes(command, &mut meter);
        }
        CollabRequest::ClearContinuation { app_id } => meter.string_capacity(app_id),
        CollabRequest::ReportHandoff { app_id, message } => {
            meter.string_capacity(app_id);
            meter.bytes(message);
        }
        CollabRequest::WorkspaceControlInput { event, .. } => serialized_bytes(event, &mut meter),
        CollabRequest::UpdateInvocation { update, .. } => serialized_bytes(update, &mut meter),
        CollabRequest::ReportLocalDocumentAction { report, .. } => {
            meter.string_capacity(&report.revision);
            meter.string_capacity(&report.resource);
            meter.string_capacity(&report.action);
            if let Some(argument) = &report.argument {
                meter.string_capacity(argument);
            }
            meter.string_capacity(&report.encoded.type_id);
            meter.bytes(&report.encoded.payload);
            serialized_bytes(&report.context, &mut meter);
            meter.add_product(report.original_heads.capacity(), 32);
            meter.add_product(report.mutation_hashes.capacity(), 32);
            if let Some(group) = &report.group {
                meter.string_capacity(&group.id);
                meter.string_capacity(&group.label);
                meter.string_capacity(&group.kind);
            }
        }
        CollabRequest::RecordLocalEffect { address, update } => {
            meter.string_capacity(&address.resource);
            meter.string_capacity(&address.action);
            serialized_bytes(update, &mut meter);
        }
        CollabRequest::ReportTaskRun { lease, update, .. } => {
            meter.string_capacity(lease);
            serialized_bytes(update, &mut meter);
        }
        CollabRequest::InvokeTaskAction {
            lease,
            resource,
            action,
            arguments,
            ..
        } => {
            meter.string_capacity(lease);
            meter.string_capacity(resource);
            meter.string_capacity(action);
            json_map_bytes(arguments, &mut meter);
        }
        CollabRequest::TaskRunCancellation { lease, .. } => meter.string_capacity(lease),
    }
    meter.finish()
}

pub(super) fn command_bytes(command: &SdkCommand) -> Option<usize> {
    match command {
        SdkCommand::BrowserServiceSession { route, command, .. } => {
            let mut meter = ByteMeter(Some(std::mem::size_of::<SdkCommand>()));
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            serialized_bytes(command, &mut meter);
            meter.finish()
        }

        SdkCommand::Initialize { route, request, .. } => {
            let mut meter = ByteMeter(Some(std::mem::size_of::<SdkCommand>()));
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            if let runtime_collab::committed_document::Request::Initialize {
                kind, document, ..
            } = request
            {
                meter.string_capacity(kind);
                meter.add(document.capacity());
            }
            meter.finish()
        }
        SdkCommand::Hello(hello) => {
            let mut meter = ByteMeter(Some(std::mem::size_of::<SdkCommand>()));
            meter.string_capacity(&hello.route.app_id);
            if let Some(record) = &hello.route.record {
                meter.string_capacity(record);
            }
            if let Some(kind) = &hello.record_kind {
                meter.string_capacity(kind);
            }
            if let Some(handle) = &hello.handle {
                meter.string_capacity(handle);
            }
            meter.finish()
        }
        SdkCommand::Primary(request) => request_bytes(request),
        SdkCommand::ToRoute(route, request) => {
            let mut meter = ByteMeter(request_bytes(request));
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            meter.finish()
        }
        SdkCommand::Join(route, record_kind) => {
            let mut meter = ByteMeter(Some(std::mem::size_of::<SdkCommand>()));
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            if let Some(kind) = record_kind {
                meter.string_capacity(kind);
            }
            meter.finish()
        }
        SdkCommand::PrimaryReady(_)
        | SdkCommand::PublicationBarrier(_)
        | SdkCommand::FlushSettings
        | SdkCommand::FlushLiveActivity => Some(std::mem::size_of::<SdkCommand>()),
        #[cfg(test)]
        SdkCommand::KeyScheduleProbe(_) => Some(std::mem::size_of::<SdkCommand>()),
        // The group was allocated by the caller before entering the bridge,
        // and installation is synchronous under `&mut self`, so at most one
        // is ever queued. Only the command itself is counted: reserving the
        // whole budget would fail closed whenever any other command was still
        // queued, or its reservation not yet released after the reply.
        SdkCommand::InstallEncryption { spec, .. } => {
            let mut meter = ByteMeter(Some(std::mem::size_of::<SdkCommand>()));
            meter.string_capacity(&spec.app_id);
            if let Some(record) = &spec.record {
                meter.string_capacity(record);
            }
            meter.finish()
        }
        SdkCommand::PublishActionDirectory { app_id, .. } => {
            std::mem::size_of::<SdkCommand>().checked_add(app_id.capacity())
        }
        SdkCommand::WifiContinuity {
            app_id, command, ..
        } => {
            let mut meter = ByteMeter(Some(std::mem::size_of::<SdkCommand>()));
            meter.string_capacity(app_id);
            serialized_bytes(command, &mut meter);
            meter.finish()
        }
        SdkCommand::DisplayControl { command, .. } => {
            std::mem::size_of::<SdkCommand>().checked_add(command.capacity())
        }
        SdkCommand::SubscribePersonChat { peer, original, .. } => std::mem::size_of::<SdkCommand>()
            .checked_add(peer.capacity())?
            .checked_add(original.capacity()),
        SdkCommand::PersonChat {
            command, original, ..
        } => std::mem::size_of::<SdkCommand>()
            .checked_add(command.capacity())?
            .checked_add(original.capacity()),
        SdkCommand::Speakers {
            app_id, command, ..
        } => {
            let mut meter = ByteMeter(Some(std::mem::size_of::<SdkCommand>()));
            meter.string_capacity(app_id);
            serialized_bytes(command, &mut meter);
            meter.finish()
        }
    }
}

fn presence_payload_bytes(payload: &PresencePayload, meter: &mut ByteMeter) {
    if let PresencePayload::Following {
        target: Some(target),
    } = payload
    {
        meter.string_capacity(target);
    }
    if let PresencePayload::Interaction(update) = payload {
        serialized_bytes(update.as_ref(), meter);
    }
    if let PresencePayload::Status { text, .. } = payload {
        meter.string_capacity(text);
    }
}

fn workspace_control_event_bytes(
    event: &runtime_collab::WorkspaceControlEvent,
    meter: &mut ByteMeter,
) {
    match event {
        runtime_collab::WorkspaceControlEvent::Frame { encoded_h264, .. } => {
            meter.bytes(encoded_h264);
        }
        runtime_collab::WorkspaceControlEvent::Closed(
            runtime_collab::WorkspaceControlCloseReason::Denied(message),
        ) => {
            meter.string_capacity(message);
        }
        _ => {}
    }
}

fn context_event_bytes(
    collaborators: &[Collaborator],
    collaborator_capacity: usize,
    permissions: &[String],
    permission_capacity: usize,
    meter: &mut ByteMeter,
) {
    meter.add_product(collaborator_capacity, std::mem::size_of::<Collaborator>());
    for collaborator in collaborators {
        meter.string_capacity(&collaborator.id);
        meter.string_capacity(&collaborator.name);
    }
    meter.add_product(permission_capacity, std::mem::size_of::<String>());
    for permission in permissions {
        meter.string_capacity(permission);
    }
}

/// Carry one request frame to the session's action host and decode its
/// answer. Every request is answered — a transport failure becomes the
/// refusal the request would have received — so a caller waiting on its
/// invocation id or correlation always hears back.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
pub(super) async fn remote_action_round_trip(
    channel: &runtime_collab::ChannelHandle,
    message: tilo_actions::remote::RemoteActionMessage,
) -> tilo_actions::remote::RemoteActionMessage {
    use tilo_actions::remote::RemoteActionFrame;

    let answer = async {
        let frame = RemoteActionFrame::new(message.clone())
            .encode()
            .map_err(|error| error.to_string())?;
        let reply = channel
            .remote_action(&frame)
            .await
            .map_err(|error| error.to_string())?;
        RemoteActionFrame::decode(&reply)
            .map(|frame| frame.message)
            .map_err(|error| error.to_string())
    }
    .await;
    answer.unwrap_or_else(|reason| {
        tilo_actions::remote::refuse(
            &message,
            tilo_actions::remote::ActionHostError::Unavailable { reason },
        )
        .unwrap_or(message)
    })
}

fn action_catalogue_bytes(
    surfaces: &[BrokerActionSurface],
    surface_capacity: usize,
    meter: &mut ByteMeter,
) {
    meter.add_product(surface_capacity, std::mem::size_of::<BrokerActionSurface>());
    for surface in surfaces {
        meter.string_capacity(&surface.app_id);
        meter.string_capacity(&surface.cluster_id);
        meter.string_capacity(&surface.record_id);
        meter.add_product(
            surface.actions.capacity(),
            std::mem::size_of::<ExportedAction>(),
        );
        for action in &surface.actions {
            exported_action_bytes(action, meter);
        }
    }
}

/// `surfaces` trimmed until their retained size fits `limit`, and the apps of
/// the surfaces dropped. Inert surfaces (an app only offered to open) go
/// before live ones, the last listed first, so the operations of the open
/// windows survive longest.
pub(super) fn fit_catalogue(
    mut surfaces: Vec<BrokerActionSurface>,
    limit: usize,
) -> (Vec<BrokerActionSurface>, Vec<String>) {
    let size = |surfaces: &[BrokerActionSurface]| {
        let mut meter = ByteMeter(Some(std::mem::size_of::<CollabEvent>()));
        action_catalogue_bytes(surfaces, surfaces.len(), &mut meter);
        meter.0.unwrap_or(usize::MAX)
    };
    // Measured as kept: a decoded list may hold spare capacity.
    for surface in &mut surfaces {
        surface.actions.shrink_to_fit();
    }
    let empty = size(&[]);
    let mut total = size(&surfaces);
    if total <= limit {
        return (surfaces, Vec::new());
    }
    let mut dropped = Vec::new();
    while total > limit {
        let Some(at) = surfaces
            .iter()
            .rposition(|surface| !surface.live)
            .or_else(|| surfaces.len().checked_sub(1))
        else {
            break;
        };
        let surface = surfaces.remove(at);
        total = total.saturating_sub(size(std::slice::from_ref(&surface)).saturating_sub(empty));
        dropped.push(surface.app_id);
    }
    surfaces.shrink_to_fit();
    (surfaces, dropped)
}

fn scrollback_batch_bytes(
    ops: &[Box<SignedOp>],
    op_capacity: usize,
    decrypted_payloads: &[Option<Vec<u8>>],
    payload_capacity: usize,
    meter: &mut ByteMeter,
) {
    meter.add_product(op_capacity, std::mem::size_of::<Box<SignedOp>>());
    for op in ops {
        meter.bytes(&op.payload);
    }
    meter.add_product(payload_capacity, std::mem::size_of::<Option<Vec<u8>>>());
    for payload in decrypted_payloads.iter().flatten() {
        meter.bytes(payload);
    }
}

fn task_run_bytes(run: &runtime_collab::TaskRunEnvelope, meter: &mut ByteMeter) {
    meter.string_capacity(&run.task_id);
    meter.string_capacity(&run.title);
    meter.string_capacity(&run.lease);
    json_bytes(&run.action_context, meter);
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive event meter must account for every bounded queue variant without a permissive fallback"
)]
pub(super) fn event_diagnostics(event: &CollabEvent) -> (&'static str, Option<usize>) {
    let mut meter = ByteMeter(Some(std::mem::size_of::<CollabEvent>()));
    let kind = match event {
        CollabEvent::CommittedDocument { route, .. } => {
            // Capture bytes retain their own process-wide preallocation permit.
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            "CommittedDocument"
        }
        CollabEvent::Context {
            collaborators,
            permissions,
        } => {
            context_event_bytes(
                collaborators,
                collaborators.capacity(),
                permissions,
                permissions.capacity(),
                &mut meter,
            );
            "Context"
        }
        CollabEvent::FollowChanged { peer_id } => {
            if let Some(peer_id) = peer_id {
                meter.string_capacity(peer_id);
            }
            "FollowChanged"
        }
        CollabEvent::Handoff { request } => {
            meter.add_product(1, std::mem::size_of_val(request));
            if let HandoffRequest::OwnerView { view } = request {
                meter.string_capacity(&view.room);
                meter.string_capacity(&view.app);
                meter.string_capacity(&view.source);
                if let Some(metadata) = &view.projection {
                    meter.add_product(metadata.bytes.capacity(), 1);
                }
            }
            if let HandoffRequest::Retire { successor, .. } = request {
                meter.string_capacity(&successor.room);
                meter.string_capacity(&successor.record);
                meter.string_capacity(&successor.by);
            }
            "Handoff"
        }
        CollabEvent::RecordsArrived { records } => {
            meter.add_product(records.capacity(), std::mem::size_of::<String>());
            for record in records {
                meter.string_capacity(record);
            }
            "RecordsArrived"
        }
        CollabEvent::RecordUnavailable { route } => {
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            "RecordUnavailable"
        }
        CollabEvent::DocumentSnapshotRequested { route } => {
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            "DocumentSnapshotRequested"
        }
        CollabEvent::ChannelAccess { route, .. } => {
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            "ChannelAccess"
        }
        CollabEvent::RemoteCursorMoved { user_id, .. } => {
            meter.string_capacity(user_id);
            "RemoteCursorMoved"
        }
        CollabEvent::RemotePresenceUpdate {
            user_id,
            key,
            payload,
        } => {
            meter.string_capacity(user_id);
            serialized_bytes(key, &mut meter);
            presence_payload_bytes(payload, &mut meter);
            "RemotePresenceUpdate"
        }
        CollabEvent::ActionArrived {
            type_id,
            payload,
            from_user_id,
        } => {
            meter.string_capacity(type_id);
            meter.bytes(payload);
            meter.string_capacity(from_user_id);
            "ActionArrived"
        }
        CollabEvent::Document {
            envelope,
            from_user_id,
            decrypted_payload,
        } => {
            meter.bytes(&envelope.payload);
            meter.string_capacity(from_user_id);
            if let Some(payload) = decrypted_payload {
                meter.bytes(payload);
            }
            "Document"
        }
        CollabEvent::ScrollbackBatch {
            ops,
            decrypted_payloads,
            ..
        } => {
            scrollback_batch_bytes(
                ops,
                ops.capacity(),
                decrypted_payloads,
                decrypted_payloads.capacity(),
                &mut meter,
            );
            "ScrollbackBatch"
        }
        CollabEvent::Invocation {
            resource,
            action,
            argument,
            actor,
            context,
            ..
        } => {
            meter.string_capacity(resource);
            meter.string_capacity(action);
            if let Some(argument) = argument {
                meter.string_capacity(argument);
            }
            meter.string_capacity(actor);
            if let Some(context) = context {
                serialized_bytes(context.as_ref(), &mut meter);
            }
            "Invocation"
        }
        CollabEvent::InvocationCancellation { .. } => "InvocationCancellation",
        CollabEvent::RemoteAction { message } => {
            serialized_bytes(message, &mut meter);
            "RemoteAction"
        }
        CollabEvent::ActionCatalogue { surfaces } => {
            action_catalogue_bytes(surfaces, surfaces.capacity(), &mut meter);
            "ActionCatalogue"
        }
        CollabEvent::LocalDocumentActionReported { result, .. } => {
            match result {
                Ok(observation) => {
                    if let Some(result) = &observation.result {
                        meter.string_capacity(&result.update_json);
                        if let Some(json) = &result.admission_json {
                            meter.string_capacity(json);
                        }
                        if let Some(json) = &result.terminal_json {
                            meter.string_capacity(json);
                        }
                    }
                }
                Err(
                    runtime_collab::LocalDocumentActionError::Rejected(error)
                    | runtime_collab::LocalDocumentActionError::Uncertain(error),
                ) => meter.string_capacity(error),
            }
            "LocalDocumentActionReported"
        }
        CollabEvent::InstalledApplicationLaunchResult { result, .. } => {
            match result {
                Ok((_, update)) => serialized_bytes(update, &mut meter),
                Err(error) => meter.string_capacity(error),
            }
            "InstalledApplicationLaunchResult"
        }
        CollabEvent::TaskActionInvocationResult { result, .. } => {
            match result {
                Ok((_, update)) => serialized_bytes(update, &mut meter),
                Err(error) => meter.string_capacity(error),
            }
            "TaskActionInvocationResult"
        }
        CollabEvent::TaskRun(run) => {
            task_run_bytes(run, &mut meter);
            "TaskRun"
        }
        CollabEvent::TaskRunCancellation { result, .. } => {
            if let Err(error) = result {
                meter.string_capacity(error);
            }
            "TaskRunCancellation"
        }
        CollabEvent::TaskRunReportResult { result, .. } => {
            if let Err(error) = result {
                meter.string_capacity(error);
            }
            "TaskRunReportResult"
        }
        CollabEvent::TaskRunControl { control, .. } => {
            serialized_bytes(control, &mut meter);
            "TaskRunControl"
        }
        CollabEvent::WorkspaceControl { event, .. } => {
            workspace_control_event_bytes(event, &mut meter);
            "WorkspaceControl"
        }
        CollabEvent::NativeMediaCommandResult { result, .. } => {
            match result {
                Ok(reply) => serialized_bytes(reply, &mut meter),
                Err(error) => meter.string_capacity(error),
            }
            "NativeMediaCommandResult"
        }
        CollabEvent::RecordContent { route, hash, bytes } => {
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            meter.string_capacity(hash);
            meter.bytes(bytes);
            "RecordContent"
        }
        CollabEvent::RecordContentState { route, hash, .. } => {
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            meter.string_capacity(hash);
            "RecordContentState"
        }
        CollabEvent::RealtimeCommandResult { result, .. } => {
            match result {
                Ok(reply) => serialized_bytes(reply, &mut meter),
                Err(error) => meter.string_capacity(error),
            }
            "RealtimeCommandResult"
        }
        CollabEvent::SelectedDiscoveryResult { result, .. } => {
            match result {
                Ok(value) => serialized_bytes(value, &mut meter),
                Err(error) => meter.string_capacity(error),
            }
            "SelectedDiscoveryResult"
        }
        CollabEvent::OpenSelectedV2Result { result, .. } => {
            match result {
                Ok(reply) => serialized_bytes(reply, &mut meter),
                Err(error) => meter.string_capacity(error),
            }
            "OpenSelectedV2Result"
        }
        CollabEvent::OpenSelectedResult { result, .. } => {
            match result {
                Ok(reply) => serialized_bytes(reply, &mut meter),
                Err(error) => meter.string_capacity(error),
            }
            "OpenSelectedResult"
        }
        CollabEvent::LocalFileClipboardResult { result, .. } => {
            match result {
                Ok(reply) => serialized_bytes(reply, &mut meter),
                Err(error) => meter.string_capacity(error),
            }
            "LocalFileClipboardResult"
        }
        CollabEvent::NearbyTransferCommandResult { result, .. } => {
            match result {
                Ok(reply) => serialized_bytes(reply, &mut meter),
                Err(error) => meter.string_capacity(error),
            }
            "NearbyTransferCommandResult"
        }
        CollabEvent::NearbyTransferChanged => "NearbyTransferChanged",
        CollabEvent::SpeakersChanged => "SpeakersChanged",
        CollabEvent::DrivesChanged => "DrivesChanged",
        CollabEvent::ApplicationsChanged => "ApplicationsChanged",
        CollabEvent::Realtime(event) => {
            serialized_bytes(event, &mut meter);
            "Realtime"
        }
        CollabEvent::HistoryAgents { cluster_id, agents } => {
            meter.string_capacity(cluster_id);
            meter.add_product(
                agents.capacity(),
                std::mem::size_of::<collab_types::history::RosterAgent>(),
            );
            for agent in agents {
                meter.string_capacity(&agent.person_name);
                meter.string_capacity(&agent.agent_id);
                meter.string_capacity(&agent.label);
            }
            "HistoryAgents"
        }
    };
    (kind, meter.finish())
}
