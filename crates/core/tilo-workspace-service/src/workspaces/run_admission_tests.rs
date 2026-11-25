//! Runs admitted to a workspace are pinned to the material they were admitted
//! under, and a member copy enrolled again ends them.
#![allow(
    clippy::unwrap_used,
    clippy::significant_drop_tightening,
    reason = "in a test this is the assertion, and each fixture is kept for its test"
)]

use std::sync::Arc;

use super::super::MemberDevice;
use super::super::PreAdmitRequest;
use super::super::WorkspaceSummary;
use super::super::fixtures::{TestHost, device_cert, governor, signed_pre_admission, test_host};
use super::Refusal;
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

/// A device serving workspace `name`, the device it invited, and the
/// invitation that device redeems.
async fn shared(
    serving_state: &std::path::Path,
    name: &str,
    seed: u8,
) -> (TestHost, (SigningKey, DeviceCert), WorkspaceSummary, String) {
    let (serving_key, serving_cert) = governor();
    let serving = test_host(serving_state, &serving_key, &serving_cert).await;
    let created = serving
        .workspaces()
        .create(name)
        .await
        .expect("a workspace is created");
    let device = device_cert(
        &SigningKey::from_bytes(&[seed; 32]),
        "laptop",
        DeviceCapabilities::member(),
    );
    let issued = serving
        .workspaces()
        .pre_admit(
            &serving_cert,
            PreAdmitRequest {
                request_id: [seed; 32],
                grant: signed_pre_admission(&created.workspace, &[seed; 8]),
                recipient_user_public_key: Some(device.1.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .expect("the serving device issues an invitation");
    let invitation = tilo_peer_state::join::encode_invitation_text(&issued.signed_invitation);
    (serving, device, created, invitation)
}

/// A service on `state`, serving the member copies of `(key, cert)`.
async fn member_service(
    state: &std::path::Path,
    (key, cert): (SigningKey, DeviceCert),
) -> TestHost {
    let service = test_host(state, &key, &cert).await;
    service
        .workspaces()
        .set_member_device(Arc::new(MemberDevice {
            cert: cert.clone(),
            signer: Arc::new(key),
        }));
    service
}

#[tokio::test]
async fn a_run_admitted_to_a_member_copy_ends_when_the_copy_is_enrolled_again() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Run", 21).await;
    let member = member_service(member_state.path(), device.clone()).await;
    let workspace = member
        .workspaces()
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    let actor = member
        .workspaces()
        .member_actor(workspace)
        .await
        .expect("the device acts for a person");
    let admission = member
        .workspaces()
        .admit_execution(&workspace.to_string(), &actor, &device.1, Vec::new())
        .await
        .expect("the run is admitted");
    admission
        .check(member.workspaces())
        .expect("the run's material holds");
    // Another invitation redeemed enrolls the copy anew: the run admitted
    // under the enrollment before it is over.
    let again = {
        let serving_cert = serving.host().device_cert().clone();
        let issued = serving
            .workspaces()
            .pre_admit(
                &serving_cert,
                PreAdmitRequest {
                    request_id: [23; 32],
                    grant: signed_pre_admission(&created.workspace, b"enrolled again"),
                    recipient_user_public_key: Some(device.1.user_public_key),
                    inviter_name: None,
                },
            )
            .await
            .expect("the serving device issues another invitation");
        tilo_peer_state::join::encode_invitation_text(&issued.signed_invitation)
    };
    member
        .workspaces()
        .join_member(&again)
        .await
        .expect("the copy is enrolled again");
    assert_eq!(
        admission.check(member.workspaces()),
        Err(Refusal::Ended),
        "a run admitted under another enrollment ends"
    );
}

#[tokio::test]
async fn a_run_admitted_for_another_person_is_refused() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (_serving, device, _created, invitation) = shared(serving_state.path(), "Strict", 22).await;
    let member = member_service(member_state.path(), device.clone()).await;
    let workspace = member
        .workspaces()
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    assert!(
        member
            .workspaces()
            .admit_execution(
                &workspace.to_string(),
                &hex::encode([7; 32]),
                &device.1,
                Vec::new(),
            )
            .await
            .is_err(),
        "a run is admitted only as the person the device acts for"
    );
}

#[tokio::test]
async fn a_workspace_this_device_governs_admits_a_run_of_its_own_device() {
    let state = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(state.path(), &key, &cert).await;
    let created = host
        .workspaces()
        .create("Mine")
        .await
        .expect("a workspace is created");
    let actor = hex::encode(cert.user_public_key);
    let admission = host
        .workspaces()
        .admit_execution(&created.workspace, &actor, &cert, Vec::new())
        .await
        .expect("the workspace's own device is admitted");
    admission
        .check(host.workspaces())
        .expect("the run's material holds");
    assert!(
        !admission.caller().is_foreign_device(),
        "a run of the person whose root certifies this device is on their own device"
    );
}

/// A run admitted under any certificate but this device's own is a foreign device's: another
/// device of the same person, or this device certified as a foreign one.
#[tokio::test]
async fn a_run_under_another_devices_certificate_is_a_foreign_devices() {
    let state = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(state.path(), &key, &cert).await;
    let created = host
        .workspaces()
        .create("Mine")
        .await
        .expect("a workspace is created");
    let actor = hex::encode(cert.user_public_key);
    let (_, other) = device_cert(
        &SigningKey::from_bytes(&[1; 32]),
        "phone",
        DeviceCapabilities::member(),
    );
    let mut flagged = cert.clone();
    flagged.capabilities.foreign = true;
    tilo_user_identity::sign_device_cert(&mut flagged, &SigningKey::from_bytes(&[1; 32])).unwrap();
    for (what, presented) in [("another device", other), ("a foreign device", flagged)] {
        let admission = host
            .workspaces()
            .admit_execution(&created.workspace, &actor, &presented, Vec::new())
            .await
            .unwrap_or_else(|error| panic!("{what} is admitted: {error}"));
        assert!(
            admission.caller().is_foreign_device(),
            "{what} acts as a foreign device"
        );
    }
}

/// What a host's dispatch is handed back once its person answers: the calls it runs again.
#[derive(Default)]
struct Handed(std::sync::Mutex<Vec<tilo_actions::InvocationId>>);

impl tilo_actions::remote::asked::AskedDispatch for Handed {
    fn context(&self, invocation: tilo_actions::InvocationId) -> serde_json::Value {
        serde_json::json!({ "invocation": invocation.0.to_string() })
    }

    fn readmit(
        &self,
        _caller: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
        _context: &serde_json::Value,
    ) -> Result<(), tilo_actions::remote::ActionCallError> {
        self.0.lock().unwrap().push(request.invocation);
        Ok(())
    }

    fn declined(
        &self,
        _caller: &tilo_actions::remote::Caller,
        _invocation: tilo_actions::InvocationId,
    ) -> Result<(), tilo_actions::remote::ActionCallError> {
        Ok(())
    }
}

/// The answer the workspace's invocations list for the call that waits, found the way a reader
/// finds it: the call is listed by the id its asked record names, with its own `resolve`.
fn listed_answer(
    store: &tilo_actions::remote::asked::AskedStore,
    workspace: &str,
    record: &str,
) -> tilo_actions::ActionAddress {
    let mut listed = Vec::new();
    crate::apps::collab_app_broker::with_questions(&mut listed, store.waiting_in(workspace), None);
    let answering = tilo_actions::remote::workspace_invocations::tree_rows(&listed)
        .into_iter()
        .find_map(|row| row.action)
        .expect("the call that waits is listed with its answer")
        .address;
    assert_eq!(
        answering,
        tilo_actions::ActionAddress::new(record.to_owned(), tilo_actions::remote::asked::RESOLVE),
        "the listed answer is the one its asked record names"
    );
    answering
}

/// The person this device acts for answers what their agent asked through the run the device
/// admits for them, which is on their own device: their approve hands the call back once.
/// The answer is the one the workspace's invocations list for the call that waits, which the
/// device kept no invocation record of; listing it lets no one else answer. Another person's
/// device and the agent that asked are refused as not theirs, and a foreign device of their own
/// answers nothing for them.
#[tokio::test]
async fn the_persons_admitted_run_answers_what_their_agent_asked_and_no_other_device_does() {
    use tilo_actions::remote::asked::{self, AskedBy, AskedFacts, Named};
    use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest, RemoteActionTarget};
    let state = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(state.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let created = workspaces
        .create("Mine")
        .await
        .expect("a workspace is created");
    let workspace = created.workspace;
    let admission = workspaces
        .admit_execution(
            &workspace,
            &hex::encode(cert.user_public_key),
            &cert,
            Vec::new(),
        )
        .await
        .expect("the device's own person is admitted");
    let store = workspaces
        .asked_store()
        .expect("the host keeps the calls that wait");
    let handed = Arc::new(Handed::default());
    let dispatch: Arc<dyn asked::AskedDispatch> = handed.clone();
    let records = store.for_dispatch("probe", Arc::downgrade(&dispatch));
    let agent = Caller::delegated_agent("scout", cert.user_public_key, [9; 32])
        .in_workspace(workspace.clone());
    let call = InvokeRequest::plain(
        RemoteActionTarget {
            provider: "record:clocks:alarms".into(),
            address: tilo_actions::ActionAddress::new("clocks.alarms", "set-alarm"),
        },
        serde_json::Map::new(),
    );
    let at = records
        .write_asked(
            &workspace,
            &agent,
            &call,
            &AskedFacts::by(AskedBy::Agent(Named {
                id: "scout".into(),
                name: "Scout".into(),
            })),
        )
        .expect("the agent's call waits for its person");
    // The answer as a walk of the workspace's invocations finds it.
    let answering = listed_answer(&store, &workspace, &at.record);
    let resolve = || {
        let mut approve = serde_json::Map::new();
        approve.insert("resolution".into(), serde_json::json!("approve"));
        InvokeRequest::plain(
            RemoteActionTarget {
                provider: tilo_actions::remote::workspace_invocations::PROVIDER.into(),
                address: answering.clone(),
            },
            approve,
        )
    };
    let answer = |by: &Caller| {
        workspaces
            .resolve_asked(by, &resolve())
            .wait_timeout(std::time::Duration::from_secs(5))
            .ok()
            .flatten()
            .expect("the answer is admitted or refused at once")
    };
    let refusal = |answered: Result<_, ActionCallError>| match answered {
        Err(ActionCallError::Refused { reason }) => reason,
        other => panic!("expected a refusal: {other:?}"),
    };

    let another = Caller::authenticated([2; 32], [5; 32]).in_workspace(workspace.clone());
    assert!(
        refusal(answer(&another)).contains(asked::NOT_YOURS),
        "another person's device answers nothing"
    );
    assert!(
        refusal(answer(&agent)).contains(asked::NOT_YOURS),
        "the agent that asked answers nothing for its person"
    );
    let foreign = Caller::authenticated(cert.user_public_key, cert.device_public_key)
        .in_workspace(workspace.clone())
        .from_foreign_device(true);
    assert!(
        refusal(answer(&foreign)).contains(asked::ON_YOUR_DEVICE),
        "a foreign device answers nothing for its person"
    );
    assert!(
        handed.0.lock().unwrap().is_empty(),
        "nothing was handed back"
    );

    answer(admission.caller()).expect("their own device answers");
    assert_eq!(
        *handed.0.lock().unwrap(),
        vec![call.invocation],
        "their approve hands the call back once"
    );
}
