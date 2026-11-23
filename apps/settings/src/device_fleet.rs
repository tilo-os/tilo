//! The person's devices, as every device of theirs lists them: the record of their own workspace
//! and who is there, both followed, joined as every list of them is
//! ([`tilo_server_protocol::devices::fleet`]); and the `devices` actions that name and revoke one.

use ui::icons::Icon;
use ui::tilo_actions::remote::device_tickets::{self, FleetDevice, KeyKind};
use ui::tilo_actions::remote::{Confirmation, InvokeArgument, PRESENCE, ResourceReadRequest};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceView {
    pub label: String,
    pub key: [u8; 32],
    pub fingerprint: String,
    pub current: bool,
    pub revoked: bool,
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

pub(crate) fn revoke(
    remote: &ui::RemoteActions,
    device: &[u8; 32],
    reason: &str,
) -> ui::InvocationCall {
    let mut arguments = serde_json::Map::new();
    arguments.insert("device".into(), hex::encode(device).into());
    arguments.insert("reason".into(), reason.into());
    remote.invoke(
        device_tickets::target(device_tickets::REVOKE),
        InvokeArgument::Json(arguments),
        Some(Confirmation::Confirmed),
    )
}

/// Name `device` `name`: what the person's device lists read it by from then
/// on.
pub(crate) fn rename(
    remote: &ui::RemoteActions,
    device: &[u8; 32],
    name: &str,
) -> ui::InvocationCall {
    let mut arguments = serde_json::Map::new();
    arguments.insert("device".into(), hex::encode(device).into());
    arguments.insert("name".into(), name.into());
    remote.invoke(
        device_tickets::target(device_tickets::RENAME),
        InvokeArgument::Json(arguments),
        None,
    )
}

/// The reads of the person's devices this page follows, joined.
#[derive(Default)]
pub(crate) struct FleetRead {
    /// The person, and this device among theirs, as this device's identity names them.
    who: Option<([u8; 32], [u8; 32])>,
    /// Their devices, their tab groups, their list of workspaces, and who is in their own
    /// workspace: each followed, and read again as it changes.
    reads: Vec<ui::FollowedRead>,
    seen: Vec<u64>,
    /// Whether the list has been read once, or failed to be.
    pub(crate) answered: bool,
    pub(crate) error: Option<String>,
}

impl FleetRead {
    /// Follow the person's devices from now on, as this device's identity names the person.
    pub(crate) fn follow(&mut self, remote: &ui::RemoteActions) {
        use tilo_server_protocol::devices::{DEVICES, TAB_GROUPS};
        let identity = ui::local_identity::load();
        let Some((person, this)) = identity
            .and_then(|identity| Some((identity.group_public_key, identity.device_public_key?)))
        else {
            self.answered = true;
            self.error = Some("this device does not know whose it is yet".to_owned());
            return;
        };
        let own = tilo_action_policy::roles::person_workspace_id(&person);
        let follow = |(app, record): (&str, &str)| {
            remote.follow_resource(
                None,
                ResourceReadRequest {
                    workspace: own.clone(),
                    app: app.to_owned(),
                    record: record.to_owned(),
                    joined: false,
                    after: None,
                    wait_ms: None,
                },
            )
        };
        self.reads = vec![
            follow(DEVICES),
            follow(TAB_GROUPS),
            follow(tilo_server_protocol::workspaces::IDENTITY_LIST),
            follow((PRESENCE, "")),
        ];
        self.seen = vec![0; self.reads.len()];
        self.who = Some((person, this));
    }

    /// What the reads answered since the last look: the devices as they are now, or why there are
    /// none. `None` while nothing new came, or the devices are not read yet.
    pub(crate) fn take(&mut self) -> Option<Result<Vec<DeviceView>, String>> {
        use tilo_server_protocol::devices::{FleetRecords, fleet, here_in, record_value};
        let (person, this) = self.who?;
        let revisions: Vec<u64> = self
            .reads
            .iter()
            .map(|read| read.call().revision())
            .collect();
        if revisions == self.seen {
            return None;
        }
        self.seen = revisions;
        // The devices record decides the list; the others add to it, and one this reader may not
        // read, or that no one has written yet, adds nothing. The list waits for every read, so
        // it never shows a device offline only because presence answered last.
        let mut values = Vec::new();
        for (at, read) in self.reads.iter().enumerate() {
            match read.call().get() {
                ui::Arrival::Ready(value) => values.push(value),
                ui::Arrival::Failed(error) if at == 0 => {
                    let error = error.to_string();
                    self.answered = true;
                    self.error = Some(error.clone());
                    return Some(Err(error));
                }
                ui::Arrival::Failed(_) => values.push(serde_json::Value::Null),
                ui::Arrival::Pending { .. } => {
                    self.seen.clear();
                    return None;
                }
            }
        }
        let [devices, tab_groups, workspaces, here] =
            <[serde_json::Value; 4]>::try_from(values).ok()?;
        let records = FleetRecords {
            devices: record_value(&devices),
            tab_groups: record_value(&tab_groups),
            workspaces: record_value(&workspaces),
        };
        self.answered = true;
        self.error = None;
        Some(Ok(fleet(&person, &this, &records, &here_in(&here))
            .into_iter()
            .map(view)
            .collect()))
    }
}

/// `device` as a row of the page shows it.
fn view(device: FleetDevice) -> DeviceView {
    let mut key = [0; 32];
    // The join lists a device by the key its root certified, so it is always one.
    let _ = hex::decode_to_slice(&device.key_hex, &mut key);
    DeviceView {
        label: device.label,
        key,
        fingerprint: device.fingerprint,
        current: device.current,
        revoked: device.revoked,
        kind: device.kind,
        browser: device.browser,
        agent: device.agent,
        issued_ms: device.issued_ms,
        expires_ms: device.expires_ms,
        last_active_ms: device.last_active_ms,
        online: device.online,
        workspace: device.workspace,
    }
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
