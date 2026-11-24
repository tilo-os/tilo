//! The Wi-Fi passphrases the person's devices share, kept by this device's daemon in the person's
//! own workspace ([`Devices::keep_wifi`]): Settings shares the passphrase this device learned with
//! the person's other devices, or joins with one they shared. Each passphrase is sealed to every
//! device of theirs ([`Credential`]) and never travels or stays in the clear.
//!
//! The daemon's broker answers Settings with it ([`WifiContinuityServices`]), and a desktop
//! session's broker reaches the same through the device's dispatch ([`PROVIDER`]): its `lookup`
//! answers the passphrase sealed to this device alone, and its `share` takes one sealed to this
//! device alone, so what the dispatch records of either call holds no passphrase either.

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use serde_json::{Map, Value, json};
use tilo_actions::api::{ActionAddress, ActionDescriptor, InputSpec, PlacedAction};
use tilo_actions::contract::{ActionContract, ActionKind, ConfirmationPolicy, RiskLevel};
use tilo_actions::params::{ActionParam, ParamType};
use tilo_actions::remote::{Admitted, Caller, InvokeArgument, InvokeRequest, Outcome, Pending};
use tilo_actions::{ActionFailure, ActionOutput, ExecutionLocus, OperationKind};
use tilo_workspace_service::apps::services::WifiContinuityServices;
use tilo_workspace_service::workspaces::PersonResources;
use tilo_workspace_service::workspaces::person_workspace::devices::{DEVICES, Devices, KeyKind};
use tilo_workspace_service::workspaces::person_workspace::wifi::{Credential, Security, Zeroizing};
use tilo_workspace_service::workspaces::providers::{Providers, Reach};

/// The provider the shared passphrases are answered under on the device's dispatch.
pub const PROVIDER: &str = "tilo.wifi";
/// The resource its actions are on.
pub const RESOURCE: &str = "wifi";
/// The passphrase of a network the person's devices share, sealed to this device alone:
/// `{ssid, security}` answers `{credential}`, the credential's hex or null.
pub const LOOKUP: &str = "lookup";
/// Share a passphrase with every device of the person's: `{credential}`, sealed to this device
/// alone, answers `{ssid}`.
pub const SHARE: &str = "share";

/// The person's shared Wi-Fi passphrases as this device keeps and opens them.
pub(crate) struct Wifi {
    personal: PersonResources,
    key: SigningKey,
}

impl Wifi {
    /// The passphrases kept in `personal`, opened and sealed with this device's `key`.
    pub(crate) fn new(personal: PersonResources, key: SigningKey) -> Arc<Self> {
        Arc::new(Self { personal, key })
    }

    /// Answer the shared passphrases on the device's dispatch, for callers on this device alone.
    pub(crate) fn register(self: &Arc<Self>, providers: &Providers) {
        providers.provide(
            PROVIDER,
            tilo_app_id::apps::SETTINGS,
            Reach::ThisDevice,
            Arc::clone(self) as Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
        );
    }

    /// The passphrase of `ssid` the person's devices share, opened for this device.
    fn password(&self, ssid: &str, security: Security) -> Option<Zeroizing<String>> {
        permitted().ok()?;
        lookup(
            &self.personal.read::<Devices>(DEVICES)?,
            &self.key,
            ssid,
            security,
        )
    }

    /// Share `password` of `ssid` with every device of the person's.
    fn share(&self, ssid: &str, security: Security, password: &str) -> Result<(), String> {
        permitted()?;
        let own = self.key.verifying_key().to_bytes();
        // Sealed to every device of the person's their workspace holds, this one included: a
        // device certified later opens it once a device that knows the passphrase shares it again.
        let recipients = self
            .personal
            .kept_devices()
            .into_iter()
            .filter(|cert| KeyKind::of(cert) == KeyKind::Device)
            .map(|cert| cert.device_public_key)
            .chain(std::iter::once(own));
        let credential = Credential::seal(
            ssid,
            security,
            password,
            own,
            recipients,
            tilo_clock::unix_now_ms(),
        )
        .map_err(|refused| refused.to_string())?;
        let (network, kept) = (credential.network(), credential.to_hex());
        self.personal.apply(DEVICES, move || {
            Devices::KEEP_WIFI.with((network.clone(), kept.clone()))
        });
        Ok(())
    }

