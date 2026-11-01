//! The person's other devices, published: what each one is, what it can take
//! from this one, and the sounds that travel between them.
//!
//! `session:devices` is one reading of the device fabric and of the remote
//! audio service, the same reading the Quick Settings Devices section lists,
//! the root page counts with and an accessory's page moves to. A device is
//! what its own machine says of itself — its label, whether it is here, what
//! it can show and play, whether it wakes to a magic packet — so the reading
//! is not anyone's personal content, and every app that may read the
//! session's resources reads it as it reads what the displays show.
//!
//! A device can take what plays here while media continuity is on, and this
//! session publishes none: what a device can take is read from what this
//! session does know — it is online, it is not this device, and a player here
//! has a stream to send.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_device_fabric::DeviceId;
use tilo_device_fabric::link::SharedLink;
use tilo_quick_settings::device::DeviceHost;
use tilo_quick_settings::hosted::{
    DevicesReading, PersonalDevice, SendableSound, SoundAsk, SoundRoute,
};
use tilo_service::{Service, Started, reading};
use tilo_speaker_service::remote_audio::{self, RemoteAudioPhase};

use crate::i18n::t;

/// The resource the person's devices are published under.
pub const RESOURCE: &str = "session:devices";

/// What kind of resource [`RESOURCE`] is.
pub const KIND: &str = "session.devices";

/// The person's devices as the fabric reads them, what plays here that they
/// can take, and the remote audio the service reports, as one reading.
///
/// A sound the picker dropped is no longer one this device can send, and the
/// service keeps a sound only while it can still be sent, so a pick that
/// names a sound which left is forgotten here.
#[must_use]
pub(crate) fn current(
    fabric: &SharedLink,
    device: &DeviceHost,
    chosen: &RefCell<Option<u64>>,
) -> DevicesReading {
    let playing = fabric
        .require_media()
        .is_ok()
        .then(|| device.playing_source())
        .flatten()
        .map(|source| source.title);
    let devices = read(fabric);
    let audio = remote_audio::snapshot(None);
    if chosen
        .borrow()
        .is_some_and(|chosen| !audio.sources.iter().any(|source| source.token == chosen))
    {
        *chosen.borrow_mut() = None;
    }
    let label_of = |peer: &DeviceId| {
        devices
            .iter()
            .find(|device| device.id == peer.as_str())
            .map_or_else(|| peer.as_str().to_owned(), |device| device.label.clone())
    };
    DevicesReading {
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
        chosen: *chosen.borrow(),
    }
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

/// The person's devices as the session's services publish them: read off the
/// fabric every pass, so a device that comes or goes is one revision.
pub(crate) struct Served {
    fabric: SharedLink,
    device: DeviceHost,
    /// The sound of this device's that a press on a device would send.
    chosen: Rc<RefCell<Option<u64>>>,
    /// The reading last reported, so a pass that changed nothing is no
    /// revision.
    published: RefCell<Option<DevicesReading>>,
}

impl Served {
    /// The service publishing what `fabric`, this device and `chosen` read.
    pub(crate) fn new(
        fabric: SharedLink,
        device: DeviceHost,
        chosen: Rc<RefCell<Option<u64>>>,
    ) -> Self {
        Self {
            fabric,
            device,
            chosen,
            published: RefCell::new(None),
        }
    }

    /// The reading as it stands now.
    fn current(&self) -> DevicesReading {
        current(&self.fabric, &self.device, &self.chosen)
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![ResourceSpec::new(RESOURCE, KIND, t("session-devices"))]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| {
            let published = self.published.borrow().clone();
            let current = published.unwrap_or_else(|| self.current());
            reading(&current)
        })
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        _arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        // What a press on a device does is the session's own: it sends what
        // plays here over the fabric, or opens Settings where the device is
        // managed, so there is nothing here to invoke.
        Err(DoorError::Unknown(ActionAddress::new(resource, action)))
    }

    fn poll(&mut self) -> bool {
        let now = self.current();
        let mut published = self.published.borrow_mut();
        if published.as_ref() == Some(&now) {
            return false;
        }
        *published = Some(now);
        true
    }
}
