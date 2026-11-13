//! The person's trusted devices, as the host's identity actions answer them.

use ui::icons::Icon;
use ui::tilo_actions::remote::identity_admin::{self, FleetDevice, KeyKind};
use ui::tilo_actions::remote::{Confirmation, InvokeArgument};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a separate fact the list shows about one key"
)]
pub struct DeviceView {
    pub label: String,
    pub key: [u8; 32],
    pub fingerprint: String,
    pub current: bool,
    pub revoked: bool,
    pub can_host: bool,
    pub kind: KeyKind,
    pub browser: Option<String>,
    pub agent: Option<String>,
    pub issued_ms: Option<i64>,
    pub expires_ms: Option<i64>,
    pub last_active_ms: Option<i64>,
    pub online: bool,
    pub workspace: Option<String>,
}

const PHONE_WORDS: &[&str] = &[
    "phone", "pixel", "android", "galaxy", "mobile", "oneplus", "redmi", "xiaomi", "huawei",
    "motorola", "moto",
];
const TABLET_WORDS: &[&str] = &["ipad", "tablet", "tab"];
const LAPTOP_WORDS: &[&str] = &[
    "laptop",
    "notebook",
    "macbook",
    "chromebook",
    "thinkpad",
    "ideapad",
    "ultrabook",
    "framework",
    "surface",
    "xps",
];
const DESKTOP_WORDS: &[&str] = &[
    "desktop",
    "tower",
    "workstation",
    "imac",
    "mac mini",
    "mac studio",
    "server",
];

#[must_use]
pub fn device_icon(label: &str) -> Icon {
    let label = label.to_lowercase();
    let named = |words: &[&str]| words.iter().any(|word| label.contains(word));
    if named(TABLET_WORDS) {
        Icon::Tablet
    } else if named(LAPTOP_WORDS) {
        Icon::Laptop
    } else if named(DESKTOP_WORDS) {
        Icon::Monitor
    } else if named(PHONE_WORDS) {
        Icon::Smartphone
    } else {
        Icon::Monitor
    }
}

pub(crate) fn list(remote: &ui::RemoteActions) -> ui::InvocationCall {
    remote.invoke(
        identity_admin::target(identity_admin::DEVICES),
        InvokeArgument::None,
        None,
    )
}

pub(crate) fn revoke(
    remote: &ui::RemoteActions,
    device: &[u8; 32],
    reason: &str,
) -> ui::InvocationCall {
    let mut arguments = serde_json::Map::new();
    arguments.insert("device".into(), hex::encode(device).into());
    arguments.insert("reason".into(), reason.into());
    arguments.insert("confirm".into(), true.into());
    remote.invoke(
        identity_admin::target(identity_admin::REVOKE),
        InvokeArgument::Json(arguments),
        Some(Confirmation::Confirmed),
    )
}

pub(crate) fn devices(value: Option<serde_json::Value>) -> Result<Vec<DeviceView>, String> {
    let listed: Vec<FleetDevice> =
        serde_json::from_value(value.unwrap_or(serde_json::Value::Array(Vec::new())))
            .map_err(|_| "the device list the host answered is not readable".to_string())?;
    views(listed)
}

pub(crate) fn views(listed: Vec<FleetDevice>) -> Result<Vec<DeviceView>, String> {
    listed
        .into_iter()
        .map(|device| {
            let mut key = [0; 32];
            hex::decode_to_slice(&device.key_hex, &mut key)
                .map_err(|_| "the host answered a malformed device key".to_string())?;
            Ok(DeviceView {
                label: device.label,
                key,
                fingerprint: device.fingerprint,
                current: device.current,
                revoked: device.revoked,
                can_host: device.can_host,
                kind: device.kind,
                browser: device.browser,
                agent: device.agent,
                issued_ms: device.issued_ms,
                expires_ms: device.expires_ms,
                last_active_ms: device.last_active_ms,
                online: device.online,
                workspace: device.workspace,
            })
        })
        .collect()
}

/// What a settled host call answered, or why it did not.
///
/// # Errors
///
/// The host's refusal or failure, or that it was cancelled.
pub(crate) fn settled(
    call: &ui::InvocationCall,
) -> Option<Result<ui::tilo_actions::ActionOutput, String>> {
    use ui::tilo_actions::InvocationUpdate;
    let answer = call.get();
    if !answer.is_finished() {
        return None;
    }
    Some(match answer {
        ui::Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => Ok(output),
        ui::Arrival::Ready(InvocationUpdate::Failed { error, .. }) => Err(error.message),
        ui::Arrival::Failed(error) => Err(error.to_string()),
        ui::Arrival::Ready(_) | ui::Arrival::Pending { .. } => {
            Err("the host stopped before it answered".into())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_shows_the_machine_its_label_names() {
        for (label, icon) in [
            ("Pixel 9", Icon::Smartphone),
            ("Chrome on Android", Icon::Smartphone),
            ("Old phone", Icon::Smartphone),
            ("iPhone 15", Icon::Smartphone),
            ("Safari on iPad", Icon::Tablet),
            ("Galaxy Tab S9", Icon::Tablet),
            ("Framework 16", Icon::Laptop),
            ("Ada's laptop", Icon::Laptop),
            ("MacBook Pro", Icon::Laptop),
            ("Mac mini", Icon::Monitor),
            ("Office desktop", Icon::Monitor),
            ("Chrome on Linux", Icon::Monitor),
            ("Ada's machine", Icon::Monitor),
        ] {
            assert_eq!(device_icon(label), icon, "{label}");
        }
    }
}
