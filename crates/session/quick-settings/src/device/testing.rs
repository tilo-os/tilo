//! A stand-in machine and the person's settings in memory, for headless
//! tests of the panel on a device and of the sessions that host it.

#![allow(clippy::unwrap_used, reason = "a poisoned stand-in is a failed test")]

use std::sync::{Arc, Mutex};

use media::audio::default_node::VolumeState;
use settings_store::hardware::QuickSettingsConfig;

use super::{DeviceReads, Preferences};
use crate::DisplayBrightness;
use crate::actions::QsActionAvailability;

/// The stand-in machine: what it reads, and every change made to it.
#[derive(Clone, Default)]
pub struct Machine(Arc<Mutex<Inner>>);

#[derive(Default)]
struct Inner {
    brightness: f32,
    volume: Option<VolumeState>,
    bluetooth: bool,
    power: Option<os_actions::PowerProfile>,
    radios: os_actions::RadioState,
    vpns: Vec<os_actions::VpnConnectionInfo>,
    networks: Vec<os_actions::WifiNetworkInfo>,
    accessories: Vec<os_actions::BtDeviceInfo>,
    battery: u8,
    /// Changes made, in order.
    calls: Vec<String>,
    /// Changes the machine refuses, by name.
    refuses: Vec<&'static str>,
}

impl Machine {
    /// A laptop on Wi-Fi with Bluetooth headphones and a VPN.
    #[must_use]
    pub fn laptop() -> Self {
        let machine = Self::default();
        {
            let mut inner = machine.0.lock().unwrap();
            inner.brightness = 0.7;
            inner.volume = Some(VolumeState {
                level: 0.3,
                muted: false,
            });
            inner.bluetooth = true;
            inner.power = Some(os_actions::PowerProfile::Performance);
            inner.radios = os_actions::RadioState {
                wifi_available: true,
                wifi_enabled: true,
                ..os_actions::RadioState::default()
            };
            inner.vpns = vec![os_actions::VpnConnectionInfo {
                name: "Work".into(),
                uuid: "work".into(),
                active: false,
                kind: "wireguard".into(),
            }];
            inner.networks = vec![os_actions::WifiNetworkInfo {
                ssid: "Home".into(),
                signal: 4,
                secure: true,
                security: os_actions::WifiSecurity::default(),
                connected: true,
                known: true,
            }];
            inner.accessories = vec![os_actions::BtDeviceInfo {
                name: "Headphones".into(),
                mac: "00:1B:66:A1:B2:C3".into(),
                battery: 80,
                connected: true,
            }];
            inner.battery = 64;
        }
        machine
    }

    /// The changes made, oldest first.
    #[must_use]
    pub fn calls(&self) -> Vec<String> {
        self.0.lock().unwrap().calls.clone()
    }

    /// Refuse the change named `name` from now on.
    pub fn refuse(&self, name: &'static str) {
        self.0.lock().unwrap().refuses.push(name);
    }

    fn change(&self, call: String, name: &'static str) -> Result<(), String> {
        let mut inner = self.0.lock().unwrap();
        inner.calls.push(call);
        if inner.refuses.contains(&name) {
            Err(format!("{name} refused"))
        } else {
            Ok(())
        }
    }
}

impl DeviceReads for Machine {
    fn brightness(&self) -> Option<f32> {
        Some(self.0.lock().unwrap().brightness)
    }
    fn displays(&self) -> Vec<DisplayBrightness> {
        Vec::new()
    }
    fn output_device(&self) -> bool {
        true
    }
    fn volume(&self) -> Option<VolumeState> {
        self.0.lock().unwrap().volume
    }
    fn bluetooth(&self) -> Option<bool> {
        Some(self.0.lock().unwrap().bluetooth)
    }
    fn power(&self) -> Option<os_actions::PowerProfile> {
        self.0.lock().unwrap().power
    }
    fn power_profiles(&self) -> Option<Vec<os_actions::PowerProfile>> {
        Some(vec![
            os_actions::PowerProfile::Saver,
            os_actions::PowerProfile::Balanced,
            os_actions::PowerProfile::Performance,
        ])
    }
    fn wired(&self) -> Option<os_actions::WiredInfo> {
        None
    }
    fn battery(&self) -> tilo_power_mgmt::BatterySnapshot {
        tilo_power_mgmt::BatterySnapshot {
            present: true,
            percent: self.0.lock().unwrap().battery,
            state: tilo_power_mgmt::BatteryState::Discharging,
        }
    }
    fn battery_time(&self) -> Option<tilo_power_mgmt::BatteryTime> {
        None
    }
    fn radios(&self) -> Option<os_actions::RadioState> {
        Some(self.0.lock().unwrap().radios.clone())
    }
    fn privacy(&self) -> os_actions::PrivacyStatus {
        os_actions::PrivacyStatus::default()
    }
    fn vpns(&self) -> Option<Vec<os_actions::VpnConnectionInfo>> {
        Some(self.0.lock().unwrap().vpns.clone())
    }
    fn link(&self) -> Option<os_actions::LinkStatus> {
        Some(os_actions::LinkStatus {
            kind: os_actions::LinkKind::Wifi,
            name: Some("Home".into()),
            signal: 4,
        })
    }
    fn wifi_networks(&self) -> Result<Vec<os_actions::WifiNetworkInfo>, String> {
        Ok(self.0.lock().unwrap().networks.clone())
    }
    fn scan_wifi(&self) -> Result<(), String> {
        Ok(())
    }
    fn bt_devices(&self) -> Option<Vec<os_actions::BtDeviceInfo>> {
        Some(self.0.lock().unwrap().accessories.clone())
    }
    fn firmware_updates(&self) -> Option<usize> {
        Some(2)
    }
    fn availability(&self) -> QsActionAvailability {
        QsActionAvailability::all()
    }
}

