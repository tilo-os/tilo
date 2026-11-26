//! The Wi-Fi passphrases the person's devices share, kept by this device's daemon in the person's
//! own workspace ([`Devices::keep_wifi`]): a saved network's `share-with-devices` shares the
//! passphrase this device learned with the person's other devices, and a join without one uses
//! the passphrase they shared. Each passphrase is sealed to every
//! device of theirs ([`Credential`]): it is opened only to hand it to this computer's network
//! settings, and never leaves this device or stays anywhere in the clear.
//!
//! The person's own session on this device reaches it through the device's dispatch
//! ([`PROVIDER`]), which hands the passphrase to the system's network settings: its `lookup`
//! opens a shared passphrase here and answers it, and its `share` takes one and seals it to each
//! of the person's devices. The device key that opens and seals them never leaves the daemon.
//! Both answer only the person themselves, never an app or an agent, and both are secret
//! answers ([`tilo_actions::SECRET_ANSWER_TAG`]): no record of either call keeps the passphrase.

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use serde_json::{Map, Value, json};
use tilo_actions::api::{ActionAddress, ActionDescriptor, InputSpec, PlacedAction};
use tilo_actions::contract::{ActionContract, ActionKind, ConfirmationPolicy, RiskLevel};
use tilo_actions::params::{ActionParam, ParamType};
use tilo_actions::remote::{Admitted, Caller, InvokeArgument, InvokeRequest, Outcome, Pending};
use tilo_actions::{ActionFailure, ActionOutput, ExecutionLocus, OperationKind};
use tilo_workspace_service::workspaces::person_workspace::devices::{DEVICES, Devices, KeyKind};
use tilo_workspace_service::workspaces::person_workspace::wifi::{Credential, Security, Zeroizing};
use tilo_workspace_service::workspaces::providers::{Providers, Reach};
use tilo_workspace_service::workspaces::{FollowedRecord, Workspaces, person_workspace};

/// The provider the shared passphrases are answered under on the device's dispatch.
pub const PROVIDER: &str = "tilo.wifi";
/// The resource its actions are on.
pub const RESOURCE: &str = "wifi";
/// The passphrase of a network the person's devices share, opened on this device:
/// `{ssid, security}` answers `{password}`, the passphrase or null.
pub const LOOKUP: &str = "lookup";
/// Share a passphrase with every device of the person's: `{ssid, security, password}` answers
/// `{ssid}`.
pub const SHARE: &str = "share";

/// Why a caller other than the person themselves is refused a passphrase.
pub const THE_PERSONS_OWN: &str =
    "only you, at this computer, get or share a Wi-Fi passphrase: no app or agent does";

/// The person's shared Wi-Fi passphrases as this device keeps and opens them.
pub(crate) struct Wifi {
    /// The workspaces that keep the person's own, where a shared passphrase is written.
    workspaces: Workspaces,
    /// The record of the person's devices there, as it changes.
    devices: FollowedRecord,
    key: SigningKey,
}

impl Wifi {
    /// The passphrases kept in `devices`, the record of the person's devices in their own
    /// workspace that `workspaces` keep, opened and sealed with this device's `key`.
    pub(crate) fn new(
        workspaces: Workspaces,
        devices: FollowedRecord,
        key: SigningKey,
    ) -> Arc<Self> {
        Arc::new(Self {
            workspaces,
            devices,
            key,
        })
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
            &self.devices.state::<Devices>().ok()?,
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
        // A device confined to one workspace (a workspace's VM) is no machine of theirs that
        // joins their networks, and opens none.
        let person = self.workspaces.device_cert().user_public_key;
        let recipients = self
            .devices
            .state::<Devices>()
            .map(|kept| kept.certified_at(&person, tilo_clock::unix_now_ns()))
            .unwrap_or_default()
            .into_iter()
            .filter(|cert| {
                KeyKind::of(cert) == KeyKind::Device && cert.capabilities.workspace.is_none()
            })
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
        let keep = Devices::KEEP_WIFI.with((credential.network(), credential.to_hex()));
        // Kept once this device holds the person's workspace, which it may not yet as it starts.
        let workspaces = self.workspaces.clone();
        tilo_rt::app().spawn(async move {
            if let Err(error) = person_workspace::apply_when_held(&workspaces, DEVICES, keep).await
            {
                tracing::warn!(%error, "a shared Wi-Fi passphrase was not kept");
            }
        });
        Ok(())
    }
}

