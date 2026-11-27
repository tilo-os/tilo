//! Which device takes a turn of work: a device whose person's entry offers
//! compute claims an offloaded invocation's turn only when everything it needs
//! is here, and a member device asking it over the device action service gets
//! the turn run there.

use std::sync::Mutex;

use tilo_actions::InvocationUpdate;
use tilo_actions::remote::ActionCallError;
use tilo_resource::Id;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_server_protocol::workspaces::Role;

use super::fixtures::entry_join_as;
use super::servers::{self, Servers};
use super::tests::{guest_device, hosting_device, test_host};
use super::*;
use crate::work::{
    Actor, HostLinux, Ineligible, Job, KeepsContent, NotTaken, Turn, keep_done, takes_turn,
};

/// What this device keeps of its workspaces' content, as a test sets it.
struct Keeps(bool);

impl KeepsContent for Keeps {
    fn holds(&self, _workspace: &str, _job: &Job) -> bool {
        self.0
    }
}

/// The provider that performs the test's action on the host's dispatch: the
/// action as the device that took its turn runs it.
#[derive(Default)]
struct Photos {
    /// What each run was given: the device that asked and the pictures.
    ran: Mutex<Vec<([u8; 32], Vec<serde_json::Value>)>>,
}

impl tilo_actions::remote::ActionProvider for Photos {
    fn serves(&self, _provider: &str) -> bool {
        false
    }

    /// Its thumbnails, a change of a picture, and `melt`, which Photos' surface declares nothing
    /// of ([`declare_photos`]).
    fn published(&self) -> Vec<tilo_actions::ResourceSpec> {
        let action = |id: &str, label: &str, kind: tilo_actions::ActionKind| {
            let params = vec![
                tilo_actions::ActionParam::required(
                    "photos",
                    tilo_actions::ParamType::Array,
                    "The pictures",
                )
                .as_content_list(),
            ];
            let mut action = tilo_actions::ActionSpec::typed_operation(
                id,
                label,
                "",
                tilo_actions::OperationKind::Effect,
                "test/photos",
                params,
                |_| tilo_actions::EncodedAction {
                    type_id: "test/photos".into(),
                    payload: Vec::new(),
                },
            );
            action.type_mut().contract.kind = kind;
            action
        };
        let mut spec =
            tilo_actions::ResourceSpec::new("photos.library", "photos.library", "Photos");
        spec.app_id = Some(PHOTOS.into());
        spec.actions = vec![
            action(
                THUMBNAILS,
                "Make thumbnails",
                tilo_actions::ActionKind::Read,
            ),
            action(RETOUCH, "Retouch", tilo_actions::ActionKind::Undoable),
            action(UNDECLARED, "Melt", tilo_actions::ActionKind::Undoable),
        ];
        vec![spec]
    }

