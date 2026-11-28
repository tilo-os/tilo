//! The person's speakers, published as `session:speakers` (`device.speakers`).
//!
//! The reading is what Settings' speakers page shows — the person's devices with their outputs and
//! microphones, the groups they form, each group's calibration run — and the verbs are the
//! commands the page sends, carried out by the speaker service on the device fabric
//! ([`tilo_speaker_service::settings`]). A tilolang view reads this resource and calls its actions,
//! so Settings' page, the shell and an agent change the same groups, and nothing holds a copy of
//! them.

use std::cell::RefCell;

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_service::{Service, Started, decode, offer, reading};
use tilo_speaker_service::published::{SpeakerEffect, Speakers};

use crate::i18n::t;
use crate::platform::fabric::SharedLink;

/// The resource the person's speakers are published under.
pub(crate) const RESOURCE: &str = "session:speakers";

/// The speakers as a declared type: the reading, and the verbs that change the groups.
#[derive(tilo_resource::Row)]
#[row(kind = "session.speakers", label = "Speakers")]
pub(crate) struct SpeakersRoot;

/// The speakers as the session serves them: the fabric the groups live in, and the reading last
/// reported, so a pass that changed nothing is no revision.
pub(crate) struct Served {
    fabric: SharedLink,
    published: RefCell<Option<Speakers>>,
}

impl Served {
    /// The service publishing what `fabric` says of the person's speakers.
    pub(crate) fn new(fabric: SharedLink) -> Self {
        Self {
            fabric,
            published: RefCell::new(None),
        }
    }

