//! The person's devices as this session admits them: the record of their own workspace, followed
//! through the record API, answered by one [`DevicesTrust`] every admission in this process asks.
//!
//! The session keeps none of their devices itself. A session a daemon started reads the record
//! from that daemon on its shell link, as every app's read of it is decided there, and asks it
//! for the revocations the person's root signed, which a record's mark cannot say; a session the
//! host placed in a workspace's VM reads the record from the host that certified it; and a
//! session that keeps the workspaces itself ([`ServiceSlot`]) answers from their service, which
//! is where its record lives. Each fills the same source, so a device is admitted here wherever
//! the host that holds the record says it is, and refused when that host cannot be asked.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::Value;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReadAnswer, RemoteActionMessage, RemoteActionTarget,
    ResourceReadRequest,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_desktop_model::WorkspaceId;
use tilo_identity::sources::{DeviceTrust, DevicesSeen, DevicesTrust, install_device_trust};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::workspace_documents::DocumentRead;
use tilo_session_client::daemon_documents::{DaemonShell, document_read};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::apps::services::DeviceDaemon;
use tilo_workspace_service::service::ServiceSlot;
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::person_workspace::devices::{DEVICES, Devices};

use crate::hosted_workspaces::HostedWorkspaces;

/// How long one held read of the person's devices waits for the record to move, ms.
const FOLLOW_WAIT_MS: u32 = 25_000;
/// How long a follow waits before it reads again after a read that did not arrive or did not
/// move, so a record still coming to this device is not asked in a loop.
const ASK_AGAIN: Duration = Duration::from_secs(1);

/// The trust this process answers its admissions through, installed once.
fn trust() -> &'static Arc<DevicesTrust> {
    static TRUST: OnceLock<Arc<DevicesTrust>> = OnceLock::new();
    TRUST.get_or_init(|| {
        let trust = Arc::new(DevicesTrust::new());
        install_device_trust(Arc::clone(&trust) as Arc<dyn DeviceTrust>);
        trust
    })
}

/// Install the session's answer for the person's devices, before the broker or the identity
/// authority decides anything: from the `daemon` it was started by, from the `vm` host that
/// placed it, or from `service` when this session keeps the workspaces itself.
///
/// A session with none of these knows no device of the person's, and installs an answer that
/// admits none.
pub(crate) fn install(
    daemon: &DeviceDaemon,
    service: &ServiceSlot,
    person: Option<[u8; 32]>,
    vm: Option<&Arc<HostedWorkspaces>>,
) {
    let trust = trust();
    let Some(person) = person else {
        trust.reading(|| None);
        return;
    };
    if let Ok(link) = daemon {
        install_from_daemon(
            DaemonShell::new(Ok(link.clone()), tilo_rt::app().handle().clone()),
            person,
        );
        return;
    }
    if let Some(hosted) = vm {
        follow_host(trust, Arc::clone(hosted), person);
        return;
    }
    let slot = service.clone();
    trust.reading(move || {
        slot.service()
            .map(|service| seen_of_host(service.workspaces()))
    });
}

/// What a follow keeps of the last reading: the revision it stands at, and what the record said.
#[derive(Clone, Default)]
struct Cached {
    revision: Option<String>,
    seen: Option<DevicesSeen>,
}

impl Cached {
    /// Take `readings` in as the person's devices. Whether the record moved to them.
    fn take(&mut self, person: [u8; 32], readings: &Value) -> bool {
        let revision = readings["revision"].as_str().map(str::to_owned);
        if revision.is_none() || revision == self.revision {
            return false;
        }
        match seen_of_readings(person, readings) {
            Some(seen) => {
                self.revision = revision;
                self.seen = Some(seen);
                true
            }
            None => false,
        }
    }
}

/// Install the session's answer from `shell`, the daemon's own link, and follow the person's
/// record on it from now on.
fn install_from_daemon(shell: DaemonShell, person: [u8; 32]) {
    answer_from_daemon(trust(), shell, person);
}

/// [`install_from_daemon`], on the trust this process answers through.
fn answer_from_daemon(trust: &Arc<DevicesTrust>, shell: DaemonShell, person: [u8; 32]) {
    let cache = Arc::new(Mutex::new(Cached::default()));
    trust.reading({
        let cache = Arc::clone(&cache);
        move || cache.lock_recover().seen.clone()
    });
    // The daemon holds the record already: a session that served before its first reading would
    // refuse this device, and every other of the person's, once.
    if let Ok(readings) = shell.read_blocking(record_read(person, None)) {
        cache.lock_recover().take(person, &readings);
    }
    if let Some(seen) = personal_devices_blocking(&shell, person) {
        cache.lock_recover().seen = Some(seen);
    }
    let after = cache.lock_recover().revision.clone();
    shell.runtime().clone().spawn(async move {
        follow(cache, person, after, Follow::Daemon(shell)).await;
    });
}