    fn query(
        &self,
        _caller: &tilo_actions::remote::Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>,
    > {
        let resources = self.published();
        tilo_actions::remote::Pending::ready(Ok(tilo_actions::remote::CataloguePage::published(
            "test/photos",
            &resources,
            &query,
        )))
    }

    fn invoke(
        &self,
        caller: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        let invocation = request.invocation;
        let photos = match &request.argument {
            tilo_actions::remote::InvokeArgument::Json(arguments) => arguments
                .get("photos")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        self.ran.lock().unwrap().push((
            caller.device_public_key().copied().unwrap_or_default(),
            photos.clone(),
        ));
        let named: Vec<String> = photos
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect();
        let output = tilo_actions::ActionOutput {
            value: Some(serde_json::json!({ "ran": named })),
            resources: Vec::new(),
        };
        let state = tilo_actions::remote::InvocationState::new(
            invocation,
            tilo_actions::InvocationUpdate::Succeeded { invocation, output },
        );
        tilo_actions::remote::Pending::ready(Ok(state.receipt()))
    }

    fn observe(
        &self,
        _caller: &tilo_actions::remote::Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _caller: &tilo_actions::remote::Caller,
        _id: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        tilo_actions::remote::Pending::ready(Err(
            tilo_actions::remote::ActionCallError::NotCancellable {
                reason: "the test's action answers at once".into(),
            },
        ))
    }
}

/// Photos' thumbnails of one photograph, as a device asks for them.
fn job(input: [u8; 32]) -> Job {
    job_of(THUMBNAILS, input)
}

/// Photos' action `action` over one photograph, as a device asks for it.
fn job_of(action: &str, input: [u8; 32]) -> Job {
    Job {
        app: PHOTOS.into(),
        resource: "photos.library".into(),
        action: action.into(),
        argument: tilo_actions::remote::InvokeArgument::Json(
            serde_json::json!({
                "photos": [hex::encode(input)],
            })
            .as_object()
            .unwrap()
            .clone(),
        ),
        offload: tilo_actions::Offload {
            gpu: false,
            cpu_time_ms: 60_000,
            memory_bytes: 512 * 1024 * 1024,
            output_bytes: 8 * 1024 * 1024,
        },
        inputs: vec![input],
    }
}

/// The app whose actions the turns run.
const PHOTOS: &str = "so.tilo.photos";
/// Photos' thumbnails: it reads the pictures it is given, as the thumbnail engine declares it.
const THUMBNAILS: &str = "make-thumbnails";
/// A change of a picture, which a viewer's role does not reach.
const RETOUCH: &str = "retouch";
/// An action Photos' provider answers and its surface declares nothing of.
const UNDECLARED: &str = "melt";

/// Photos' surface as the host knows it: what a call of each of its actions is judged by.
fn declare_photos(host: &super::fixtures::TestHost) {
    let surface = collab_types::surface::canonical(&serde_json::json!({
        "format": 2,
        "resources": [{
            "kind": "photos.library",
            "label": "Photos",
            "version": 1,
            "fields": [],
            "verbs": [
                { "id": THUMBNAILS, "kind": "read" },
                { "id": RETOUCH, "kind": "undoable" },
            ],
        }],
    }));
    super::fixtures::declare_app(host, PHOTOS, surface.as_bytes());
}

/// The person's entry for `device` among their servers, offering compute when
/// `compute` says so: the Servers workspace founded here, the device enrolled.
async fn offer_compute(host: &super::fixtures::TestHost, device: &DeviceCert, compute: bool) {
    let workspaces = host.workspaces();
    let admin = person_workspace_id(&device.user_public_key);
    if servers::read(workspaces).await.is_err() {
        workspaces
            .create_workspace("Servers", Some(&admin))
            .await
            .unwrap();
        servers::apply(
            workspaces,
            Servers::ENROLL_SERVER.with((
                hex::encode(device.device_public_key),
                "host".into(),
                serde_json::to_string(&host.node_addr()).unwrap(),
            )),
        )
        .await
        .unwrap();
    }
    servers::apply(
        workspaces,
        Servers::SET_APPETITE.with((
            Id::from_raw(hex::encode(device.device_public_key)),
            0,
            compute,
            0,
        )),
    )
    .await
    .unwrap();
}

/// A device that may not take a turn never claims it, whichever reason holds;
/// one that may claims it, renews its own claim, takes it again when asked
/// twice, and lets it go.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_claims_a_turn_only_when_everything_it_needs_is_here() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let host = test_host(dir.path(), &key, &cert).await;
    declare_photos(&host);
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Photos").await.unwrap().workspace;
    let (editor_key, editor) = guest_device();
    let (viewer_key, viewer) = guest_device();
    entry_join_as(
        workspaces,
        &cert,
        &workspace,
        (&editor_key, &editor),
        61,
        Role::Editor,
    )
    .await;
    entry_join_as(
        workspaces,
        &cert,
        &workspace,
        (&viewer_key, &viewer),
        62,
        Role::Viewer,
    )
    .await;
    let turn = |requester: &DeviceCert| Turn::Invocation {
        id: tilo_actions::InvocationId::new(),
        requester: requester.device_public_key,
        job: job([7; 32]),
    };
    let unclaimed = |turn: &Turn, why: Ineligible| {
        let turn = turn.clone();
        let workspace = workspace.clone();
        async move {
            let taken = takes_turn(workspaces, &workspace, &turn).await;
            assert_eq!(taken.err(), Some(NotTaken::Ineligible(why)), "{why}");
            assert_eq!(
                workspaces
                    .lease_holder(&workspace, &turn.lease())
                    .await
                    .unwrap(),
                None,
                "a device that may not take a turn writes no claim: {why}"
            );
        }
    };

    unclaimed(&turn(&editor), Ineligible::OffersNoCompute).await;
    offer_compute(&host, &cert, false).await;
    unclaimed(&turn(&editor), Ineligible::OffersNoCompute).await;
    offer_compute(&host, &cert, true).await;
    workspaces.run_jobs(Arc::new(Keeps(true)));
    unclaimed(&turn(&editor), Ineligible::CannotRun).await;
    workspaces.providers().provide(
        "test/photos",
        "so.tilo.photos",
        crate::workspaces::providers::Reach::Devices,
        Arc::new(Photos::default()),
    );
    workspaces.run_jobs(Arc::new(Keeps(false)));
    unclaimed(&turn(&editor), Ineligible::InputsElsewhere).await;
    workspaces.run_jobs(Arc::new(Keeps(true)));
    // A viewer reads the pictures and changes none of them.
    let retouch = Turn::Invocation {
        id: tilo_actions::InvocationId::new(),
        requester: viewer.device_public_key,
        job: job_of(RETOUCH, [7; 32]),
    };
    unclaimed(&retouch, Ineligible::NotPermitted).await;

    let turn = turn(&editor);
    let lease = turn.lease();
    let claim = takes_turn(workspaces, &workspace, &turn).await.unwrap();
    assert_eq!(
        workspaces.lease_holder(&workspace, &lease).await.unwrap(),
        Some(cert.device_public_key),
        "the claim is a lease of the workspace"
    );
    assert!(claim.renew().await, "the holder renews its claim");
    let again = takes_turn(workspaces, &workspace, &turn).await;
    assert!(
        again.is_ok(),
        "a turn this device holds is its own: {:?}",
        again.err()
    );
    claim.release().await;
    assert_eq!(
        workspaces.lease_holder(&workspace, &lease).await.unwrap(),
        None,
        "a finished turn is let go"
    );
    host.shutdown().await;
}

/// Linux machines as a test sets them: whether there is room for another.
struct Linux {
    room: bool,
}

impl HostLinux for Linux {
    fn machine_lease(&self) -> String {
        tilo_peer_state::vm_lease("default")
    }

