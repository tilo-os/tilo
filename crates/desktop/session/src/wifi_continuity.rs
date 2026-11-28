//! Wi-Fi between the person's certified devices: a saved network's password is shared with their
//! other devices (`share-with-devices` on its `saved-network` row), and a network one of them
//! shared is joined without asking for its password. The passwords are the person's and the
//! device's daemon keeps them in their own workspace ([`tilo_runtime::device::wifi`]); this session
//! reads only what this computer's `NetworkManager` holds. The daemon opens and seals the passwords
//! with this device's key, which never leaves it: this session hands it a password to share and
//! is answered one to join, and hands that straight to `NetworkManager`.

use std::sync::{Arc, OnceLock};

use serde_json::{Map, Value};
use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
use tilo_connectivity_service::device::{Change, Device, Readings};
use tilo_runtime::device::wifi::{
    LOOKUP, PROVIDER, RESOURCE, SHARE, lookup_arguments, permitted, share_arguments,
};
use tilo_session_client::daemon_documents::DaemonShell;
use tilo_workspace_service::workspaces::person_workspace::wifi::Security;

/// This device's way to the person's other devices, which the session attaches once its daemon's
/// link is up: until then, and on a computer with no identity of theirs, nothing is shared.
#[derive(Clone, Default)]
pub struct WifiSharing(Arc<OnceLock<Arc<dyn Passphrases>>>);

impl WifiSharing {
    /// Share through `daemon`, as this device's person.
    pub(crate) fn attach(&self, daemon: DaemonShell) {
        if let Some(wifi) = WifiContinuity::new(daemon) {
            let _ = self.0.set(Arc::new(wifi));
        }
    }

    /// `device`, sharing its saved networks' passwords with the person's other devices and joining
    /// the networks they shared.
    #[must_use]
    pub fn device(&self, device: Arc<dyn Device>) -> SharingDevice {
        SharingDevice {
            device,
            sharing: self.clone(),
        }
    }

    /// Share the passphrase of this device's hotspot `ssid` with the person's other devices, as a
    /// saved network's is shared: what one of them joins it with.
    pub(crate) fn share_hotspot(&self, ssid: &str, password: &str) -> Result<(), String> {
        permitted()?;
        self.passphrases()?.share(ssid, Security::Wpa2, password)
    }

    /// The shared passphrases this device asks.
    fn passphrases(&self) -> Result<&Arc<dyn Passphrases>, String> {
        self.0.get().ok_or_else(|| {
            "this device is not signed in to Tilo, so it has no other devices to share with"
                .to_owned()
        })
    }
}

/// The person's shared Wi-Fi passphrases, as this session reads and writes them: the daemon's
/// records in product, and a test's own.
trait Passphrases: Send + Sync {
    /// The password of `ssid` at `security` one of the person's devices shared, opened here.
    fn lookup(&self, ssid: &str, security: Security) -> Result<Option<String>, String>;

    /// Share `password` of `ssid` with the person's devices.
    fn share(&self, ssid: &str, security: Security, password: &str) -> Result<(), String>;

    /// The parts of this machine's saved profile `profile` that can travel between devices.
    fn saved_profile(
        &self,
        profile: &str,
    ) -> Result<os_actions::network::PortableWifiProfileSecret, String>;
}

/// A device whose saved Wi-Fi passwords the person's other devices share.
pub struct SharingDevice {
    device: Arc<dyn Device>,
    sharing: WifiSharing,
}

impl Device for SharingDevice {
    fn read(&self) -> Readings {
        self.device.read()
    }

    fn change(&self, change: &Change) -> Result<(), String> {
        match change {
            Change::ShareWifi { profile } => {
                permitted()?;
                share_saved_profile(self.sharing.passphrases()?.as_ref(), profile)
            }
            // A protected network this device cannot join without its password, which another of
            // the person's devices shared, is joined with the password they shared.
            Change::Join {
                network,
                password: None,
                hidden,
            } => {
                self.device
                    .change(change)
                    .or_else(|refused| match self.shared_password(network) {
                        Some(password) => self.device.change(&Change::Join {
                            network: network.clone(),
                            password: Some(password),
                            hidden: *hidden,
                        }),
                        None => Err(refused),
                    })
            }
            _ => self.device.change(change),
        }
    }

