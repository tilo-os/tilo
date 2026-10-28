//! Reading and changing what network settings shows: devices, access
//! points, active connections, radios, saved profiles, and bringing
//! connections up and down.
//!
//! Every change goes to `NetworkManager` as a method call, so polkit decides
//! as it does for any other client. A secret a connection needs is handed
//! over in the call that needs it or through a secret agent this process
//! registers for that one activation ([`NmClient::activate_with_secrets`]);
//! it is never written into argv, a file or a log.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, MatchRule, MessageStream};
use zeroize::Zeroizing;

use crate::client::{
    ACTIVE_INTERFACE, BUS_NAME, DEVICE_INTERFACE, NmClient, NmError, PROFILE_INTERFACE,
    PROPERTIES_INTERFACE, ROOT_INTERFACE, ROOT_PATH, SETTINGS_INTERFACE, SIGNAL_QUEUE,
    WIRELESS_INTERFACE, ensure_running, proxy,
};
use crate::model::{
    AccessPoint, ActiveConnection, ActiveState, Device, DeviceKind, IpDetails, Radios,
    activation_failure,
};
use crate::settings::{IpFamily, Setting, Settings};

/// Where the saved profiles live.
const SETTINGS_PATH: &str = "/org/freedesktop/NetworkManager/Settings";
const WIRED_INTERFACE: &str = "org.freedesktop.NetworkManager.Device.Wired";
const ACCESS_POINT_INTERFACE: &str = "org.freedesktop.NetworkManager.AccessPoint";
const AGENT_MANAGER_PATH: &str = "/org/freedesktop/NetworkManager/AgentManager";
const AGENT_MANAGER_INTERFACE: &str = "org.freedesktop.NetworkManager.AgentManager";
/// Where `NetworkManager` calls a secret agent.
const AGENT_PATH: &str = "/org/freedesktop/NetworkManager/SecretAgent";

/// A saved connection profile.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    /// Its object.
    pub path: String,
    pub settings: Settings,
    /// The file it is kept in; empty for one kept in memory.
    pub filename: String,
}

/// No object: `/`.
fn none() -> OwnedObjectPath {
    OwnedObjectPath::default()
}

fn object(path: &str) -> Result<OwnedObjectPath, NmError> {
    OwnedObjectPath::try_from(path).map_err(|error| NmError::Bus(error.into()))
}

/// An object property, `None` for `/`.
fn object_of(path: &OwnedObjectPath) -> Option<String> {
    (path.as_str() != "/").then(|| path.as_str().to_owned())
}

impl NmClient {
    /// Every device `NetworkManager` knows. One removed mid-read is left out.
    ///
    /// # Errors
    /// [`NmError::Bus`] when `NetworkManager` cannot be asked.
    pub async fn devices(&self) -> Result<Vec<Device>, NmError> {
        ensure_running(&self.connection).await?;
        let root = proxy(&self.connection, ROOT_PATH, ROOT_INTERFACE).await?;
        let paths: Vec<OwnedObjectPath> = root.get_property("Devices").await?;
        let mut devices = Vec::new();
        for path in paths {
            if let Ok(device) = read_device(&self.connection, path.as_str()).await {
                devices.push(device);
            }
        }
        Ok(devices)
    }

    /// The access points every Wi-Fi device sees now, as `NetworkManager`
    /// last scanned them: a read that never waits for the radio. One gone
    /// mid-read is left out.
    ///
    /// # Errors
    /// [`NmError::Bus`] when `NetworkManager` cannot be asked.
    pub async fn access_points(&self) -> Result<Vec<AccessPoint>, NmError> {
        let mut points = Vec::new();
        for device in self.devices().await? {
            if device.kind != DeviceKind::Wifi {
                continue;
            }
            let Ok(wireless) = proxy(&self.connection, &device.path, WIRELESS_INTERFACE).await
            else {
                continue;
            };
            let paths: Vec<OwnedObjectPath> = wireless
                .get_property("AccessPoints")
                .await
                .unwrap_or_default();
            let active = wireless
                .get_property::<OwnedObjectPath>("ActiveAccessPoint")
                .await
                .ok()
                .as_ref()
                .and_then(object_of);
            for path in paths {
                if let Ok(mut point) = read_access_point(&self.connection, path.as_str()).await {
                    point.device.clone_from(&device.path);
                    point.active = active.as_deref() == Some(path.as_str());
                    points.push(point);
                }
            }
        }
        Ok(points)
    }