    fn has_room(&self) -> bool {
        self.room
    }

    fn runs(&self) -> Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync> {
        Arc::new(tilo_actions::remote::TestProvider::new())
    }
}

/// A Linux run is taken by a device that runs Linux machines and has room for
/// one more, or that keeps the workspace's machine already, room or not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_takes_a_linux_run_when_it_runs_linux_and_has_room() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Build").await.unwrap().workspace;
    let turn = || Turn::Linux {
        id: tilo_actions::InvocationId::new(),
        requester: cert.device_public_key,
        run: Box::new(tilo_actions::remote::InvokeRequest {
            asks: false,
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_action_services::environment_commands::target(),
            argument: tilo_actions::remote::InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }),
    };
    let refused = |why: Ineligible| {
        let workspace = workspace.clone();
        async move {
            let taken = takes_turn(workspaces, &workspace, &turn()).await;
            assert_eq!(taken.err(), Some(NotTaken::Ineligible(why)), "{why}");
        }
    };

    refused(Ineligible::RunsNoLinux).await;
    workspaces.run_linux(Arc::new(Linux { room: false }));
    refused(Ineligible::NoRoomForLinux).await;
    assert_eq!(
        Ineligible::NoRoomForLinux.to_string(),
        "this device has no room for another Linux machine"
    );
    workspaces.run_linux(Arc::new(Linux { room: true }));
    takes_turn(workspaces, &workspace, &turn())
        .await
        .expect("a device with room takes it")
        .release()
        .await;

