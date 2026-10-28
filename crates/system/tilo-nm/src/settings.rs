//! A saved connection profile's settings as `NetworkManager` hands them over
//! D-Bus (`a{sa{sv}}`), and the properties Tilo reads and writes in them,
//! typed. No bus here.
//!
//! `NetworkManager` leaves a property out when it holds its default, so a
//! reader answers with the default `NetworkManager` documents for a missing
//! one, never "unknown".

use std::collections::HashMap;
use std::net::IpAddr;

use zbus::zvariant::{OwnedValue, Value};

/// One setting's properties (`ssid`, `psk`, `method`, …).
pub type Setting = HashMap<String, OwnedValue>;

/// A profile's settings: setting name (`connection`, `ipv4`,
/// `802-11-wireless`, …) to its properties.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings(pub HashMap<String, Setting>);

/// An address family's setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IpFamily {
    V4,
    V6,
}

impl IpFamily {
    /// The setting it is kept in.
    #[must_use]
    pub const fn setting(self) -> &'static str {
        match self {
            Self::V4 => "ipv4",
            Self::V6 => "ipv6",
        }
    }

    const fn longest_prefix(self) -> u32 {
        match self {
            Self::V4 => 32,
            Self::V6 => 128,
        }
    }
}

/// A value as a property holds it. Only a file descriptor cannot be owned,
/// and no setting holds one.
fn owned(value: Value<'_>) -> Option<OwnedValue> {
    OwnedValue::try_from(value).ok()
}

impl Settings {
    /// A property's raw value.
    #[must_use]
    pub fn get(&self, setting: &str, key: &str) -> Option<&OwnedValue> {
        self.0.get(setting)?.get(key)
    }

    /// Whether the profile has `setting` at all.
    #[must_use]
    pub fn has(&self, setting: &str) -> bool {
        self.0.contains_key(setting)
    }

    #[must_use]
    pub fn string(&self, setting: &str, key: &str) -> Option<String> {
        <&str>::try_from(self.get(setting, key)?)
            .ok()
            .map(str::to_owned)
    }

    #[must_use]
    pub fn boolean(&self, setting: &str, key: &str) -> Option<bool> {
        bool::try_from(self.get(setting, key)?).ok()
    }

    #[must_use]
    pub fn uint(&self, setting: &str, key: &str) -> Option<u32> {
        u32::try_from(self.get(setting, key)?).ok()
    }

    #[must_use]
    pub fn int(&self, setting: &str, key: &str) -> Option<i32> {
        i32::try_from(self.get(setting, key)?).ok()
    }

    #[must_use]
    pub fn uint64(&self, setting: &str, key: &str) -> Option<u64> {
        u64::try_from(self.get(setting, key)?).ok()
    }

    /// A string list (`as`); empty when missing.
    #[must_use]
    pub fn strings(&self, setting: &str, key: &str) -> Vec<String> {
        self.get(setting, key)
            .and_then(|value| Vec::<String>::try_from(value.clone()).ok())
            .unwrap_or_default()
    }

    /// A byte string (`ay`); empty when missing.
    #[must_use]
    pub fn bytes(&self, setting: &str, key: &str) -> Vec<u8> {
        self.get(setting, key)
            .and_then(|value| Vec::<u8>::try_from(value.clone()).ok())
            .unwrap_or_default()
    }

    /// A string map (`a{ss}`, a VPN's `data`); empty when missing.
    #[must_use]
    pub fn string_map(&self, setting: &str, key: &str) -> HashMap<String, String> {
        self.get(setting, key)
            .and_then(|value| HashMap::<String, String>::try_from(value.clone()).ok())
            .unwrap_or_default()
    }

    /// A list of dictionaries (`aa{sv}`, `address-data`, `peers`); empty
    /// when missing.
    #[must_use]
    pub fn dicts(&self, setting: &str, key: &str) -> Vec<Setting> {
        self.get(setting, key)
            .and_then(|value| Vec::<Setting>::try_from(value.clone()).ok())
            .unwrap_or_default()
    }

