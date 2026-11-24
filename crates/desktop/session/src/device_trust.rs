//! The person's devices as this session admits them: the record of their own workspace, followed
//! through the record API, answered by one [`DevicesTrust`] every admission in this process asks.
//!
//! The session keeps none of their devices itself, but this device, which its own certificate
//! names and which the record will list: that is admitted before the record answers, and by a
//! session no daemon started at all. A session the host placed in a workspace's VM reads the
//! record from the host that certified it; one that reaches no host admits this device alone. A
//! session the device's daemon started asks that daemon instead: it keeps the record and answers
//! every admission in its own process. Each reads the same record, and each revocation in it, the
//! root's own signature, is checked against that root as it is read: one that does not verify
//! refuses nothing. A device is admitted here wherever the host that holds the record says it is,
//! and refused when that host cannot be asked.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::Value;
use tilo_actions::remote::{ReadAnswer, ResourceReadRequest};
use tilo_desktop_model::WorkspaceId;
use tilo_identity::sources::{DeviceTrust, DevicesSeen, DevicesTrust, install_device_trust};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::workspace_documents::DocumentRead;
use tilo_session_client::daemon_documents::document_read;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::apps::services::DeviceDaemon;
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
/// authority decides anything: from the `vm` host that placed it.
///
/// `daemon` is kept for the call site that builds a session's services: a session the device's
/// daemon started has that daemon's own answer already installed in its process, and installs
/// none here.
///
/// `own` is this session's device certificate. It is admitted from the first call, whatever the
/// record of the person's devices has answered yet: it is this device the record will list, and a
/// session the daemon did not start may never read the record at all. A revocation the record
/// carries still takes it back, and no other device is admitted without the record.
///
/// A session with none of these knows no device of the person's, and installs an answer that
/// admits none.
pub(crate) fn install(
    _daemon: &DeviceDaemon,
    own: Option<DeviceCert>,
    vm: Option<&Arc<HostedWorkspaces>>,
) {
    let trust = trust();
    let Some(own) = own else {
        trust.reading(|| None);
        return;
    };
    if let Some(hosted) = vm {
        follow_host(trust, Arc::clone(hosted), own);
        return;
    }
    // Neither reaches the record: this device is admitted on its own certificate, and no other.
    trust.reading(move || with_own(&own, None));
}