    // The device keeping the workspace's machine takes its runs, full or not.
    workspaces
        .write_lease(&workspace, &tilo_peer_state::vm_lease("default"), 30_000)
        .await
        .unwrap();
    workspaces.run_linux(Arc::new(Linux { room: false }));
    takes_turn(workspaces, &workspace, &turn())
        .await
        .expect("the device keeping the machine takes it")
        .release()
        .await;
    host.shutdown().await;
}

/// A member device asks the host to take a turn over the device action
/// service: the host claims it, answers that it runs, and the device follows it
/// to the output; asked again, it answers the same run. A device the workspace
/// does not let run the action is refused, and nothing runs for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_device_asks_and_the_host_runs_its_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let host = test_host(dir.path(), &key, &cert).await;
    declare_photos(&host);
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Photos").await.unwrap().workspace;
    offer_compute(&host, &cert, true).await;
    let photos = Arc::new(Photos::default());
    workspaces.run_jobs(Arc::new(Keeps(true)));
    workspaces.providers().provide(
        "test/photos",
        "so.tilo.photos",
        crate::workspaces::providers::Reach::Devices,
        Arc::clone(&photos) as Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
    );
    let address = host.node_addr();

    let mut endpoints = Vec::new();
    for (seed, role) in [(71, Role::Editor), (72, Role::Viewer)] {
        let (device_key, device) = guest_device();
        entry_join_as(
            workspaces,
            &cert,
            &workspace,
            (&device_key, &device),
            seed,
            role,
        )
        .await;
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(iroh::SecretKey::from_bytes(&device_key.to_bytes()))
            .bind()
            .await
            .unwrap();
        let actions = tilo_peer_state::actions::ActionService::new(
            device_key.clone(),
            Arc::new(tilo_actions::remote::TestProvider::new()),
        );
        let remote = Arc::new(actions.remote(&endpoint, &address, &workspace));
        let id = tilo_actions::InvocationId::new();
        // The viewer asks for a change of a picture, which its role does not reach.
        let action = if role == Role::Viewer {
            RETOUCH
        } else {
            THUMBNAILS
        };
        let ask = || {
            let remote = Arc::clone(&remote);
            async move {
                tokio::time::timeout(
                    Duration::from_secs(20),
                    remote.invoke_until_settled(
                        job_of(action, [seed; 32]).take_request(id),
                        Duration::from_secs(20),
                    ),
                )
                .await
                .expect("the host answers")
                .expect("the host answered")
            }
        };
        if role == Role::Viewer {
            let refused = ask().await;
            assert!(
                matches!(&refused, Err(ActionCallError::Refused { reason }) if reason.contains("does not let")),
                "a device the workspace does not let run the action is refused: {refused:?}"
            );
        } else {
            for _ in 0..2 {
                let update = ask().await.expect("the turn ran");
                let InvocationUpdate::Succeeded { output, .. } = update else {
                    panic!("the turn ran to its output: {update:?}");
                };
                assert_eq!(
                    output.value,
                    Some(serde_json::json!({ "ran": [hex::encode([seed; 32])] })),
                    "the run read the input the device named"
                );
            }
            assert_eq!(
                *photos.ran.lock().unwrap(),
                vec![(
                    device.device_public_key,
                    vec![serde_json::json!(hex::encode([seed; 32]))],
                )],
                "the action ran once, for the device that asked, with what it named"
            );
        }
        endpoints.push(endpoint);
    }
    assert_eq!(
        photos.ran.lock().unwrap().len(),
        1,
        "nothing ran for the viewer"
    );
    for endpoint in endpoints {
        endpoint.close().await;
    }
    host.shutdown().await;
}