    /// Set a property, making its setting when there is none.
    pub fn set<'v>(&mut self, setting: &str, key: &str, value: impl Into<Value<'v>>) {
        if let Some(value) = owned(value.into()) {
            self.0
                .entry(setting.to_owned())
                .or_default()
                .insert(key.to_owned(), value);
        }
    }

    /// Set a property to `NetworkManager`'s default by leaving it out.
    pub fn remove(&mut self, setting: &str, key: &str) {
        if let Some(properties) = self.0.get_mut(setting) {
            properties.remove(key);
        }
    }

    /// The profile's name (`connection.id`).
    #[must_use]
    pub fn id(&self) -> String {
        self.string("connection", "id").unwrap_or_default()
    }

    /// The profile's stable identity (`connection.uuid`).
    #[must_use]
    pub fn uuid(&self) -> String {
        self.string("connection", "uuid").unwrap_or_default()
    }

    /// Its type (`connection.type`): `802-11-wireless`, `802-3-ethernet`,
    /// `gsm`, `vpn`, `wireguard`, …
    #[must_use]
    pub fn kind(&self) -> String {
        self.string("connection", "type").unwrap_or_default()
    }

    /// Whether it connects by itself; on by default.
    #[must_use]
    pub fn autoconnect(&self) -> bool {
        self.boolean("connection", "autoconnect").unwrap_or(true)
    }

    /// When it last connected, in seconds since the Unix epoch; `None` when
    /// it never has.
    #[must_use]
    pub fn timestamp(&self) -> Option<u64> {
        self.uint64("connection", "timestamp")
            .filter(|stamp| *stamp > 0)
    }

    /// `connection.metered` as `NMMetered` (`0` unknown, the default).
    #[must_use]
    pub fn metered(&self) -> u32 {
        self.int("connection", "metered")
            .and_then(|raw| u32::try_from(raw).ok())
            .unwrap_or(0)
    }

    /// An address family's method (`auto`, `manual`, `disabled`, …).
    #[must_use]
    pub fn method(&self, family: IpFamily) -> Option<String> {
        self.string(family.setting(), "method")
    }

    /// The family's own addresses, each with its prefix (`192.0.2.10/24`).
    #[must_use]
    pub fn addresses(&self, family: IpFamily) -> Vec<String> {
        self.dicts(family.setting(), "address-data")
            .iter()
            .filter_map(|entry| {
                let address = <&str>::try_from(entry.get("address")?).ok()?;
                let prefix = u32::try_from(entry.get("prefix")?).ok()?;
                Some(format!("{address}/{prefix}"))
            })
            .collect()
    }

    /// The family's own gateway.
    #[must_use]
    pub fn gateway(&self, family: IpFamily) -> Option<String> {
        self.string(family.setting(), "gateway")
            .filter(|gateway| !gateway.is_empty())
    }

    /// The family's own name servers. Newer `NetworkManager` keeps them as
    /// text (`dns-data`), older as numbers (`dns`); both are read.
    #[must_use]
    pub fn dns(&self, family: IpFamily) -> Vec<String> {
        let setting = family.setting();
        let text = self.strings(setting, "dns-data");
        if !text.is_empty() {
            return text;
        }
        match family {
            IpFamily::V4 => self
                .get(setting, "dns")
                .and_then(|value| Vec::<u32>::try_from(value.clone()).ok())
                .unwrap_or_default()
                .into_iter()
                // Network byte order, as it travels.
                .map(|raw| std::net::Ipv4Addr::from(u32::from_be(raw)).to_string())
                .collect(),
            IpFamily::V6 => self
                .get(setting, "dns")
                .and_then(|value| Vec::<Vec<u8>>::try_from(value.clone()).ok())
                .unwrap_or_default()
                .into_iter()
                .filter_map(|raw| <[u8; 16]>::try_from(raw).ok())
                .map(|raw| std::net::Ipv6Addr::from(raw).to_string())
                .collect(),
        }
    }

    /// Give the family these addresses (`address/prefix`; a bare address
    /// takes the longest prefix).
    ///
    /// # Errors
    /// An entry that is not an address of the family.
    pub fn set_addresses(&mut self, family: IpFamily, addresses: &[String]) -> Result<(), String> {
        let mut entries: Vec<Setting> = Vec::new();
        for entry in addresses {
            let (address, prefix) = entry.split_once('/').unwrap_or((entry, ""));
            let ip = address
                .parse::<IpAddr>()
                .ok()
                .filter(|ip| ip.is_ipv4() == (family == IpFamily::V4))
                .ok_or_else(|| format!("{entry} is not an address of this family"))?;
            let prefix = if prefix.is_empty() {
                family.longest_prefix()
            } else {
                prefix
                    .parse::<u32>()
                    .ok()
                    .filter(|prefix| *prefix <= family.longest_prefix())
                    .ok_or_else(|| format!("{entry} has an invalid prefix"))?
            };
            let mut dict = Setting::new();
            if let (Some(address), Some(prefix)) = (
                owned(Value::from(ip.to_string())),
                owned(Value::from(prefix)),
            ) {
                dict.insert("address".into(), address);
                dict.insert("prefix".into(), prefix);
            }
            entries.push(dict);
        }
        let setting = family.setting();
        // The numeric form is the older spelling of the same list; leaving
        // it would contradict the new one.
        self.remove(setting, "addresses");
        if entries.is_empty() {
            self.remove(setting, "address-data");
        } else {
            self.set(setting, "address-data", entries);
        }
        Ok(())
    }

    /// Give the family its own gateway, or (`None`) none.
    ///
    /// # Errors
    /// A gateway that is not an address of the family.
    pub fn set_gateway(&mut self, family: IpFamily, gateway: Option<&str>) -> Result<(), String> {
        let setting = family.setting();
        match gateway.filter(|gateway| !gateway.is_empty()) {
            Some(gateway) => {
                let valid = gateway
                    .parse::<IpAddr>()
                    .is_ok_and(|ip| ip.is_ipv4() == (family == IpFamily::V4));
                if !valid {
                    return Err(format!("{gateway} is not a gateway address of this family"));
                }
                self.set(setting, "gateway", gateway);
            }
            None => self.remove(setting, "gateway"),
        }
        Ok(())
    }

    /// Give the family these name servers (empty: none of its own).
    ///
    /// # Errors
    /// An entry that is not an address of the family.
    pub fn set_dns(&mut self, family: IpFamily, servers: &[String]) -> Result<(), String> {
        let setting = family.setting();
        let mut v4 = Vec::new();
        let mut v6 = Vec::new();
        for server in servers {
            match server.parse::<IpAddr>() {
                Ok(IpAddr::V4(ip)) if family == IpFamily::V4 => v4.push(u32::from(ip).to_be()),
                Ok(IpAddr::V6(ip)) if family == IpFamily::V6 => v6.push(ip.octets().to_vec()),
                _ => return Err(format!("{server} is not a name server of this family")),
            }
        }
        // The text form is the newer spelling of the same list; the numeric
        // one is understood by every version.
        self.remove(setting, "dns-data");
        if servers.is_empty() {
            self.remove(setting, "dns");
        } else if family == IpFamily::V4 {
            self.set(setting, "dns", v4);
        } else {
            self.set(setting, "dns", v6);
        }
        Ok(())
    }

    /// A certificate or key a property names by path (`802-1x.ca-cert`):
    /// `NetworkManager` keeps it as the bytes `file://<path>` and a NUL.
    #[must_use]
    pub fn path(&self, setting: &str, key: &str) -> Option<String> {
        let bytes = self.bytes(setting, key);
        let text = std::str::from_utf8(&bytes).ok()?;
        let path = text.strip_prefix("file://")?.trim_end_matches('\0');
        (!path.is_empty()).then(|| path.to_owned())
    }

    /// Name a certificate or key by `path`; empty removes it.
    pub fn set_path(&mut self, setting: &str, key: &str, path: &str) {
        if path.is_empty() {
            self.remove(setting, key);
        } else {
            let mut bytes = format!("file://{path}").into_bytes();
            bytes.push(0);
            self.set(setting, key, bytes);
        }
    }
}

