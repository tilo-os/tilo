//! How many bytes a queued item costs: the meter queue admission charges,
//! and one measuring function per kind of command, request and event the
//! bridge carries. `None` throughout means a size computation overflowed,
//! which queue admission treats exactly like durable overload.

use super::super::handoff::HandoffRequest;
use super::DocumentMutation;
use super::{CollabEvent, CollabRequest, Member, PresencePayload, SdkCommand};

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

    pub(super) fn member(&mut self, member: &tilo_actions::MemberId) {
        self.add(member.id().len());
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
            resource_type,
            handle,
            ..
        } => {
            meter.string_capacity(app_id);
            if let Some(record) = record {
                meter.string_capacity(record);
            }
            if let Some(kind) = resource_type {
                meter.string_capacity(kind);
            }
            if let Some(handle) = handle {
                meter.string_capacity(handle);
            }
        }
        CollabRequest::Document { mutation, .. } => match mutation {
            DocumentMutation::Snapshot(data) | DocumentMutation::Incremental(data) => {
                meter.bytes(data)
            }
        },
        CollabRequest::Presence { key, payload } => {
            serialized_bytes(key, &mut meter);
            presence_payload_bytes(payload, &mut meter);
        }
        CollabRequest::OpenWorkspaceControl { .. }
        | CollabRequest::CloseWorkspaceControl { .. }
        | CollabRequest::ClearContinuation => {}
        CollabRequest::RemoteAction { message } => serialized_bytes(message, &mut meter),
        CollabRequest::ExternalModeration { command, .. } => serialized_bytes(command, &mut meter),
        CollabRequest::OfferContent { hash, bytes } => {
            meter.string_capacity(hash);
            meter.bytes(bytes);
        }
        CollabRequest::WantContent { hash } => meter.string_capacity(hash),
        CollabRequest::PublishActionSurface { delta } => serialized_bytes(delta, &mut meter),
        CollabRequest::ReportContinuation { metadata, .. } => {
            meter.bytes(metadata);
        }
        CollabRequest::PublishSearchIndex { delta_json } => {
            meter.bytes(delta_json);
        }
        CollabRequest::NativeMediaCommand { command, .. } => serialized_bytes(command, &mut meter),
        CollabRequest::ResourceCommand { command, .. } => meter.bytes(command),
        CollabRequest::DecideShownActions { actions } => serialized_bytes(actions, &mut meter),
        CollabRequest::ReportHandoff { message } => {
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
                if let Some(label) = &group.label {
                    meter.string_capacity(label);
                }
                meter.string_capacity(&group.kind);
            }
        }
        CollabRequest::ResourceLocalEffect { address, update } => {
            meter.string_capacity(&address.resource);
            meter.string_capacity(&address.action);
            serialized_bytes(update, &mut meter);
        }
    }
    meter.finish()
}

