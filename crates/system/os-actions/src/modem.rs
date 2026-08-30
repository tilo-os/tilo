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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimInfo {
    pub path: String,
    pub label: String,
    pub operator: Option<String>,
}

pub fn list_mobile_sims() -> Result<Vec<SimInfo>, String> {
    let list = crate::process::run_capture("mmcli", &["--list-modems"])?;
    let mut out = Vec::new();
    for modem in list
        .lines()
        .filter_map(|line| line.split_whitespace().find(|p| p.starts_with("/org/")))
    {
        let details =
            crate::process::run_capture("mmcli", &["--modem", modem, "--output-keyvalue"])?;
        let sim = details.lines().find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.trim()
                .ends_with("generic.sim")
                .then(|| v.trim().to_string())
        });
        let Some(path) = sim.filter(|p| p.starts_with("/org/freedesktop/ModemManager1/SIM/"))
        else {
            continue;
        };
        let sim_data = crate::process::run_capture("mmcli", &["--sim", &path, "--output-keyvalue"])
            .unwrap_or_default();
        let operator = sim_data.lines().find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.trim()
                .ends_with("properties.operator-name")
                .then(|| v.trim().to_string())
                .filter(|v| !v.is_empty() && v != "--")
        });
        let slot = path.rsplit('/').next().unwrap_or("SIM").to_string();
        out.push(SimInfo {
            path,
            label: operator.clone().unwrap_or_else(|| format!("SIM {slot}")),
            operator,
        });
    }
    Ok(out)
}

pub fn unlock_mobile_sim_on(sim: &str, pin: &str) -> Result<(), String> {
    validate_pin(pin)?;
    if !sim.starts_with("/org/freedesktop/ModemManager1/SIM/")
        || !sim
            .rsplit('/')
            .next()
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("invalid ModemManager SIM object".into());
    }
    let sim = sim.to_string();

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

pub fn unlock_mobile_sim(pin: &str) -> Result<(), String> {
    validate_pin(pin)?;
    let sims = list_mobile_sims()?;
    let [sim] = sims.as_slice() else {
        return Err(if sims.is_empty() {
            "ModemManager reports no SIM".into()
        } else {
            "more than one SIM is present; choose one before unlocking".into()
        });
    };
    unlock_mobile_sim_on(&sim.path, pin)
}

fn validate_pin(pin: &str) -> Result<(), String> {
    if !(4..=8).contains(&pin.len()) || !pin.bytes().all(|b| b.is_ascii_digit()) {
        return Err("SIM PIN must contain 4–8 digits".into());
    }
    Ok(())
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