    /// The connections `NetworkManager` has active. One torn down mid-read
    /// is left out.
    ///
    /// # Errors
    /// [`NmError::Bus`] when `NetworkManager` cannot be asked.
    pub async fn active_connections(&self) -> Result<Vec<ActiveConnection>, NmError> {
        ensure_running(&self.connection).await?;
        let root = proxy(&self.connection, ROOT_PATH, ROOT_INTERFACE).await?;
        let paths: Vec<OwnedObjectPath> = root.get_property("ActiveConnections").await?;
        let mut active = Vec::new();
        for path in paths {
            if let Ok(connection) = read_active(&self.connection, path.as_str()).await {
                active.push(connection);
            }
        }
        Ok(active)
    }

    /// What `device` holds of `family` now: its addresses, gateway and name
    /// servers. Empty while it holds none.
    ///
    /// # Errors
    /// [`NmError::Bus`] when the device cannot be read.
    pub async fn ip_details(&self, device: &str, family: IpFamily) -> Result<IpDetails, NmError> {
        let (property, interface) = match family {
            IpFamily::V4 => ("Ip4Config", "org.freedesktop.NetworkManager.IP4Config"),
            IpFamily::V6 => ("Ip6Config", "org.freedesktop.NetworkManager.IP6Config"),
        };
        let device = proxy(&self.connection, device, DEVICE_INTERFACE).await?;
        let Some(config) = object_of(&device.get_property(property).await?) else {
            return Ok(IpDetails::default());
        };
        let config = proxy(&self.connection, &config, interface).await?;
        let addresses: Vec<Setting> = config.get_property("AddressData").await.unwrap_or_default();
        let addresses = addresses
            .iter()
            .filter_map(|entry| {
                let address = <&str>::try_from(entry.get("address")?).ok()?;
                let prefix = u32::try_from(entry.get("prefix")?).ok()?;
                Some(format!("{address}/{prefix}"))
            })
            .collect();
        let gateway = config
            .get_property::<String>("Gateway")
            .await
            .ok()
            .filter(|gateway| !gateway.is_empty());
        let dns = match family {
            IpFamily::V4 => config
                .get_property::<Vec<Setting>>("NameserverData")
                .await
                .unwrap_or_default()
                .iter()
                .filter_map(|entry| {
                    <&str>::try_from(entry.get("address")?)
                        .ok()
                        .map(str::to_owned)
                })
                .collect(),
            IpFamily::V6 => config
                .get_property::<Vec<Vec<u8>>>("Nameservers")
                .await
                .unwrap_or_default()
                .into_iter()
                .filter_map(|raw| <[u8; 16]>::try_from(raw).ok())
                .map(|raw| std::net::Ipv6Addr::from(raw).to_string())
                .collect(),
        };
        Ok(IpDetails {
            addresses,
            gateway,
            dns,
        })
    }

    /// An Ethernet device's negotiated speed in Mb/s; `None` while it has
    /// no link.
    ///
    /// # Errors
    /// [`NmError::Bus`] when the device cannot be read.
    pub async fn wired_speed(&self, device: &str) -> Result<Option<u32>, NmError> {
        let wired = proxy(&self.connection, device, WIRED_INTERFACE).await?;
        let speed: u32 = wired.get_property("Speed").await?;
        Ok((speed > 0).then_some(speed))
    }