/// `device`'s ask, over the device action service, that the host run Photos' `action` over the
/// picture `seed`: what the host answered.
async fn ask_turn(
    host: &super::fixtures::TestHost,
    workspace: &str,
    (device_key, endpoint): (&ed25519_dalek::SigningKey, &iroh::Endpoint),
    action: &str,
    seed: u8,
) -> Result<InvocationUpdate, ActionCallError> {
    let actions = tilo_peer_state::actions::ActionService::new(
        device_key.clone(),
        Arc::new(tilo_actions::remote::TestProvider::new()),
    );
    let remote = Arc::new(actions.remote(endpoint, &host.node_addr(), workspace));
    tokio::time::timeout(
        Duration::from_secs(20),
        remote.invoke_until_settled(
            job_of(action, [seed; 32]).take_request(tilo_actions::InvocationId::new()),
            Duration::from_secs(20),
        ),
    )
    .await
    .expect("the host answers")
    .expect("the host answered")
}

// A guest of a workspace runs what their grants reach at the kind each action is declared as: a
// viewer has a read run for them and is refused a change until a member grants them changes of
// the app, and an action the app declares nothing of is refused whoever asks.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_is_judged_at_the_kind_its_action_declares() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let host = test_host(dir.path(), &key, &cert).await;
    declare_photos(&host);
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Photos").await.unwrap().workspace;
    offer_compute(&host, &cert, true).await;
    let photos = Arc::new(Photos::default());
    workspaces.run_jobs(Arc::new(Keeps(true)));
    workspaces.providers().provide(
        "test/photos",
        PHOTOS,
        crate::workspaces::providers::Reach::Devices,
        Arc::clone(&photos) as Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
    );
    let (viewer_key, viewer) = guest_device();
    entry_join_as(
        workspaces,
        &cert,
        &workspace,
        (&viewer_key, &viewer),
        81,
        Role::Viewer,
    )
    .await;
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&viewer_key.to_bytes()))
        .bind()
        .await
        .unwrap();
    let ask = |action: &'static str, seed: u8| {
        let (host, workspace, viewer_key, endpoint) = (&host, &workspace, &viewer_key, &endpoint);
        async move { ask_turn(host, workspace, (viewer_key, endpoint), action, seed).await }
    };
    let refused = |answer: &Result<InvocationUpdate, ActionCallError>| matches!(answer, Err(ActionCallError::Refused { reason }) if reason.contains("does not let"));

    // A read is the viewer's to run.
    let read = ask(THUMBNAILS, 1).await;
    assert!(
        matches!(read, Ok(InvocationUpdate::Succeeded { .. })),
        "a viewer has a read run for them: {read:?}"
    );

    // A change is not, until the owner grants the viewer changes of Photos.
    let change = ask(RETOUCH, 2).await;
    assert!(refused(&change), "a viewer is refused a change: {change:?}");
    super::fixtures::answer_grant(
        workspaces,
        &workspace,
        tilo_action_policy::Grant::new(
            "viewer-retouches",
            tilo_action_policy::Grantee::person(hex::encode(viewer.user_public_key)),
            tilo_action_policy::TreePath::workspace(&workspace).app(PHOTOS),
            tilo_action_policy::KindSet::UNDOABLE,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(hex::encode(cert.user_public_key)),
        ),
    )
    .await
    .expect("the owner grants the viewer changes of Photos");
    let granted = ask(RETOUCH, 3).await;
    assert!(
        matches!(granted, Ok(InvocationUpdate::Succeeded { .. })),
        "the change the viewer was granted runs: {granted:?}"
    );

    // An action Photos declares nothing of is refused, granted kinds or not.
    let undeclared = ask(UNDECLARED, 4).await;
    assert!(
        refused(&undeclared),
        "an action no declaration names is refused: {undeclared:?}"
    );
    let ran: Vec<serde_json::Value> = photos
        .ran
        .lock()
        .unwrap()
        .iter()
        .flat_map(|(_, pictures)| pictures.clone())
        .collect();
    assert_eq!(
        ran,
        vec![
            serde_json::json!(hex::encode([1; 32])),
            serde_json::json!(hex::encode([3; 32])),
        ],
        "only the read and the granted change ran"
    );
    endpoint.close().await;
    host.shutdown().await;
}