    /// `password` of `ssid`, sealed to this device alone: what goes on the dispatch in its place.
    fn sealed_here(
        &self,
        ssid: &str,
        security: Security,
        password: &str,
    ) -> Result<Credential, String> {
        let own = self.key.verifying_key().to_bytes();
        Credential::seal(
            ssid,
            security,
            password,
            own,
            [own],
            tilo_clock::unix_now_ms(),
        )
        .map_err(|refused| refused.to_string())
    }
}

/// Whether the person lets this device share Wi-Fi with their other devices: Wi-Fi continuity
/// under the master switch, each on unless they turned it off.
///
/// # Errors
/// Either switch is off, said as Settings words it.
pub fn permitted() -> Result<(), String> {
    use settings_store::continuity_privacy::{ContinuityCapability, ON_UNLESS_SET};
    let document = settings_store::load();
    let on = |name: &str| match document.get("continuity") {
        None => ON_UNLESS_SET,
        Some(Value::Object(section)) => section
            .get(name)
            .map_or(ON_UNLESS_SET, |value| value.as_bool() == Some(true)),
        Some(_) => false,
    };
    (on("enabled") && on(ContinuityCapability::WifiSettings.key()))
        .then_some(())
        .ok_or_else(|| "Wi-Fi continuity is turned off in this device's settings".to_owned())
}

/// The newest passphrase of `ssid` among `devices`' that opens for `key`'s device.
fn lookup(
    devices: &Devices,
    key: &SigningKey,
    ssid: &str,
    security: Security,
) -> Option<Zeroizing<String>> {
    let mut kept = devices
        .wifi
        .iter()
        .filter_map(|(_, network)| Credential::from_hex(&network.credential))
        .filter(|credential| credential.ssid == ssid && credential.security == security)
        .collect::<Vec<_>>();
    kept.sort_by_key(|credential| std::cmp::Reverse(credential.sealed_unix_ms));
    kept.iter().find_map(|credential| credential.open(key).ok())
}

/// The security Settings names, as a passphrase is kept for it.
const fn security(security: runtime_collab::PortableWifiSecurity) -> Security {
    match security {
        runtime_collab::PortableWifiSecurity::Wpa2 => Security::Wpa2,
        runtime_collab::PortableWifiSecurity::Wpa3 => Security::Wpa3,
    }
}

impl WifiContinuityServices for Wifi {
    fn command(
        &self,
        command: runtime_collab::WifiContinuityCommand,
    ) -> runtime_collab::WifiContinuityReply {
        use runtime_collab::{WifiContinuityCommand as Command, WifiContinuityReply as Reply};
        let rejected = |reason: String| Reply::Rejected {
            reason: reason.chars().take(128).collect(),
        };
        match command {
            Command::Lookup {
                ssid,
                security: mode,
            } => self
                .password(&ssid, security(mode))
                .map_or(Reply::NotFound, |password| Reply::Password {
                    password: password.to_string(),
                }),
            Command::Share {
                ssid,
                security: mode,
                password,
            } => {
                let password = Zeroizing::new(password);
                match self.share(&ssid, security(mode), &password) {
                    Ok(()) => Reply::Shared { ssid },
                    Err(reason) => rejected(reason),
                }
            }
            // Where this computer's network wakes it and what it saved are its desktop session's
            // to read.
            Command::NetworkWake { .. } | Command::ShareSavedProfile { .. } => {
                rejected("this device's desktop session reads its saved networks".to_owned())
            }
        }
    }
}

/// The person's Wi-Fi continuity as the apps attached to this daemon reach it: the passphrases
/// are the daemon's, and what this computer's networks wake and saved is asked of the desktop
/// session serving its seat.
pub(crate) struct WithSeat {
    pub(crate) daemon: Arc<Wifi>,
    pub(crate) seat: Arc<dyn WifiContinuityServices>,
}

impl WifiContinuityServices for WithSeat {
    fn command(
        &self,
        command: runtime_collab::WifiContinuityCommand,
    ) -> runtime_collab::WifiContinuityReply {
        use runtime_collab::WifiContinuityCommand as Command;
        match command {
            Command::NetworkWake { .. } | Command::ShareSavedProfile { .. } => {
                self.seat.command(command)
            }
            Command::Lookup { .. } | Command::Share { .. } => self.daemon.command(command),
        }
    }
}

