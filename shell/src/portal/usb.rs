//! User-mediated USB-device selection for the portal frontend.
//!
//! The frontend enumerates devices and performs the actual narrow device
//! opening. The backend authenticates that frontend, shows an unspoofable
//! shell sheet, and returns only the approved identifiers.

use std::collections::{HashMap, HashSet};

use zbus::interface;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::shell::portal_dialog::{
    PortalDialogQueue, ReviewDecision, ReviewRequest, UsbDeviceDecision, UsbDeviceRequest,
    UsbDeviceRow,
};

use super::lifecycle::{LifecycleEntry, LifecycleResource, PortalLifecycle};
use super::{PortalContext, auth::authenticate_call};

const MAX_DEVICES: usize = 64;
const MAX_IDENTIFIER: usize = 512;
const MAX_LABEL: usize = 256;

pub struct UsbBackend {
    ctx: PortalContext,
    dialogs: PortalDialogQueue,
    lifecycle: PortalLifecycle,
}

impl UsbBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self::with_lifecycle(ctx, PortalLifecycle::new())
    }

    pub fn with_lifecycle(ctx: PortalContext, lifecycle: PortalLifecycle) -> Self {
        Self {
            ctx,
            dialogs: PortalDialogQueue::global(),
            lifecycle,
        }
    }
}

struct RequestedDevice {
    id: String,
    writable: bool,
    label: Option<String>,
}

#[interface(name = "org.freedesktop.impl.portal.Usb")]
impl UsbBackend {
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
        devices: Vec<(
            String,
            HashMap<String, OwnedValue>,
            HashMap<String, OwnedValue>,
        )>,
        _options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let sender = header.sender().cloned().map(OwnedUniqueName::from);
        let Ok(auth) =
            authenticate_call(&self.ctx, sender.as_ref(), &app_id, Some(&parent_window)).await
        else {
            return (2, HashMap::new());
        };
        let Some(requested) = validate_devices(devices) else {
            return (2, HashMap::new());
        };
        if requested.is_empty() {
            return approved_result(&requested, &[]);
        }
        let request_path = handle.to_string();
        let dialogs = self.dialogs.clone();
        let cancel_path = request_path.clone();
        let entry = LifecycleEntry {
            owner: auth.owner.to_string(),
            app_id: auth.caller.app_id,
            resource: LifecycleResource::Prompt,
        };
        let registered = self
            .lifecycle
            .register_request(connection, &request_path, entry, move |_| {
                let _ = dialogs.cancel(&cancel_path);
            })
            .await;
        if !matches!(registered, Ok(true)) {
            return (2, HashMap::new());
        }
        let receiver = match self.dialogs.enqueue_usb_devices(UsbDeviceRequest {
            review: ReviewRequest {
                handle: request_path.clone(),
                app_id,
                title: "Choose USB devices".into(),
                reason: "Select devices individually; read-write can modify a device.".into(),
                resource: "Space or click toggles a row. Access ends on release or disconnect."
                    .into(),
                confirm_label: "Allow selected".into(),
            },
            devices: requested
                .iter()
                .map(|device| UsbDeviceRow {
                    id: device.id.clone(),
                    label: device
                        .label
                        .clone()
                        .unwrap_or_else(|| "USB device".to_string()),
                    writable: device.writable,
                })
                .collect(),
        }) {
            Ok(receiver) => receiver,
            Err(_) => {
                self.lifecycle
                    .complete_request_object(connection, &request_path)
                    .await;
                return (2, HashMap::new());
            }
        };
        let response = match receiver.await {
            Ok(UsbDeviceDecision {
                decision: ReviewDecision::Confirmed,
                approved,
            }) => approved_result(&requested, &approved),
            Ok(UsbDeviceDecision {
                decision: ReviewDecision::Cancelled,
                ..
            })
            | Err(_) => (1, HashMap::new()),
        };
        self.lifecycle
            .complete_request_object(connection, &request_path)
            .await;
        response
    }
}

fn validate_devices(
    devices: Vec<(
        String,
        HashMap<String, OwnedValue>,
        HashMap<String, OwnedValue>,
    )>,
) -> Option<Vec<RequestedDevice>> {
    if devices.len() > MAX_DEVICES {
        return None;
    }
    let mut seen = HashSet::new();
    devices
        .into_iter()
        .map(|(id, info, access)| {
            if id.is_empty()
                || id.len() > MAX_IDENTIFIER
                || id.chars().any(char::is_control)
                || !seen.insert(id.clone())
            {
                return None;
            }
            let writable = access
                .get("writable")
                .and_then(|value| value.try_clone().ok())
                .and_then(|value| bool::try_from(value).ok())
                .unwrap_or(false);
            let label = ["device", "product", "name"]
                .into_iter()
                .find_map(|key| {
                    info.get(key)
                        .and_then(|value| value.try_clone().ok())
                        .and_then(|value| String::try_from(value).ok())
                })
                .filter(|label| {
                    !label.is_empty()
                        && label.len() <= MAX_LABEL
                        && !label.chars().any(char::is_control)
                });
            Some(RequestedDevice {
                id,
                writable,
                label,
            })
        })
        .collect()
}

fn approved_result(
    devices: &[RequestedDevice],
    approved_ids: &[String],
) -> (u32, HashMap<String, OwnedValue>) {
    let approved_ids: HashSet<&str> = approved_ids.iter().map(String::as_str).collect();
    let approved: Vec<(String, HashMap<String, OwnedValue>)> = devices
        .iter()
        .filter(|device| approved_ids.contains(device.id.as_str()))
        .map(|device| {
            let mut access = HashMap::new();
            access.insert("writable".into(), OwnedValue::from(device.writable));
            (device.id.clone(), access)
        })
        .collect();
    let mut result = HashMap::new();
    let Ok(value) = OwnedValue::try_from(Value::new(approved)) else {
        return (2, HashMap::new());
    };
    result.insert("devices".into(), value);
    (0, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_and_duplicate_identifiers_fail_closed() {
        let device = |id: &str| (id.to_string(), HashMap::new(), HashMap::new());
        assert!(validate_devices(vec![device("a"), device("a")]).is_none());
        assert!(validate_devices(vec![device("bad\nidentifier")]).is_none());
    }

    #[test]
    fn approval_returns_only_id_and_requested_write_bit() {
        let devices = vec![RequestedDevice {
            id: "device-1".into(),
            writable: false,
            label: Some("Camera".into()),
        }];
        let (response, result) = approved_result(&devices, &["device-1".into()]);
        assert_eq!(response, 0);
        assert!(result.contains_key("devices"));
    }

    #[test]
    fn approval_is_an_exact_subset_and_cannot_upgrade_write_access() {
        let devices = vec![
            RequestedDevice {
                id: "read-only".into(),
                writable: false,
                label: None,
            },
            RequestedDevice {
                id: "read-write".into(),
                writable: true,
                label: None,
            },
        ];
        let (response, result) =
            approved_result(&devices, &["read-write".into(), "not-requested".into()]);
        assert_eq!(response, 0);
        let result = result.get("devices").unwrap().try_clone().unwrap();
        let rows = Vec::<(String, HashMap<String, OwnedValue>)>::try_from(result).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "read-write");
        assert_eq!(
            bool::try_from(rows[0].1.get("writable").unwrap().try_clone().unwrap()).unwrap(),
            true
        );
    }
}