/// A firing of the host's person's automation, started on `starter` `ago_ms` ago.
fn firing(cert: &DeviceCert, starter: [u8; 32], ago_ms: i64) -> Turn {
    Turn::once(
        "shortcuts.shortcut:tidy#run",
        serde_json::json!({ "invocation": uuid::Uuid::new_v4().to_string() }),
    )
    .acting_as(Actor::Person(hex::encode(cert.user_public_key)))
    .started(
        starter,
        tilo_clock::unix_now_ms_i64().saturating_sub(ago_ms),
    )
}

/// `device`'s claim on `lease` of `workspace`, written as that device would, now.
async fn claim_as(workspaces: &Workspaces, workspace: &str, device: &DeviceCert, lease: &str) {
    let key = workspaces.inner.host.key.clone();
    let now = system_time(tilo_clock::unix_now_ns());
    let copy = workspaces.workspace(workspace).await.unwrap();
    let claimed = copy
        .lock()
        .await
        .change(
            &key,
            now,
            |_, candidate| {
                let claimed = candidate.workspace_mut().take_lease(
                    device,
                    lease,
                    std::time::Duration::from_millis(1),
                    tilo_peer_state::unranked(),
                    now,
                )?;
                Ok((claimed.is_some(), true))
            },
            |record, _| Ok(record.collaboration.clone()),
        )
        .unwrap();
    assert!(claimed, "the other device's claim is written");
}

/// A turn goes to the device that started it at once; an always-on server of
/// the person waits a few seconds for the starter's claim and then takes it;
/// any other device waits longer; a device that is not the saver's never takes
/// a firing; and a firing kept done is never taken again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_firing_goes_to_its_starter_then_a_server_then_any_device() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Home").await.unwrap().workspace;
    let elsewhere = [9; 32];

    let started_here = firing(&cert, cert.device_public_key, 0);
    let claim = takes_turn(workspaces, &workspace, &started_here)
        .await
        .expect("the starter takes its turn at once");
    keep_done(
        workspaces,
        &workspace,
        &started_here,
        serde_json::json!({ "run": 1 }),
    )
    .await
    .unwrap();
    claim.release().await;
    assert_eq!(
        takes_turn(workspaces, &workspace, &started_here)
            .await
            .err(),
        Some(NotTaken::Done(serde_json::json!({ "run": 1 }))),
        "a firing kept done is never taken again, and answers what it answered"
    );

    let Err(NotTaken::Waiting { at_ms }) =
        takes_turn(workspaces, &workspace, &firing(&cert, elsewhere, 0)).await
    else {
        panic!("a device that is no server waits for the starter");
    };
    assert!(
        at_ms >= tilo_clock::unix_now_ms_i64() + 20_000,
        "it waits the longer grace: {at_ms}"
    );
    assert!(
        takes_turn(workspaces, &workspace, &firing(&cert, elsewhere, 31_000))
            .await
            .is_ok(),
        "once the longer grace passed, any eligible device takes it"
    );

    offer_compute(&host, &cert, false).await;
    let Err(NotTaken::Waiting { at_ms }) =
        takes_turn(workspaces, &workspace, &firing(&cert, elsewhere, 0)).await
    else {
        panic!("an always-on server waits a moment for the starter");
    };
    assert!(
        at_ms <= tilo_clock::unix_now_ms_i64() + 10_000,
        "a server waits only the short grace: {at_ms}"
    );
    assert!(
        takes_turn(workspaces, &workspace, &firing(&cert, elsewhere, 6_000))
            .await
            .is_ok(),
        "a server takes the turn the starter did not claim in time"
    );

    let (_, stranger) = guest_device();
    let theirs = Turn::once(
        "shortcuts.shortcut:theirs#run",
        serde_json::json!({ "invocation": "1" }),
    )
    .acting_as(Actor::Person(hex::encode(stranger.user_public_key)))
    .started(cert.device_public_key, tilo_clock::unix_now_ms_i64());
    assert_eq!(
        takes_turn(workspaces, &workspace, &theirs).await.err(),
        Some(NotTaken::Ineligible(Ineligible::CannotActAs)),
        "a device that is no server runs only its own person's automations"
    );
    offer_compute(&host, &cert, true).await;
    assert!(
        takes_turn(workspaces, &workspace, &theirs).await.is_ok(),
        "an always-on server runs a member's automations for them"
    );
    host.shutdown().await;
}