    /// The reading as it stands now.
    fn current(&self) -> Speakers {
        tilo_speaker_service::published::reading(self.fabric.as_ref())
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![offer::<SpeakerEffect>(
            tilo_resource::LiveRow::typed::<SpeakersRoot>(RESOURCE, t("session-speakers"))
                .into_resource(),
        )]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| {
            let published = self.published.borrow().clone();
            reading(&published.unwrap_or_else(|| self.current()))
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
        let effect = decode::<SpeakerEffect>(&self.resources(), resource, action, arguments)?;
        let command = effect.command().map_err(ApiError::InvalidArguments)?;
        if !command.valid() {
            return Err(ApiError::InvalidArguments(
                "the speakers take no group, device or name of that shape".to_owned(),
            ));
        }
        match tilo_speaker_service::settings::handle(&self.fabric, command) {
            runtime_collab::SpeakerReply::Done { .. } => {
                // The next reader reads the groups as the command left them.
                self.published.borrow_mut().take();
                Ok(Started::Done(tilo_actions::ActionOutput::default()))
            }
            runtime_collab::SpeakerReply::Rejected { reason } => Err(ApiError::Failed(reason)),
            runtime_collab::SpeakerReply::Snapshot { .. } => {
                // Only a snapshot command answers with a snapshot, and none of the verbs is one.
                Err(ApiError::Failed(
                    "the speakers answered a question nothing asked".to_owned(),
                ))
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::fabric::{
        DeviceAbout, DeviceId, DeviceKind, EndpointSelector, FabricCapabilities,
        PersonalFabricAction, PersonalFabricSendError,
    };
    use tilo_speaker_service::devices::{Record, Speakers};
    use tilo_user_identity::DeviceCert;
    use tilo_workspace_service::workspaces::person_workspace::devices::Devices;
    use tilo_workspace_service::workspaces::person_workspace::speaker::SpeakerEndpoint;

    use tilo_peer_state::fixture::member_cert;
    /// This laptop's person and device key, as the record keeps them.
    fn laptop() -> ([u8; 32], ed25519_dalek::SigningKey, DeviceCert) {
        let root = ed25519_dalek::SigningKey::from_bytes(&[21; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let cert = member_cert(&root, &key, "This laptop");
        (root.verifying_key().to_bytes(), key, cert)
    }

    /// This laptop's devices, with the one output it publishes: the record's, and no copy beside
    /// it.
    struct Fabric {
        local: DeviceId,
        person: [u8; 32],
        record: Devices,
    }

    impl Fabric {
        fn laptop() -> Self {
            let (person, key, cert) = laptop();
            let local =
                DeviceId::new(hex::encode(key.verifying_key().to_bytes())).expect("a device id");
            let mut record =
                tilo_resource::Replica::<Devices>::new().in_app("so.tilo.model", "Model");
            record
                .apply(Devices::keeping(&cert).expect("the device certificate encodes"))
                .expect("the device is kept");
            let about = DeviceAbout {
                label: "This laptop".into(),
                kind: DeviceKind::Laptop,
                capabilities: FabricCapabilities {
                    audio_output: true,
                    audio_group: true,
                    ..FabricCapabilities::default()
                },
            };
            record
                .apply(Devices::KEEP_FACT.with((
                    local.as_str().to_owned(),
                    hex::encode(postcard::to_allocvec(&cert).expect("the certificate encodes")),
                    tilo_network_actor::fabric::ABOUT_FACT.to_owned(),
                    hex::encode(postcard::to_allocvec(&about).expect("what it is encodes")),
                )))
                .expect("the device says what it is");
            let endpoints = vec![SpeakerEndpoint {
                selector: EndpointSelector::mint(&local, "alsa_output.analog-stereo").to_hex(),
                microphone: false,
                label: "Speakers".into(),
                is_default: true,
            }];
            record
                .apply(Devices::SAY_SPEAKER_ENDPOINTS.with((
                    local.as_str().to_owned(),
                    hex::encode(postcard::to_allocvec(&endpoints).expect("endpoints encode")),
                )))
                .expect("the device offers its output");
            Self {
                local,
                person,
                record: record.state().expect("the record reads"),
            }
        }
    }

    impl Record for Fabric {
        fn record(&self) -> Option<Devices> {
            Some(self.record.clone())
        }

        fn apply(&self, _: tilo_resource::Invoke<Devices>) -> Result<(), String> {
            Ok(())
        }

        fn person(&self) -> [u8; 32] {
            self.person
        }
    }

    impl Speakers for Fabric {
        fn online_device_ids(&self) -> std::collections::BTreeSet<DeviceId> {
            std::collections::BTreeSet::from([self.local.clone()])
        }

        fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)> {
            Some((self.local.clone(), "This laptop".into()))
        }

        fn try_send_personal_fabric_action(
            &self,
            _: &DeviceId,
            _: PersonalFabricAction,
        ) -> Result<[u8; 16], PersonalFabricSendError> {
            Err(PersonalFabricSendError::Rejected("no transport".into()))
        }

        fn require_media(&self) -> Result<(), String> {
            Ok(())
        }

        fn known(&self) -> tilo_speaker_service::devices::Listing {
            let online = self.online_device_ids();
            tilo_speaker_service::devices::known_from(
                &self.record,
                &self.person,
                Some(&self.local),
                &online,
            )
        }
    }

    #[test]
    fn the_speakers_are_the_devices_reading_and_its_verbs_refuse_what_cannot_be_a_group() {
        let mut served = Served::new(std::sync::Arc::new(Fabric::laptop()));
        let reading = served.read(RESOURCE).expect("the speakers are published");
        assert_eq!(reading["devices"][0]["label"], "This laptop");
        assert_eq!(
            reading["devices"][0]["outputs"].as_array().map(Vec::len),
            Some(1),
            "the output this laptop publishes is what a set can take"
        );
        assert!(served.poll(), "the first pass publishes the reading");
        assert!(!served.poll(), "a pass that changed nothing is no revision");

        let mut identify = Map::new();
        identify.insert("device".into(), Value::from(hex::encode([9; 32])));
        identify.insert("output".into(), Value::from("not an output"));
        let refused = served.invoke(RESOURCE, "identify", &identify).err();
        assert!(
            matches!(refused, Some(ApiError::InvalidArguments(_))),
            "an output that is no selector never reaches the speakers: {refused:?}"
        );
        assert!(
            matches!(
                served
                    .invoke("session:elsewhere", "identify", &identify)
                    .err(),
                Some(ApiError::Unknown(_))
            ),
            "the verbs are the speakers' own"
        );
    }
}
