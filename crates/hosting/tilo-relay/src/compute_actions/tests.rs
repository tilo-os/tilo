//! Compute placement as `tilo.compute` actions: offered only to the relay
//! owner acting themselves, answered from the workspace's live worker
//! advertisements, and composed beside the host's other providers.

#![allow(
    clippy::significant_drop_tightening,
    reason = "each test's plane is the subject until its last assertion; the lint's suggested drop lands inside a loop that still uses it"
)]

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use serde_json::{Value, json};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionHostError, Caller, CataloguePage, CatalogueQuery, InvokeArgument, InvokeRequest, Pending,
    ReceiptStatus, RemoteActionMessage, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionOutput};

use super::{COMPUTE_PROVIDER, ComputeActions, ComputePlane, PLACE_JOB, compute_resource};
use crate::app_serving::AppServingPlane;
use crate::compute_transport::{COMPUTE_TRANSPORT_SCHEMA_VERSION, ComputeWorkerAdvertisement};
use crate::mesh::PowerAvailability;
use crate::workspace::{Membership, WorkspaceRuntime};

const OWNER: [u8; 32] = [1; 32];
const OWNER_DEVICE: [u8; 32] = [2; 32];
const WORKSPACE: u64 = 7;
const APP: &str = "org.tilo.thumbnails";

/// What a pending answer comes to, on this runtime.
async fn settled<T: Send + 'static>(pending: Pending<T>) -> T {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    pending.then(move |value| {
        let _ = sender.send(value);
    });
    tilo_test_env::within("the compute answer", receiver)
        .await
        .unwrap()
        .expect("answered")
}

/// What worker `worker` advertises at `now`.
fn advertisement(
    worker: [u8; 32],
    power: PowerAvailability,
    active: u32,
    now: u64,
) -> ComputeWorkerAdvertisement {
    ComputeWorkerAdvertisement {
        version: COMPUTE_TRANSPORT_SCHEMA_VERSION,
        worker,
        workspace_id: WORKSPACE,
        apps: BTreeSet::from([APP.to_owned()]),
        accelerators: BTreeSet::new(),
        assignment_capacity: 4,
        active_assignments: active,
        environment_capacity: 0,
        active_environments: 0,
        power,
        energy_saver: false,
        issued_at_ms: now,
        expires_at_ms: now.saturating_add(30_000),
    }
}

/// Placement over a relay carrying workspace [`WORKSPACE`], where a laptop
/// on battery and a busier desktop on external power advertise compute.
fn placement(state_root: &std::path::Path) -> ComputeActions {
    let mut runtime =
        WorkspaceRuntime::open(state_root, WORKSPACE, false, Membership::default()).unwrap();
    let now = tilo_clock::unix_now_ms();
    for (worker, power, active) in [
        ([5; 32], PowerAvailability::Battery(70), 0),
        ([6; 32], PowerAvailability::External, 1),
    ] {
        runtime
            .compute
            .advertise(
                worker,
                WORKSPACE,
                now,
                advertisement(worker, power, active, now),
            )
            .unwrap();
    }
    let workspaces = Arc::new(tokio::sync::Mutex::new(HashMap::from([(
        WORKSPACE,
        Arc::new(tokio::sync::Mutex::new(runtime)),
    )])));
    ComputeActions::new(OWNER, workspaces)
}

fn alone(compute: ComputeActions) -> ComputePlane {
    ComputePlane {
        compute,
        rest: None,
    }
}

fn owner() -> Caller {
    Caller::authenticated(OWNER, OWNER_DEVICE)
}

fn place(argument: Value) -> RemoteActionMessage {
    let Value::Object(argument) = argument else {
        panic!("arguments are an object");
    };
    RemoteActionMessage::Invoke(InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: COMPUTE_PROVIDER.into(),
            address: ActionAddress::new(compute_resource(WORKSPACE), PLACE_JOB),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    })
}

fn catalogue(provider: Option<&str>) -> RemoteActionMessage {
    RemoteActionMessage::CatalogueRequest {
        correlation: 1,
        query: CatalogueQuery {
            provider: provider.map(Into::into),
            max_results: 100,
            ..CatalogueQuery::default()
        },
    }
}

