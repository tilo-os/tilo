//! The people and devices a host certifies for Files: names for display, and
//! the keys that sign.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use fs_protocol::Ed25519PublicKey;
use tilo_clock::try_unix_now_ns;
use tilo_user_identity::{
    MAX_TRUSTED_IDENTITY_DISPLAY_NAME_BYTES, MAX_TRUSTED_IDENTITY_PROJECTION_BYTES,
    TrustedIdentityProjectionV1, TrustedIdentityProjectionV2,
};

pub const TRUSTED_IDENTITIES_ENV: &str = "TILO_TRUSTED_IDENTITIES";
pub const DEFAULT_TRUSTED_IDENTITIES_PATH: &str =
    "/run/tilo/identity-projection/trusted-identities.json";
pub const MAX_TRUSTED_IDENTITIES_BYTES: u64 = MAX_TRUSTED_IDENTITY_PROJECTION_BYTES;
pub const MAX_IDENTITY_LABEL_BYTES: usize = MAX_TRUSTED_IDENTITY_DISPLAY_NAME_BYTES;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedHuman {
    pub person: Ed25519PublicKey,
    pub display_name: String,
    pub devices: Vec<TrustedDevice>,
    pub agents: Vec<TrustedAgent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedDevice {
    pub device: Ed25519PublicKey,
    pub label: String,
    pub capabilities: tilo_user_identity::DeviceCapabilities,
    pub issued_at_ns: u64,
    pub expires_at_ns: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedAgent {
    pub device: Ed25519PublicKey,
    pub agent_id: String,
    pub label: String,
    pub color: [u8; 3],
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustedHumanDirectory {
    humans: Vec<TrustedHuman>,
}

impl TrustedHumanDirectory {
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = match tilo_files::native::bounded::read(path, MAX_TRUSTED_IDENTITIES_BYTES) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Ok(Self::default()),
            Err(error) if error.kind() == std::io::ErrorKind::FileTooLarge => {
                return Err("Identity directory exceeds its size bound".into());
            }
            Err(error) => return Err(format!("Identity directory unavailable: {error}")),
        };
        let header: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Identity directory is invalid: {error}"))?;
        match header
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
        {
            Some(version)
                if version
                    == u64::from(tilo_user_identity::TRUSTED_IDENTITY_PROJECTION_V1_VERSION) =>
            {
                let projection = serde_json::from_slice(&bytes)
                    .map_err(|error| format!("Identity directory is invalid: {error}"))?;
                Self::from_projection(projection)
            }
            Some(version)
                if version
                    == u64::from(tilo_user_identity::TRUSTED_IDENTITY_PROJECTION_VERSION) =>
            {
                let projection = serde_json::from_slice(&bytes)
                    .map_err(|error| format!("Identity directory is invalid: {error}"))?;
                Self::from_projection_v2(projection)
            }
            _ => Err("Identity directory has an unsupported schema".into()),
        }
    }

    pub fn load_default() -> Result<Self, String> {
        Self::load(&Self::default_path())
    }

    #[must_use]
    pub fn default_path() -> PathBuf {
        std::env::var_os(TRUSTED_IDENTITIES_ENV)
            .filter(|value| !value.is_empty())
            .map_or_else(
                || PathBuf::from(DEFAULT_TRUSTED_IDENTITIES_PATH),
                PathBuf::from,
            )
    }

    pub fn from_projection(projection: TrustedIdentityProjectionV1) -> Result<Self, String> {
        projection
            .validate()
            .map_err(|error| format!("Identity directory is invalid: {error}"))?;
        let mut roots = BTreeSet::new();
        let mut humans = Vec::with_capacity(projection.humans.len());
        for identity in projection.humans {
            ed25519_dalek::VerifyingKey::from_bytes(&identity.root_public_key)
                .map_err(|_| "Identity directory contains an invalid human root key")?;
            if !roots.insert(identity.root_public_key) {
                return Err("Identity directory contains a duplicate human root key".into());
            }
            let display_name = identity.display_name.trim();
            if display_name.is_empty()
                || display_name.len() > MAX_IDENTITY_LABEL_BYTES
                || display_name.chars().any(char::is_control)
            {
                return Err("Identity directory contains an invalid display name".into());
            }
            humans.push(TrustedHuman {
                person: Ed25519PublicKey::from_bytes(identity.root_public_key),
                display_name: display_name.to_string(),
                devices: Vec::new(),
                agents: Vec::new(),
            });
        }
        humans.sort_by(|left, right| {
            left.display_name
                .to_lowercase()
                .cmp(&right.display_name.to_lowercase())
                .then_with(|| left.person.as_bytes().cmp(right.person.as_bytes()))
        });
        Ok(Self { humans })
    }

    pub fn from_projection_v2(projection: TrustedIdentityProjectionV2) -> Result<Self, String> {
        Self::from_projection_v2_at(projection, try_unix_now_ns().unwrap_or(u64::MAX))
    }

    fn from_projection_v2_at(
        projection: TrustedIdentityProjectionV2,
        now_ns: u64,
    ) -> Result<Self, String> {
        projection
            .validate()
            .map_err(|error| format!("Identity directory is invalid: {error}"))?;
        let mut humans = projection
            .humans
            .into_iter()
            .map(|identity| {
                let (agents, devices): (Vec<_>, Vec<_>) = identity
                    .devices
                    .into_iter()
                    .filter(|device| !device.certificate.is_expired_at(now_ns))
                    .partition(|device| device.agent.is_some());
                TrustedHuman {
                    person: Ed25519PublicKey::from_bytes(identity.root_public_key),
                    display_name: identity.display_name,
                    devices: devices
                        .into_iter()
                        .map(|device| TrustedDevice {
                            device: Ed25519PublicKey::from_bytes(
                                device.certificate.device_public_key,
                            ),
                            label: device.certificate.device_label,
                            capabilities: device.certificate.capabilities,
                            issued_at_ns: device.certificate.issued_at_ns,
                            expires_at_ns: device.certificate.expires_at_ns,
                        })
                        .collect(),
                    agents: agents
                        .into_iter()
                        .filter_map(|device| {
                            let agent = device.agent?;
                            Some(TrustedAgent {
                                device: Ed25519PublicKey::from_bytes(
                                    device.certificate.device_public_key,
                                ),
                                agent_id: agent.agent_id,
                                label: agent.label,
                                color: agent.color,
                            })
                        })
                        .collect(),
                }
            })
            .collect::<Vec<_>>();
        humans.sort_by(|left, right| {
            left.display_name
                .to_lowercase()
                .cmp(&right.display_name.to_lowercase())
                .then_with(|| left.person.as_bytes().cmp(right.person.as_bytes()))
        });
        Ok(Self { humans })
    }

    pub fn humans(&self) -> &[TrustedHuman] {
        &self.humans
    }

    /// A label can improve presentation but can never choose authority. The
    /// caller has already supplied the authoritative root key.
    pub fn display_name(&self, person: &Ed25519PublicKey) -> Option<&str> {
        self.human(person).map(|human| human.display_name.as_str())
    }

    pub fn human(&self, person: &Ed25519PublicKey) -> Option<&TrustedHuman> {
        self.humans.iter().find(|human| human.person == *person)
    }

    pub fn display_name_or_fingerprint(&self, person: &Ed25519PublicKey) -> String {
        self.display_name(person)
            .map_or_else(|| fingerprint(person.as_bytes()), str::to_string)
    }

    #[must_use]
    pub fn history_authors(&self) -> ui::history_source::AuthorDirectory {
        let mut authors = ui::history_source::AuthorDirectory::new();
        for human in &self.humans {
            let person = hex::encode(human.person.as_bytes());
            authors.register_person(
                &person,
                ui::history_source::author_for_identity(human.display_name.clone(), &person),
            );
            for agent in &human.agents {
                let [red, green, blue] = agent.color;
                authors.register_agent(
                    &hex::encode(agent.device.as_bytes()),
                    ui::history_source::HistoryAgent {
                        principal: format!("agent:{}", agent.agent_id),
                        label: agent.label.clone(),
                        rgb: (red, green, blue),
                        person: person.clone(),
                    },
                );
            }
        }
        authors
    }
}

pub fn fingerprint(key: &[u8; 32]) -> String {
    format!("{}…{}", hex::encode(&key[..6]), hex::encode(&key[26..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_user_identity::{GovernedHumanIdentityV1, TRUSTED_IDENTITY_PROJECTION_V1_VERSION};

    fn projection(rows: Vec<([u8; 32], &str)>) -> TrustedIdentityProjectionV1 {
        TrustedIdentityProjectionV1 {
            schema_version: TRUSTED_IDENTITY_PROJECTION_V1_VERSION,
            humans: rows
                .into_iter()
                .map(|(root_public_key, display_name)| GovernedHumanIdentityV1 {
                    root_public_key,
                    display_name: display_name.to_string(),
                })
                .collect(),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn identity_projection_is_display_only_and_refuses_malformed_input() {
        {
            let known = ed25519_dalek::SigningKey::from_bytes(&[7; 32])
                .verifying_key()
                .to_bytes();
            let unknown = ed25519_dalek::SigningKey::from_bytes(&[8; 32])
                .verifying_key()
                .to_bytes();
            let directory =
                TrustedHumanDirectory::from_projection(projection(vec![(known, "Ada Lovelace")]))
                    .unwrap();
            let known = Ed25519PublicKey::from_bytes(known);
            let unknown = Ed25519PublicKey::from_bytes(unknown);
            assert_eq!(
                directory.display_name_or_fingerprint(&known),
                "Ada Lovelace"
            );
            assert_eq!(
                directory.display_name_or_fingerprint(&unknown),
                fingerprint(unknown.as_bytes())
            );
            assert_eq!(directory.humans()[0].person, known);
        }

        {
            let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32])
                .verifying_key()
                .to_bytes();
            let duplicate = projection(vec![(root, "Alice"), (root, "Mallory")]);
            assert!(TrustedHumanDirectory::from_projection(duplicate).is_err());
            assert!(
                TrustedHumanDirectory::from_projection(projection(vec![(root, "bad\nname")]))
                    .is_err()
            );
        }

        {
            let first = ed25519_dalek::SigningKey::from_bytes(&[11; 32])
                .verifying_key()
                .to_bytes();
            let second = ed25519_dalek::SigningKey::from_bytes(&[12; 32])
                .verifying_key()
                .to_bytes();
            let directory = TrustedHumanDirectory::from_projection(projection(vec![
                (first, "Alex"),
                (second, "Alex"),
            ]))
            .unwrap();

            assert_eq!(directory.humans().len(), 2);
            assert_eq!(
                directory
                    .human(&Ed25519PublicKey::from_bytes(first))
                    .unwrap()
                    .person,
                Ed25519PublicKey::from_bytes(first)
            );
            assert_eq!(
                directory
                    .human(&Ed25519PublicKey::from_bytes(second))
                    .unwrap()
                    .person,
                Ed25519PublicKey::from_bytes(second)
            );
        }

        {
            let dir = tempfile::tempdir().unwrap();
            assert!(
                TrustedHumanDirectory::load(&dir.path().join("missing.json"))
                    .unwrap()
                    .humans()
                    .is_empty()
            );
            let root = ed25519_dalek::SigningKey::from_bytes(&[10; 32])
                .verifying_key()
                .to_bytes();
            let path = dir.path().join("trusted.json");
            std::fs::write(
                &path,
                serde_json::to_vec(&projection(vec![(root, "Grace Hopper")])).unwrap(),
            )
            .unwrap();
            let loaded = TrustedHumanDirectory::load(&path).unwrap();
            assert_eq!(loaded.humans()[0].display_name, "Grace Hopper");
        }

        {
            let mut projection = TrustedIdentityProjectionV1::empty();
            projection.schema_version += 1;
            assert!(TrustedHumanDirectory::from_projection(projection).is_err());
        }

        {
            let root = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
            let device = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
            let mut certificate = tilo_user_identity::DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: device.verifying_key().to_bytes(),
                device_label: "Ada's laptop".into(),
                capabilities: tilo_user_identity::DeviceCapabilities::member(),
                issued_at_ns: 7,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            };
            tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
            let directory = TrustedHumanDirectory::from_projection_v2(
                tilo_user_identity::TrustedIdentityProjectionV2 {
                    schema_version: tilo_user_identity::TRUSTED_IDENTITY_PROJECTION_VERSION,
                    humans: vec![tilo_user_identity::GovernedHumanIdentityV2 {
                        root_public_key: root.verifying_key().to_bytes(),
                        display_name: "Ada Lovelace".into(),
                        devices: vec![tilo_user_identity::GovernedDeviceIdentityV2 {
                            certificate,
                            agent: None,
                        }],
                    }],
                },
            )
            .unwrap();
            let human = &directory.humans()[0];
            assert_eq!(human.display_name, "Ada Lovelace");
            assert_eq!(human.person.as_bytes(), &root.verifying_key().to_bytes());
            assert_eq!(human.devices[0].label, "Ada's laptop");
            assert_eq!(
                human.devices[0].capabilities,
                tilo_user_identity::DeviceCapabilities::member()
            );
            assert_eq!(
                human.devices[0].device.as_bytes(),
                &device.verifying_key().to_bytes()
            );
        }
    }

    #[test]
    fn agent_devices_name_history_and_never_receive_shares() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
        let person = root.verifying_key().to_bytes();
        let signed = |seed: u8, capabilities| {
            let device = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
            let mut certificate = tilo_user_identity::DeviceCert {
                user_public_key: person,
                device_public_key: device.verifying_key().to_bytes(),
                device_label: "device".into(),
                capabilities,
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
                issuer: None,
            };
            tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
            certificate
        };
        let laptop = signed(42, tilo_user_identity::DeviceCapabilities::member());
        let scribe = signed(43, tilo_user_identity::CertificationPolicy::agent().ceiling);
        let (laptop_key, scribe_key) = (laptop.device_public_key, scribe.device_public_key);
        let directory = TrustedHumanDirectory::from_projection_v2(
            tilo_user_identity::TrustedIdentityProjectionV2 {
                schema_version: tilo_user_identity::TRUSTED_IDENTITY_PROJECTION_VERSION,
                humans: vec![tilo_user_identity::GovernedHumanIdentityV2 {
                    root_public_key: person,
                    display_name: "Ada Lovelace".into(),
                    devices: vec![
                        tilo_user_identity::GovernedDeviceIdentityV2 {
                            certificate: laptop,
                            agent: None,
                        },
                        tilo_user_identity::GovernedDeviceIdentityV2 {
                            certificate: scribe,
                            agent: Some(tilo_user_identity::GovernedAgentIdentityV2 {
                                agent_id: "scribe".into(),
                                label: "Scribe".into(),
                                color: [10, 20, 30],
                            }),
                        },
                    ],
                }],
            },
        )
        .unwrap();
        let human = &directory.humans()[0];
        assert_eq!(
            human
                .devices
                .iter()
                .map(|device| *device.device.as_bytes())
                .collect::<Vec<_>>(),
            [laptop_key],
            "sharing with Ada never reaches her agent"
        );
        assert_eq!(human.agents.len(), 1);

        let authors = directory.history_authors();
        let by_agent = authors.resolve_file_actor(&person, &scribe_key).unwrap();
        assert_eq!(by_agent.name, "Scribe");
        assert_eq!(ui::author_line(&by_agent), "Scribe for Ada Lovelace");
        assert_eq!(by_agent.rgb(), (10, 20, 30));
        let attribution = by_agent.attribution.unwrap();
        assert_eq!(attribution.actor, "agent:scribe");
        assert_eq!(attribution.on_behalf_of, Some(hex::encode(person)));
        let by_person = authors.resolve_file_actor(&person, &laptop_key).unwrap();
        assert_eq!(by_person.name, "Ada Lovelace");
        assert_eq!(by_person.attribution, None);
    }
}
