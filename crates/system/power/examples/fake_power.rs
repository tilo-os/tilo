//! A fake for the power authorities a machine's system bus serves, for a
//! session that serves its own system bus on a machine with none behind them:
//! a check's VM.
//!
//! `fake-power --bus unix:path=…` answers, on that bus, the interfaces
//! Settings reads: power-profiles-daemon's `net.hadess.PowerProfiles` (an
//! active profile and the profiles it offers) and `UPower`'s manager and display
//! device (a machine with no battery and no lid). It prints one line once it
//! owns the names and runs until it is killed.
//!
//! The real daemons need what a check's image has no systemd to start: a
//! `power-profiles-daemon` reading platform profiles from firmware, and an
//! `upowerd` whose first call asks polkit. This answers the same interfaces
//! with what such a machine reports.

#![allow(
    clippy::unused_self,
    reason = "the interfaces mirror the power daemons' fixed D-Bus signatures: a property takes \
              the object and reads nothing of it"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const PROFILES_PATH: &str = "/net/hadess/PowerProfiles";
const UPOWER_PATH: &str = "/org/freedesktop/UPower";
const DISPLAY_DEVICE_PATH: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
/// The profiles a machine offers, in power-profiles-daemon's order.
const PROFILES: [&str; 3] = ["performance", "balanced", "power-saver"];

fn lock(active: &Mutex<String>) -> std::sync::MutexGuard<'_, String> {
    active
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn entry(value: &str) -> Option<OwnedValue> {
    OwnedValue::try_from(Value::from(value)).ok()
}

struct Profiles {
    active: Arc<Mutex<String>>,
}

#[zbus::interface(name = "net.hadess.PowerProfiles")]
impl Profiles {
    #[zbus(property)]
    fn active_profile(&self) -> String {
        lock(&self.active).clone()
    }

    #[zbus(property)]
    fn set_active_profile(&self, profile: String) -> zbus::fdo::Result<()> {
        if !PROFILES.contains(&profile.as_str()) {
            return Err(zbus::fdo::Error::InvalidArgs(profile));
        }
        *lock(&self.active) = profile;
        Ok(())
    }

    #[zbus(property)]
    fn profiles(&self) -> Vec<HashMap<String, OwnedValue>> {
        PROFILES
            .iter()
            .filter_map(|name| {
                Some(HashMap::from([
                    ("Profile".to_owned(), entry(name)?),
                    ("Driver".to_owned(), entry("platform_profile")?),
                ]))
            })
            .collect()
    }

    #[zbus(property)]
    const fn actions(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(property)]
    const fn performance_degraded(&self) -> String {
        String::new()
    }

    #[zbus(property)]
    const fn active_profile_holds(&self) -> Vec<HashMap<String, OwnedValue>> {
        Vec::new()
    }
}

struct UPower;

#[zbus::interface(name = "org.freedesktop.UPower")]
impl UPower {
    #[zbus(property)]
    const fn on_battery(&self) -> bool {
        false
    }

    #[zbus(property)]
    const fn lid_is_present(&self) -> bool {
        false
    }

    #[zbus(property)]
    const fn lid_is_closed(&self) -> bool {
        false
    }

    #[zbus(property)]
    const fn daemon_version(&self) -> &'static str {
        "1.90.0"
    }

    fn enumerate_devices(&self) -> Vec<OwnedObjectPath> {
        OwnedObjectPath::try_from(DISPLAY_DEVICE_PATH)
            .into_iter()
            .collect()
    }
}

struct DisplayDevice;

#[zbus::interface(name = "org.freedesktop.UPower.Device")]
impl DisplayDevice {
    /// `UPower`'s `Type` for a machine with nothing to draw from.
    #[zbus(property, name = "Type")]
    const fn device_type(&self) -> u32 {
        0
    }

    #[zbus(property)]
    const fn is_present(&self) -> bool {
        false
    }

    #[zbus(property)]
    const fn percentage(&self) -> f64 {
        0.0
    }

    #[zbus(property)]
    const fn state(&self) -> u32 {
        0
    }

    #[zbus(property)]
    const fn capacity(&self) -> f64 {
        0.0
    }
}

/// Answer the power interfaces on `address` until killed.
async fn serve(address: &str) -> Result<(), Box<dyn std::error::Error>> {
    let active = Arc::new(Mutex::new("balanced".to_owned()));
    let connection = zbus::connection::Builder::address(address)?
        .name("net.hadess.PowerProfiles")?
        // UPower 1.90 also answers the profiles under its own name, which is
        // the one some clients follow.
        .name("org.freedesktop.UPower")?
        .name("org.freedesktop.UPower.PowerProfiles")?
        .serve_at(PROFILES_PATH, Profiles { active })?
        .serve_at(UPOWER_PATH, UPower)?
        .serve_at(DISPLAY_DEVICE_PATH, DisplayDevice)?
        .build()
        .await?;
    println!(
        "fake power: {} on {address}",
        connection
            .unique_name()
            .map(std::string::ToString::to_string)
            .unwrap_or_default()
    );
    std::io::Write::flush(&mut std::io::stdout())?;
    // The session stops it with its own group; nothing else ends it.
    tokio::signal::ctrl_c().await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut address = std::env::var("DBUS_SYSTEM_BUS_ADDRESS").unwrap_or_default();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--bus" => address = args.next().unwrap_or_default(),
            other => return Err(format!("unknown argument {other}; use --bus unix:path=…").into()),
        }
    }
    if address.is_empty() {
        return Err(
            "no bus to serve on: pass --bus unix:path=… or set DBUS_SYSTEM_BUS_ADDRESS".into(),
        );
    }
    serve(&address).await?;
    Ok(())
}
