//! The person's other devices in quick settings: the rows the Devices section
//! lists, and what pressing one does with what plays here.
//!
//! The session publishes the reading ([`crate::devices`]): `session:devices`
//! carries the person's devices as the fabric says they are, what plays here
//! that one of them can take, and the sounds that travel between them. One
//! reading of it becomes the section's rows, the count on the root page and
//! the devices an accessory can move to ([`land`]), and the panel is written
//! only when that reading moved. A pressed row is answered here ([`follow`]):
//! what plays here is sent to that device, or Settings opens where the device
//! is managed.
//!
//! The same section lists the sounds this device can send to another, the
//! routes those sounds travel on, and the devices asking for this device's
//! sound. The remote audio service owns all of it: the reading carries it,
//! and the chosen sound lives here because a press names only the row it was
//! made on.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_desktop::overlays::Slot;
use tilo_device_fabric::link::SharedLink;
use tilo_device_fabric::{DeviceId, MediaSourceLocator, PersonalFabricAction};
use tilo_quick_settings::device::SessionRequest;
use tilo_quick_settings::hosted::{ASK_KEY, DevicesReading, ROUTE_KEY, SOUND_KEY};
use tilo_quick_settings::{QsEffect, RowPart};
use tilo_speaker_service::remote_audio;

use crate::{Shared, devices};

/// What the panel was last fed of the person's devices, so the session's
/// reading is read every pass but the panel is written only when what it
/// shows moved.
#[derive(Default)]
pub(crate) struct Devices {
    fed: RefCell<Option<DevicesReading>>,
    /// The sound of this device's that a press on a device would send, as
    /// the reading carries it.
    pub(crate) chosen: Rc<RefCell<Option<u64>>>,
}

/// Land the person's devices in the panel: the Devices section's rows, the
/// count on the root page and the devices an accessory can move to, as the
/// session publishes them. Returns whether the panel shows that reading now.
pub(crate) fn land(shared: &Shared) -> bool {
    let Some(device) = &shared.device else {
        return false;
    };
    if !shared.overlays.is_open(Slot::QuickSettings) {
        return false;
    }
    let Some(reading) = shared
        .services
        .borrow()
        .read(devices::RESOURCE)
        .and_then(|reading| serde_json::from_value::<DevicesReading>(reading).ok())
    else {
        return false;
    };
    if shared.devices.fed.borrow().as_ref() == Some(&reading) {
        return true;
    }
    let rows = reading.rows();
    let count = reading.count();
    let targets = reading.targets();
    *shared.devices.fed.borrow_mut() = Some(reading);
    device.update(|state| {
        state.device_rows = rows;
        state.personal_devices = Some(count);
        state.handoff_targets = Some(targets);
    });
    true
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
    let reading = devices::current(fabric, &shared.readings, &shared.devices.chosen);
    let Some(device) = reading.devices.into_iter().find(|device| device.id == row) else {
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
    let source = {
        let readings = shared.readings.borrow();
        readings.handover()
    }
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
