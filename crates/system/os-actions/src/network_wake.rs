//! Persist magic-packet wake through `NetworkManager` without reconnecting.
//!
//! Driver support and current configuration are observations, not proof that
//! the whole machine resumes. No operation here suspends or reconnects it.

use serde::{Deserialize, Serialize};

use crate::network::{validate_uuid, with_nm};
use crate::process::{run_capture, which};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkWakeStatus {
    pub profile: String,
    pub interface: Option<String>,
    pub configured: bool,
    pub supported: Option<bool>,
    pub applied: Option<bool>,
    /// Reapply can fail after the profile is saved. Preserve that distinction.
    pub problem: Option<String>,
}

/// Read the saved profile and the active driver's magic-packet setting.
///
/// # Errors
/// Refuses invalid profiles, unsupported profile types and unreadable settings.
pub fn status(profile: &str) -> Result<NetworkWakeStatus, String> {
    observe(profile, &SystemProfiles, &system_command)
}

/// Change only magic-packet wake, persisting through subsequent activations.
///
/// # Errors
/// Enabling requires an active interface whose driver reports magic-packet
/// support. `NetworkManager` retains its normal authorization checks.
pub fn configure(profile: &str, enabled: bool) -> Result<NetworkWakeStatus, String> {
    change(profile, enabled, &SystemProfiles, &system_command)
}

// Host services can retain a PATH without sbin. Locate these two read-only
// system tools at their fixed installed paths without widening PATH.
fn system_command(tool: &str, args: &[&str]) -> Result<String, String> {
    if matches!(tool, "iw" | "ethtool") {
        let path = format!("/usr/sbin/{tool}");
        if which(&path) {
            return run_capture(&path, args);
        }
    }
    run_capture(tool, args)
}

type Runner<'a> = dyn Fn(&str, &[&str]) -> Result<String, String> + 'a;

/// Which wake setting a profile has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Medium {
    /// `802-11-wireless.wake-on-wlan`; `NM_SETTING_WIRELESS_WAKE_ON_WLAN_MAGIC`
    /// is `0x08`, its 4-way-handshake flag `0x40`.
    Wifi,
    /// `802-3-ethernet.wake-on-lan`; `NM_SETTING_WIRED_WAKE_ON_LAN_MAGIC` is
    /// `0x40`, its ARP flag `0x20`.
    Ethernet,
}

impl Medium {
    const fn setting(self) -> (&'static str, &'static str) {
        match self {
            Self::Wifi => ("802-11-wireless", "wake-on-wlan"),
            Self::Ethernet => ("802-3-ethernet", "wake-on-lan"),
        }
    }

    const fn magic(self) -> u32 {
        match self {
            Self::Wifi => 0x08,
            Self::Ethernet => 0x40,
        }
    }
}

/// What this needs of `NetworkManager`, behind one seam so the sequence is
/// tested against a fake.
trait Profiles {
    /// The profile's medium and its wake flags.
    fn wake(&self, profile: &str) -> Result<(Medium, u32), String>;
    /// The interface the profile is active on, if it is.
    fn active_interface(&self, profile: &str) -> Result<Option<String>, String>;
    /// Save the wake flags.
    fn set_wake(&self, profile: &str, medium: Medium, flags: u32) -> Result<(), String>;
    /// Apply the saved profile to `interface` without reconnecting it.
    fn reapply(&self, interface: &str) -> Result<(), String>;
}

/// `NetworkManager` on the system bus.
struct SystemProfiles;

impl Profiles for SystemProfiles {
    fn wake(&self, profile: &str) -> Result<(Medium, u32), String> {
        let settings = with_nm(|nm| async move { Ok(nm.profile(profile).await?.settings) })?;
        let medium = match settings.kind().as_str() {
            "802-11-wireless" => Medium::Wifi,
            "802-3-ethernet" => Medium::Ethernet,
            _ => {
                return Err(
                    "magic-packet wake is offered only for Wi-Fi and Ethernet profiles".into(),
                );
            }
        };
        let (setting, key) = medium.setting();
        // NetworkManager's DEFAULT (1) when the profile leaves it out.
        Ok((medium, settings.uint(setting, key).unwrap_or(1)))
    }

