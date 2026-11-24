//! The device's own records: read, written and followed through the one record API before anyone
//! claims the device, and read in the person's workspace once they do.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tilo_actions::remote::{
    self, ActionCallError, ActionProvider, Caller, DEVICE_SETTINGS, ReadAnswer, ReceiptStatus,
    ResourceAddress, ResourceCopy, ResourceReadRequest, ResourceReads, device_record_address,
};
use tilo_actions::{
    ActionAddress, InvocationId, InvokeArgument, InvokeRequest, RemoteActionTarget,
};

use super::{DeviceRecords, REMOVE, RESOURCE, SET};

const DEVICE: [u8; 32] = [7; 32];
const PERSON: [u8; 32] = [9; 32];
const STRANGER: [u8; 32] = [1; 32];

/// The device's own actions, keeping its records below `dir`.
fn device(dir: &Path) -> crate::device::unclaimed::DeviceOnly {
    let keys = tilo_device_identity::IdentityPaths::under(dir.join("identity"));
    crate::device::unclaimed::DeviceOnly::new(
        ed25519_dalek::SigningKey::from_bytes(&DEVICE),
        tilo_device_identity::login::Place::at(dir.join("login"), keys),
    )
}

/// A request invoking the setting action `action` with `args`.
fn request(action: &str, args: Value) -> InvokeRequest {
    let Value::Object(args) = args else {
        panic!("a setting action takes an object");
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: super::PROVIDER.to_owned(),
            address: ActionAddress::new(RESOURCE, action),
        },
        argument: InvokeArgument::Json(args),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The value a reading of `record` in `space` holds, as the record API answers it.
async fn reading(
    reads: &Arc<dyn ResourceReads>,
    caller: &Caller,
    space: &str,
    record: &str,
) -> Value {
    let request = ResourceReadRequest {
        workspace: space.to_owned(),
        app: DEVICE_SETTINGS.to_owned(),
        record: record.to_owned(),
        joined: false,
        after: None,
        wait_ms: None,
        answer: ReadAnswer::Reading,
    };
    let answer = reads
        .answer_read(caller, 0, request)
        .await
        .expect("the device answers the read");
    let remote::RemoteActionMessage::ResourceReading { result, .. } = answer else {
        panic!("a read is answered by a reading");
    };
    let readings: Value = serde_json::from_str(&result.expect("the device reads its record"))
        .expect("a reading reads as JSON");
    readings["records"][0]["value"].clone()
}

/// The revision a reading of `record` in `space` stands at.
async fn revision(
    reads: &Arc<dyn ResourceReads>,
    caller: &Caller,
    space: &str,
    record: &str,
) -> String {
    let request = ResourceReadRequest {
        workspace: space.to_owned(),
        app: DEVICE_SETTINGS.to_owned(),
        record: record.to_owned(),
        joined: false,
        after: None,
        wait_ms: None,
        answer: ReadAnswer::Reading,
    };
    let answer = reads
        .answer_read(caller, 0, request)
        .await
        .expect("the device answers the read");
    let remote::RemoteActionMessage::ResourceReading { result, .. } = answer else {
        panic!("a read is answered by a reading");
    };
    let readings: Value = serde_json::from_str(&result.expect("the device reads its record"))
        .expect("a reading reads as JSON");
    readings["revision"]
        .as_str()
        .expect("a reading names its revision")
        .to_owned()
}

/// The records directory of the device `device`'s place below `dir`.
fn records_dir(dir: &Path) -> std::path::PathBuf {
    dir.join("login").join(super::RECORDS)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_reads_writes_and_follows_its_own_setting_before_anyone_claims_it() {
    let dir = tempfile::tempdir().unwrap();
    let provider = device(dir.path());
    let caller = Caller::local_system();
    let space = tilo_actions::remote::device_record_space(&DEVICE);
    let reads = provider
        .resource_reads()
        .expect("the device answers reads of its own records");

    // Nothing is written yet: the record is absent, not refused.
    let address = device_record_address(&DEVICE, "keymap");
    assert_eq!(
        reads
            .read(&caller, &address)
            .expect("the device reads its records"),
        ResourceCopy::Absent
    );

    // The typed action writes it, and the record API reads it back.
    let receipt = provider
        .invoke(
            &caller,
            request(SET, json!({"name": "keymap", "value": {"layout": "us"}})),
        )
        .await
        .expect("the device answers the call")
        .expect("the device's dispatch takes the setting");
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "the setting was kept: {receipt:?}"
    );
    assert_eq!(
        reading(&reads, &caller, &space, "keymap").await,
        json!({"layout": "us"})
    );

    // A reader following the record is woken by the next write, and reads the new value.
    let after = revision(&reads, &caller, &space, "keymap").await;
    let follow = {
        let reads = Arc::clone(&reads);
        let caller = caller.clone();
        let space = space.clone();
        tokio::spawn(async move {
            reads
                .answer_read(
                    &caller,
                    0,
                    ResourceReadRequest {
                        workspace: space,
                        app: DEVICE_SETTINGS.to_owned(),
                        record: "keymap".to_owned(),
                        joined: false,
                        after: Some(after),
                        wait_ms: Some(5_000),
                        answer: ReadAnswer::Reading,
                    },
                )
                .await
        })
    };
    // Long enough for the follow to be watching, so the write has to wake it.
    tokio::time::sleep(Duration::from_millis(100)).await;
    provider
        .invoke(
            &caller,
            request(SET, json!({"name": "keymap", "value": {"layout": "de"}})),
        )
        .await
        .expect("the device answers the call")
        .expect("the second write is taken");
    let remote::RemoteActionMessage::ResourceReading { result, .. } = follow
        .await
        .expect("the follow ran")
        .expect("the follow is answered")
    else {
        panic!("a follow is answered by a reading");
    };
    let readings: Value =
        serde_json::from_str(&result.expect("the follow reads")).expect("a reading reads as JSON");
    assert_eq!(
        readings["records"][0]["value"],
        json!({"layout": "de"}),
        "the follow woke on the write and read the new setting"
    );

    // Removing it through the action leaves the record absent again.
    provider
        .invoke(&caller, request(REMOVE, json!({"name": "keymap"})))
        .await
        .expect("the device answers the call")
        .expect("the device's dispatch forgets the setting");
    assert_eq!(
        reads
            .read(&caller, &address)
            .expect("the device reads its records"),
        ResourceCopy::Absent
    );

    // A stranger reads nothing, before or after the device is claimed.
    let refused = reads
        .read(&Caller::authenticated(STRANGER, [2; 32]), &address)
        .expect_err("a stranger reads no setting of this device");
    assert!(
        matches!(refused, ActionCallError::Refused { .. }),
        "a stranger is refused: {refused:?}"
    );
    assert!(
        records_dir(dir.path()).exists(),
        "the device keeps its records on disk"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_setting_is_read_in_the_persons_workspace_once_they_found_it() {
    let dir = tempfile::tempdir().unwrap();
    let provider = device(dir.path());
    let caller = Caller::local_system();
    let reads = provider
        .resource_reads()
        .expect("the device answers reads of its own records");
    provider
        .invoke(
            &caller,
            request(SET, json!({"name": "network", "value": {"wifi": "home"}})),
        )
        .await
        .expect("the device answers the call")
        .expect("the device keeps the setting");

    // The person's services carry the device's records into their own workspace: the same record
    // is read there, as them on this device, and nowhere else.
    let records: Arc<DeviceRecords> = provider.records();
    records.carry_into(&PERSON);
    let workspace = tilo_server_protocol::servers::person_workspace_id(&PERSON);
    let as_person = Caller::authenticated(PERSON, DEVICE);
    assert_eq!(
        reading(&reads, &as_person, &workspace, "network").await,
        json!({"wifi": "home"})
    );
    let in_workspace = ResourceAddress {
        workspace: workspace.clone(),
        app: DEVICE_SETTINGS.to_owned(),
        record: "network".to_owned(),
    };
    let refused = reads
        .read(&Caller::authenticated(STRANGER, [2; 32]), &in_workspace)
        .expect_err("a stranger reads nothing in the person's workspace either");
    assert!(
        matches!(refused, ActionCallError::Refused { .. }),
        "a stranger is refused: {refused:?}"
    );
    let refused = reads.read(
        &Caller::authenticated(PERSON, DEVICE),
        &device_record_address(&DEVICE, "network"),
    );
    assert!(
        refused.is_ok(),
        "the person reads the device's own space as its device: {refused:?}"
    );
    // The person's id names the workspace the setting is carried into, not a device space.
    assert_ne!(
        workspace,
        tilo_actions::remote::device_record_space(&PERSON)
    );
}
