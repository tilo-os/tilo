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
//! ([`press::press`]), as it carries out every other verb
//! published here.
//!
//! Beside the devices it reads whether this device is one of the person's, the workspaces its
//! layout lists, and the verb that makes it theirs ([`DevicesAction::LogIn`]): what a shell on a
//! device that is not logged in yet shows, and what it logs in with.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::hosted::{
    DeviceWorkspace, DevicesReading, MediaOffer, PersonalDevice, RowPart, SendableSound, SoundAsk,
    SoundRoute,
};
use crate::platform::fabric::DeviceId;
use crate::platform::fabric::SharedLink;
use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_lock::MutexRecover as _;
use tilo_resource::Effect;
use tilo_session_core::desktop::state::DesktopHandle;
use tilo_service::{Service, Started, decode, offer, reading};
use tilo_speaker_service::remote_audio::{self, RemoteAudioPhase};

use crate::i18n::t;

pub(crate) mod press;

/// The resource the person's devices are published under.
pub const RESOURCE: &str = "session:devices";

/// The person's devices as the session publishes them: its declared type, whose reading the
/// panel and the page show and whose press the session carries out.
#[derive(tilo_resource::Row)]
#[row(kind = "session.devices", label = "Devices")]
pub struct Devices;

/// What the resource offers beside its reading: a press on one of its rows.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.devices")]
pub enum DevicesAction {
    /// Press one of the person's devices
    #[effect(kind = "outside")]
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
    #[effect(kind = "outside")]
    #[effect(own_device)]
    HandOff {
        /// The accessory's address.
        #[param("The accessory's address")]
        address: String,
        /// The device that takes it, as the reading lists its id.
        #[param("The device to move it to")]
        device: String,
    },
    /// Make this device one of the person's, with the ticket another of their devices printed
    #[effect(kind = "outside")]
    #[effect(own_device)]
    LogIn {
        /// The ticket the person's other device printed.
        #[param("The ticket to log in with")]
        ticket: String,
    },
    /// Start a new person on this device, when no person's certificate names it yet
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Found,
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
        // What the layout lists and whether the person is logged in here are the session's
        // ([`Served::current`]), not what the fabric and the audio service say.
        ..DevicesReading::default()
    }
}