    fn active_interface(&self, profile: &str) -> Result<Option<String>, String> {
        with_nm(|nm| async move {
            let Some(active) = nm
                .active_connections()
                .await?
                .into_iter()
                .find(|active| active.uuid == profile)
            else {
                return Ok(None);
            };
            Ok(nm
                .devices()
                .await?
                .into_iter()
                .find(|device| active.devices.contains(&device.path))
                .map(|device| device.interface))
        })
    }

    fn set_wake(&self, profile: &str, medium: Medium, flags: u32) -> Result<(), String> {
        with_nm(|nm| async move {
            nm.edit(profile, |settings| {
                let (setting, key) = medium.setting();
                settings.set(setting, key, flags);
                Ok(())
            })
            .await
        })
    }

    fn reapply(&self, interface: &str) -> Result<(), String> {
        with_nm(|nm| async move {
            let device = nm
                .devices()
                .await?
                .into_iter()
                .find(|device| device.interface == interface)
                .ok_or_else(|| tilo_nm::NmError::NotFound(format!("{interface} is gone")))?;
            nm.reapply(&device).await
        })
    }
}

fn medium(profile: &str, nm: &dyn Profiles) -> Result<(Medium, u32), String> {
    validate_uuid(profile)?;
    nm.wake(profile)
}

fn active_interface(profile: &str, nm: &dyn Profiles) -> Result<Option<String>, String> {
    let Some(interface) = nm.active_interface(profile)? else {
        return Ok(None);
    };
    if interface.is_empty()
        || interface.len() > 15
        || interface.starts_with('-')
        || !interface
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err("NetworkManager returned an invalid interface name".into());
    }
    Ok(Some(interface))
}

fn driver(interface: &str, wifi: bool, run: &Runner<'_>) -> Result<(bool, bool), String> {
    if !wifi {
        let text = run("ethtool", &[interface])?;
        let field = |key: &str| text.lines().find_map(|line| line.trim().strip_prefix(key));
        let supported =
            field("Supports Wake-on:").ok_or("Ethernet wake support was not reported")?;
        let applied = field("Wake-on:").ok_or("Ethernet wake configuration was not reported")?;
        return Ok((supported.contains('g'), applied.contains('g')));
    }
    let info = run("iw", &["dev", interface, "info"])?;
    let index = info
        .lines()
        .find_map(|line| line.trim().strip_prefix("wiphy "))
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or("the Wi-Fi interface has no reported physical radio")?;
    let phy = format!("phy{index}");
    let support = run("iw", &["phy", &phy, "info"])?;
    let support = support
        .split_once("WoWLAN support:")
        .map_or("", |(_, part)| part);
    let supported = support
        .lines()
        .take_while(|line| line.trim().is_empty() || line.trim().starts_with('*'))
        .any(|line| line.contains("wake up on magic packet"));
    Ok((supported, wowlan_magic_packet(&phy, run)?))
}

/// Whether the Wi-Fi radio `phy` (`phy0`, the name under
/// `/sys/class/ieee80211`) wakes the machine on a magic packet now. Wi-Fi
/// drivers do not answer the ethtool wake query that Ethernet ones do.
///
/// # Errors
/// Refuses an invalid radio name; reports an unreadable configuration.
pub fn wifi_magic_packet_enabled(phy: &str) -> Result<bool, String> {
    if !phy
        .strip_prefix("phy")
        .is_some_and(|index| !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("invalid Wi-Fi radio name".into());
    }
    wowlan_magic_packet(phy, &system_command)
}

fn wowlan_magic_packet(phy: &str, run: &Runner<'_>) -> Result<bool, String> {
    let state = run("iw", &["phy", phy, "wowlan", "show"])?;
    if !state.contains("WoWLAN is enabled:") && !state.contains("WoWLAN is disabled.") {
        return Err("Wi-Fi wake configuration was not reported".into());
    }
    Ok(state.contains("wake up on magic packet") && state.contains("is enabled"))
}