    /// `NetworkManager`'s radio switches.
    ///
    /// # Errors
    /// [`NmError::Bus`] when `NetworkManager` cannot be asked.
    pub async fn radios(&self) -> Result<Radios, NmError> {
        ensure_running(&self.connection).await?;
        let root = proxy(&self.connection, ROOT_PATH, ROOT_INTERFACE).await?;
        Ok(Radios {
            wifi_enabled: root.get_property("WirelessEnabled").await?,
            wifi_hardware_enabled: root
                .get_property("WirelessHardwareEnabled")
                .await
                .unwrap_or(true),
            wwan_enabled: root.get_property("WwanEnabled").await?,
            wwan_hardware_enabled: root
                .get_property("WwanHardwareEnabled")
                .await
                .unwrap_or(true),
        })
    }

    /// Turn `NetworkManager`'s Wi-Fi switch on or off.
    ///
    /// # Errors
    /// [`NmError::Refused`] when `NetworkManager` or polkit refuses.
    pub async fn set_wifi_enabled(&self, enabled: bool) -> Result<(), NmError> {
        self.set_root("WirelessEnabled", enabled).await
    }

    /// Turn `NetworkManager`'s mobile broadband switch on or off.
    ///
    /// # Errors
    /// [`NmError::Refused`] when `NetworkManager` or polkit refuses.
    pub async fn set_wwan_enabled(&self, enabled: bool) -> Result<(), NmError> {
        self.set_root("WwanEnabled", enabled).await
    }

    async fn set_root(&self, property: &str, value: bool) -> Result<(), NmError> {
        ensure_running(&self.connection).await?;
        let root = proxy(&self.connection, ROOT_PATH, ROOT_INTERFACE).await?;
        root.set_property(property, value)
            .await
            .map_err(|error| NmError::Refused(error.to_string()))
    }

    /// Every saved profile, with its settings (secrets left out). One
    /// removed mid-read is left out.
    ///
    /// # Errors
    /// [`NmError::Bus`] when `NetworkManager` cannot be asked.
    pub async fn profiles(&self) -> Result<Vec<Profile>, NmError> {
        ensure_running(&self.connection).await?;
        let settings = proxy(&self.connection, SETTINGS_PATH, SETTINGS_INTERFACE).await?;
        let paths: Vec<OwnedObjectPath> = settings
            .call("ListConnections", &())
            .await
            .map_err(NmError::of_call)?;
        let mut profiles = Vec::new();
        for path in paths {
            if let Ok(profile) = read_profile(&self.connection, path.as_str()).await {
                profiles.push(profile);
            }
        }
        Ok(profiles)
    }

    /// The saved profile `uuid`.
    ///
    /// # Errors
    /// [`NmError::NotFound`] when there is none.
    pub async fn profile(&self, uuid: &str) -> Result<Profile, NmError> {
        ensure_running(&self.connection).await?;
        let settings = proxy(&self.connection, SETTINGS_PATH, SETTINGS_INTERFACE).await?;
        let path: OwnedObjectPath = settings
            .call("GetConnectionByUuid", &(uuid,))
            .await
            .map_err(|_| NmError::NotFound(format!("no saved network has the id {uuid}")))?;
        read_profile(&self.connection, path.as_str()).await
    }

    /// A profile's secrets of `setting` (`802-11-wireless-security`,
    /// `wireguard`, …). `NetworkManager` asks polkit, which may ask the
    /// person to authenticate.
    ///
    /// # Errors
    /// [`NmError::Refused`] when the request is refused.
    pub async fn secrets(&self, profile: &Profile, setting: &str) -> Result<Settings, NmError> {
        let connection = proxy(&self.connection, &profile.path, PROFILE_INTERFACE).await?;
        let secrets: HashMap<String, Setting> = connection
            .call("GetSecrets", &(setting,))
            .await
            .map_err(NmError::of_call)?;
        Ok(Settings(secrets))
    }