    fn scan_operators(&self, modem: &str) -> Result<Vec<os_actions::Operator>, String> {
        self.device.scan_operators(modem)
    }

    fn send_code(&self, modem: &str, command: &str) -> Result<os_actions::CodeAnswer, String> {
        self.device.send_code(modem, command)
    }

    fn answer_code(&self, modem: &str, response: &str) -> Result<os_actions::CodeAnswer, String> {
        self.device.answer_code(modem, response)
    }

    fn cancel_code(&self, modem: &str) -> Result<(), String> {
        self.device.cancel_code(modem)
    }
}

impl SharingDevice {
    /// The password of `network` one of the person's devices shared, opened here.
    fn shared_password(&self, network: &str) -> Option<String> {
        permitted().ok()?;
        let wifi = self.sharing.passphrases().ok()?;
        [Security::Wpa3, Security::Wpa2]
            .into_iter()
            .find_map(|security| match wifi.lookup(network, security) {
                Ok(found) => found,
                Err(error) => {
                    tracing::warn!(%error, "a shared Wi-Fi password was not looked up");
                    None
                }
            })
    }
}

/// The daemon's shared passwords, as this session asks them.
pub(crate) struct WifiContinuity {
    daemon: DaemonShell,
    /// The person's own workspace, where the daemon keeps the passwords.
    workspace: String,
}

impl WifiContinuity {
    /// The Wi-Fi continuity of this device's person, through `daemon`: `None` on a computer with
    /// no identity of theirs.
    fn new(daemon: DaemonShell) -> Option<Self> {
        let identity = tilo_session_client::identity::device_identity()?;
        Some(Self {
            daemon,
            workspace: tilo_server_protocol::servers::person_workspace_id(
                &identity.device_cert.user_public_key,
            ),
        })
    }

    /// Ask the daemon's Wi-Fi provider `action` with `arguments`, as the person: what it answered.
    fn ask(&self, action: &str, arguments: Map<String, Value>) -> Result<Value, String> {
        let link = self.daemon.link()?;
        let request = InvokeRequest {
            argument: InvokeArgument::Json(arguments),
            ..InvokeRequest::plain(
                RemoteActionTarget {
                    provider: PROVIDER.to_owned(),
                    address: tilo_actions::ActionAddress::new(RESOURCE, action),
                },
                serde_json::Map::new(),
            )
        };
        let (answered, answer) = std::sync::mpsc::sync_channel(1);
        let workspace = self.workspace.clone();
        self.daemon.runtime().spawn(async move {
            let _ = answered.send(link.dispatch().settled(Some(workspace), request).await);
        });
        answer
            .recv()
            .map_err(|_| "the daemon's connection stopped before it answered".to_owned())?
    }

    /// The password of `ssid` the person's devices share, which the daemon opened.
    fn lookup(&self, ssid: &str, security: Security) -> Result<Option<String>, String> {
        let answer = self.ask(LOOKUP, lookup_arguments(ssid, security))?;
        Ok(answer
            .get("password")
            .and_then(Value::as_str)
            .map(str::to_owned))
    }

    /// Share `password` of `ssid` with the person's devices: the daemon seals it to each of theirs.
    fn share(&self, ssid: &str, security: Security, password: &str) -> Result<(), String> {
        self.ask(SHARE, share_arguments(ssid, security, password))
            .map(|_| ())
    }
}

impl Passphrases for WifiContinuity {
    fn lookup(&self, ssid: &str, security: Security) -> Result<Option<String>, String> {
        Self::lookup(self, ssid, security)
    }

    fn share(&self, ssid: &str, security: Security, password: &str) -> Result<(), String> {
        Self::share(self, ssid, security, password)
    }

    fn saved_profile(
        &self,
        profile: &str,
    ) -> Result<os_actions::network::PortableWifiProfileSecret, String> {
        os_actions::portable_wifi_profile_secret(profile)
    }
}

