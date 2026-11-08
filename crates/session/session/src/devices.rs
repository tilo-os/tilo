//! The person's other devices, published: what each one is, what it can take
//! from this one, the sounds that travel between them, and the session one of
//! them hands this one.
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
//!
//! What a press on a row does needs the fabric, the wakes and the search, so
//! it is the session's own: the resource publishes the press
//! ([`DevicesAction::Press`]) and the loop carries it out
//! ([`crate::mount::devices::press`]), as it carries out every other verb
//! published here.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_device_fabric::DeviceId;
use tilo_device_fabric::link::SharedLink;
use tilo_quick_settings::hosted::{
    DevicesReading, PersonalDevice, SendableSound, SoundAsk, SoundRoute,
};
use tilo_quick_settings::{MediaOffer, RowPart};
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};
use tilo_speaker_service::remote_audio::{self, RemoteAudioPhase};

use crate::i18n::t;

/// The resource the person's devices are published under.
pub const RESOURCE: &str = "session:devices";

/// What kind of resource [`RESOURCE`] is.
pub const KIND: &str = "session.devices";

/// What the resource offers beside its reading: a press on one of its rows.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.devices")]
pub enum DevicesAction {
    /// Press one of the person's devices
    #[effect(own_device)]
    Press {
        /// The row's key, as the reading lists it.
        #[param("The row, as the reading lists it")]
        row: String,
        /// What was pressed on it: its body, the action it names, or its answer to a device asking
        /// for this one's sound.
        #[param("The part of the row that was pressed")]
        part: RowPart,
    },
    /// Move a paired Bluetooth accessory to another of the person's devices
    #[effect(own_device)]
    HandOff {
        /// The accessory's address.
        #[param("The accessory's address")]
        address: String,
        /// The device that takes it, as the reading lists its id.
        #[param("The device to move it to")]
        device: String,
    },
}

/// The person's devices as the fabric reads them, what plays here that they can take, the remote
/// audio the service reports, and the session one of them hands this one, as one reading.
/// A sound the picker dropped is no longer one this device can send, and the service keeps a sound
/// only while it can still be sent, so a pick that names a sound which left is forgotten here.
#[must_use]
pub(crate) fn current(
    fabric: &SharedLink,
    readings: &crate::status::Shared,
    chosen: &RefCell<Option<u64>>,
    handoff: &RefCell<Option<MediaOffer>>,
) -> DevicesReading {
    let playing = fabric
        .require_media()
        .is_ok()
        .then(|| readings.borrow().handover())
        .flatten()
        .map(|handover| handover.title);
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
        offer: handoff.borrow().clone(),
    }
}

/// Read the person's devices off `fabric` : this machine first, then those here, then by name.
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

/// The person's devices as the session's services publish them: read off the fabric every pass, so
/// a device that comes or goes is one revision.
pub(crate) struct Served {
    fabric: SharedLink,
    /// What the services read of this device, of which the reading says what plays here that a
    /// device can take.
    readings: crate::status::Shared,
    /// The sound of this device's that a press on a device would send.
    chosen: Rc<RefCell<Option<u64>>>,
    /// The session another of the person's devices handed this one, until the person continues it
    /// here.
    handoff: Rc<RefCell<Option<MediaOffer>>>,
    /// The presses the person made on the rows, waiting for the loop that holds the fabric, the
    /// wakes and the search.
    pressed: Rc<RefCell<Vec<Press>>>,
    /// The accessories the person asked to move to another device.
    handed: Rc<RefCell<Vec<HandOff>>>,
    /// The reading last reported, so a pass that changed nothing is no revision.
    published: RefCell<Option<DevicesReading>>,
}

/// What a press on a row of the Devices section asks of the session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Press {
    pub(crate) row: String,
    pub(crate) part: RowPart,
}

/// One accessory asked to move to another device: its address, and the device it goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HandOff {
    pub(crate) address: String,
    pub(crate) device: String,
}

impl Served {
    /// The service publishing what `fabric` , `readings` , `chosen` and `handoff` read, whose
    /// presses `pressed` carries to the loop.
    pub(crate) fn new(
        fabric: SharedLink,
        readings: crate::status::Shared,
        chosen: Rc<RefCell<Option<u64>>>,
        handoff: Rc<RefCell<Option<MediaOffer>>>,
        pressed: Rc<RefCell<Vec<Press>>>,
        handed: Rc<RefCell<Vec<HandOff>>>,
    ) -> Self {
        Self {
            fabric,
            readings,
            chosen,
            handoff,
            pressed,
            handed,
            published: RefCell::new(None),
        }
    }

    /// The reading as it stands now.
    fn current(&self) -> DevicesReading {
        current(&self.fabric, &self.readings, &self.chosen, &self.handoff)
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        let mut resource =
            offer::<DevicesAction>(ResourceSpec::new(RESOURCE, KIND, t("session-devices")));
        // The panel keeps the words the rest of the desktop names them by.
        for spec in &mut resource.actions {
            if spec.id == "press" {
                spec.type_mut().label = t("session-devices-press");
            }
        }
        vec![resource]
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
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        if resource != RESOURCE {
            return Err(DoorError::Unknown(ActionAddress::new(resource, action)));
        }
        match decode::<DevicesAction>(&self.resources(), resource, action, arguments)? {
            // What a press does is the session's own: it sends what plays here over the fabric,
            // wakes the device, or opens Settings where the device is managed.
            DevicesAction::Press { row, part } => {
                self.pressed.borrow_mut().push(Press { row, part });
            }
            // Moving an accessory is the session's too: it needs the fabric and both devices'
            // Bluetooth, so the loop carries it out.
            DevicesAction::HandOff { address, device } => {
                self.handed.borrow_mut().push(HandOff { address, device });
            }
        }
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
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
