use super::{
    DeviceCert, Event, JsError, RefCell, RevocationEntry, RevocationList, Serialize, WorkspaceId,
    js_error,
};

/// One device in a call, as `web/voice.mjs` reads it: device and person as hex.
#[derive(Serialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is its own field the page reads: publishes, camera, screen, muted, deafened"
)]
pub(super) struct JsCaller {
    device: String,
    /// The person, empty while nobody named them.
    user: String,
    /// The call lets this device's person speak in it.
    publishes: bool,
    camera: bool,
    /// It shares a screen.
    screen: bool,
    /// Its microphone is off.
    muted: bool,
    /// It hears nothing of the call.
    deafened: bool,
}

/// A voice channel's call, as `web/voice-peer.mjs` lists it: its devices, and the roster the
/// channel shows of its people.
#[derive(Serialize)]
pub(super) struct JsChannel {
    channel: String,
    name: String,
    members: Vec<JsCaller>,
    people: Vec<String>,
    cameras: Vec<String>,
    screens: Vec<String>,
    muted: Vec<String>,
    deafened: Vec<String>,
    /// The call was ended: nobody is in it until it is restarted.
    ended: bool,
}

/// `channels` as the page reads them.
pub(super) fn js_channels(channels: Vec<tilo_live::calls::Channel>) -> Vec<JsChannel> {
    channels
        .into_iter()
        .map(|channel| JsChannel {
            ended: channel.rule.ended,
            people: channel.people(),
            cameras: channel.cameras(),
            screens: channel.screens(),
            muted: channel.muted(),
            deafened: channel.deafened(),
            members: channel
                .callers
                .into_iter()
                .map(|caller| JsCaller {
                    publishes: channel.rule.may(caller.person.as_deref()).publish,
                    device: hex::encode(caller.device),
                    user: caller.person.unwrap_or_default(),
                    camera: caller.camera,
                    screen: caller.screen,
                    muted: caller.muted,
                    deafened: caller.deafened,
                })
                .collect(),
            channel: channel.channel,
            name: channel.name,
        })
        .collect()
}

/// A call event for `web/voice.mjs`.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(super) enum JsVoiceEvent {
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
    pub(super) source: &'static str,
    pub(super) layers: Vec<&'static str>,
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
        Event::CallAudio { .. } | Event::CallPicture { .. } | Event::CallDemand { .. }
    )
}