/// What the session answers of `own`'s person, with `own`'s key among the certified: the
/// certificate their root signed for this device is this session's own to present before any host
/// lists it, and a revocation the record carries still takes it back.
fn with_own(own: &DeviceCert, seen: Option<DevicesSeen>) -> Option<DevicesSeen> {
    match seen {
        Some(mut seen) if seen.person == own.user_public_key => {
            seen.certified.insert(own.device_public_key);
            Some(seen)
        }
        Some(seen) => Some(seen),
        None => Some(DevicesSeen::of_host(
            own.user_public_key,
            [own.device_public_key],
            [],
        )),
    }
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

/// Answer from `hosted`, the host's read of the person's record, and follow it from now on.
fn follow_host(trust: &Arc<DevicesTrust>, hosted: Arc<HostedWorkspaces>, own: DeviceCert) {
    let person = own.user_public_key;
    let cache = Arc::new(Mutex::new(Cached::default()));
    trust.reading({
        let cache = Arc::clone(&cache);
        move || with_own(&own, cache.lock_recover().seen.clone())
    });
    hosted.runtime().spawn(async move {
        follow(cache, person, None, hosted).await;
    });
}

/// The person's devices record as the host that placed this session reads it, once it moves past
/// `after`.
async fn read_from_host(
    hosted: &HostedWorkspaces,
    person: [u8; 32],
    after: Option<String>,
) -> Result<Value, String> {
    let read = record_read(person, after);
    let workspace = read.workspace.clone();
    let pending = hosted
        .read_record(&workspace, workspace_id(person), read)
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

/// Keep the last reading of the person's devices, reading again from `after` whenever the record
/// moves.
async fn follow(
    cache: Arc<Mutex<Cached>>,
    person: [u8; 32],
    mut after: Option<String>,
    hosted: Arc<HostedWorkspaces>,
) {
    loop {
        match read_from_host(&hosted, person, after.clone()).await {
            Ok(readings) => {
                let moved = cache.lock_recover().take(person, &readings);
                after = readings["revision"].as_str().map(str::to_owned);
                if !moved {
                    // The record did not move: read again, so a record still coming to this
                    // device is not asked in a loop.
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
        Ok(DocumentRead::Absent) => Some(DevicesSeen::of_host(person, [], [])),
        Ok(_) | Err(_) => None,
    }
}

/// What the record's state says of the keys the person's root certified and the revocations it
/// signed: each entry the record holds is checked against that root as it is read.
fn seen_of_record(person: [u8; 32], record: &Devices) -> DevicesSeen {
    let listed = record.listed(&person);
    DevicesSeen::of_host(
        person,
        listed.iter().map(|listed| listed.cert.device_public_key),
        listed
            .iter()
            .filter_map(|listed| listed.device.revocation(&listed.cert)),
    )
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use tilo_user_identity::{
        DeviceCapabilities, RevocationEntry, RevocationReason, sign_device_cert,
        sign_revocation_entry,
    };
    use tilo_workspace_service::workspaces::Workspaces;
    use tilo_workspace_service::workspaces::person_workspace;
    use tilo_workspace_service::workspaces::person_workspace::devices;

    /// The fixture's person: the root every certificate and revocation here is signed by.
    fn root() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[1; 32])
    }

    /// The entry `root` signs to take `device` back.
    fn revocation(device: [u8; 32]) -> RevocationEntry {
        let root = root();
        let mut entry = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: device,
            revoked_at_ns: 1,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, &root).expect("the person's root signs");
        entry
    }

    /// The person's own workspace, created on `workspaces`, in which their devices record lives.
    fn person_workspace_of(workspaces: &Workspaces, person: [u8; 32]) {
        tilo_rt::app()
            .block_on(async {
                let id = workspace_id(person).to_string();
                workspaces
                    .create_for(person_workspace::NAME, Some(id.as_str()), person)
                    .await
            })
            .expect("the runtime runs")
            .expect("the person's own workspace");
    }

    /// A revocation the person's root did not sign refuses nothing: the entry a record holds is a
    /// claim until it verifies against the root that certified the device, so a forged one leaves
    /// the device admitted and its key certified.
    #[test]
    fn a_forged_revocation_in_the_record_refuses_nothing() {
        let root = root();
        let person = root.verifying_key().to_bytes();
        let device = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: person,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).expect("the person's root certifies");
        let mut record = Devices::default();
        let key = hex::encode(cert.device_public_key);
        record
            .keep_device(
                key.clone(),
                hex::encode(postcard::to_allocvec(&cert).unwrap()),
            )
            .expect("the record keeps the device");

        // Another root signs the entry: a valid signature that is not this person's.
        let stranger = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let mut forged = RevocationEntry {
            user_public_key: stranger.verifying_key().to_bytes(),
            revoked_device_public_key: cert.device_public_key,
            revoked_at_ns: 1,
            reason: RevocationReason::Stolen,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut forged, &stranger).expect("another root signs its own entry");
        let kept = record
            .devices
            .get_mut(&tilo_resource::Id::from_raw(key))
            .expect("the record holds the device");
        kept.revocation = Some(hex::encode(postcard::to_allocvec(&forged).unwrap()));

        let seen = seen_of_record(person, &record);
        assert!(
            seen.revocations.is_empty(),
            "an entry another root signed refuses nothing"
        );
        assert_eq!(
            seen.certified.iter().copied().collect::<Vec<_>>(),
            vec![cert.device_public_key],
            "the device stays admitted"
        );
    }

    /// A session the host placed in a VM reads the person's record through its host: the
    /// revocation their root signed arrives with it, verified, so the session refuses the device.
    #[test]
    fn a_vm_sessions_read_of_the_record_sees_the_revocation() {
        let _home = tilo_test_env::TestHome::new().expect("a home");
        let daemon = crate::daemon_fixture::TestDaemon::start();
        let cert = daemon.certificate();
        let person = cert.user_public_key;
        let workspaces = daemon.workspaces().clone();
        person_workspace_of(&workspaces, person);
        tilo_rt::app()
            .block_on(async { workspaces.keep_issued_device(&cert, "").await })
            .expect("the runtime runs");
        let entry = revocation(cert.device_public_key);
        tilo_rt::app()
            .block_on(async { devices::revoke(&workspaces, &person, &entry).await })
            .expect("the runtime runs")
            .expect("the device's entry takes the root's revocation");

        // What a host's read of the record answers a session it placed in a VM.
        let readings = daemon
            .shell()
            .read_blocking(record_read(person, None))
            .expect("the daemon reads the person's record");
        let seen = seen_of_readings(person, &readings).expect("the record reads");
        assert_eq!(
            seen.revocations,
            vec![entry],
            "the root's entry arrives with the record"
        );
        assert!(
            seen.revoked(&cert.device_public_key),
            "and the session refuses the device by it"
        );
    }
}