/// One `WireGuard` peer of a profile (`wireguard.peers`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WireGuardPeer {
    pub public_key: String,
    pub endpoint: Option<String>,
    pub allowed_ips: Vec<String>,
    /// Seconds between keepalives; `None` sends none.
    pub keepalive: Option<u32>,
    /// A secret: never read back; written when given, kept when not.
    pub preshared_key: Option<zeroize::Zeroizing<String>>,
}

impl Settings {
    /// The profile's `WireGuard` peers.
    #[must_use]
    pub fn wireguard_peers(&self) -> Vec<WireGuardPeer> {
        self.dicts("wireguard", "peers")
            .iter()
            .filter_map(|peer| {
                let text = |key: &str| {
                    peer.get(key)
                        .and_then(|value| <&str>::try_from(value).ok())
                        .map(str::to_owned)
                };
                Some(WireGuardPeer {
                    public_key: text("public-key").filter(|key| !key.is_empty())?,
                    endpoint: text("endpoint").filter(|endpoint| !endpoint.is_empty()),
                    allowed_ips: peer
                        .get("allowed-ips")
                        .and_then(|value| Vec::<String>::try_from(value.clone()).ok())
                        .unwrap_or_default(),
                    keepalive: peer
                        .get("persistent-keepalive")
                        .and_then(|value| u32::try_from(value).ok())
                        .filter(|seconds| *seconds > 0),
                    preshared_key: None,
                })
            })
            .collect()
    }

