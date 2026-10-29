//! `org.freedesktop.impl.portal.Usb`: which USB devices an app may open.
//!
//! The frontend enumerates the devices and opens the ones allowed; the
//! backend asks the person (a question of kind [`USB_QUESTION`] listing the
//! devices) and answers with exactly the ones they approved, named in the
//! answer's `uris` by the identifiers the question offered.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use zbus::interface;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::reply::{Ended, OrFail, Outcome, Reply, Results, answer};
use crate::{Portal, option, plain_text};

/// The kind of the question letting an app open USB devices asks.
pub const USB_QUESTION: &str = "portal.usb";

const MAX_DEVICES: usize = 64;

/// One device an app asks for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbDevice {
    /// The frontend's identifier for it.
    pub id: String,
    /// Its name, as the device reports it.
    pub label: Option<String>,
    /// Whether the app asks to write to it too.
    pub writable: bool,
}

/// What the sheet shows for USB devices an app asks for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbQuestion {
    pub devices: Vec<UsbDevice>,
}

type Requested = Vec<(
    String,
    HashMap<String, OwnedValue>,
    HashMap<String, OwnedValue>,
)>;

/// The devices asked for; `None` when one is malformed or repeated.
fn validate(devices: Requested) -> Option<Vec<UsbDevice>> {
    if devices.len() > MAX_DEVICES {
        return None;
    }
    let mut seen = HashSet::new();
    devices
        .into_iter()
        .map(|(id, info, access)| {
            (!id.is_empty() && plain_text(&id, 512) && seen.insert(id.clone())).then_some(())?;
            let label = ["device", "product", "name"]
                .into_iter()
                .find_map(|key| option::<String>(&info, key))
                .filter(|label| !label.is_empty() && plain_text(label, 256));
            Some(UsbDevice {
                id,
                label,
                writable: option::<bool>(&access, "writable").unwrap_or(false),
            })
        })
        .collect()
}

/// The reply approving the devices of `devices` named in `approved`.
fn approved_result(devices: &[UsbDevice], approved: &[String]) -> Outcome {
    let approved: HashSet<&str> = approved.iter().map(String::as_str).collect();
    let rows: Vec<(String, HashMap<String, OwnedValue>)> = devices
        .iter()
        .filter(|device| approved.contains(device.id.as_str()))
        .map(|device| {
            let access =
                HashMap::from([("writable".to_owned(), OwnedValue::from(device.writable))]);
            (device.id.clone(), access)
        })
        .collect();
    let mut results = Results::new();
    results.insert(
        "devices".into(),
        OwnedValue::try_from(Value::new(rows)).or_fail()?,
    );
    Ok(results)
}

/// The `org.freedesktop.impl.portal.Usb` interface.
pub struct Usb {
    portal: Portal,
}

impl Usb {
    #[must_use]
    pub const fn new(portal: Portal) -> Self {
        Self { portal }
    }
}

#[interface(name = "org.freedesktop.impl.portal.Usb")]
impl Usb {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        1
    }

    async fn acquire_devices(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        parent_window: String,
        app_id: String,
        devices: Requested,
        _options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async {
            let path = handle.as_str();
            let auth = self
                .portal
                .authenticate(&header, path, &app_id, Some(&parent_window))
                .await?;
            let devices = validate(devices).ok_or(Ended::Failed)?;
            if devices.is_empty() {
                return approved_result(&devices, &[]);
            }
            let asked = self.portal.ask(
                &auth.caller,
                path,
                USB_QUESTION,
                UsbQuestion {
                    devices: devices.clone(),
                },
            );
            let answered = self
                .portal
                .requests
                .run(connection, path, auth.owner.as_str(), asked)
                .await?;
            approved_result(&devices, &answered.uris)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_is_an_exact_subset_of_valid_requests() {
        let device = |id: &str| (id.to_owned(), HashMap::new(), HashMap::new());
        assert!(validate(vec![device("a"), device("a")]).is_none());
        assert!(validate(vec![device("bad\nidentifier")]).is_none());
        let devices = vec![
            UsbDevice {
                id: "read-only".into(),
                label: None,
                writable: false,
            },
            UsbDevice {
                id: "read-write".into(),
                label: None,
                writable: true,
            },
        ];
        let results =
            approved_result(&devices, &["read-write".into(), "not-requested".into()]).unwrap();
        let rows = Vec::<(String, HashMap<String, OwnedValue>)>::try_from(
            results.get("devices").unwrap().try_clone().unwrap(),
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "read-write");
        assert!(bool::try_from(rows[0].1.get("writable").unwrap().try_clone().unwrap()).unwrap());
    }
}