/// Share the password of the profile `profile_uuid` saved on this machine, reading only the parts
/// that can travel between the person's devices.
fn share_saved_profile(passphrases: &dyn Passphrases, profile_uuid: &str) -> Result<(), String> {
    let profile = passphrases.saved_profile(profile_uuid)?;
    let security = match profile.security {
        os_actions::WifiSecurity::Wpa2 => Security::Wpa2,
        os_actions::WifiSecurity::Wpa3 => Security::Wpa3,
        _ => return Err("only a network with a password is shared".to_owned()),
    };
    passphrases.share(&profile.ssid, security, &profile.password)
}

#[cfg(test)]
impl WifiSharing {
    /// The sharing of `passphrases`, as the daemon's link gives it in product.
    fn sharing(passphrases: Arc<dyn Passphrases>) -> Self {
        let once = OnceLock::new();
        let _ = once.set(passphrases);
        Self(Arc::new(once))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use tilo_connectivity_service::device::{Change, Device, Readings};

    use super::{Passphrases, Security, WifiSharing};

    /// A device that records what it is asked and refuses a join without a password.
    #[derive(Default)]
    struct Radio(Mutex<Vec<Change>>);

    impl Device for Radio {
        fn read(&self) -> Readings {
            Readings::default()
        }

        fn change(&self, change: &Change) -> Result<(), String> {
            self.0.lock().unwrap().push(change.clone());
            match change {
                Change::Join { password: None, .. } => Err("the network needs a password".into()),
                _ => Ok(()),
            }
        }

        fn scan_operators(&self, _modem: &str) -> Result<Vec<os_actions::Operator>, String> {
            Err("this radio has no modem".into())
        }

        fn send_code(
            &self,
            _modem: &str,
            _command: &str,
        ) -> Result<os_actions::CodeAnswer, String> {
            Err("this radio has no modem".into())
        }

        fn answer_code(
            &self,
            _modem: &str,
            _response: &str,
        ) -> Result<os_actions::CodeAnswer, String> {
            Err("this radio has no modem".into())
        }

        fn cancel_code(&self, _modem: &str) -> Result<(), String> {
            Err("this radio has no modem".into())
        }
    }

    /// The person's shared passphrases as a daemon keeps them, in memory.
    #[derive(Default)]
    struct Shared {
        saved: Mutex<HashMap<String, os_actions::network::PortableWifiProfileSecret>>,
        kept: Mutex<HashMap<(String, Security), String>>,
        shares: Mutex<Vec<(String, Security, String)>>,
    }

    impl Shared {
        /// The profile `uuid` saves on this machine.
        fn saving(
            &self,
            uuid: &str,
            ssid: &str,
            security: os_actions::WifiSecurity,
            password: &str,
        ) {
            self.saved.lock().unwrap().insert(
                uuid.to_owned(),
                os_actions::network::PortableWifiProfileSecret {
                    ssid: ssid.to_owned(),
                    security,
                    password: zeroize::Zeroizing::new(password.to_owned()),
                },
            );
        }

        /// Another of the person's devices sharing `password` of `ssid`.
        fn sharing(&self, ssid: &str, security: Security, password: &str) {
            self.kept
                .lock()
                .unwrap()
                .insert((ssid.to_owned(), security), password.to_owned());
        }

        fn shares(&self) -> Vec<(String, Security, String)> {
            self.shares.lock().unwrap().clone()
        }
    }

    impl Passphrases for Shared {
        fn lookup(&self, ssid: &str, security: Security) -> Result<Option<String>, String> {
            Ok(self
                .kept
                .lock()
                .unwrap()
                .get(&(ssid.to_owned(), security))
                .cloned())
        }

        fn share(&self, ssid: &str, security: Security, password: &str) -> Result<(), String> {
            self.shares
                .lock()
                .unwrap()
                .push((ssid.to_owned(), security, password.to_owned()));
            Ok(())
        }

        fn saved_profile(
            &self,
            profile: &str,
        ) -> Result<os_actions::network::PortableWifiProfileSecret, String> {
            self.saved
                .lock()
                .unwrap()
                .get(profile)
                .map(|saved| os_actions::network::PortableWifiProfileSecret {
                    ssid: saved.ssid.clone(),
                    security: saved.security,
                    password: saved.password.clone(),
                })
                .ok_or_else(|| "that profile is not saved here".to_owned())
        }
    }

    /// Before the session reaches the daemon, sharing a saved network says why it cannot and asks
    /// nothing of the radio, a join that needs a password fails as the radio said, and every
    /// other change reaches the radio as it is.
    #[test]
    fn a_device_not_yet_reaching_the_daemon_shares_nothing_and_joins_as_the_radio_does() {
        let radio = Arc::new(Radio::default());
        let device = WifiSharing::default().device(Arc::clone(&radio) as Arc<dyn Device>);

        let refused = device
            .change(&Change::ShareWifi {
                profile: "uuid-home".into(),
            })
            .expect_err("nothing to share with");
        assert!(!refused.is_empty(), "the refusal says why");
        assert!(
            radio.0.lock().unwrap().is_empty(),
            "the radio is not asked to share"
        );

        let join = Change::Join {
            network: "Home".into(),
            password: None,
            hidden: false,
        };
        assert_eq!(
            device.change(&join),
            Err("the network needs a password".into()),
            "with no password shared, the radio's own refusal"
        );
        device
            .change(&Change::Wifi(false))
            .expect("the radio takes it");
        assert_eq!(*radio.0.lock().unwrap(), [join, Change::Wifi(false)]);
    }

    /// A saved network's passphrase is shared with the person's other devices, as the network
    /// settings' own `share-with-devices` asks.
    #[test]
    fn a_saved_networks_password_reaches_the_persons_other_devices() {
        let shared = Arc::new(Shared::default());
        shared.saving(
            "uuid-home",
            "Home",
            os_actions::WifiSecurity::Wpa2,
            "correct horse",
        );
        let radio = Arc::new(Radio::default());
        let device = WifiSharing::sharing(Arc::clone(&shared) as Arc<dyn Passphrases>)
            .device(Arc::clone(&radio) as Arc<dyn Device>);

        device
            .change(&Change::ShareWifi {
                profile: "uuid-home".into(),
            })
            .expect("the saved network is shared");
        assert_eq!(
            shared.shares(),
            [(
                "Home".to_owned(),
                Security::Wpa2,
                "correct horse".to_owned()
            )],
            "only what travels between devices leaves: its name, security and password"
        );
        assert!(
            radio.0.lock().unwrap().is_empty(),
            "sharing changes nothing on this device's own radio"
        );

        // A network with no passphrase is not one to share, and neither is an unknown profile.
        shared.saving("uuid-open", "Cafe", os_actions::WifiSecurity::Open, "");
        assert_eq!(
            device
                .change(&Change::ShareWifi {
                    profile: "uuid-open".into(),
                })
                .expect_err("an open network has nothing to share"),
            "only a network with a password is shared"
        );
        assert!(
            device
                .change(&Change::ShareWifi {
                    profile: "uuid-gone".into(),
                })
                .is_err(),
            "a profile this machine does not hold is not shared"
        );
        assert_eq!(shared.shares().len(), 1, "nothing else was shared");
    }

    /// Joining a network without its password uses the one another of the person's devices
    /// shared, and the radio is asked once with it; when nobody shared one, the radio's own
    /// refusal stands.
    #[test]
    fn a_join_uses_the_password_another_device_shared() {
        let shared = Arc::new(Shared::default());
        shared.sharing("Home", Security::Wpa2, "sealed elsewhere");
        let radio = Arc::new(Radio::default());
        let device = WifiSharing::sharing(Arc::clone(&shared) as Arc<dyn Passphrases>)
            .device(Arc::clone(&radio) as Arc<dyn Device>);

        let join = Change::Join {
            network: "Home".into(),
            password: None,
            hidden: false,
        };
        device
            .change(&join)
            .expect("the network one of their devices shared is joined");
        assert_eq!(
            *radio.0.lock().unwrap(),
            [
                join,
                Change::Join {
                    network: "Home".into(),
                    password: Some("sealed elsewhere".to_owned()),
                    hidden: false,
                }
            ],
            "the radio is tried as it is, then with the password the person's devices share"
        );

        // A network nobody shared is the radio's to refuse.
        let stranger = Change::Join {
            network: "Theirs".into(),
            password: None,
            hidden: false,
        };
        assert_eq!(
            device.change(&stranger),
            Err("the network needs a password".into())
        );
    }
}