fn observe(
    profile: &str,
    nm: &dyn Profiles,
    run: &Runner<'_>,
) -> Result<NetworkWakeStatus, String> {
    let (medium, flags) = medium(profile, nm)?;
    let interface = active_interface(profile, nm)?;
    let (supported, applied, problem) = match interface.as_deref() {
        Some(interface) => match driver(interface, medium == Medium::Wifi, run) {
            Ok((supported, applied)) => (Some(supported), Some(applied), None),
            Err(error) => (None, None, Some(error)),
        },
        None => (None, None, None),
    };
    Ok(NetworkWakeStatus {
        profile: profile.into(),
        interface,
        configured: flags & medium.magic() != 0,
        supported,
        applied,
        problem,
    })
}

fn change(
    profile: &str,
    enabled: bool,
    nm: &dyn Profiles,
    run: &Runner<'_>,
) -> Result<NetworkWakeStatus, String> {
    let before = observe(profile, nm, run)?;
    if enabled && before.supported != Some(true) {
        return Err(before.problem.unwrap_or_else(|| {
            if before.interface.is_none() {
                "connect this profile before enabling magic-packet wake".into()
            } else {
                "the active driver does not report magic-packet wake support".into()
            }
        }));
    }
    let (medium, _) = medium(profile, nm)?;
    // Magic packet alone, or nothing at all (`NONE`).
    nm.set_wake(profile, medium, if enabled { medium.magic() } else { 0 })?;
    let reapply = if active_interface(profile, nm)? == before.interface {
        before
            .interface
            .as_deref()
            .map(|interface| nm.reapply(interface))
            .transpose()
    } else {
        Err("the active connection changed; its wake setting was not reapplied".into())
    };
    let mut after = observe(profile, nm, run)?;
    if after.configured != enabled {
        return Err("NetworkManager did not retain the requested wake setting".into());
    }
    if let Err(error) = reapply {
        after.problem = Some(format!(
            "wake setting saved; it could not be applied to the active connection: {error}"
        ));
    } else if after.interface.is_some() && after.applied != Some(enabled) {
        after.problem = Some("wake setting saved; the active driver has not applied it".into());
    }
    Ok(after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    const PROFILE: &str = "11111111-1111-4111-8111-111111111111";

    /// `NetworkManager` and the driver tools as far as wake goes.
    struct NetworkManager {
        calls: RefCell<Vec<String>>,
        medium: Cell<Medium>,
        /// The saved flags; `None` follows `configured`.
        flags: Cell<Option<u32>>,
        configured: Cell<bool>,
        applied: Cell<bool>,
        inactive: Cell<bool>,
        unsupported: Cell<bool>,
        reapply_failed: Cell<bool>,
    }

    impl Default for NetworkManager {
        fn default() -> Self {
            Self {
                calls: RefCell::default(),
                medium: Cell::new(Medium::Wifi),
                flags: Cell::default(),
                configured: Cell::default(),
                applied: Cell::default(),
                inactive: Cell::default(),
                unsupported: Cell::default(),
                reapply_failed: Cell::default(),
            }
        }
    }

    impl Profiles for NetworkManager {
        fn wake(&self, _profile: &str) -> Result<(Medium, u32), String> {
            let medium = self.medium.get();
            let configured = if self.configured.get() {
                medium.magic()
            } else {
                0
            };
            let flags = self.flags.get().unwrap_or(configured);
            Ok((medium, flags))
        }
        fn active_interface(&self, _profile: &str) -> Result<Option<String>, String> {
            Ok((!self.inactive.get()).then(|| "wlan0".into()))
        }
        fn set_wake(&self, profile: &str, medium: Medium, flags: u32) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("modify {profile} {medium:?} {flags:#x}"));
            self.configured.set(flags & medium.magic() != 0);
            Ok(())
        }
        fn reapply(&self, interface: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("reapply {interface}"));
            if self.reapply_failed.get() {
                return Err("not authorized".into());
            }
            self.applied.set(self.configured.get());
            Ok(())
        }
    }

    impl NetworkManager {
        fn run(&self, tool: &str, args: &[&str]) -> Result<String, String> {
            match (tool, args) {
                ("iw", ["dev", "wlan0", "info"]) => Ok("Interface wlan0\n\twiphy 0\n".into()),
                ("iw", ["phy", "phy0", "info"]) => Ok(if self.unsupported.get() {
                    "WoWLAN support:\n\t* wake up on disconnect\n"
                } else {
                    "WoWLAN support:\n\t* wake up on magic packet\n"
                }
                .into()),
                ("iw", ["phy", "phy0", "wowlan", "show"]) => Ok(if self.applied.get() {
                    "WoWLAN is enabled:\n * wake up on magic packet\n"
                } else {
                    "WoWLAN is disabled.\n"
                }
                .into()),
                ("ethtool", ["wlan0"]) => Ok("Supports Wake-on: g\nWake-on: d\n".into()),
                _ => Err(format!("unexpected command: {tool} {args:?}")),
            }
        }
    }

    #[test]
    fn network_wake_reads_networkmanager_magic_and_not_handshake_flags() {
        let nm = NetworkManager::default();
        // NMSettingWirelessWakeOnWLan: MAGIC = 0x08, 4WAY_HANDSHAKE = 0x40.
        for (flags, expected) in [(8, true), (64, false), (72, true), (0, false), (1, false)] {
            nm.flags.set(Some(flags));
            let status = observe(PROFILE, &nm, &|tool, args| nm.run(tool, args)).unwrap();
            assert_eq!(status.configured, expected, "NetworkManager flags {flags}");
        }
        assert!(nm.calls.borrow().is_empty(), "reading changes nothing");
    }

    #[test]
    fn network_wake_reads_wired_magic_and_not_arp_flags() {
        let nm = NetworkManager::default();
        nm.medium.set(Medium::Ethernet);
        // NMSettingWiredWakeOnLan: MAGIC = 0x40, ARP = 0x20.
        for (flags, expected) in [(64, true), (32, false), (96, true), (0, false)] {
            nm.flags.set(Some(flags));
            let status = observe(PROFILE, &nm, &|tool, args| nm.run(tool, args)).unwrap();
            assert_eq!(status.configured, expected, "NetworkManager flags {flags}");
            assert_eq!(status.supported, Some(true));
        }
    }

    #[test]
    fn persists_and_reapplies_supported_magic_packet_without_reconnecting() {
        let nm = NetworkManager::default();
        let status = change(PROFILE, true, &nm, &|tool, args| nm.run(tool, args)).unwrap();
        assert!(status.configured);
        assert_eq!(status.applied, Some(true));
        assert_eq!(status.supported, Some(true));
        assert_eq!(status.problem, None);
        assert_eq!(
            nm.calls.borrow().as_slice(),
            [format!("modify {PROFILE} Wifi 0x8"), "reapply wlan0".into()]
        );
        assert!(
            !change(PROFILE, false, &nm, &|tool, args| nm.run(tool, args))
                .unwrap()
                .configured
        );
    }

    #[test]
    fn rejects_unsupported_inactive_and_invalid_profiles_before_mutation() {
        for inactive in [false, true] {
            let nm = NetworkManager::default();
            nm.inactive.set(inactive);
            nm.unsupported.set(!inactive);
            assert!(change(PROFILE, true, &nm, &|tool, args| nm.run(tool, args)).is_err());
            assert!(nm.calls.borrow().is_empty());
        }
        let nm = NetworkManager::default();
        assert!(change("--hostile", true, &nm, &|tool, args| nm.run(tool, args)).is_err());
        assert!(nm.calls.borrow().is_empty());
    }

    #[test]
    fn reports_saved_but_not_applied_instead_of_claiming_enabled() {
        let nm = NetworkManager::default();
        nm.reapply_failed.set(true);
        let status = change(PROFILE, true, &nm, &|tool, args| nm.run(tool, args)).unwrap();
        assert!(status.configured);
        assert_eq!(status.applied, Some(false));
        assert!(status.problem.unwrap().contains("saved"));
        assert!(nm.configured.get());
    }

    #[test]
    fn wifi_magic_packet_wake_is_read_from_the_named_radio_only() {
        let nm = NetworkManager::default();
        assert_eq!(wowlan_magic_packet("phy0", &|t, a| nm.run(t, a)), Ok(false));
        nm.applied.set(true);
        assert_eq!(wowlan_magic_packet("phy0", &|t, a| nm.run(t, a)), Ok(true));
        for name in ["", "phy", "wlan0", "phy0 ", "-phy0", "phy-1"] {
            assert!(wifi_magic_packet_enabled(name).is_err(), "{name:?}");
        }
    }
}
