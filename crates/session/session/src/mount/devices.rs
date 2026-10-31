//! The person's other devices in quick settings: the rows the Devices section
//! lists, and what pressing one does with what plays here.
//!
//! The session holds the device fabric and the panel never does: one reading
//! of the fabric a pass becomes the section's rows, the count on the root page
//! and the devices an accessory can move to ([`land`]), and the panel is
//! written only when that reading moved. A pressed row is answered here
//! ([`follow`]): what plays here is sent to that device, or Settings opens
//! where the device is managed.
//!
//! The same section lists the sounds this device can send to another, the
//! routes those sounds travel on, and the devices asking for this device's
//! sound. The remote audio service owns all of it ([`remote_audio`]): this
//! mount reads its snapshot a pass and hands it every decision, and the
//! chosen sound lives here because a press names only the row it was made on.
//!
//! `DeviceFabric::handoff_media` asks which devices can take a session from
//! this device's own media in its snapshot, and this session publishes none,
//! so what a device can take is read here from what this session does know: it
//! is online, it is not this device, media continuity is on, and a player here
//! has a stream to send.

use std::cell::RefCell;

use tilo_device_fabric::link::SharedLink;
use tilo_device_fabric::{DeviceId, MediaSourceLocator, PersonalFabricAction};
use tilo_quick_settings::device::SessionRequest;
use tilo_quick_settings::hosted::{
    ASK_KEY, PersonalDevice, ROUTE_KEY, SOUND_KEY, SendableSound, SoundAsk, SoundRoute,
    device_rows, handoff_targets, remote_audio_rows,
};
use tilo_quick_settings::{QsEffect, RowPart};
use tilo_speaker_service::remote_audio::{self, RemoteAudioPhase};

use crate::{DeviceHost, Shared};

/// What the panel was last fed of the person's devices, so the fabric is read
/// every pass but the panel is written only when what it shows moved.
#[derive(Default)]
pub(crate) struct Devices {
    fed: RefCell<Option<Reading>>,
    /// The sound of this device's that a press on a device would send.
    chosen: RefCell<Option<u64>>,
}

/// The person's devices as the fabric reads them, what plays here that they
/// can take, and the remote audio the service reports.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Reading {
    devices: Vec<PersonalDevice>,
    /// What plays here, titled, while media continuity is on: what a press on
    /// another device sends.
    playing: Option<String>,
    sounds: Vec<SendableSound>,
    routes: Vec<SoundRoute>,
    asks: Vec<SoundAsk>,
    chosen: Option<u64>,
}

/// Read the person's devices off `fabric`: this machine first, then those
/// here, then by name.
fn read(fabric: &SharedLink) -> Vec<PersonalDevice> {
    let local = fabric
        .local_device_fabric_identity()
        .map(|(device, _)| device);
    let converged = fabric.device_fabric();
    let mut devices: Vec<PersonalDevice> = converged
        .snapshots()
        .values()
        .map(|snapshot| PersonalDevice {
            id: snapshot.owner.as_str().to_owned(),
            label: snapshot.presence.label.clone(),
            online: snapshot.presence.online,
            local: local.as_ref() == Some(&snapshot.owner),
            display: snapshot.presence.capabilities.display,
            wakeable: snapshot
                .presence
                .wake_routes
                .iter()
                .any(|route| route.magic_packet),
            audio_output: snapshot.presence.capabilities.audio_output,
        })
        .collect();
    devices.sort_by(|left, right| {
        right
            .local
            .cmp(&left.local)
            .then_with(|| right.online.cmp(&left.online))
            .then_with(|| left.label.cmp(&right.label))
    });
    devices
}

/// Feed the panel the person's devices while it is open: the Devices
/// section's rows, the count on the root page and the devices an accessory
/// can move to, from one reading of the fabric and of remote audio.
pub(crate) fn land(shared: &Shared) {
    let Some(device) = &shared.device else {
        return;
    };
    if !shared
        .quick_settings
        .as_ref()
        .is_some_and(|panel| panel.open.get())
    {
        return;
    }
    let Some(fabric) = &shared.fabric else {
        return;
    };
    let playing = fabric
        .require_media()
        .is_ok()
        .then(|| device.playing_source())
        .flatten()
        .map(|source| source.title);
    let devices = read(fabric);
    let audio = remote_audio::snapshot(None);
    // A sound the picker dropped is no longer one this device can send, and
    // the service keeps a sound only while it can still be sent.
    if shared
        .devices
        .chosen
        .borrow()
        .is_some_and(|chosen| !audio.sources.iter().any(|source| source.token == chosen))
    {
        *shared.devices.chosen.borrow_mut() = None;
    }
    let chosen = *shared.devices.chosen.borrow();
    let label_of = |peer: &DeviceId| {
        devices
            .iter()
            .find(|device| device.id == peer.as_str())
            .map_or_else(|| peer.as_str().to_owned(), |device| device.label.clone())
    };
    let reading = Reading {
        sounds: audio
            .sources
            .iter()
            .map(|source| SendableSound {
                token: source.token,
                label: source.label.clone(),
                application: source.application,
            })
            .collect(),
        routes: audio
            .routes
            .iter()
            .map(|route| SoundRoute {
                id: hex::encode(route.session_id),
                label: route.label.clone(),
                detail: remote_audio::phase_line(route, &label_of(&route.peer)),
                stopped: route.phase == RemoteAudioPhase::Interrupted,
            })
            .collect(),
        asks: audio
            .consents
            .iter()
            .map(|consent| SoundAsk {
                id: hex::encode(consent.session_id),
                media: consent.label.clone(),
                source: label_of(&consent.source),
            })
            .collect(),
        playing,
        devices,
        chosen,
    };
    if shared.devices.fed.borrow().as_ref() == Some(&reading) {
        return;
    }
    let mut rows = remote_audio_rows(
        &reading.sounds,
        &reading.routes,
        &reading.asks,
        reading.chosen,
    );
    rows.extend(device_rows(
        &reading.devices,
        reading.playing.as_deref(),
        reading.chosen.is_some(),
    ));
    let targets = handoff_targets(&reading.devices);
    let count = reading.devices.len();
    *shared.devices.fed.borrow_mut() = Some(reading);
    device.update(|state| {
        state.device_rows = rows;
        state.personal_devices = Some(count);
        state.handoff_targets = Some(targets);
    });
}

