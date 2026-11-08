//! The person's other devices in quick settings: what pressing one of the section's rows does with
//! what plays here.
//! The panel reads `session:devices` itself, so the rows, the count on the root page and the card's
//! offer are the published reading. A pressed row is one of that resource's verbs, which the
//! session carries out here ([`press`]): sending what plays here to that device, answering a device
//! asking for this one's sound, stopping a route, or opening Settings where the device is managed.
//! The sounding out, the routes those sounds travel on and the devices asking for this device's
//! sound belong to the remote audio service. The chosen sound lives here because a press names only
//! the row it was made on.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_device_fabric::link::SharedLink;
use tilo_device_fabric::{DeviceId, MediaSourceLocator, PersonalFabricAction};
use tilo_quick_settings::RowPart;
use tilo_quick_settings::hosted::{ASK_KEY, ROUTE_KEY, SOUND_KEY};
use tilo_speaker_service::remote_audio;

use crate::{Shared, devices};

/// What a press on a row of the Devices section may use up: the sound picked for it, and the
/// presses the loop carries out.
#[derive(Default)]
pub(crate) struct Devices {
    /// The sound of this device's that a press on a device would send, as the reading carries it.
    pub(crate) chosen: Rc<RefCell<Option<u64>>>,
    /// The presses the published verb took, carried out by the loop.
    pub(crate) pressed: Rc<RefCell<Vec<devices::Press>>>,
    /// The accessories the person asked to move to another device, carried out by the loop.
    pub(crate) handed: Rc<RefCell<Vec<devices::HandOff>>>,
}

impl Shared {
    /// Carry out the presses the published verb took and holds here: a press on a row of the
    /// person's devices, which sends what plays here over the fabric, wakes the device or opens
    /// Settings where it is managed.
    pub(crate) fn carry_out_presses(&self) {
        let pressed = std::mem::take(&mut *self.devices.pressed.borrow_mut());
        for asked in pressed {
            press(self, &asked.row, asked.part);
        }
        let handed = std::mem::take(&mut *self.devices.handed.borrow_mut());
        for asked in handed {
            self.bluetooth.ask(&asked.address, &asked.device);
        }
    }
}

/// What pressing `part` of `row` does: pick a sound to send, send what was picked, answer a device
/// asking for this one's sound, stop a route, or open Settings where a device is managed. A button
/// a row does not offer, and a row whose device left the fabric, do nothing.
pub(crate) fn press(shared: &Shared, row: &str, part: RowPart) {
    let Some(fabric) = &shared.fabric else {
        return;
    };
    if let Some(token) = row.strip_prefix(SOUND_KEY) {
        // ignored-failure: a row that names no device chooses nothing
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
    // A device row's press is its body; the action named beside it is the same send, said in words.
    if !matches!(part, RowPart::Row | RowPart::Action) {
        return;
    }
    let Ok(target) = DeviceId::new(row) else {
        return;
    };
    let reading = devices::current(
        fabric,
        &shared.readings,
        &shared.devices.chosen,
        &shared.handoff,
    );
    let Some(device) = reading.devices.into_iter().find(|device| device.id == row) else {
        return;
    };
    if device.local {
        return;
    }
    // A device that is away and offered a way to be woken takes a wake-up signal: nothing plays
    // there to take, so this is what its press is for.
    if !device.online && device.wakeable {
        if let Err(refusal) = shared.waking.request(shared, row) {
            // The panel has no line to show a refusal on; a wake that could
            // not be planned changes nothing the person sees.
            // ignored-failure: the panel has no line for a refusal, and a wake that was not planned changes nothing
            tracing::debug!(%refusal, device = row, "a device could not be woken");
        }
        return;
    }
    // A sound of this device's that was picked goes to this device, which the service offers it to;
    // the picker's entry is spent with it, so the next press sends what plays here again.
    let chosen = *shared.devices.chosen.borrow();
    if let Some(token) = chosen
        && device.online
        && device.audio_output
    {
        match remote_audio::request(fabric.as_ref(), token, &target) {
            Ok(_) => *shared.devices.chosen.borrow_mut() = None,
            // ignored-failure: the card shows what plays here, and the service's reading is the rows
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
        crate::quick_settings::open_settings(shared, device.settings_link());
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
        // ignored-failure: the card already shows what plays here; a send that did not leave changes neither
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
        // ignored-failure: the service's reading is what the rows show, so the next pass shows the route
        tracing::debug!(%refusal, route = id, "the route was not stopped");
    }
}

/// Answer a device asking to play a sound of this one's: its allowance or its refusal, and nothing
/// for a press on anything else.
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
        // ignored-failure: the ask stays on screen so the person can answer it again
        tracing::debug!(%refusal, ask = id, "the answer did not leave");
    }
}

/// The session a row key names, as the id the service knows it by.
fn session_id_of(id: &str) -> Option<[u8; 16]> {
    <[u8; 16]>::try_from(hex::decode(id).ok()?.as_slice()).ok()
}