/// The answer `plane` gives `caller` for `message`.
async fn ask(
    plane: &ComputePlane,
    caller: &Caller,
    message: RemoteActionMessage,
) -> RemoteActionMessage {
    assert!(
        plane.answers(caller, &message),
        "the plane answers {message:?}"
    );
    settled(plane.serve(WORKSPACE, caller, message).expect("a reply")).await
}

fn page(reply: RemoteActionMessage) -> CataloguePage {
    match reply {
        RemoteActionMessage::CatalogueResponse { result, .. } => result.unwrap(),
        other => panic!("not a catalogue: {other:?}"),
    }
}

fn status(reply: RemoteActionMessage) -> ReceiptStatus {
    match reply {
        RemoteActionMessage::Receipt(receipt) => receipt.status,
        other => panic!("not a receipt: {other:?}"),
    }
}

fn output(reply: RemoteActionMessage) -> Value {
    match status(reply) {
        ReceiptStatus::Completed {
            output: ActionOutput {
                value: Some(value), ..
            },
        } => value,
        other => panic!("not completed: {other:?}"),
    }
}

#[tokio::test]
async fn the_owner_is_told_the_best_placed_device_and_why() {
    let root = tempfile::tempdir().unwrap();
    let plane = alone(placement(root.path()));

    let offered = page(ask(&plane, &owner(), catalogue(Some(COMPUTE_PROVIDER))).await);
    let actions = offered
        .rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .map(|action| action.address.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        actions,
        vec![ActionAddress::new(compute_resource(WORKSPACE), PLACE_JOB)]
    );

    // The desktop on external power wins over the idle laptop on battery.
    let placed = output(ask(&plane, &owner(), place(json!({ "app": APP }))).await);
    assert_eq!(placed["worker"], json!(hex::encode([6; 32])));
    let candidates = placed["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[1]["device"], json!(hex::encode([5; 32])));
    assert_eq!(candidates[1]["eligible"], json!(true));
    assert_eq!(candidates[1]["rank"]["power"], json!("battery"));

    // A job neither can run is placed nowhere, each with its reason.
    let nowhere = output(ask(&plane, &owner(), place(json!({ "app": APP, "gpu": true }))).await);
    assert_eq!(nowhere["worker"], Value::Null);
    assert!(
        nowhere["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|candidate| candidate["reason"] == json!("no_gpu")),
        "{nowhere}"
    );
    let other_app = output(ask(&plane, &owner(), place(json!({ "app": "org.tilo.other" }))).await);
    assert_eq!(other_app["worker"], Value::Null);

    // Retrying one invocation answers what it came to.
    let once = place(json!({ "app": APP }));
    let first = output(ask(&plane, &owner(), once.clone()).await);
    let again = output(ask(&plane, &owner(), once).await);
    assert_eq!(first, again);
}

#[tokio::test]
async fn only_the_owner_acting_themselves_may_place() {
    let root = tempfile::tempdir().unwrap();
    let plane = alone(placement(root.path()));
    let strangers = [
        Caller::authenticated([9; 32], [10; 32]),
        Caller::delegated_agent("agent-1", OWNER, [11; 32]),
        Caller::bot("bot-1", OWNER, [12; 32]),
    ];
    for stranger in strangers {
        let offered = page(ask(&plane, &stranger, catalogue(Some(COMPUTE_PROVIDER))).await);
        assert!(offered.rows.is_empty(), "{stranger:?} is offered nothing");
        let refused = status(ask(&plane, &stranger, place(json!({ "app": APP }))).await);
        assert_eq!(
            refused,
            ReceiptStatus::Rejected {
                error: ActionHostError::UnknownAction
            },
            "{stranger:?} may not place"
        );
        // Nor does this provider answer the whole catalogue for them.
        assert!(!plane.answers(&stranger, &catalogue(None)));
    }

    // Another workspace's resource is not an action here.
    let mut elsewhere = place(json!({ "app": APP }));
    if let RemoteActionMessage::Invoke(request) = &mut elsewhere {
        request.target.address = ActionAddress::new(compute_resource(8), PLACE_JOB);
    }
    assert_eq!(
        status(ask(&plane, &owner(), elsewhere).await),
        ReceiptStatus::Rejected {
            error: ActionHostError::UnknownAction
        }
    );
    // Malformed terms are refused before anything is placed.
    assert!(matches!(
        status(ask(&plane, &owner(), place(json!({ "gpu": true }))).await),
        ReceiptStatus::Rejected {
            error: ActionHostError::InvalidRequest { .. }
        }
    ));
}

/// Named with its inputs' bytes and the asking device's own run time, a job
/// whose inputs would take longer to move than that run is kept with its
/// requester, with the estimate; light inputs still go to the worker.
#[tokio::test]
async fn inputs_heavier_than_the_gain_keep_the_job_with_its_requester() {
    let root = tempfile::tempdir().unwrap();
    let compute = placement(root.path());
    {
        let workspace = compute
            .shared
            .workspaces
            .lock()
            .await
            .get(&WORKSPACE)
            .cloned()
            .unwrap();
        let mut runtime = workspace.lock().await;
        // 10 MB/s from the asking device to either worker.
        for worker in [[5; 32], [6; 32]] {
            runtime.compute.observe_transfer(
                (OWNER_DEVICE, worker),
                10_000_000,
                std::time::Duration::from_secs(1),
            );
        }
    }
    let plane = alone(compute);

    let kept = output(
        ask(
            &plane,
            &owner(),
            place(json!({ "app": APP, "input_bytes": 100_000_000, "local_wall_ms": 500 })),
        )
        .await,
    );
    assert_eq!(kept["worker"], Value::Null, "{kept}");
    assert!(
        kept["reason"]
            .as_str()
            .unwrap()
            .starts_with("inputs_heavier_than_gain"),
        "{kept}"
    );
    assert_eq!(kept["estimate"]["worker"], json!(hex::encode([6; 32])));
    assert!(kept["estimate"]["transfer_ms"].as_u64().unwrap() >= 10_000);
    assert_eq!(kept["estimate"]["local_wall_ms"], json!(500));
    assert_eq!(kept["candidates"].as_array().unwrap().len(), 2);

    let sent = output(
        ask(
            &plane,
            &owner(),
            place(json!({ "app": APP, "input_bytes": 1_000_000, "local_wall_ms": 30_000 })),
        )
        .await,
    );
    assert_eq!(sent["worker"], json!(hex::encode([6; 32])), "{sent}");
    // Bytes alone, with nothing to weigh them against, change nothing.
    let unweighed = output(
        ask(
            &plane,
            &owner(),
            place(json!({ "app": APP, "input_bytes": 100_000_000 })),
        )
        .await,
    );
    assert_eq!(unweighed["worker"], json!(hex::encode([6; 32])));

    // Named input by input, their bytes are what is weighed; nothing is
    // known held yet, so all of them would move.
    let photo = |seed: u8| json!({ "digest": hex::encode([seed; 32]), "bytes": 50_000_000 });
    let listed = output(
        ask(
            &plane,
            &owner(),
            place(json!({
                "app": APP,
                "local_wall_ms": 500,
                "inputs": [photo(1), photo(2), photo(1)],
            })),
        )
        .await,
    );
    assert_eq!(listed["worker"], Value::Null, "{listed}");
    assert_eq!(listed["estimate"]["input_bytes"], json!(100_000_000));
    assert_eq!(listed["estimate"]["held_bytes"], json!(0));

    let too_many = (0..=64).map(photo).collect::<Vec<_>>();
    for malformed in [
        json!({ "app": APP, "input_bytes": -1, "local_wall_ms": 5 }),
        json!({ "app": APP, "input_bytes": 5, "local_wall_ms": "fast" }),
        json!({ "app": APP, "local_wall_ms": 5, "inputs": "all" }),
        json!({ "app": APP, "local_wall_ms": 5, "inputs": [{ "digest": "zz", "bytes": 1 }] }),
        json!({ "app": APP, "local_wall_ms": 5, "inputs": [{ "digest": hex::encode([1; 32]) }] }),
        json!({ "app": APP, "local_wall_ms": 5, "inputs": too_many }),
    ] {
        assert!(matches!(
            status(ask(&plane, &owner(), place(malformed)).await),
            ReceiptStatus::Rejected {
                error: ActionHostError::InvalidRequest { .. }
            }
        ));
    }
}

/// The host's other providers, as one page naming one provider.
struct Rest;

impl AppServingPlane for Rest {
    fn serve(
        &self,
        _workspace_id: u64,
        _caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        let RemoteActionMessage::CatalogueRequest { correlation, query } = message else {
            return None;
        };
        let mut page = CataloguePage::from_rows("9".into(), Vec::new(), &query);
        page.providers = vec![tilo_actions::remote::CatalogueProvider {
            provider: "tilo.workspace".into(),
            app_id: "tilo.workspace".into(),
            room: String::new(),
            record_id: String::new(),
            browsable: false,
        }];
        Some(Pending::ready(RemoteActionMessage::CatalogueResponse {
            correlation,
            result: Ok(page),
        }))
    }
}

fn provider_names(page: &CataloguePage) -> Vec<&str> {
    page.providers
        .iter()
        .map(|provider| provider.provider.as_str())
        .collect()
}

#[tokio::test]
async fn the_whole_catalogue_names_compute_beside_the_other_providers() {
    let root = tempfile::tempdir().unwrap();
    let own = page(ask(&alone(placement(root.path())), &owner(), catalogue(None)).await);
    assert_eq!(own.rows.len(), 1);
    assert_eq!(provider_names(&own), vec![COMPUTE_PROVIDER]);

    let other_root = tempfile::tempdir().unwrap();
    let beside = ComputePlane {
        compute: placement(other_root.path()),
        rest: Some(Arc::new(Rest)),
    };
    let merged = page(ask(&beside, &owner(), catalogue(None)).await);
    assert_eq!(merged.revision, "9", "the rest's page");
    assert_eq!(
        provider_names(&merged),
        vec!["tilo.workspace", COMPUTE_PROVIDER]
    );
    // A stranger gets the rest's page as it was.
    let stranger = Caller::authenticated([9; 32], [10; 32]);
    let unmerged = page(ask(&beside, &stranger, catalogue(None)).await);
    assert_eq!(provider_names(&unmerged), vec!["tilo.workspace"]);
}

/// The host's other providers, answering each catalogue with the device
/// whose certificate the transport named, if any, as its revision.
struct Certified;

impl AppServingPlane for Certified {
    fn serve(
        &self,
        _workspace_id: u64,
        _caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        answer_naming(message, "uncertified".into())
    }

    fn serve_certified(
        &self,
        _workspace_id: u64,
        _caller: &Caller,
        certificate: &tilo_user_identity::DeviceCert,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        answer_naming(message, hex::encode(certificate.device_public_key))
    }
}

fn answer_naming(
    message: RemoteActionMessage,
    revision: String,
) -> Option<Pending<RemoteActionMessage>> {
    let RemoteActionMessage::CatalogueRequest { correlation, query } = message else {
        return None;
    };
    Some(Pending::ready(RemoteActionMessage::CatalogueResponse {
        correlation,
        result: Ok(CataloguePage::from_rows(revision, Vec::new(), &query)),
    }))
}

#[tokio::test]
async fn the_rest_is_handed_the_certificate_the_transport_verified() {
    let root = tempfile::tempdir().unwrap();
    let plane = ComputePlane {
        compute: placement(root.path()),
        rest: Some(Arc::new(Certified)),
    };
    let certificate = tilo_user_identity::DeviceCert {
        user_public_key: OWNER,
        device_public_key: OWNER_DEVICE,
        device_label: "laptop".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    // The host's own operations need the certificate (entering a room
    // admits it), whether asked alone or within the whole catalogue.
    for message in [catalogue(Some("tilo.host")), catalogue(None)] {
        let answer = plane
            .serve_certified(7, &owner(), &certificate, message)
            .expect("a reply");
        assert_eq!(
            page(settled(answer).await).revision,
            hex::encode(OWNER_DEVICE)
        );
    }
    let uncertified = plane
        .serve(7, &owner(), catalogue(Some("tilo.host")))
        .expect("a reply");
    assert_eq!(page(settled(uncertified).await).revision, "uncertified");
}