    /// Replace `profile`'s settings with `settings` and save them. Secrets
    /// left out keep their saved values.
    ///
    /// # Errors
    /// [`NmError::Refused`] when `NetworkManager` refuses the settings.
    pub async fn update(&self, profile: &Profile, settings: &Settings) -> Result<(), NmError> {
        let connection = proxy(&self.connection, &profile.path, PROFILE_INTERFACE).await?;
        connection
            .call::<_, _, ()>("Update", &(&settings.0,))
            .await
            .map_err(NmError::of_call)
    }

    /// Read profile `uuid`, change its settings with `edit`, and save them.
    ///
    /// # Errors
    /// [`NmError::NotFound`] for no such profile, what `edit` refuses, or
    /// [`NmError::Refused`].
    pub async fn edit(
        &self,
        uuid: &str,
        edit: impl FnOnce(&mut Settings) -> Result<(), String> + Send,
    ) -> Result<(), NmError> {
        let profile = self.profile(uuid).await?;
        let mut settings = profile.settings.clone();
        edit(&mut settings).map_err(NmError::Refused)?;
        if settings == profile.settings {
            return Ok(());
        }
        self.update(&profile, &settings).await
    }

    /// Save a new profile. A missing `connection.uuid` is made here. Its id.
    ///
    /// # Errors
    /// [`NmError::Refused`] when `NetworkManager` refuses the settings.
    pub async fn add(&self, mut settings: Settings) -> Result<String, NmError> {
        ensure_running(&self.connection).await?;
        let uuid = ensure_uuid(&mut settings);
        let manager = proxy(&self.connection, SETTINGS_PATH, SETTINGS_INTERFACE).await?;
        manager
            .call::<_, _, OwnedObjectPath>("AddConnection", &(&settings.0,))
            .await
            .map_err(NmError::of_call)?;
        Ok(uuid)
    }

    /// Delete a saved profile.
    ///
    /// # Errors
    /// [`NmError::Refused`] when `NetworkManager` refuses.
    pub async fn delete(&self, profile: &Profile) -> Result<(), NmError> {
        let connection = proxy(&self.connection, &profile.path, PROFILE_INTERFACE).await?;
        connection
            .call::<_, _, ()>("Delete", &())
            .await
            .map_err(NmError::of_call)
    }

    /// Bring `profile` up, on `device` (`None`: the one `NetworkManager`
    /// picks), through the access point `specific` names when given.
    /// With `wait`, return when it is up or has failed, within `wait`;
    /// without, as soon as `NetworkManager` has taken the request.
    ///
    /// # Errors
    /// [`NmError::Refused`] when it is refused, [`NmError::ActivationFailed`]
    /// when it comes up and fails, [`NmError::Timeout`] past `wait`.
    pub async fn activate(
        &self,
        profile: &Profile,
        device: Option<&str>,
        specific: Option<&str>,
        wait: Option<Duration>,
    ) -> Result<(), NmError> {
        ensure_running(&self.connection).await?;
        let states = self.active_states().await?;
        let root = proxy(&self.connection, ROOT_PATH, ROOT_INTERFACE).await?;
        let device = device.map_or_else(|| Ok(none()), object)?;
        let specific = specific.map_or_else(|| Ok(none()), object)?;
        let active: OwnedObjectPath = root
            .call(
                "ActivateConnection",
                &(object(&profile.path)?, device, specific),
            )
            .await
            .map_err(NmError::of_call)?;
        match wait {
            Some(wait) => self.settle(states, active.as_str(), wait).await,
            None => Ok(()),
        }
    }

    /// Save `settings` as a new profile and bring it up on `device`, through
    /// the access point `specific` names when given. `NetworkManager` fills
    /// in what the access point says (its security). Returns when it is up
    /// or has failed, within `wait`. Its id.
    ///
    /// # Errors
    /// As [`Self::activate`].
    pub async fn add_and_activate(
        &self,
        mut settings: Settings,
        device: &str,
        specific: Option<&str>,
        wait: Duration,
    ) -> Result<String, NmError> {
        ensure_running(&self.connection).await?;
        let uuid = ensure_uuid(&mut settings);
        let states = self.active_states().await?;
        let root = proxy(&self.connection, ROOT_PATH, ROOT_INTERFACE).await?;
        let specific = specific.map_or_else(|| Ok(none()), object)?;
        let (_, active): (OwnedObjectPath, OwnedObjectPath) = root
            .call(
                "AddAndActivateConnection",
                &(&settings.0, object(device)?, specific),
            )
            .await
            .map_err(NmError::of_call)?;
        self.settle(states, active.as_str(), wait).await?;
        Ok(uuid)
    }

