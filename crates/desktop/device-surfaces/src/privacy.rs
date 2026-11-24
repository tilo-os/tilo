//! What the devices of this machine are doing with the person's privacy, as a view of another
//! language reads it: the camera, the microphone, the screen and the location, whether the
//! machine has each one, whether a switch blocks it, what uses it now and who was answered what
//! about it.
//!
//! The reading is the session's own: the switches are the machine's, the uses are the capture
//! streams `PipeWire` attributes to the program behind them and the captures the session's portal
//! brokers, and the answers are the person's, kept in their own workspace and following them to
//! their other devices. A use is always named by the program it is, never by a name the program
//! gave itself.

use serde::{Deserialize, Serialize};

/// One device of the machine a person is asked about, and the application leaves it alone.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    /// The cameras (`/dev/video*`).
    #[default]
    Camera,
    /// The microphones (the `PipeWire` sources).
    Microphone,
    /// The picture of the screen: a screenshot or a screen share.
    Screen,
    /// Where the device is, asked of the platform each time.
    Location,
    /// The keyboard and pointer: injecting input, or capturing it.
    Input,
    /// The sound the machine plays.
    SystemAudio,
}

/// Whether the machine's own switch lets its camera or microphone be used.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum SwitchState {
    /// The machine says nothing about one: a desktop without the switch, or one this person may
    /// not read.
    #[default]
    Unavailable,
    /// Nothing blocks it.
    Allowed,
    /// A switch in software blocks it.
    SoftwareBlocked,
    /// A switch on the machine itself blocks it.
    HardwareBlocked,
}

/// How long an answer the person gave stands.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Standing {
    /// For one use.
    #[default]
    Once,
    /// While this session lasts.
    Session,
    /// Until a moment, in Unix milliseconds.
    Until,
    /// Until the person takes it back.
    UntilRevoked,
}

/// One device's posture on this machine, as a person reads it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Posture {
    /// Which device it is about.
    pub capability: Capability,
    /// Whether the machine has it at all.
    pub present: bool,
    /// Whether the machine's own switch blocks it.
    pub switch: SwitchState,
    /// Whether the microphone is muted at the system's mixer.
    pub muted: bool,
    /// Whether the mix says whether it is muted: a machine without a microphone says nothing.
    pub mute_known: bool,
    /// Whether this session can say which application uses it now. A device the platform asks
    /// about as each use is not one the session sees: it says so rather than count no use.
    pub watched: bool,
    /// What uses it now.
    pub in_use: Vec<InUse>,
}

/// One application using a device of this machine now.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct InUse {
    /// The application, as the session certified it.
    pub app: String,
    /// What people call it, from the device's own lists.
    pub name: String,
}

/// One answer the person gave about a device of theirs, and how long it stands.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Answer {
    /// The answer as the resource it is: the consent the session's portal publishes
    /// (`portal:consent:…`), whose `forget` takes it back.
    #[serde(rename = "$address", default)]
    pub address: tilo_resource::Handle<ConsentRow>,
    /// The application it answers for.
    pub app: String,
    /// What people call it, from the device's own lists.
    pub name: String,
    /// Which device it is about.
    pub capability: Capability,
    /// Whether the person allowed it, rather than refused it.
    pub allowed: bool,
    /// How long the answer stands.
    pub standing: Standing,
    /// The moment it ends at, in Unix milliseconds, when it is `until`; else `0`.
    pub until_unix_ms: i64,
    /// The device of the person's the answer stands on, as its certificate names it.
    pub device: String,
    /// Whether that device is this one.
    pub this_device: bool,
}

/// One application that stored or read the person's saved passwords and keys.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct SecretApp {
    /// The application, as the secret store names it (`binary:/usr/bin/chromium`).
    pub id: String,
    /// What people call it, from the device's own lists.
    pub name: String,
    /// Whether it may use the saved passwords and keys now.
    pub allowed: bool,
    /// How many times it used the store.
    pub uses: u64,
    /// How many saved items it keeps.
    pub items: u32,
}

/// The applications that use this device's saved passwords and keys.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct SavedSecrets {
    /// Each application, in the order the store lists them.
    pub apps: Vec<SecretApp>,
    /// Why the store cannot be listed, when it cannot: an empty list then means unknown, never
    /// that no application uses it.
    #[serde(default)]
    pub read_error: Option<String>,
}

/// Whether a protection of this device is on, off, or cannot be read here.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Protection {
    /// The device cannot say, or this is not a device it can be read on.
    #[default]
    Unknown,
    /// It is in force.
    On,
    /// It is not in force.
    Off,
}

/// The firewall in force on this device.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum FirewallInForce {
    /// The firewall could not be read.
    #[default]
    Unknown,
    /// `firewalld` serves it.
    Firewalld,
    /// The kernel's own ruleset carries it.
    Nftables,
    /// Nothing filters the device's traffic.
    Off,
}

/// What protects this device itself, apart from any workspace: its disk, its firmware and its
/// firewall.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Security {
    /// Whether the disk this system runs from is encrypted.
    pub disk_encryption: Protection,
    /// Whether the firmware verifies what it boots.
    pub secure_boot: Protection,
    /// The firewall in force.
    pub firewall: FirewallInForce,
    /// Why the firewall could not be read, when it could not.
    #[serde(default)]
    pub firewall_error: Option<String>,
}