    /// Change the peer with `peer`'s public key (adding it when there is
    /// none) to `peer`, keeping what else it holds (its pre-shared key's
    /// flags and the like).
    pub fn set_wireguard_peer(&mut self, peer: &WireGuardPeer) {
        let mut peers = self.dicts("wireguard", "peers");
        let at = peers.iter().position(|existing| {
            existing
                .get("public-key")
                .and_then(|value| <&str>::try_from(value).ok())
                == Some(peer.public_key.as_str())
        });
        let mut entry = match at {
            Some(at) => peers.remove(at),
            None => Setting::new(),
        };
        let mut put = |key: &str, value: Option<Value<'_>>| match value.and_then(owned) {
            Some(value) => {
                entry.insert(key.to_owned(), value);
            }
            None => {
                entry.remove(key);
            }
        };
        put("public-key", Some(Value::from(peer.public_key.as_str())));
        put("endpoint", peer.endpoint.as_deref().map(Value::from));
        put(
            "allowed-ips",
            (!peer.allowed_ips.is_empty()).then(|| Value::from(peer.allowed_ips.clone())),
        );
        put(
            "persistent-keepalive",
            Some(Value::from(peer.keepalive.unwrap_or(0))),
        );
        if let Some(key) = &peer.preshared_key {
            put("preshared-key", Some(Value::from(key.as_str())));
            put("preshared-key-flags", Some(Value::from(0_u32)));
        }
        peers.insert(at.unwrap_or(peers.len()), entry);
        self.set("wireguard", "peers", peers);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_properties_read_as_networkmanagers_defaults() {
        let settings = Settings::default();
        assert!(settings.autoconnect(), "autoconnect is on by default");
        assert_eq!(settings.metered(), 0);
        assert_eq!(settings.timestamp(), None);
        assert!(settings.dns(IpFamily::V4).is_empty());
    }