    /// [`Self::activate`] `profile`, answering `NetworkManager`'s request for
    /// the secrets it is not saved with: `secrets` maps a setting
    /// (`802-1x`) to its secret properties (`password`). A secret agent
    /// answers for this one activation and is gone when this returns, so
    /// the secrets are never stored.
    ///
    /// # Errors
    /// As [`Self::activate`], or when the agent cannot be registered.
    pub async fn activate_with_secrets(
        &self,
        profile: &Profile,
        secrets: HashMap<String, HashMap<String, Zeroizing<String>>>,
        wait: Duration,
    ) -> Result<(), NmError> {
        ensure_running(&self.connection).await?;
        let served = self
            .connection
            .object_server()
            .at(
                AGENT_PATH,
                SecretAgent {
                    profile: profile.path.clone(),
                    secrets,
                },
            )
            .await?;
        let manager = proxy(
            &self.connection,
            AGENT_MANAGER_PATH,
            AGENT_MANAGER_INTERFACE,
        )
        .await?;
        // One agent per process; NetworkManager tells agents apart by this
        // name within one person's session.
        let identifier = format!("so.tilo.connect.p{}", std::process::id());
        let registered = manager
            .call::<_, _, ()>("Register", &(identifier.as_str(),))
            .await
            .map_err(NmError::of_call);
        let activated = match registered {
            Ok(()) => self.activate(profile, None, None, Some(wait)).await,
            Err(error) => Err(error),
        };
        let _ = manager.call::<_, _, ()>("Unregister", &()).await;
        if served {
            let _ = self
                .connection
                .object_server()
                .remove::<SecretAgent, _>(AGENT_PATH)
                .await;
        }
        activated
    }

    /// Take down an active connection.
    ///
    /// # Errors
    /// [`NmError::Refused`] when `NetworkManager` refuses.
    pub async fn deactivate(&self, active: &ActiveConnection) -> Result<(), NmError> {
        let root = proxy(&self.connection, ROOT_PATH, ROOT_INTERFACE).await?;
        root.call::<_, _, ()>("DeactivateConnection", &(object(&active.path)?,))
            .await
            .map_err(NmError::of_call)
    }

    /// Disconnect a device: its connection goes down and it stays down
    /// until asked to connect again.
    ///
    /// # Errors
    /// [`NmError::Refused`] when `NetworkManager` refuses.
    pub async fn disconnect(&self, device: &Device) -> Result<(), NmError> {
        let proxy = proxy(&self.connection, &device.path, DEVICE_INTERFACE).await?;
        proxy
            .call::<_, _, ()>("Disconnect", &())
            .await
            .map_err(NmError::of_call)
    }

    /// Connect a device with the saved profile `NetworkManager` picks for
    /// it. Returns once the request is taken.
    ///
    /// # Errors
    /// [`NmError::Refused`] when `NetworkManager` refuses.
    pub async fn connect(&self, device: &Device) -> Result<(), NmError> {
        let root = proxy(&self.connection, ROOT_PATH, ROOT_INTERFACE).await?;
        root.call::<_, _, OwnedObjectPath>(
            "ActivateConnection",
            &(none(), object(&device.path)?, none()),
        )
        .await
        .map(drop)
        .map_err(NmError::of_call)
    }

    /// Apply a device's saved profile again without reconnecting it.
    ///
    /// # Errors
    /// [`NmError::Refused`] when `NetworkManager` refuses.
    pub async fn reapply(&self, device: &Device) -> Result<(), NmError> {
        let proxy = proxy(&self.connection, &device.path, DEVICE_INTERFACE).await?;
        let unchanged: HashMap<String, Setting> = HashMap::new();
        proxy
            .call::<_, _, ()>("Reapply", &(unchanged, 0_u64, 0_u32))
            .await
            .map_err(NmError::of_call)
    }

