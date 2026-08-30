//! Mobile-broadband SIM operations through ModemManager.
//!
//! The PIN is sent as a D-Bus message body. It never appears in argv, an
//! environment variable, NetworkManager profile storage, or qOS settings.

use std::sync::OnceLock;

use tokio::runtime::Runtime;

#[zbus::proxy(interface = "org.freedesktop.ModemManager1.Sim")]
trait ModemManagerSim {
    fn send_pin(&self, pin: &str) -> zbus::Result<()>;
}

fn runtime() -> Result<&'static Runtime, String> {
    static RT: OnceLock<Option<Runtime>> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()
    })
    .as_ref()
    .ok_or_else(|| "could not start the mobile-broadband runtime".to_string())
}

/// Unlock the first SIM ModemManager reports. Multi-SIM selection belongs in
/// a later device picker; this function refuses ambiguity instead of sending
/// a PIN to an arbitrary modem.
pub fn unlock_mobile_sim(pin: &str) -> Result<(), String> {
    if !(4..=8).contains(&pin.len()) || !pin.bytes().all(|b| b.is_ascii_digit()) {
        return Err("SIM PIN must contain 4–8 digits".into());
    }
    let list = crate::process::run_capture("mmcli", &["--list-modems"])?;
    let modems: Vec<&str> = list
        .lines()
        .filter_map(|line| {
            line.split_whitespace()
                .find(|part| part.starts_with("/org/"))
        })
        .collect();
    let [modem] = modems.as_slice() else {
        return Err(if modems.is_empty() {
            "ModemManager reports no modem".into()
        } else {
            "more than one modem is present; choose a SIM before unlocking".into()
        });
    };
    let details = crate::process::run_capture("mmcli", &["--modem", modem, "--output-keyvalue"])?;
    let sim = details
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .ends_with("generic.sim")
                .then(|| value.trim().to_string())
        })
        .filter(|path| path.starts_with("/org/freedesktop/ModemManager1/SIM/"))
        .ok_or("ModemManager did not report a SIM object")?;

    runtime()?.block_on(async move {
        let connection = zbus::Connection::system()
            .await
            .map_err(|e| format!("cannot reach ModemManager: {e}"))?;
        let proxy = ModemManagerSimProxy::builder(&connection)
            .destination("org.freedesktop.ModemManager1")
            .map_err(|e| format!("invalid ModemManager service: {e}"))?
            .path(sim)
            .map_err(|e| format!("invalid SIM object: {e}"))?
            .build()
            .await
            .map_err(|e| format!("cannot bind SIM object: {e}"))?;
        proxy
            .send_pin(pin)
            .await
            .map_err(|e| format!("SIM unlock failed: {e}"))
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn invalid_pin_is_rejected_before_discovery() {
        assert!(
            super::unlock_mobile_sim("12ab")
                .unwrap_err()
                .contains("digits")
        );
        assert!(super::unlock_mobile_sim("123").unwrap_err().contains("4–8"));
    }
}