/// Whether `caller` is the person themselves on this device, as their own session asks: never an
/// app, an agent, or a call an app makes for someone it serves.
///
/// # Errors
/// [`THE_PERSONS_OWN`] for anyone else.
fn the_persons_own(caller: &Caller, admitted: &Admitted) -> Result<(), String> {
    (caller.acting().person_id().is_some()
        && caller.acting().app().is_none()
        && caller.launched_app().is_none()
        && admitted.serving.is_none())
    .then_some(())
    .ok_or_else(|| THE_PERSONS_OWN.to_owned())
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
            // A passphrase goes to its caller and nowhere else: no record keeps it.
            tags: vec![tilo_actions::SECRET_ANSWER_TAG.to_owned()],
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
            vec![
                ActionParam::required("ssid", ParamType::Text, "The network's name"),
                ActionParam::required("security", ParamType::Text, "wpa2 or wpa3"),
                ActionParam::required("password", ParamType::Secret, "The passphrase"),
            ],
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
        caller: &Caller,
        request: &InvokeRequest,
        admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        the_persons_own(caller, admitted)?;
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
        let (Some(ssid), Some(security)) =
            (text("ssid"), text("security").and_then(Security::named))
        else {
            return Err("a Wi-Fi passphrase is of a network and its security".to_owned());
        };
        let answered = match address.action.as_str() {
            LOOKUP => Ok(json!({
                "password": self.password(ssid, security).as_deref()
            })),
            SHARE => {
                let password = text("password").ok_or("a share carries the passphrase")?;
                self.share(ssid, security, password)
                    .map(|()| json!({"ssid": ssid}))
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

/// The arguments of a [`SHARE`] of `password` of `ssid`.
#[must_use]
pub fn share_arguments(ssid: &str, security: Security, password: &str) -> Map<String, Value> {
    let mut arguments = lookup_arguments(ssid, security);
    arguments.insert("password".to_owned(), json!(password));
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
        let (key, cert) = tilo_workspace_service::workspaces::fixtures::hosting_device();
        let host =
            tilo_workspace_service::workspaces::fixtures::test_host(dir.path(), &key, &cert).await;
        let workspaces = host.workspaces().clone();
        let own = tilo_workspace_service::workspaces::person_workspace::found(&workspaces)
            .await
            .expect("the person's own workspace");
        let devices = workspaces.follow_record(&own, DEVICES);
        let wifi = Wifi::new(workspaces.clone(), devices.clone(), key.clone());
        let caller =
            Caller::authenticated(cert.user_public_key, cert.device_public_key).in_workspace(own);

        shares_a_sealed_passphrase(&wifi, &caller, &devices, &key).await;
        refuses_a_passphrase_to_every_other(&wifi, &caller, cert.user_public_key).await;
    }

    /// The person's share is kept sealed in their workspace, and this device joins with it.
    async fn shares_a_sealed_passphrase(
        wifi: &Wifi,
        caller: &Caller,
        devices: &FollowedRecord,
        key: &SigningKey,
    ) {
        assert_eq!(
            ask(
                wifi,
                caller,
                SHARE,
                share_arguments("Home", Security::Wpa2, "a passphrase")
            )
            .await,
            Ok(json!({"ssid": "Home"}))
        );
        // The person's workspace keeps it sealed, and this device joins with it once it is there.
        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(20))
            .expect("a deadline twenty seconds out is representable");
        let answer = loop {
            let answer = ask(
                wifi,
                caller,
                LOOKUP,
                lookup_arguments("Home", Security::Wpa2),
            )
            .await
            .expect("a lookup answers");
            if !answer["password"].is_null() || std::time::Instant::now() > deadline {
                break answer;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        assert_eq!(answer, json!({"password": "a passphrase"}));
        let kept = devices.state::<Devices>().expect("the devices record");
        assert!(
            !kept.wifi.is_empty()
                && kept.wifi.iter().all(|(_, network)| {
                    Credential::from_hex(&network.credential)
                        .is_some_and(|sealed| sealed.open(key).is_ok())
                        && !network.credential.contains(&hex::encode("a passphrase"))
                }),
            "the person's workspace keeps the passphrase sealed to their devices, never in the clear"
        );
    }

    /// Nobody but the person themselves gets or gives one: not an agent acting for them, not
    /// an app, and not an app serving them.
    async fn refuses_a_passphrase_to_every_other(wifi: &Wifi, caller: &Caller, person: [u8; 32]) {
        assert_eq!(
            ask(
                wifi,
                caller,
                LOOKUP,
                lookup_arguments("Nowhere", Security::Wpa2)
            )
            .await,
            Ok(json!({"password": null})),
            "a network nobody shared has no passphrase"
        );
        assert!(
            ask(
                wifi,
                caller,
                SHARE,
                lookup_arguments("Home", Security::Wpa2)
            )
            .await
            .is_err(),
            "a share carries the passphrase"
        );

        let person = hex::encode(person);
        let agent = Caller::agent_turn(
            ("an-agent", person.clone()),
            person.clone(),
            tilo_action_policy::turn::Turn::new("chat/workspace/channels/1", "m1"),
        );
        let others = [
            ("an agent", agent),
            ("an app", Caller::launched("org.example.app")),
            ("an app's own caller", Caller::local_app("org.example.app")),
        ];
        for (who, other) in &others {
            for (action, arguments) in [
                (LOOKUP, lookup_arguments("Home", Security::Wpa2)),
                (
                    SHARE,
                    share_arguments("Home", Security::Wpa2, "another passphrase"),
                ),
            ] {
                let refused = ask(wifi, other, action, arguments).await;
                assert!(
                    refused
                        .as_ref()
                        .is_err_and(|refusal| refusal.contains(THE_PERSONS_OWN)),
                    "{who} is refused a {action}: {refused:?}"
                );
            }
        }
        assert_eq!(
            ask(
                wifi,
                caller,
                LOOKUP,
                lookup_arguments("Home", Security::Wpa2)
            )
            .await,
            Ok(json!({"password": "a passphrase"})),
            "a refused share changes nothing"
        );
    }
}