    /// Active connections' state changes, subscribed before an activation is
    /// asked for so its outcome cannot slip past.
    async fn active_states(&self) -> Result<MessageStream, NmError> {
        Ok(MessageStream::for_match_rule(
            MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(BUS_NAME)?
                .interface(ACTIVE_INTERFACE)?
                .member("StateChanged")?
                .build(),
            &self.connection,
            Some(SIGNAL_QUEUE),
        )
        .await?)
    }

    /// Wait for the active connection `active` to come up or fail.
    async fn settle(
        &self,
        mut states: MessageStream,
        active: &str,
        wait: Duration,
    ) -> Result<(), NmError> {
        let settled = async {
            // It may have come up before the subscription saw anything. A
            // failure is read from its signal, which carries the reason and
            // is queued already: the subscription came first.
            if read_active(&self.connection, active)
                .await
                .is_ok_and(|connection| connection.state == ActiveState::Activated)
            {
                return Ok(());
            }
            while let Some(signal) = states.next().await {
                let Ok(signal) = signal else { continue };
                if signal
                    .header()
                    .path()
                    .map(zbus::zvariant::ObjectPath::as_str)
                    != Some(active)
                {
                    continue;
                }
                let Ok((state, reason)) = signal.body().deserialize::<(u32, u32)>() else {
                    continue;
                };
                match ActiveState::from_raw(state) {
                    ActiveState::Activated => return Ok(()),
                    ActiveState::Deactivated => {
                        return Err(NmError::ActivationFailed(activation_failure(reason)));
                    }
                    _ => {}
                }
            }
            Err(NmError::Disconnected)
        };
        tokio::time::timeout(wait, settled)
            .await
            .unwrap_or(Err(NmError::Timeout))
    }
}

/// Give `settings` a `connection.uuid` when it has none; the uuid.
fn ensure_uuid(settings: &mut Settings) -> String {
    let uuid = settings.uuid();
    if !uuid.is_empty() {
        return uuid;
    }
    let uuid = uuid::Uuid::new_v4().to_string();
    settings.set("connection", "uuid", uuid.as_str());
    uuid
}

/// Every property of `interface` on `path`, in one call: a list of access
/// points or devices is read in one round trip per object.
async fn all(connection: &Connection, path: &str, interface: &str) -> Result<Setting, NmError> {
    let properties = proxy(connection, path, PROPERTIES_INTERFACE).await?;
    properties
        .call("GetAll", &(interface,))
        .await
        .map_err(NmError::of_call)
}

/// One property of `all`, if it is there and of type `T`.
fn property<T: TryFrom<OwnedValue>>(all: &Setting, name: &str) -> Option<T> {
    T::try_from(all.get(name)?.clone()).ok()
}

/// One property `all` must have.
fn required<T: TryFrom<OwnedValue>>(all: &Setting, name: &str) -> Result<T, NmError> {
    property(all, name).ok_or_else(|| NmError::NotFound(format!("{name} could not be read")))
}

async fn read_device(connection: &Connection, path: &str) -> Result<Device, NmError> {
    let device = all(connection, path, DEVICE_INTERFACE).await?;
    Ok(Device {
        path: path.to_owned(),
        interface: required(&device, "Interface")?,
        kind: DeviceKind::from_raw(required(&device, "DeviceType")?),
        state: required(&device, "State")?,
        hw_address: property(&device, "HwAddress").unwrap_or_default(),
        active: property::<OwnedObjectPath>(&device, "ActiveConnection")
            .as_ref()
            .and_then(object_of),
    })
}