/// The two actions, as the device's dispatch lists them.
fn descriptors() -> Vec<ActionDescriptor> {
    let descriptor = |action: &str, label: &str, kind, params| ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new(RESOURCE, action),
        resource_kind: "tilo.wifi".to_owned(),
        resource_label: "Wi-Fi".to_owned(),
        action_label: label.to_owned(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        locus: ExecutionLocus::ClientLocal,
        kind: OperationKind::Effect,
        contract: ActionContract {
            kind,
            risk: RiskLevel::Low,
            confirmation: ConfirmationPolicy::None,
            ..ActionContract::default()
        },
        input: InputSpec::None,
        params,
        view: None,
    };
    vec![
        descriptor(
            LOOKUP,
            "Find a shared Wi-Fi passphrase",
            ActionKind::Read,
            vec![
                ActionParam::required("ssid", ParamType::Text, "The network's name"),
                ActionParam::required("security", ParamType::Text, "wpa2 or wpa3"),
            ],
        ),
        descriptor(
            SHARE,
            "Share a Wi-Fi passphrase with your devices",
            ActionKind::Outside,
            vec![ActionParam::required(
                "credential",
                ParamType::Text,
                "The passphrase, sealed to this device",
            )],
        ),
    ]
}

impl tilo_actions::remote::Admits for Wifi {
    const PROVIDER: &'static str = PROVIDER;
    const APP: &'static str = tilo_app_id::apps::SETTINGS;

    fn rows(&self) -> Vec<PlacedAction> {
        tilo_actions::remote::admitted::top_level(descriptors())
    }

    fn start(
        &self,
        _caller: &Caller,
        request: &InvokeRequest,
        _admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        let address = &request.target.address;
        if address.resource != RESOURCE {
            return Err(format!(
                "this device publishes no resource `{}`",
                address.resource
            ));
        }
        let InvokeArgument::Json(arguments) = &request.argument else {
            return Err("a Wi-Fi action takes its arguments as an object".to_owned());
        };
        let text = |name: &str| arguments.get(name).and_then(Value::as_str);
        let answered = match address.action.as_str() {
            LOOKUP => {
                let (Some(ssid), Some(security)) =
                    (text("ssid"), text("security").and_then(Security::named))
                else {
                    return Err("a lookup names a network and its security".to_owned());
                };
                self.password(ssid, security)
                    .map(|password| self.sealed_here(ssid, security, &password))
                    .transpose()
                    .map(|credential| json!({"credential": credential.map(|c| c.to_hex())}))
            }
            SHARE => {
                let credential = text("credential")
                    .and_then(Credential::from_hex)
                    .ok_or("a share carries a passphrase sealed to this device")?;
                credential
                    .open(&self.key)
                    .map_err(|refused| refused.to_string())
                    .and_then(|password| {
                        self.share(&credential.ssid, credential.security, &password)
                    })
                    .map(|()| json!({"ssid": credential.ssid}))
            }
            other => return Err(format!("Wi-Fi has no action `{other}`")),
        };
        Ok(Pending::ready(
            answered
                .map(|value| ActionOutput {
                    value: Some(value),
                    ..ActionOutput::default()
                })
                .map_err(|message| {
                    Box::new(ActionFailure {
                        code: "refused".to_owned(),
                        message,
                        details: None,
                        retryable: false,
                    })
                }),
        ))
    }
}