/// The device that started a firing claimed it and went quiet: no other device
/// claims over its live claim, and once it wrote nothing for a minute the next
/// device takes the turn over.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_that_goes_quiet_loses_its_turn_to_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Home").await.unwrap().workspace;
    let (editor_key, editor) = guest_device();
    entry_join_as(
        workspaces,
        &cert,
        &workspace,
        (&editor_key, &editor),
        63,
        Role::Editor,
    )
    .await;
    let turn = firing(&cert, editor.device_public_key, 0);
    claim_as(workspaces, &workspace, &editor, &turn.lease()).await;
    assert_eq!(
        takes_turn(workspaces, &workspace, &turn).await.err(),
        Some(NotTaken::TakenBy(editor.device_public_key)),
        "a live claim of another device is never claimed over"
    );

    // A minute and more later, read on this test's own thread.
    let wall = tilo_clock::testing::SharedWall::default();
    wall.pin(tilo_clock::wall() + std::time::Duration::from_secs(62));
    wall.follow();
    let taken = takes_turn(workspaces, &workspace, &turn).await;
    let claimer = workspaces.lease_holder(&workspace, &turn.lease()).await;
    wall.release();
    assert!(
        taken.is_ok(),
        "the quiet device's turn is taken over: {:?}",
        taken.err()
    );
    assert_eq!(claimer.unwrap(), Some(cert.device_public_key));
    host.shutdown().await;
}

/// `device`'s row keeping `lease` of `workspace` done, written as that device would, now.
#[cfg(feature = "apps")]
async fn done_as(workspaces: &Workspaces, workspace: &str, device: &DeviceCert, lease: &str) {
    let key = workspaces.inner.host.key.clone();
    let now = system_time(tilo_clock::unix_now_ns());
    let copy = workspaces.workspace(workspace).await.unwrap();
    let kept = copy
        .lock()
        .await
        .change(
            &key,
            now,
            |_, candidate| {
                let kept = candidate.workspace_mut().keep_done(
                    device,
                    lease,
                    serde_json::json!("migrated"),
                    now,
                )?;
                Ok((kept, true))
            },
            |record, _| Ok(record.collaboration.clone()),
        )
        .unwrap();
    assert!(kept, "the other device's done row is written");
}

