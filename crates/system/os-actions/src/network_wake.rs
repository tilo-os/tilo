//! Persist magic-packet wake through `NetworkManager` without reconnecting.
//!
//! Driver support and current configuration are observations, not proof that
//! the whole machine resumes. No operation here suspends or reconnects it.

use serde::{Deserialize, Serialize};

use crate::network::validate_uuid;
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
    observe(profile, &system_command)
}

/// Change only magic-packet wake, persisting through subsequent activations.
///
/// # Errors
/// Enabling requires an active interface whose driver reports magic-packet
/// support. `NetworkManager` retains its normal authorization checks.
pub fn configure(profile: &str, enabled: bool) -> Result<NetworkWakeStatus, String> {
    change(profile, enabled, &system_command)
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

fn property(profile: &str, run: &Runner<'_>) -> Result<&'static str, String> {
    validate_uuid(profile)?;
    let kind = run(
        "nmcli",
        &[
            "-g",
            "connection.type",
            "connection",
            "show",
            "uuid",
            profile,
        ],
    )?;
    match kind.trim() {
        "802-11-wireless" => Ok("802-11-wireless.wake-on-wlan"),
        "802-3-ethernet" => Ok("802-3-ethernet.wake-on-lan"),
        _ => Err("magic-packet wake is offered only for Wi-Fi and Ethernet profiles".into()),
    }
}

fn active_interface(profile: &str, run: &Runner<'_>) -> Result<Option<String>, String> {
    let output = run(
        "nmcli",
        &["-t", "-f", "UUID,DEVICE", "connection", "show", "--active"],
    )?;
    for line in output.lines() {
        let Some((uuid, interface)) = line.split_once(':') else {
            continue;
        };
        if uuid == profile && interface != "--" && !interface.is_empty() {
            if interface.len() > 15
                || interface.starts_with('-')
                || !interface
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            {
                return Err("NetworkManager returned an invalid interface name".into());
            }
            return Ok(Some(interface.into()));
        }
    }
    Ok(None)
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
    let state = run("iw", &["phy", &phy, "wowlan", "show"])?;
    if !state.contains("WoWLAN is enabled:") && !state.contains("WoWLAN is disabled.") {
        return Err("Wi-Fi wake configuration was not reported".into());
    }
    Ok((
        supported,
        state.contains("wake up on magic packet") && state.contains("is enabled"),
    ))
}

fn observe(profile: &str, run: &Runner<'_>) -> Result<NetworkWakeStatus, String> {
    let property = property(profile, run)?;
    let wifi = property == "802-11-wireless.wake-on-wlan";
    let setting = run(
        "nmcli",
        &["-g", property, "connection", "show", "uuid", profile],
    )?;
    let number = setting
        .split_whitespace()
        .next()
        .and_then(|value| {
            value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
                .map_or_else(
                    || value.parse::<u32>().ok(),
                    |hex| u32::from_str_radix(hex, 16).ok(),
                )
        })
        .ok_or("NetworkManager returned an unreadable wake setting")?;
    let interface = active_interface(profile, run)?;
    let (supported, applied, problem) = match interface.as_deref() {
        Some(interface) => match driver(interface, wifi, run) {
            Ok((supported, applied)) => (Some(supported), Some(applied), None),
            Err(error) => (None, None, Some(error)),
        },
        None => (None, None, None),
    };
    Ok(NetworkWakeStatus {
        profile: profile.into(),
        interface,
        configured: number & if wifi { 0x08 } else { 0x40 } != 0,
        supported,
        applied,
        problem,
    })
}

fn change(profile: &str, enabled: bool, run: &Runner<'_>) -> Result<NetworkWakeStatus, String> {
    let before = observe(profile, run)?;
    if enabled && before.supported != Some(true) {
        return Err(before.problem.unwrap_or_else(|| {
            if before.interface.is_none() {
                "connect this profile before enabling magic-packet wake".into()
            } else {
                "the active driver does not report magic-packet wake support".into()
            }
        }));
    }
    let property = property(profile, run)?;
    run(
        "nmcli",
        &[
            "connection",
            "modify",
            "uuid",
            profile,
            property,
            if enabled { "magic" } else { "none" },
        ],
    )?;
    let reapply = if active_interface(profile, run)? == before.interface {
        before
            .interface
            .as_deref()
            .map(|interface| run("nmcli", &["device", "reapply", interface]))
            .transpose()
    } else {
        Err("the active connection changed; its wake setting was not reapplied".into())
    };
    let mut after = observe(profile, run)?;
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

    #[derive(Default)]
    struct NetworkManager {
        calls: RefCell<Vec<Vec<String>>>,
        configured: Cell<bool>,
        applied: Cell<bool>,
        inactive: Cell<bool>,
        unsupported: Cell<bool>,
        reapply_failed: Cell<bool>,
    }

    impl NetworkManager {
        fn run(&self, tool: &str, args: &[&str]) -> Result<String, String> {
            self.calls.borrow_mut().push(
                std::iter::once(tool)
                    .chain(args.iter().copied())
                    .map(str::to_owned)
                    .collect(),
            );
            match (tool, args) {
                ("nmcli", ["-g", "connection.type", ..]) => Ok("802-11-wireless\n".into()),
                ("nmcli", ["-g", "802-11-wireless.wake-on-wlan", ..]) => {
                    Ok(if self.configured.get() { "0x8" } else { "0x0" }.into())
                }
                ("nmcli", ["-t", ..]) => Ok(if self.inactive.get() {
                    String::new()
                } else {
                    format!("{PROFILE}:wlan0\n")
                }),
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
                (
                    "nmcli",
                    [
                        "connection",
                        "modify",
                        "uuid",
                        PROFILE,
                        "802-11-wireless.wake-on-wlan",
                        mode,
                    ],
                ) => {
                    self.configured.set(*mode == "magic");
                    Ok(String::new())
                }
                ("nmcli", ["device", "reapply", "wlan0"]) => {
                    if self.reapply_failed.get() {
                        return Err("not authorized".into());
                    }
                    self.applied.set(self.configured.get());
                    Ok(String::new())
                }
                _ => Err(format!("unexpected command: {tool} {args:?}")),
            }
        }
    }

    #[test]
    fn network_wake_reads_networkmanager_magic_and_not_handshake_flags() {
        let nm = NetworkManager::default();
        // NMSettingWirelessWakeOnWLan: MAGIC = 0x08, 4WAY_HANDSHAKE = 0x40.
        for (flags, expected) in [(8, true), (64, false), (72, true), (0, false)] {
            let status = observe(PROFILE, &|tool, args| {
                if tool == "nmcli" && args.starts_with(&["-g", "802-11-wireless.wake-on-wlan"]) {
                    Ok(format!("0x{flags:x}"))
                } else {
                    nm.run(tool, args)
                }
            })
            .unwrap();
            assert_eq!(status.configured, expected, "NetworkManager flags {flags}");
        }
        assert!(
            !nm.calls
                .borrow()
                .iter()
                .any(|call| call.iter().any(|arg| arg == "modify"))
        );
    }

    #[test]
    fn network_wake_reads_wired_magic_and_not_arp_flags() {
        let nm = NetworkManager::default();
        // NMSettingWiredWakeOnLan: MAGIC = 0x40, ARP = 0x20.
        for (flags, expected) in [(64, true), (32, false), (96, true), (0, false)] {
            let status = observe(PROFILE, &|tool, args| match (tool, args) {
                ("nmcli", ["-g", "connection.type", ..]) => Ok("802-3-ethernet".into()),
                ("nmcli", ["-g", "802-3-ethernet.wake-on-lan", ..]) => Ok(format!("0x{flags:x}")),
                ("ethtool", ["wlan0"]) => Ok("Supports Wake-on: g\nWake-on: d\n".into()),
                _ => nm.run(tool, args),
            })
            .unwrap();
            assert_eq!(status.configured, expected, "NetworkManager flags {flags}");
            assert_eq!(status.supported, Some(true));
        }
    }

    #[test]
    fn persists_and_reapplies_supported_magic_packet_without_reconnecting() {
        let nm = NetworkManager::default();
        let status = change(PROFILE, true, &|tool, args| nm.run(tool, args)).unwrap();
        assert!(status.configured);
        assert_eq!(status.applied, Some(true));
        assert_eq!(status.supported, Some(true));
        assert_eq!(status.problem, None);
        let calls = nm.calls.borrow();
        assert!(calls.iter().any(|call| call
            == &[
                "nmcli",
                "connection",
                "modify",
                "uuid",
                PROFILE,
                "802-11-wireless.wake-on-wlan",
                "magic"
            ]));
        assert!(
            !calls
                .iter()
                .flatten()
                .any(|arg| matches!(arg.as_str(), "up" | "down" | "disconnect"))
        );
        drop(calls);
        assert!(
            !change(PROFILE, false, &|tool, args| nm.run(tool, args))
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
            assert!(change(PROFILE, true, &|tool, args| nm.run(tool, args)).is_err());
            assert!(
                !nm.calls
                    .borrow()
                    .iter()
                    .flatten()
                    .any(|arg| arg == "modify")
            );
        }
        let nm = NetworkManager::default();
        assert!(change("--hostile", true, &|tool, args| nm.run(tool, args)).is_err());
        assert!(nm.calls.borrow().is_empty());
    }

    #[test]
    fn reports_saved_but_not_applied_instead_of_claiming_enabled() {
        let nm = NetworkManager::default();
        nm.reapply_failed.set(true);
        let status = change(PROFILE, true, &|tool, args| nm.run(tool, args)).unwrap();
        assert!(status.configured);
        assert_eq!(status.applied, Some(false));
        assert!(status.problem.unwrap().contains("saved"));
        assert!(nm.configured.get());
    }
}
