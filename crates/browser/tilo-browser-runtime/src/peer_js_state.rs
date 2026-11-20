use super::*;

/// `{publish, subscribe}`.
#[derive(Serialize, Deserialize)]
pub(super) struct JsDirections {
    publish: bool,
    subscribe: bool,
}

impl From<tilo_server_protocol::voice::Directions> for JsDirections {
    fn from(value: tilo_server_protocol::voice::Directions) -> Self {
        Self {
            publish: value.publish,
            subscribe: value.subscribe,
        }
    }
}

/// One call member: device and person as hex.
#[derive(Serialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is its own field the page reads: publishes, camera, screen, muted, deafened"
)]
pub(super) struct JsVoiceMember {
    device: String,
    user: String,
    publishes: bool,
    camera: bool,
    /// It shares a screen.
    screen: bool,
    /// Its microphone is off, or it may not publish.
    muted: bool,
    /// It hears nothing of the call.
    deafened: bool,
}

pub(super) fn voice_members(members: Vec<tilo_server_protocol::voice::VoiceMember>) -> Vec<JsVoiceMember> {
    members
        .into_iter()
        .map(|member| JsVoiceMember {
            device: hex::encode(member.device_public_key),
            user: hex::encode(member.user_public_key),
            publishes: member.publishes,
            camera: member.showing.camera,
            screen: member.showing.screen,
            muted: member.showing.muted,
            deafened: member.showing.deafened,
        })
        .collect()
}

/// A call grant for `web/voice.mjs`: ids and the token as hex.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct JsVoiceGrant {
    call: String,
    epoch: u64,
    directions: JsDirections,
    members: Vec<JsVoiceMember>,
}

/// A call of the workspace as `web/voice.mjs` lists it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct JsCall {
    call: String,
    epoch: u64,
    active: bool,
    allowed: JsDirections,
    app: String,
    record: String,
    channel: String,
    members: Vec<JsVoiceMember>,
}

pub(super) fn call_state(state: &tilo_server_protocol::voice::CallState) -> serde_json::Value {
    serde_json::json!({
        "call": hex::encode(state.call),
        "epoch": state.epoch,
        "active": state.active,
        "allowed": JsDirections::from(state.allowed),
    })
}

/// A call event for `web/voice.mjs`.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(super) enum JsVoiceEvent {
    Calls {
        workspace: String,
        calls: Option<Vec<JsCall>>,
        error: Option<String>,
    },
    CallResult {
        workspace: String,
        request: String,
        call: Option<serde_json::Value>,
        error: Option<String>,
    },
    Granted {
        workspace: String,
        grant: JsVoiceGrant,
    },
    Ended {
        workspace: String,
        call: String,
        reason: String,
    },
    /// One frame of a call's voice, as the device `from` published it on the
    /// workspace's live session: the page's app plays it.
    Heard {
        workspace: String,
        from: String,
        sequence: u64,
        timestamp48k: u64,
        opus: String,
    },
    /// One picture of a call on the workspace's live session: the tilo-video/1
    /// object of `from`'s camera or screen, in hex, as the app's decoder
    /// draws it.
    Picture {
        workspace: String,
        from: String,
        source: &'static str,
        keyframe: bool,
        object: String,
    },
    /// What the call now shows of this page's pictures: the layers to encode
    /// of each source, and the sources to start over at a keyframe.
    Demand {
        workspace: String,
        wanted: Vec<JsWanted>,
        keyframes: Vec<&'static str>,
    },
}

/// The layers of one source a call shows.
#[derive(Serialize)]
pub(super) struct JsWanted {
    source: &'static str,
    layers: Vec<&'static str>,
}

/// The name `web/voice-video.mjs` gives `source`.
pub(super) fn js_source(source: tilo_live::video::Source) -> &'static str {
    match source {
        tilo_live::video::Source::Camera => "camera",
        tilo_live::video::Source::Screen => "screen",
    }
}

/// The name `web/voice-video.mjs` gives `layer`.
pub(super) fn js_layer(layer: tilo_live::video::Layer) -> &'static str {
    match layer {
        tilo_live::video::Layer::Low => "low",
        tilo_live::video::Layer::Mid => "mid",
        tilo_live::video::Layer::High => "high",
    }
}

/// The source a name of `web/voice-video.mjs` is.
pub(super) fn source_of_name(name: &str) -> Option<tilo_live::video::Source> {
    match name {
        "camera" => Some(tilo_live::video::Source::Camera),
        "screen" => Some(tilo_live::video::Source::Screen),
        _ => None,
    }
}

/// The layer a name of `web/voice-video.mjs` is.
pub(super) fn layer_of_name(name: &str) -> Option<tilo_live::video::Layer> {
    match name {
        "low" => Some(tilo_live::video::Layer::Low),
        "mid" => Some(tilo_live::video::Layer::Mid),
        "high" => Some(tilo_live::video::Layer::High),
        _ => None,
    }
}