    #[test]
    fn addresses_gateways_and_name_servers_round_trip() {
        let mut settings = Settings::default();
        settings.set("ipv4", "addresses", vec![vec![1_u32, 24, 0]]);
        settings
            .set_addresses(
                IpFamily::V4,
                &["192.0.2.10/24".into(), "198.51.100.1".into()],
            )
            .unwrap();
        assert_eq!(
            settings.get("ipv4", "addresses"),
            None,
            "no stale numeric copy"
        );
        assert_eq!(
            settings.addresses(IpFamily::V4),
            ["192.0.2.10/24", "198.51.100.1/32"]
        );
        settings
            .set_gateway(IpFamily::V4, Some("192.0.2.1"))
            .unwrap();
        assert_eq!(settings.gateway(IpFamily::V4).as_deref(), Some("192.0.2.1"));
        settings
            .set_dns(IpFamily::V4, &["1.1.1.1".into(), "9.9.9.9".into()])
            .unwrap();
        assert_eq!(settings.dns(IpFamily::V4), ["1.1.1.1", "9.9.9.9"]);
        settings
            .set_dns(IpFamily::V6, &["2001:db8::53".into()])
            .unwrap();
        assert_eq!(settings.dns(IpFamily::V6), ["2001:db8::53"]);
        settings.set("ipv4", "dns-data", vec!["8.8.8.8".to_owned()]);
        assert_eq!(
            settings.dns(IpFamily::V4),
            ["8.8.8.8"],
            "the text form wins"
        );

        assert!(
            settings
                .set_addresses(IpFamily::V4, &["2001:db8::1/64".into()])
                .is_err()
        );
        assert!(
            settings
                .set_addresses(IpFamily::V4, &["10.0.0.1/33".into()])
                .is_err()
        );
        assert!(
            settings
                .set_gateway(IpFamily::V6, Some("10.0.0.1"))
                .is_err()
        );
        assert!(settings.set_dns(IpFamily::V4, &["::1".into()]).is_err());
        settings.set_addresses(IpFamily::V4, &[]).unwrap();
        assert!(settings.addresses(IpFamily::V4).is_empty());
    }

    #[test]
    fn certificate_paths_are_file_uris_ending_in_nul() {
        let mut settings = Settings::default();
        settings.set_path("802-1x", "ca-cert", "/etc/pki/campus.pem");
        assert_eq!(
            settings.bytes("802-1x", "ca-cert"),
            b"file:///etc/pki/campus.pem\0"
        );
        assert_eq!(
            settings.path("802-1x", "ca-cert").as_deref(),
            Some("/etc/pki/campus.pem")
        );
        settings.set_path("802-1x", "ca-cert", "");
        assert_eq!(settings.path("802-1x", "ca-cert"), None);
    }

    #[test]
    fn a_wireguard_peer_is_changed_in_place_keeping_what_else_it_holds() {
        let mut settings = Settings::default();
        let mut first = Setting::new();
        first.insert(
            "public-key".into(),
            OwnedValue::try_from(Value::from("AAA=")).unwrap(),
        );
        first.insert("preshared-key-flags".into(), OwnedValue::from(1_u32));
        settings.set("wireguard", "peers", vec![first]);
        settings.set_wireguard_peer(&WireGuardPeer {
            public_key: "AAA=".into(),
            endpoint: Some("home.example:51820".into()),
            allowed_ips: vec!["0.0.0.0/0".into(), "::/0".into()],
            keepalive: None,
            preshared_key: None,
        });
        let peers = settings.wireguard_peers();
        assert_eq!(
            peers,
            [WireGuardPeer {
                public_key: "AAA=".into(),
                endpoint: Some("home.example:51820".into()),
                allowed_ips: vec!["0.0.0.0/0".into(), "::/0".into()],
                keepalive: None,
                preshared_key: None,
            }]
        );
        let raw = settings.dicts("wireguard", "peers");
        assert_eq!(raw.len(), 1);
        assert!(raw[0].contains_key("preshared-key-flags"));
    }
}