/// Answer from `hosted`, the host's read of the person's record, and follow it from now on.
fn follow_host(trust: &Arc<DevicesTrust>, hosted: Arc<HostedWorkspaces>, person: [u8; 32]) {
    let cache = Arc::new(Mutex::new(Cached::default()));
    trust.reading({
        let cache = Arc::clone(&cache);
        move || cache.lock_recover().seen.clone()
    });
    hosted.runtime().spawn(async move {
        follow(cache, person, None, Follow::Host(hosted)).await;
    });
}

/// Where a follow reads the person's devices record from.
enum Follow {
    Daemon(DaemonShell),
    Host(Arc<HostedWorkspaces>),
}

impl Follow {
    /// The person's devices and the revocations their root signed, as the daemon that holds their
    /// record answers them: what the record alone cannot say, since a revocation is the root's
    /// signature and a record's mark is not one. `None` where this session has no daemon to ask.
    async fn personal_devices(&self, person: [u8; 32]) -> Option<DevicesSeen> {
        let Self::Daemon(shell) = self else {
            return None;
        };
        let workspace = workspace_id(person).to_string();
        shell
            .dispatch(Some(&workspace), &personal_devices_message())
            .await
            .ok()
            .and_then(|answer| seen_of_personal_devices(&answer))
    }

    /// The person's devices record, once it moves past `after`.
    async fn read(&self, person: [u8; 32], after: Option<String>) -> Result<Value, String> {
        let read = record_read(person, after);
        match self {
            Self::Daemon(shell) => shell.read(read).await,
            Self::Host(hosted) => {
                let pending = hosted
                    .read_record(&read.workspace, workspace_id(person), read)
                    .ok_or("no host takes this device's work")?;
                let answered = pending.await.unwrap_or_else(|| {
                    Err(tilo_actions::remote::ActionCallError::Unavailable {
                        reason: "the workspace's host did not answer".into(),
                    })
                });
                let answered = answered.map_err(|error| error.to_string())?;
                serde_json::from_str(&answered)
                    .map_err(|error| format!("the host's reading does not read: {error}"))
            }
        }
    }
}

/// Keep the last reading of the person's devices, reading again from `after` whenever the record
/// moves.
async fn follow(
    cache: Arc<Mutex<Cached>>,
    person: [u8; 32],
    mut after: Option<String>,
    feed: Follow,
) {
    loop {
        match feed.read(person, after.clone()).await {
            Ok(readings) => {
                let moved = cache.lock_recover().take(person, &readings);
                after = readings["revision"].as_str().map(str::to_owned);
                if moved {
                    // The record moved: the daemon's answer holds the revocations its root
                    // signed, which the record's marks alone cannot say.
                    if let Some(seen) = feed.personal_devices(person).await {
                        cache.lock_recover().seen = Some(seen);
                    }
                } else {
                    tokio::time::sleep(ASK_AGAIN).await;
                }
            }
            Err(error) => {
                tracing::debug!(%error, "the person's devices record did not read");
                cache.lock_recover().seen = None;
                after = None;
                tokio::time::sleep(ASK_AGAIN).await;
            }
        }
    }
}

/// The daemon's `devices#personalDevices` answer, asked of `shell` from a thread that is not a
/// runtime's: the person's devices and the revocations their root signed, as the daemon holds
/// them.
fn personal_devices_blocking(shell: &DaemonShell, person: [u8; 32]) -> Option<DevicesSeen> {
    let workspace = workspace_id(person).to_string();
    shell
        .dispatch_blocking(Some(&workspace), &personal_devices_message())
        .ok()
        .and_then(|answer| seen_of_personal_devices(&answer))
}

/// The invocation of the daemon's `devices#personalDevices` action.
fn personal_devices_message() -> RemoteActionMessage {
    use tilo_device_identity::login::{PERSONAL_DEVICES, PROVIDER, RESOURCE};
    RemoteActionMessage::Invoke(InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: ActionAddress::new(RESOURCE, PERSONAL_DEVICES),
        },
        argument: InvokeArgument::Json(serde_json::Map::new()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    })
}

/// What the daemon's `devices#personalDevices` answer says: the person's devices, the marks their
/// records hold, and the revocations their root signed.
fn seen_of_personal_devices(answer: &Value) -> Option<DevicesSeen> {
    let person = decode_key(answer.get("person")?.as_str()?)?;
    let mut certified = BTreeSet::new();
    let mut revoked = BTreeSet::new();
    for listed in answer.get("devices")?.as_array()? {
        let certificate = listed.get("certificate")?.as_str()?;
        let cert: DeviceCert = postcard::from_bytes(&hex::decode(certificate).ok()?).ok()?;
        certified.insert(cert.device_public_key);
        if listed.get("revoked").and_then(Value::as_bool) == Some(true) {
            revoked.insert(cert.device_public_key);
        }
    }
    let revocations = answer
        .get("revocations")?
        .as_array()?
        .iter()
        .filter_map(|entry| {
            let bytes = hex::decode(entry.as_str()?).ok()?;
            postcard::from_bytes(&bytes).ok()
        })
        .collect();
    Some(DevicesSeen::of_host(
        person,
        certified,
        revoked,
        revocations,
    ))
}

