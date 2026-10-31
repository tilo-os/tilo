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
//! `DeviceFabric::handoff_media` asks which devices can take a session from
//! this device's own media in its snapshot, and this session publishes none,
//! so what a device can take is read here from what this session does know: it
//! is online, it is not this device, media continuity is on, and a player here
//! has a stream to send.

use std::cell::RefCell;

use tilo_device_fabric::link::SharedLink;
use tilo_device_fabric::{DeviceId, MediaSourceLocator, PersonalFabricAction};
use tilo_quick_settings::device::SessionRequest;
use tilo_quick_settings::hosted::{PersonalDevice, device_rows, handoff_targets};
use tilo_quick_settings::{QsEffect, RowPart};

use crate::{DeviceHost, Shared};

/// What the panel was last fed of the person's devices, so the fabric is read
/// every pass but the panel is written only when what it shows moved.
#[derive(Default)]
pub(crate) struct Devices {
    fed: RefCell<Option<Reading>>,
}

/// The person's devices as the fabric reads them, and what plays here that
/// they can take.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Reading {
    devices: Vec<PersonalDevice>,
    /// What plays here, titled, while media continuity is on: what a press on
    /// another device sends.
    playing: Option<String>,
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
/// can move to, from one reading of the fabric.
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
    let reading = Reading {
        devices: read(fabric),
        playing,
    };
    if shared.devices.fed.borrow().as_ref() == Some(&reading) {
        return;
    }
    let rows = device_rows(&reading.devices, reading.playing.as_deref());
    let targets = handoff_targets(&reading.devices);
    let count = reading.devices.len();
    *shared.devices.fed.borrow_mut() = Some(reading);
    device.update(|state| {
        state.device_rows = rows;
        state.personal_devices = Some(count);
        state.handoff_targets = Some(targets);
    });
}

/// Answer a press on a device row. Whether this was one of the panel's
/// effects this session knows.
pub(crate) fn follow(shared: &Shared, request: &SessionRequest) -> bool {
    let SessionRequest::Effect(QsEffect::PressDevice { row, part }) = request else {
        return false;
    };
    press(shared, row, *part);
    true
}

/// What pressing `part` of `row` does: send what plays here to that device, or
/// open Settings where the device is managed. This machine's own row, a
/// button a device row does not offer, and a row whose device left the fabric
/// do nothing.
fn press(shared: &Shared, row: &str, part: RowPart) {
    // A device row's press is its body; the action named beside it is the
    // same send, said in words.
    if !matches!(part, RowPart::Row | RowPart::Action) {
        return;
    }
    let Some(fabric) = &shared.fabric else {
        return;
    };
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