pub(super) fn workspace_of(
    workspace_id: WorkspaceId,
    workspaces: &RefCell<std::collections::HashMap<WorkspaceId, String>>,
) -> String {
    workspaces
        .borrow()
        .get(&workspace_id)
        .cloned()
        .unwrap_or_else(|| workspace_id.to_string())
}

pub(super) fn voice_event(
    event: tilo_session::sync::VoiceEvent,
    workspaces: &RefCell<std::collections::HashMap<WorkspaceId, String>>,
) -> JsVoiceEvent {
    use tilo_session::sync::VoiceEvent;
    match event {
        VoiceEvent::Calls {
            ws: workspace_id,
            result,
        } => {
            let (calls, error) = match result {
                Ok(calls) => (
                    Some(
                        calls
                            .into_iter()
                            .map(|summary| JsCall {
                                call: hex::encode(summary.state.call),
                                epoch: summary.state.epoch,
                                active: summary.state.active,
                                allowed: summary.state.allowed.into(),
                                app: summary.app_id,
                                record: summary.document_id,
                                channel: summary.channel_id,
                                members: voice_members(summary.members),
                            })
                            .collect(),
                    ),
                    None,
                ),
                Err(error) => (None, Some(error)),
            };
            JsVoiceEvent::Calls {
                workspace: workspace_of(workspace_id, workspaces),
                calls,
                error,
            }
        }
        VoiceEvent::CallResult {
            ws: workspace_id,
            request,
            result,
        } => {
            let (call, error) = match result {
                Ok(state) => (Some(call_state(&state)), None),
                Err(error) => (None, Some(error)),
            };
            JsVoiceEvent::CallResult {
                workspace: workspace_of(workspace_id, workspaces),
                request: hex::encode(request),
                call,
                error,
            }
        }
        VoiceEvent::Granted {
            ws: workspace_id,
            grant,
        } => {
            let grant = *grant;
            JsVoiceEvent::Granted {
                workspace: workspace_of(workspace_id, workspaces),
                grant: JsVoiceGrant {
                    call: hex::encode(grant.call),
                    epoch: grant.epoch,
                    directions: grant.directions.into(),
                    members: voice_members(grant.members),
                },
            }
        }
        VoiceEvent::Ended {
            ws: workspace_id,
            call,
            reason,
        } => JsVoiceEvent::Ended {
            workspace: workspace_of(workspace_id, workspaces),
            call: hex::encode(call),
            reason,
        },
    }
}

/// A change to a call, as `web/voice.mjs` asks for it.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(super) enum JsCallOperation {
    Open {
        app: String,
        record: String,
        channel: String,
    },
    Start {
        app: String,
        record: String,
        channel: String,
        allowed: JsDirections,
    },
    End {
        call: String,
        epoch: u64,
    },
    Restart {
        call: String,
        epoch: u64,
    },
}

impl JsCallOperation {
    fn into_operation(self) -> Result<tilo_server_protocol::voice::CallOperation, JsError> {
        use tilo_server_protocol::voice::{CallOperation, Directions};
        Ok(match self {
            Self::Open {
                app,
                record,
                channel,
            } => CallOperation::Open {
                app_id: app,
                document_id: record,
                channel_id: channel,
            },
            Self::Start {
                app,
                record,
                channel,
                allowed,
            } => CallOperation::Start {
                app_id: app,
                document_id: record,
                channel_id: channel,
                allowed: Directions {
                    publish: allowed.publish,
                    subscribe: allowed.subscribe,
                },
            },
            Self::End { call, epoch } => CallOperation::End {
                call: hex32(&call, "call")?,
                epoch,
            },
            Self::Restart { call, epoch } => CallOperation::Restart {
                call: hex32(&call, "call")?,
                epoch,
            },
        })
    }
}

/// The revocations of `entries` the person `cert` certifies signed: a
/// session announces only its own person's, since a host reads a hello's
/// entries as its sender's root's and refuses one signed by another.
pub(super) fn own_revocations(
    entries: Vec<RevocationEntry>,
    cert: &DeviceCert,
) -> Result<RevocationList, JsError> {
    let mut revocations = RevocationList::new();
    for entry in entries {
        if entry.user_public_key == cert.user_public_key {
            revocations.insert(entry).map_err(js_error)?;
        }
    }
    Ok(revocations)
}

/// The events [`WasmPeer::take_durable_events`] reads.
pub(super) fn is_durable(event: &Event) -> bool {
    matches!(event, Event::Landed { .. })
}

/// The events [`WasmPeer::take_live_events`] reads.
pub(super) fn is_live(event: &Event) -> bool {
    matches!(event, Event::LiveSession { .. } | Event::Live { .. })
}

/// The events [`WasmPeer::take_voice_events`] reads: a page that never takes them (it has no
/// call open) keeps none of them.
pub(super) fn is_voice(event: &Event) -> bool {
    matches!(
        event,
        Event::Voice(_)
            | Event::CallAudio { .. }
            | Event::CallPicture { .. }
            | Event::CallDemand { .. }
    )
}