/// `text` as 32 bytes in lowercase hex, or `None`.
fn decode_key(text: &str) -> Option<[u8; 32]> {
    hex::decode(text).ok()?.try_into().ok()
}

/// The person's devices record, read whole once it moved past `after`.
fn record_read(person: [u8; 32], after: Option<String>) -> ResourceReadRequest {
    ResourceReadRequest {
        workspace: workspace_id(person).to_string(),
        app: DEVICES.0.to_owned(),
        record: DEVICES.1.to_owned(),
        joined: false,
        wait_ms: after.is_some().then_some(FOLLOW_WAIT_MS),
        after,
        answer: ReadAnswer::Document,
    }
}

/// The id of `person`'s own workspace.
fn workspace_id(person: [u8; 32]) -> WorkspaceId {
    WorkspaceId::from(collab_types::person_workspace(&person))
}

/// The person's devices a whole-document reading says, when it holds the document: `None` while
/// it is still on its way, or does not read.
fn seen_of_readings(person: [u8; 32], readings: &Value) -> Option<DevicesSeen> {
    match document_read(readings) {
        Ok(DocumentRead::Whole { bytes, .. }) => {
            let document = automerge::AutoCommit::load(&bytes).ok()?;
            let reader = tilo_resource::Reader::<Devices>::read(&document).ok()?;
            Some(seen_of_record(person, reader.state()))
        }
        Ok(DocumentRead::Absent) => Some(DevicesSeen::of_host(person, [], [], Vec::new())),
        Ok(_) | Err(_) => None,
    }
}

/// What the record's state says of the keys the person's root certified and the ones it took back.
fn seen_of_record(person: [u8; 32], record: &Devices) -> DevicesSeen {
    let listed = record.listed(&person);
    DevicesSeen::of_host(
        person,
        listed.iter().map(|listed| listed.cert.device_public_key),
        listed
            .iter()
            .filter(|listed| listed.device.revoked)
            .map(|listed| listed.cert.device_public_key),
        // A revocation is the root's signature, and the record holds only the mark: the daemon's
        // own answer beside this reading carries the entries, and a session with no daemon to
        // ask answers for the keys the record holds and refuses, which is what admission reads.
        Vec::new(),
    )
}

/// What the workspaces this session keeps say of the person's devices: the keys the record holds
/// and the revocations their hosts keep, as the service read them.
fn seen_of_host(workspaces: &Workspaces) -> DevicesSeen {
    let person = workspaces.serving_certificate().user_public_key;
    let keys = workspaces.blocked_keys();
    DevicesSeen::of_host(
        person,
        keys.certified_keys().copied(),
        keys.revoked_keys().copied(),
        workspaces
            .revocations()
            .iter()
            .filter(|entry| entry.user_public_key == person)
            .cloned()
            .collect(),
    )
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use tilo_workspace_service::workspaces::person_workspace;

    /// A device is admitted while the record holds it and refused once the person's root marks it
    /// taken back, through the same install the session makes at its start.
    #[test]
    fn a_certified_device_is_admitted_and_a_revoked_one_is_refused() {
        let _home = tilo_test_env::TestHome::new().expect("a home");
        let daemon = crate::daemon_fixture::TestDaemon::start();
        let cert = daemon.certificate();
        let person = cert.user_public_key;
        let workspaces = daemon.workspaces().clone();
        tilo_rt::app()
            .block_on(async {
                let id = workspace_id(person).to_string();
                workspaces
                    .create_for(person_workspace::NAME, Some(id.as_str()), person)
                    .await
            })
            .expect("the runtime runs")
            .expect("the person's own workspace");
        tilo_rt::app()
            .block_on(async { workspaces.keep_issued_device(&cert, "").await })
            .expect("the runtime runs");

        // The install the session makes when a daemon started it: the daemon answers the record
        // of the person's devices on the link it handed the session.
        install_from_daemon(daemon.shell(), person);

        assert_eq!(
            tilo_identity::sources::device_trust()
                .expect("the session installed its answer")
                .enrolled(&cert),
            Some(true),
            "the record holds this device"
        );
        assert!(
            tilo_identity::certificates::admitted(&person, &cert),
            "a certified device acts for its person"
        );
        assert!(
            tilo_identity::certificates::enrolled(&person, &cert),
            "and joins with them"
        );

        // Their root takes it back, on another of their devices, and the record moves.
        let key = hex::encode(cert.device_public_key);
        tilo_rt::app()
            .block_on(async {
                person_workspace::apply(&workspaces, DEVICES, Devices::REVOKE_DEVICE.with(key))
                    .await
            })
            .expect("the runtime runs")
            .expect("the device is marked taken back");
        tilo_test_env::wait_until("the session refuses the revoked device", || {
            !tilo_identity::certificates::admitted(&person, &cert)
        });
        let trust = tilo_identity::sources::device_trust().expect("the session's answer");
        assert_eq!(
            trust.revoked(&person, &cert.device_public_key),
            Some(true),
            "the record's mark is what the session refuses the device by"
        );
        assert!(
            !tilo_identity::certificates::enrolled(&person, &cert),
            "a revoked device does not join either"
        );
    }
}