pub(super) fn command_bytes(command: &SdkCommand) -> Option<usize> {
    match command {
        SdkCommand::BrowserServiceSession { route, command, .. } => {
            let mut meter = ByteMeter(Some(std::mem::size_of::<SdkCommand>()));
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.document {
                meter.string_capacity(record);
            }
            serialized_bytes(command, &mut meter);
            meter.finish()
        }

        SdkCommand::Hello(hello) => {
            let mut meter = ByteMeter(Some(std::mem::size_of::<SdkCommand>()));
            meter.string_capacity(&hello.route.app_id);
            if let Some(record) = &hello.route.document {
                meter.string_capacity(record);
            }
            if let Some(kind) = &hello.resource_type {
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
            if let Some(record) = &route.document {
                meter.string_capacity(record);
            }
            meter.finish()
        }
        SdkCommand::Join(route, resource_type) => {
            let mut meter = ByteMeter(Some(std::mem::size_of::<SdkCommand>()));
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.document {
                meter.string_capacity(record);
            }
            if let Some(kind) = resource_type {
                meter.string_capacity(kind);
            }
            meter.finish()
        }
        SdkCommand::PrimaryReady(_)
        | SdkCommand::PublicationBarrier(_)
        | SdkCommand::FlushSettings
        | SdkCommand::FlushLiveActivity
        | SdkCommand::ResumeInvocations => Some(std::mem::size_of::<SdkCommand>()),
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
            if let Some(record) = &spec.document {
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
        SdkCommand::Resources { route, command, .. } => std::mem::size_of::<SdkCommand>()
            .checked_add(route.app_id.capacity())?
            .checked_add(command.capacity()),
        SdkCommand::Published(command) => match command {
            super::PublishedCommand::Watch { resource, .. } => {
                std::mem::size_of::<SdkCommand>().checked_add(resource.len())
            }
            super::PublishedCommand::Invoke {
                resource,
                action,
                arguments,
                ..
            } => {
                let mut meter = ByteMeter(Some(std::mem::size_of::<SdkCommand>()));
                meter.add(resource.len());
                meter.string_capacity(action);
                json_map_bytes(arguments, &mut meter);
                meter.finish()
            }
        },
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
    members: &[Member],
    members_capacity: usize,
    permissions: &[String],
    permission_capacity: usize,
    apps: &[collab_types::history::NamedApp],
    meter: &mut ByteMeter,
) {
    meter.add_product(members_capacity, std::mem::size_of::<Member>());
    for member in members {
        meter.member(&member.id);
        meter.string_capacity(&member.name);
    }
    meter.add_product(permission_capacity, std::mem::size_of::<String>());
    for permission in permissions {
        meter.string_capacity(permission);
    }
    meter.add_product(
        apps.len(),
        std::mem::size_of::<collab_types::history::NamedApp>(),
    );
    for app in apps {
        meter.string_capacity(&app.id);
        meter.string_capacity(&app.name);
        for (tag, name) in &app.names {
            meter.string_capacity(tag);
            meter.string_capacity(name);
        }
    }
}

/// Carry one request frame to the session's action host and decode its
/// answer. Every request is answered — a transport failure becomes the
/// refusal the request would have received — so a caller waiting on its
/// invocation id or correlation always hears back.
#[allow(
    clippy::future_not_send,
    reason = "driven only on the shovel thread's LocalSet, whose route state is not Send"
)]
pub(super) async fn remote_action_round_trip(
    channel: &runtime_collab::RecordHandle,
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
            tilo_actions::remote::ActionCallError::Unavailable { reason },
        )
        .unwrap_or(message)
    })
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
            if let Some(record) = &route.document {
                meter.string_capacity(record);
            }
            "CommittedDocument"
        }
        CollabEvent::Context {
            members,
            permissions,
            apps,
        } => {
            context_event_bytes(
                members,
                members.capacity(),
                permissions,
                permissions.capacity(),
                apps,
                &mut meter,
            );
            "Context"
        }
        CollabEvent::FollowChanged { peer_id } => {
            if let Some(peer_id) = peer_id {
                meter.member(peer_id);
            }
            "FollowChanged"
        }
        CollabEvent::Handoff { request } => {
            meter.add_product(1, std::mem::size_of_val(request));
            if let HandoffRequest::OwnerView { view } = request {
                meter.string_capacity(&view.workspace);
                meter.string_capacity(&view.app);
                meter.string_capacity(&view.source);
                if let Some(metadata) = &view.projection {
                    meter.add_product(metadata.bytes.capacity(), 1);
                }
            }
            if let HandoffRequest::Retire { successor, .. } = request {
                meter.string_capacity(&successor.workspace);
                meter.string_capacity(&successor.document);
                meter.string_capacity(&successor.by);
            }
            "Handoff"
        }
        CollabEvent::ResourcesArrived { records } => {
            meter.add_product(records.capacity(), std::mem::size_of::<String>());
            for record in records {
                meter.string_capacity(record);
            }
            "ResourcesArrived"
        }
        CollabEvent::RecordsMoved { records } => {
            meter.add_product(records.capacity(), std::mem::size_of::<String>());
            for record in records {
                meter.string_capacity(record);
            }
            "RecordsMoved"
        }
        CollabEvent::ResourceUnavailable { route } => {
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.document {
                meter.string_capacity(record);
            }
            "ResourceUnavailable"
        }
        CollabEvent::KeyAccess { route, .. } => {
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.document {
                meter.string_capacity(record);
            }
            "KeyAccess"
        }
        CollabEvent::RemoteCursorMoved { member, .. } => {
            meter.member(member);
            "RemoteCursorMoved"
        }
        CollabEvent::RemotePresenceUpdate {
            member,
            key,
            payload,
        } => {
            meter.member(member);
            serialized_bytes(key, &mut meter);
            presence_payload_bytes(payload, &mut meter);
            "RemotePresenceUpdate"
        }
        CollabEvent::Document { route, mutation } => {
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.document {
                meter.string_capacity(record);
            }
            meter.add(mutation.data().len());
            "Document"
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
            serialized_bytes(actor, &mut meter);
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
        CollabEvent::DocumentContent { route, hash, bytes } => {
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.document {
                meter.string_capacity(record);
            }
            meter.string_capacity(hash);
            meter.bytes(bytes);
            "DocumentContent"
        }
        CollabEvent::DocumentContentState { route, hash, .. } => {
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.document {
                meter.string_capacity(record);
            }
            meter.string_capacity(hash);
            "DocumentContentState"
        }
        CollabEvent::ResourceCommandResult { result, .. } => {
            match result {
                Ok(reply) => meter.bytes(reply),
                Err(error) => meter.string_capacity(error),
            }
            "ResourceCommandResult"
        }
        CollabEvent::ActionVerdicts { verdicts } => {
            serialized_bytes(verdicts, &mut meter);
            "ActionVerdicts"
        }
        CollabEvent::NearbyTransferChanged => "NearbyTransferChanged",
        CollabEvent::SpeakersChanged => "SpeakersChanged",
        CollabEvent::DrivesChanged => "DrivesChanged",
        CollabEvent::ApplicationsChanged => "ApplicationsChanged",
        CollabEvent::Realtime(event) => {
            serialized_bytes(event, &mut meter);
            "Realtime"
        }
    };
    (kind, meter.finish())
}