/// Answer a press on a row of the Devices section. Whether this was one of
/// the panel's effects this session knows.
pub(crate) fn follow(shared: &Shared, request: &SessionRequest) -> bool {
    let SessionRequest::Effect(QsEffect::PressDevice { row, part }) = request else {
        return false;
    };
    press(shared, row, *part);
    true
}

/// What pressing `part` of `row` does: pick a sound to send, send what was
/// picked, answer a device asking for this one's sound, stop a route, or open
/// Settings where a device is managed. A button a row does not offer, and a
/// row whose device left the fabric, do nothing.
fn press(shared: &Shared, row: &str, part: RowPart) {
    let Some(fabric) = &shared.fabric else {
        return;
    };
    if let Some(token) = row.strip_prefix(SOUND_KEY) {
        *shared.devices.chosen.borrow_mut() = token.parse().ok();
        return;
    }
    if let Some(id) = row.strip_prefix(ROUTE_KEY) {
        stopped(fabric, id);
        return;
    }
    if let Some(id) = row.strip_prefix(ASK_KEY) {
        answered(fabric, id, part);
        return;
    }
    // A device row's press is its body; the action named beside it is the
    // same send, said in words.
    if !matches!(part, RowPart::Row | RowPart::Action) {
        return;
    }
    let Ok(target) = DeviceId::new(row) else {
        return;
    };
    let devices = read(fabric);
    let Some(device) = devices.iter().find(|device| device.id == row) else {
        return;
    };
    if device.local {
        return;
    }
    // A device that is away and offered a way to be woken takes a wake-up
    // signal: nothing plays there to take, so this is what its press is for.
    if !device.online && device.wakeable {
        if let Err(refusal) = shared.waking.request(shared, row) {
            // The panel has no line to show a refusal on; a wake that could
            // not be planned changes nothing the person sees.
            tracing::debug!(%refusal, device = row, "a device could not be woken");
        }
        return;
    }
    // A sound of this device's that was picked goes to this device, which the
    // service offers it to; the picker's entry is spent with it, so the next
    // press sends what plays here again.
    let chosen = *shared.devices.chosen.borrow();
    if let Some(token) = chosen
        && device.online
        && device.audio_output
    {
        match remote_audio::request(fabric.as_ref(), token, &target) {
            Ok(_) => *shared.devices.chosen.borrow_mut() = None,
            Err(refusal) => tracing::debug!(%refusal, device = row, "the sound did not leave"),
        }
        return;
    }
    let source = shared
        .device
        .as_ref()
        .and_then(DeviceHost::playing_source)
        .filter(|_| device.online && fabric.require_media().is_ok());
    let Some(source) = source else {
        super::quick_settings::open_settings(shared, device.settings_link());
        return;
    };
    let action = PersonalFabricAction::ResumeMedia {
        session_id: source.session_id,
        app_id: source.app_id,
        workspace_id: String::new(),
        title: source.title,
        locator: MediaSourceLocator::RemoteStream { url: source.url },
        position_ms: source.position_ms,
        resume_playing: source.playing,
        has_video: false,
    };
    if let Err(error) = fabric.try_send_personal_fabric_action(&target, action) {
        // The card already shows what plays here, and the device that takes
        // it shows the offer: a send that did not leave changes neither.
        tracing::debug!(%error, device = row, "what plays here did not reach the device");
    }
}

/// Stop the route `id` names, or dismiss its notice when it already stopped.
fn stopped(fabric: &SharedLink, id: &str) {
    let Some(session_id) = session_id_of(id) else {
        return;
    };
    if let Err(refusal) = remote_audio::stop(fabric.as_ref(), session_id) {
        // The service's reading is what the rows show, so a route that could
        // not be stopped is still there on the next pass.
        tracing::debug!(%refusal, route = id, "the route was not stopped");
    }
}

/// Answer a device asking to play a sound of this one's: its allowance or its
/// refusal, and nothing for a press on anything else.
fn answered(fabric: &SharedLink, id: &str, part: RowPart) {
    let approved = match part {
        RowPart::Allow => true,
        RowPart::Decline => false,
        _ => return,
    };
    let Some(session_id) = session_id_of(id) else {
        return;
    };
    if let Err(refusal) = remote_audio::authorize(fabric.as_ref(), session_id, approved) {
        // A refusal is the service's own words: the ask stays on screen so
        // the person can answer it again.
        tracing::debug!(%refusal, ask = id, "the answer did not leave");
    }
}

/// The session a row key names, as the id the service knows it by.
fn session_id_of(id: &str) -> Option<[u8; 16]> {
    <[u8; 16]>::try_from(hex::decode(id).ok()?.as_slice()).ok()
}