/// A package's migration of a resource, asked for by the app's process on each device as it
/// opens (`Command::Turn`), runs once in the workspace: with two devices online the one that
/// claimed it runs it and the other finds it done, and when the device that took it goes quiet
/// mid-way the other takes it over once its claim lapsed, and runs it once. Every device that asks
/// after it ran is given what it answered.
#[cfg(feature = "apps")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_migration_runs_once_across_devices_and_once_more_after_a_takeover() {
    use collab_types::store::wire::{Command, Reply, Turn as Answer};

    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    workspaces.set_member_device(Arc::new(MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    let workspace = workspaces.create("Home").await.unwrap().workspace;
    let (editor_key, editor) = guest_device();
    entry_join_as(
        workspaces,
        &cert,
        &workspace,
        (&editor_key, &editor),
        64,
        Role::Editor,
    )
    .await;
    let app = "so.tilo.lang.notes";
    let ask = |address: String, answer: Option<serde_json::Value>| {
        let workspace = workspace.clone();
        async move {
            let reply = workspaces
                .own_resource_command(
                    &workspace,
                    app,
                    Command::Turn {
                        address,
                        inputs: serde_json::Value::Null,
                        answer,
                    },
                    Vec::new(),
                )
                .await
                .expect("the service answers a turn");
            let Reply::Turn(answer) = reply else {
                panic!("a turn is answered as a turn: {reply:?}");
            };
            answer
        }
    };
    let lease = |address: &str| Turn::once(address, serde_json::Value::Null).lease();

    // The editor's device opened the app first and took the migration of the first notebook.
    let first = format!("{app}/notebook-1#migrate");
    claim_as(workspaces, &workspace, &editor, &lease(&first)).await;
    let waits = ask(first.clone(), None).await;
    assert_eq!(waits, Answer::Later, "another runs it");
    done_as(workspaces, &workspace, &editor, &lease(&first)).await;
    assert_eq!(
        ask(first.clone(), None).await,
        Answer::Done(serde_json::json!("migrated")),
        "and once it ran, no device runs it again: each is given its answer"
    );

    // This device opened first for the second one: it runs it, and only once.
    let second = format!("{app}/notebook-2#migrate");
    assert_eq!(ask(second.clone(), None).await, Answer::Take);
    assert_eq!(
        ask(second.clone(), None).await,
        Answer::Take,
        "asking again while it runs keeps the turn"
    );
    let answer = serde_json::json!({ "notes": 3 });
    let kept = ask(second.clone(), Some(answer.clone())).await;
    assert_eq!(kept, Answer::Done(answer.clone()));
    assert_eq!(ask(second.clone(), None).await, Answer::Done(answer));

    // The editor's device took the third and went quiet mid-way: this device waits out its claim
    // and then runs it, once.
    let third = format!("{app}/notebook-3#migrate");
    claim_as(workspaces, &workspace, &editor, &lease(&third)).await;
    assert_eq!(ask(third.clone(), None).await, Answer::Later);
    let wall = tilo_clock::testing::SharedWall::default();
    wall.pin(tilo_clock::wall() + std::time::Duration::from_secs(62));
    wall.follow();
    let taken = ask(third.clone(), None).await;
    let kept = ask(third.clone(), Some(serde_json::Value::Null)).await;
    let again = ask(third.clone(), None).await;
    wall.release();
    assert_eq!(taken, Answer::Take, "the quiet device's turn is taken over");
    assert_eq!(kept, Answer::Done(serde_json::Value::Null));
    assert_eq!(again, kept);

    // An app places only its own actions.
    let refused = workspaces
        .own_resource_command(
            &workspace,
            "so.tilo.lang.desk",
            Command::Turn {
                address: first,
                inputs: serde_json::Value::Null,
                answer: None,
            },
            Vec::new(),
        )
        .await;
    assert!(refused.is_err(), "{refused:?}");
    host.shutdown().await;
}

/// The same inputs give a once-only action the same lease however they were written: in any key
/// order, a whole number with or without a fraction.
#[test]
fn the_same_inputs_written_differently_lease_one_turn() {
    let once = |inputs: &str| {
        Turn::once(
            "so.tilo.notes/notebook-1#migrate",
            serde_json::from_str(inputs).unwrap(),
        )
        .lease()
    };
    assert_eq!(
        once(r#"{"run": 1, "of": {"b": [2.0], "a": "x"}}"#),
        once(r#"{"of": {"a": "x", "b": [2]}, "run": 1.0}"#)
    );
    assert_ne!(once(r#"{"run": 1}"#), once(r#"{"run": 2}"#));
    assert_ne!(once(r#"{"run": 1.5}"#), once(r#"{"run": 1}"#));
}