/// The arguments of a [`LOOKUP`] of `ssid`.
#[must_use]
pub fn lookup_arguments(ssid: &str, security: Security) -> Map<String, Value> {
    let mut arguments = Map::new();
    arguments.insert("ssid".to_owned(), json!(ssid));
    arguments.insert("security".to_owned(), json!(security.name()));
    arguments
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_workspace_service::workspaces::person_workspace::devices::Network;

    fn kept(credentials: &[&Credential]) -> Devices {
        let mut devices = Devices::default();
        for credential in credentials {
            devices.wifi.push(
                tilo_resource::Id::from_raw(credential.network()),
                Network {
                    credential: credential.to_hex(),
                },
            );
        }
        devices
    }

    #[test]
    fn a_shared_passphrase_opens_on_the_devices_it_was_sealed_to() {
        let desktop = SigningKey::from_bytes(&[1; 32]);
        let laptop = SigningKey::from_bytes(&[2; 32]);
        let stranger = SigningKey::from_bytes(&[3; 32]);
        let shared = Credential::seal(
            "Home",
            Security::Wpa3,
            "correct horse",
            desktop.verifying_key().to_bytes(),
            [
                desktop.verifying_key().to_bytes(),
                laptop.verifying_key().to_bytes(),
            ],
            5,
        )
        .expect("a passphrase seals");
        let devices = kept(&[&shared]);
        assert_eq!(
            lookup(&devices, &laptop, "Home", Security::Wpa3)
                .as_deref()
                .map(String::as_str),
            Some("correct horse"),
            "the laptop joins with what the desktop shared"
        );
        assert_eq!(
            lookup(&devices, &stranger, "Home", Security::Wpa3),
            None,
            "a device it was not sealed to opens nothing"
        );
        assert_eq!(
            lookup(&devices, &laptop, "Home", Security::Wpa2),
            None,
            "a passphrase is the network's of the security it was shared for"
        );
    }

    /// A call of the provider on the device's dispatch, as the person on this device asks it.
    async fn ask(
        wifi: &Wifi,
        caller: &Caller,
        action: &str,
        arguments: Map<String, Value>,
    ) -> Result<Value, String> {
        use tilo_actions::remote::{ActionProvider as _, ReceiptStatus, RemoteActionTarget};
        let receipt = wifi
            .invoke(
                caller,
                InvokeRequest {
                    invocation: tilo_actions::InvocationId::new(),
                    target: RemoteActionTarget {
                        provider: PROVIDER.to_owned(),
                        address: ActionAddress::new(RESOURCE, action),
                    },
                    argument: InvokeArgument::Json(arguments),
                    context: None,
                    confirmation: None,
                    revision: None,
                    on_behalf_of: None,
                    wait_ms: None,
                    appearance: None,
                    signed: None,
                },
            )
            .await
            .expect("the provider answers")
            .map_err(|error| error.to_string())?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
            ReceiptStatus::Failed { error } => Err(error.message),
            other => Err(format!("{other:?}")),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn settings_shares_a_passphrase_and_this_device_joins_with_it() {
        let _home = tilo_test_env::TestHome::new().expect("a home");
        let dir = tempfile::tempdir().expect("a state directory");
        let (key, cert) = tilo_workspace_service::workspaces::fixtures::governor();
        let host =
            tilo_workspace_service::workspaces::fixtures::test_host(dir.path(), &key, &cert).await;
        let workspaces = host.workspaces().clone();
        let own = tilo_workspace_service::workspaces::person_workspace::found(&workspaces)
            .await
            .expect("the person's own workspace");
        let personal = PersonResources::default();
        personal.attach(
            std::sync::Arc::new(workspaces),
            &tokio::runtime::Handle::current(),
        );
        let wifi = Wifi::new(personal.clone(), key.clone());
        let caller =
            Caller::authenticated(cert.user_public_key, cert.device_public_key).in_workspace(own);

        // Settings hands the passphrase over sealed to this device alone.
        let sealed = wifi
            .sealed_here("Home", Security::Wpa2, "a passphrase")
            .expect("a passphrase seals");
        assert!(
            !sealed.to_hex().contains(&hex::encode("a passphrase")),
            "what crosses the dispatch holds no passphrase"
        );
        let mut share = Map::new();
        share.insert("credential".to_owned(), json!(sealed.to_hex()));
        assert_eq!(
            ask(&wifi, &caller, SHARE, share).await,
            Ok(json!({"ssid": "Home"}))
        );

        // The person's workspace keeps it, and this device joins with it once it is there.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let answer = loop {
            let answer = ask(
                &wifi,
                &caller,
                LOOKUP,
                lookup_arguments("Home", Security::Wpa2),
            )
            .await
            .expect("a lookup answers");
            if !answer["credential"].is_null() || std::time::Instant::now() > deadline {
                break answer;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        let credential = answer["credential"]
            .as_str()
            .and_then(Credential::from_hex)
            .expect("the shared passphrase comes back sealed");
        assert_eq!(
            credential.open(&key).as_deref().map(String::as_str),
            Ok("a passphrase")
        );
        assert!(
            credential.open(&SigningKey::from_bytes(&[5; 32])).is_err(),
            "what the lookup answers opens on this device alone"
        );
        let kept = personal
            .read::<Devices>(DEVICES)
            .expect("the devices record");
        assert!(
            kept.wifi
                .iter()
                .all(|(_, network)| !network.credential.contains(&hex::encode("a passphrase"))),
            "the person's workspace keeps no passphrase in the clear"
        );
        assert_eq!(
            ask(
                &wifi,
                &caller,
                LOOKUP,
                lookup_arguments("Nowhere", Security::Wpa2)
            )
            .await,
            Ok(json!({"credential": null})),
            "a network nobody shared has no passphrase"
        );
        assert!(
            ask(&wifi, &caller, SHARE, Map::new()).await.is_err(),
            "a share carries a sealed passphrase"
        );
    }
}