impl os_actions::Dispatcher for Machine {
    fn set_brightness(&self, pct: f32) -> Result<(), String> {
        self.change(format!("brightness:{pct}"), "brightness")?;
        self.0.lock().unwrap().brightness = pct / 100.0;
        Ok(())
    }
    fn set_volume(&self, pct: f32) -> Result<(), String> {
        self.change(format!("volume:{pct}"), "volume")?;
        let mut inner = self.0.lock().unwrap();
        let muted = inner.volume.is_some_and(|volume| volume.muted);
        inner.volume = Some(VolumeState {
            level: pct / 100.0,
            muted,
        });
        drop(inner);
        Ok(())
    }
    fn set_mute(&self, muted: bool) -> Result<(), String> {
        self.change(format!("mute:{muted}"), "mute")?;
        let mut inner = self.0.lock().unwrap();
        if let Some(volume) = inner.volume.as_mut() {
            volume.muted = muted;
        }
        drop(inner);
        Ok(())
    }
    fn set_wifi(&self, enabled: bool) -> Result<(), String> {
        self.change(format!("wifi:{enabled}"), "wifi")?;
        self.0.lock().unwrap().radios.wifi_enabled = enabled;
        Ok(())
    }
    fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
        self.change(format!("bluetooth:{enabled}"), "bluetooth")?;
        self.0.lock().unwrap().bluetooth = enabled;
        Ok(())
    }
    fn disconnect_bluetooth(&self, address: &str) -> Result<(), String> {
        self.change(format!("disconnect:{address}"), "disconnect")?;
        for accessory in &mut self.0.lock().unwrap().accessories {
            if accessory.mac == address {
                accessory.connected = false;
            }
        }
        Ok(())
    }
    fn set_vpn_connection(&self, name: &str, enabled: bool) -> Result<(), String> {
        self.change(format!("vpn:{name}:{enabled}"), "vpn")
    }
    fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
        self.change(
            format!("join:{ssid}:{}", password.unwrap_or("")),
            "connect_wifi",
        )
    }
    fn set_power_profile(&self, profile: os_actions::PowerProfile) -> Result<(), String> {
        self.change(format!("power:{profile:?}"), "power")?;
        self.0.lock().unwrap().power = Some(profile);
        Ok(())
    }
}

/// The person's settings, in memory.
#[derive(Clone, Default)]
pub struct Kept(Arc<Mutex<Option<QuickSettingsConfig>>>);

impl Kept {
    /// The arrangement saved last, if any.
    #[must_use]
    pub fn saved(&self) -> Option<QuickSettingsConfig> {
        self.0.lock().unwrap().clone()
    }
}

impl Preferences for Kept {
    fn arrangement(&self) -> QuickSettingsConfig {
        self.0.lock().unwrap().clone().unwrap_or_default()
    }
    fn save_arrangement(&self, arrangement: &QuickSettingsConfig) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(arrangement.clone());
        Ok(())
    }
    fn night_light(&self) -> bool {
        false
    }
    fn set_night_light(&self, _on: bool) -> Result<(), String> {
        Ok(())
    }
    fn rotation_lock(&self) -> bool {
        false
    }
    fn set_rotation_lock(&self, _locked: bool) -> Result<(), String> {
        Ok(())
    }
    fn rotation_available(&self) -> bool {
        false
    }
}