/// What this machine does with the person's privacy now, as one reads it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Reading {
    /// The camera, the microphone, the screen and the location, in that order.
    pub capabilities: Vec<Posture>,
    /// What the person answered about the devices of theirs, on any of their devices, oldest
    /// first.
    pub answers: Vec<Answer>,
    /// The applications that use this device's saved passwords and keys, when the store was read:
    /// a reading that leaves it out says nothing of them.
    #[serde(default)]
    pub secrets: Option<SavedSecrets>,
    /// What protects this device itself, when it was read: a reading that leaves it out says
    /// nothing of it.
    #[serde(default)]
    pub security: Option<Security>,
}

/// What this machine does with the person's privacy, as a view of another language reads it.
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Reading, PrivacyAction>("privacy", "Privacy")
}

/// What the person may change about this device's privacy from the reading itself.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.session.privacy")]
pub enum PrivacyAction {
    /// Let an application use the saved passwords and keys again, or stop it. What it saved stays
    /// until it is forgotten.
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    SetSecretsAccess {
        #[param("The application identity the secret store recorded")]
        app: String,
        #[param("Whether the application may use saved secrets")]
        allowed: bool,
    },
    /// Remove an application's saved secrets and its record.
    #[effect(kind = "outside")]
    #[effect(own_device)]
    ForgetSecrets {
        #[param("The application identity the secret store recorded")]
        app: String,
    },
}

/// One answer the person gave, as the resource it is: the consent the session's portal publishes
/// for it, which is what its `forget` is called on.
#[derive(tilo_resource::Row)]
#[row(kind = "portal.consent", label = "Consent")]
pub struct ConsentRow;

/// What a kept device answer offers.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.portal.consent")]
pub enum ConsentEffect {
    /// Take back what the application was allowed or refused
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    Forget,
}

/// A kept device answer as a view of another language calls it: the `forget` its reading's
/// address offers.
#[must_use]
pub fn consent_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::row_surface::<ConsentRow, ConsentEffect>()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reading writes and reads as the surface declares it: what the session publishes is
    /// what a view of another language parses.
    #[test]
    fn the_reading_is_the_shape_the_surface_declares() {
        let reading = Reading {
            capabilities: vec![Posture {
                capability: Capability::Microphone,
                present: true,
                switch: SwitchState::SoftwareBlocked,
                muted: true,
                mute_known: true,
                watched: true,
                in_use: vec![InUse {
                    app: "so.tilo.meet".into(),
                    name: "Meet".into(),
                }],
            }],
            answers: vec![Answer {
                address: tilo_resource::Handle::new(
                    "portal:consent:Laptop\u{1f}so.tilo.recorder\u{1f}camera",
                ),
                app: "so.tilo.recorder".into(),
                name: "Recorder".into(),
                capability: Capability::Camera,
                allowed: true,
                standing: Standing::UntilRevoked,
                until_unix_ms: 0,
                device: "Laptop".into(),
                this_device: true,
            }],
            secrets: Some(SavedSecrets {
                apps: vec![SecretApp {
                    id: "binary:/usr/bin/chromium".into(),
                    name: "Chromium".into(),
                    allowed: true,
                    uses: 42,
                    items: 3,
                }],
                read_error: None,
            }),
            security: Some(Security {
                disk_encryption: Protection::On,
                secure_boot: Protection::Off,
                firewall: FirewallInForce::Firewalld,
                firewall_error: None,
            }),
        };
        let json = serde_json::to_value(&reading).expect("a reading writes as JSON");
        assert_eq!(
            json["capabilities"][0]["capability"],
            serde_json::json!("microphone")
        );
        assert_eq!(
            json["answers"][0]["standing"],
            serde_json::json!("until-revoked")
        );
        let read: Reading = serde_json::from_value(json).expect("the reading reads back");
        assert_eq!(read, reading);
        let surface = surface();
        assert_eq!(surface.kind, "privacy");
        assert_eq!(surface.label, "Privacy");
        let verbs: Vec<&str> = surface.verbs.iter().map(|verb| verb.id.as_str()).collect();
        assert_eq!(
            verbs,
            vec!["set-secrets-access", "forget-secrets"],
            "the reading offers what the person may change of it"
        );
    }

    /// An answer's address is the consent resource the pane calls `forget` on: the surface says
    /// which kind the row is, and that kind offers the one verb that takes an answer back.
    #[test]
    fn an_answer_is_the_consent_its_forget_is_called_on() {
        let answer = Answer {
            address: tilo_resource::Handle::new(
                "portal:consent:Laptop\u{1f}so.tilo.recorder\u{1f}camera",
            ),
            app: "so.tilo.recorder".into(),
            ..Answer::default()
        };
        let json = serde_json::to_value(&answer).expect("the answer writes as JSON");
        assert_eq!(
            json["$address"],
            serde_json::json!("portal:consent:Laptop\u{1f}so.tilo.recorder\u{1f}camera"),
            "the address is where the answer's own verb is called"
        );
        let consent = consent_surface();
        assert_eq!(consent.kind, "portal.consent");
        let verbs: Vec<&str> = consent.verbs.iter().map(|verb| verb.id.as_str()).collect();
        assert_eq!(verbs, vec!["forget"]);
    }
}