/// Read the person's devices off `fabric` : this machine first, then those here, then by name.
fn read(fabric: &SharedLink) -> Vec<PersonalDevice> {
    let local = fabric
        .local_device_fabric_identity()
        .map(|(device, _)| device);
    let converged = fabric.devices();
    let nearby = fabric.nearby_device_ids();
    let mut devices: Vec<PersonalDevice> = converged
        .devices()
        .values()
        .map(|device| PersonalDevice {
            id: device.device.as_str().to_owned(),
            label: device.about.label.clone(),
            online: device.online,
            nearby: nearby.contains(&device.device),
            local: local.as_ref() == Some(&device.device),
            display: device.about.capabilities.display,
            wakeable: device
                .presence
                .wake_routes
                .iter()
                .any(|route| route.magic_packet),
            audio_output: device.about.capabilities.audio_output,
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
    /// The person's layout, whose workspaces the reading lists.
    handle: DesktopHandle,
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
    /// Why the last login from here failed, as the daemon said it: written by the login's own
    /// thread, read on every pass.
    refused: Arc<Mutex<String>>,
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
    /// The service publishing what `fabric` , `handle` , `readings` , `chosen` and `handoff` read,
    /// whose presses `pressed` carries to the loop.
    pub(crate) fn new(
        fabric: SharedLink,
        handle: DesktopHandle,
        readings: crate::status::Shared,
        chosen: Rc<RefCell<Option<u64>>>,
        handoff: Rc<RefCell<Option<MediaOffer>>>,
        pressed: Rc<RefCell<Vec<Press>>>,
        handed: Rc<RefCell<Vec<HandOff>>>,
        refused: Arc<Mutex<String>>,
    ) -> Self {
        Self {
            fabric,
            handle,
            readings,
            chosen,
            handoff,
            pressed,
            handed,
            refused,
            published: RefCell::new(None),
        }
    }

    /// The reading as it stands now.
    fn current(&self) -> DevicesReading {
        let mut reading = current(&self.fabric, &self.readings, &self.chosen, &self.handoff);
        // A device the fabric knows this session as is one a person's certificate names it: their
        // device, logged in.
        reading.signed_in = self.fabric.local_device_fabric_identity().is_some();
        reading.workspaces = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .filter(|workspace| !workspace.id().is_nil())
                .map(|workspace| DeviceWorkspace {
                    key: workspace.id().to_string(),
                    name: tilo_session_core::desktop::names::workspace(&workspace.name),
                })
                .collect()
        });
        reading.refused.clone_from(&self.refused.lock_recover());
        reading
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        let mut resource = offer::<DevicesAction>(
            tilo_resource::LiveRow::typed::<Devices>(RESOURCE, t("session-devices"))
                .into_resource(),
        );
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
    ) -> Result<Started, ApiError> {
        if resource != RESOURCE {
            return Err(ApiError::Unknown(ActionAddress::new(resource, action)));
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
            // A login is this computer's daemon's to run, and can take a while: it answers the
            // caller when it lands, refusing in the daemon's own words.
            DevicesAction::LogIn { ticket } => {
                let label = self
                    .fabric
                    .local_device_fabric_identity()
                    .map(|(_, label)| label);
                return Ok(Started::Later(log_in(
                    ticket,
                    label,
                    Arc::clone(&self.refused),
                )));
            }
            // As a person's own computer's first command founds its person: this computer's
            // daemon does, and refuses once a person's certificate names it.
            DevicesAction::Found => {
                return Ok(Started::Later(found(Arc::clone(&self.refused))));
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

/// The agent a login from here makes this computer, when nobody named one: the same word
/// `tilo login` gives it.
const AGENT: &str = "Your agent";

/// Log this device in with `ticket`, as `tilo login <ticket>` does: this computer's daemon runs
/// the login (`devices#logIn`, [`tilo_device_identity::login::LOG_IN`]), kept where the daemon
/// keeps it, so what answers the caller is that daemon's word, its refusal included.
fn log_in(
    ticket: String,
    label: Option<String>,
    refused: Arc<Mutex<String>>,
) -> tilo_actions::remote::Pending<Result<tilo_actions::ActionOutput, ApiError>> {
    crate::landing::off_loop(async move {
        answered(log_in_with(&ticket, label.as_deref()).await, &refused)
    })
}

/// Start a new person on this device ([`tilo_device_identity::login::FOUND`]), as a person's own
/// computer's first command does, keeping why it failed in `refused` as a login does.
fn found(
    refused: Arc<Mutex<String>>,
) -> tilo_actions::remote::Pending<Result<tilo_actions::ActionOutput, ApiError>> {
    crate::landing::off_loop(async move {
        answered(
            tilo_api::daemon::this_computer().and_then(|daemon| daemon.found()),
            &refused,
        )
    })
}

/// What the daemon answered `outcome`, kept in `refused` (cleared when it went through), which
/// the reading shows under the ticket the person typed.
fn answered(
    outcome: Result<(), String>,
    refused: &Mutex<String>,
) -> Result<tilo_actions::ActionOutput, ApiError> {
    *refused.lock_recover() = outcome.as_ref().err().cloned().unwrap_or_default();
    outcome.map_err(ApiError::Failed)?;
    Ok(tilo_actions::ActionOutput::default())
}

/// [`log_in`] through this computer's daemon, as `tilo` calls it: the workspace the login enters,
/// then `devices#logIn`, as `tilo login <ticket>` asks it. A refusal is the daemon's own words.
async fn log_in_with(ticket: &str, label: Option<&str>) -> Result<(), String> {
    let said = |error: anyhow::Error| format!("{error:#}");
    let calls = tilo_api::calls::Calls::open(tilo_api::calls::Reach::default())
        .await
        .map_err(said)?;
    let workspace = calls.founded_workspace().await.map_err(said)?;
    calls
        .act(
            Some(&workspace),
            "devices",
            tilo_device_identity::login::LOG_IN,
            &serde_json::json!({"ticket": ticket.trim(), "agent": AGENT, "label": label}),
            "Log in",
        )
        .await
        .map_err(said)?;
    Ok(())
}

/// Land a session another of the person's devices hands this one, so the now-playing card offers to
/// continue it here; `None` clears it. The devices' reading carries it ([`crate::devices`]), so the
/// panel shows the offer as it shows the rows, and continuing it names the same one back.
pub(crate) fn handoff(shared: &crate::Shared, offer: Option<MediaOffer>) {
    *shared.handoff.borrow_mut() = offer;
}

/// The Devices section as a view of another language reads it (`device.devices`): the person's
/// devices, and what plays here that one of them can take. The section itself publishes the rows
/// the panel draws ([`DevicesReading`]); a view reads the devices it shows and presses the same
/// resource, so the two never say different things.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub(crate) struct DevicesView {
    pub devices: Vec<PersonalDeviceView>,
    /// What plays here, titled, while media continuity is on: what a press on another device
    /// sends.
    #[serde(default)]
    pub playing: Option<String>,
    /// The sound of this device's that a press on a device would send.
    #[serde(default)]
    pub chosen: Option<u64>,
    /// Whether this device holds a person's certificate: logged in, with workspaces of theirs to
    /// show.
    #[serde(default)]
    pub signed_in: bool,
    /// The workspaces this device's shell lists, as the person reads them.
    #[serde(default)]
    pub workspaces: Vec<DeviceWorkspace>,
    /// Why the last login failed, in the words of whoever refused it; empty when it did not.
    #[serde(default)]
    pub refused: String,
}

/// One of the person's devices as a view of another language reads it.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "what the fabric says of this device now: independent facts"
)]
pub(crate) struct PersonalDeviceView {
    /// The fabric id, which names the device in the section's actions.
    pub id: String,
    pub label: String,
    /// It has a live session with this device now.
    pub online: bool,
    /// It is heard on this device's local network, so the two reach each other directly.
    pub nearby: bool,
    /// This machine.
    pub local: bool,
    /// It shows, so its display can be arranged beside this one's.
    pub display: bool,
    /// One of its interfaces wakes on a magic packet.
    pub wakeable: bool,
    /// It plays the sound this device sends it.
    pub audio_output: bool,
}

impl From<&DevicesReading> for DevicesView {
    fn from(reading: &DevicesReading) -> Self {
        Self {
            devices: reading
                .devices
                .iter()
                .map(|device| PersonalDeviceView {
                    id: device.id.clone(),
                    label: device.label.clone(),
                    online: device.online,
                    nearby: device.nearby,
                    local: device.local,
                    display: device.display,
                    wakeable: device.wakeable,
                    audio_output: device.audio_output,
                })
                .collect(),
            playing: reading.playing.clone(),
            chosen: reading.chosen,
            signed_in: reading.signed_in,
            workspaces: reading.workspaces.clone(),
            refused: reading.refused.clone(),
        }
    }
}

/// The Devices section as a view of another language reads it (`device.devices`).
#[must_use]
pub(crate) fn surface() -> tilo_actions::surface::Surface {
    tilo_resource::surface::reading_surface::<DevicesView, DevicesAction>("devices", "Devices")
}

#[cfg(test)]
mod surface_tests {
    use super::*;

    /// A view's fields are the section's own, under the names it publishes them by: a field the
    /// view leaves out is one a view cannot read, and one it invents is a lie.
    #[test]
    fn the_view_reads_the_devices_the_section_publishes() {
        let reading = DevicesReading {
            devices: vec![PersonalDevice {
                id: "device-a".to_owned(),
                label: "Ada's laptop".to_owned(),
                online: true,
                nearby: true,
                local: false,
                display: true,
                wakeable: false,
                audio_output: true,
            }],
            playing: Some("A song".to_owned()),
            chosen: Some(7),
            signed_in: true,
            workspaces: vec![DeviceWorkspace {
                key: "w1".to_owned(),
                name: "Studio".to_owned(),
            }],
            ..DevicesReading::default()
        };
        let section = serde_json::to_value(&reading).expect("the section serialises");
        let view = serde_json::to_value(DevicesView::from(&reading)).expect("the view serialises");
        let section_fields = section.as_object().expect("the section is an object");
        let view_fields = view.as_object().expect("the view is an object");
        // Which fields, not their order: the workspace's serde_json keeps insertion order.
        assert_eq!(
            view_fields
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<&str>>(),
            std::collections::BTreeSet::from([
                "chosen",
                "devices",
                "playing",
                "refused",
                "signed_in",
                "workspaces",
            ]),
            "the view reads the devices, what plays, what would be sent, and the person's login"
        );
        for key in view_fields.keys() {
            assert!(
                section_fields.contains_key(key),
                "{key} is not the section's"
            );
        }
        let workspace = &view["workspaces"][0];
        assert_eq!(
            workspace
                .as_object()
                .expect("a workspace is an object")
                .keys()
                .map(String::from)
                .collect::<Vec<String>>(),
            ["key", "name"],
            "a workspace is named as the shell's layout names it"
        );
        let device = &view["devices"][0];
        let published = &section["devices"][0];
        let published = published.as_object().expect("a device is an object");
        for key in device.as_object().expect("a device is an object").keys() {
            assert!(
                published.contains_key(key),
                "{key} is not a field of the device the fabric reads"
            );
        }
    }

    /// What the `devices` surface declares: the fields a view reads and the verb that makes this
    /// device the person's.
    #[test]
    fn the_surface_publishes_the_login_and_the_workspaces() {
        let published = surface();
        let fields: Vec<&str> = published
            .fields
            .iter()
            .map(|field| field.path.as_str())
            .collect();
        assert_eq!(
            fields,
            [
                "devices",
                "playing",
                "chosen",
                "signed_in",
                "workspaces",
                "refused"
            ]
        );
        assert!(
            published
                .types
                .iter()
                .any(|declared| declared.name == "DeviceWorkspace"
                    && declared
                        .fields
                        .iter()
                        .map(|field| field.name.as_str())
                        .eq(["key", "name"])),
            "the workspaces carry the key and name the shell's layout uses: {:?}",
            published.types
        );
        let log_in = published
            .verbs
            .iter()
            .find(|verb| verb.id == "log-in")
            .expect("the surface publishes log-in");
        assert_eq!(
            log_in
                .params
                .iter()
                .map(|param| param.name.as_str())
                .collect::<Vec<_>>(),
            ["ticket"],
            "a login asks for the ticket another device printed"
        );
    }
}