async fn read_access_point(connection: &Connection, path: &str) -> Result<AccessPoint, NmError> {
    let point = all(connection, path, ACCESS_POINT_INTERFACE).await?;
    let ssid: Vec<u8> = required(&point, "Ssid")?;
    Ok(AccessPoint {
        path: path.to_owned(),
        device: String::new(),
        ssid: String::from_utf8_lossy(&ssid).into_owned(),
        strength: required(&point, "Strength")?,
        flags: property(&point, "Flags").unwrap_or_default(),
        wpa_flags: property(&point, "WpaFlags").unwrap_or_default(),
        rsn_flags: property(&point, "RsnFlags").unwrap_or_default(),
        frequency: property(&point, "Frequency").unwrap_or_default(),
        max_bitrate: property(&point, "MaxBitrate").unwrap_or_default(),
        active: false,
    })
}

async fn read_active(connection: &Connection, path: &str) -> Result<ActiveConnection, NmError> {
    let active = all(connection, path, ACTIVE_INTERFACE).await?;
    let devices: Vec<OwnedObjectPath> = property(&active, "Devices").unwrap_or_default();
    Ok(ActiveConnection {
        path: path.to_owned(),
        id: required(&active, "Id")?,
        uuid: required(&active, "Uuid")?,
        kind: required(&active, "Type")?,
        state: ActiveState::from_raw(required(&active, "State")?),
        devices: devices.iter().filter_map(object_of).collect(),
        profile: property::<OwnedObjectPath>(&active, "Connection")
            .as_ref()
            .and_then(object_of)
            .unwrap_or_default(),
    })
}

async fn read_profile(connection: &Connection, path: &str) -> Result<Profile, NmError> {
    let profile = proxy(connection, path, PROFILE_INTERFACE).await?;
    let settings: HashMap<String, Setting> = profile
        .call("GetSettings", &())
        .await
        .map_err(NmError::of_call)?;
    Ok(Profile {
        path: path.to_owned(),
        settings: Settings(settings),
        filename: profile.get_property("Filename").await.unwrap_or_default(),
    })
}

/// Answers `NetworkManager`'s request for one profile's unsaved secrets.
struct SecretAgent {
    profile: String,
    secrets: HashMap<String, HashMap<String, Zeroizing<String>>>,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.SecretAgent")]
#[allow(
    clippy::unused_self,
    clippy::needless_pass_by_value,
    reason = "the methods are the SecretAgent D-Bus interface; this agent answers one request \
              and has nothing to cancel, save or delete"
)]
impl SecretAgent {
    fn get_secrets(
        &self,
        connection: HashMap<String, HashMap<String, OwnedValue>>,
        connection_path: OwnedObjectPath,
        setting_name: String,
        hints: Vec<String>,
        flags: u32,
    ) -> zbus::fdo::Result<HashMap<String, HashMap<String, OwnedValue>>> {
        // The profile is named by its object; what it holds, which secrets
        // are hinted at and whether the person may be asked do not change
        // the answer.
        let _ = (connection, hints, flags);
        let secrets = (connection_path.as_str() == self.profile)
            .then(|| self.secrets.get(&setting_name))
            .flatten()
            .ok_or_else(|| zbus::fdo::Error::Failed("no secrets for this request".into()))?;
        let answer = secrets
            .iter()
            .filter_map(|(key, value)| {
                OwnedValue::try_from(Value::from(value.as_str()))
                    .ok()
                    .map(|value| (key.clone(), value))
            })
            .collect();
        Ok(HashMap::from([(setting_name, answer)]))
    }

    fn cancel_get_secrets(&self, connection_path: OwnedObjectPath, setting_name: String) {
        let _ = (connection_path, setting_name);
    }

    fn save_secrets(
        &self,
        connection: HashMap<String, HashMap<String, OwnedValue>>,
        connection_path: OwnedObjectPath,
    ) {
        let _ = (connection, connection_path);
    }

    fn delete_secrets(
        &self,
        connection: HashMap<String, HashMap<String, OwnedValue>>,
        connection_path: OwnedObjectPath,
    ) {
        let _ = (connection, connection_path);
    }
}
