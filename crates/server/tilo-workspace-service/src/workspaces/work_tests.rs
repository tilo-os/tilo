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
use super::tests::{governor, guest_device, test_host};
use super::*;
use crate::work::{HostJobs, HostLinux, Ineligible, Job, NotTaken, Turn, keep_done, takes_turn};

/// The jobs a device runs, as a test sets them: whether it runs the transform,
/// keeps the inputs and has room, and what each run was given.
struct Jobs {
    runs: bool,
    holds: bool,
    free: Option<u32>,
    ran: Mutex<Vec<Ran>>,
}

/// One run: the device it was for, the turn, and its inputs.
type Ran = ([u8; 32], tilo_actions::InvocationId, Vec<[u8; 32]>);

impl Jobs {
    fn ready() -> Self {
        Self {
            runs: true,
            holds: true,
            free: None,
            ran: Mutex::default(),
        }
    }
}

impl HostJobs for Jobs {
    fn runs(&self, _workspace: &str, _job: &Job) -> bool {
        self.runs
    }

    fn holds(&self, _workspace: &str, _job: &Job) -> bool {
        self.holds
    }

    fn free_slots(&self) -> Option<u32> {
        self.free
    }

    fn run(
        &self,
        _workspace: &str,
        job: &Job,
        requester: [u8; 32],
        invocation: tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::ActionOutput, tilo_actions::remote::ActionCallError>,
    > {
        self.ran
            .lock()
            .unwrap()
            .push((requester, invocation, job.inputs.clone()));
        let named: Vec<String> = job.inputs.iter().map(hex::encode).collect();
        tilo_actions::remote::Pending::ready(Ok(tilo_actions::ActionOutput {
            value: Some(serde_json::json!({ "ran": named })),
            resources: Vec::new(),
        }))
    }
}

/// Photos' thumbnails of one photograph, as a device asks for them.
fn job(input: [u8; 32]) -> Job {
    Job {
        app: "so.tilo.photos".into(),
        resource: "photos.library".into(),
        action: "make-thumbnails".into(),
        offload: tilo_actions::Offload {
            app: "so.tilo.thumbnails".into(),
            entrypoint: "thumbnail-batch".into(),
            gpu: false,
            cpu_time_ms: 60_000,
            memory_bytes: 512 * 1024 * 1024,
            output_bytes: 8 * 1024 * 1024,
        },
        inputs: vec![input],
    }
}

/// The person's entry for `device` among their servers, offering compute when
/// `compute` says so: the Servers workspace founded here, the device enrolled.
pub(super) async fn offer_compute(
    host: &super::fixtures::TestHost,
    device: &DeviceCert,
    compute: bool,
) {
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
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
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
    unclaimed(&turn(&editor), Ineligible::RunsNoJobs).await;
    workspaces.run_jobs(Arc::new(Jobs {
        runs: false,
        ..Jobs::ready()
    }));
    unclaimed(&turn(&editor), Ineligible::CannotRun).await;
    workspaces.run_jobs(Arc::new(Jobs {
        holds: false,
        ..Jobs::ready()
    }));
    unclaimed(&turn(&editor), Ineligible::InputsElsewhere).await;
    workspaces.run_jobs(Arc::new(Jobs {
        free: Some(0),
        ..Jobs::ready()
    }));
    unclaimed(&turn(&editor), Ineligible::AtCapacity).await;
    workspaces.run_jobs(Arc::new(Jobs::ready()));
    unclaimed(&turn(&viewer), Ineligible::NotPermitted).await;

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
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Build").await.unwrap().workspace;
    let turn = || Turn::Linux {
        id: tilo_actions::InvocationId::new(),
        requester: cert.device_public_key,
        run: Box::new(tilo_actions::remote::InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::environment_commands::target(),
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
    let (key, cert) = governor();
    let host = test_host(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    let workspace = workspaces.create("Photos").await.unwrap().workspace;
    offer_compute(&host, &cert, true).await;
    let jobs = Arc::new(Jobs::ready());
    workspaces.run_jobs(Arc::clone(&jobs) as Arc<dyn HostJobs>);
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
        let ask = || {
            let remote = Arc::clone(&remote);
            async move {
                tokio::time::timeout(
                    Duration::from_secs(20),
                    remote.invoke_until_settled(
                        job([seed; 32]).take_request(id),
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
                *jobs.ran.lock().unwrap(),
                vec![(device.device_public_key, id, vec![[seed; 32]])],
                "the turn ran once, for the device that asked"
            );
        }
        endpoints.push(endpoint);
    }
    assert_eq!(
        jobs.ran.lock().unwrap().len(),
        1,
        "nothing ran for the viewer"
    );
    for endpoint in endpoints {
        endpoint.close().await;
    }
    host.shutdown().await;
}

/// A firing of the host's person's automation, started on `starter` `ago_ms` ago.
fn firing(cert: &DeviceCert, starter: [u8; 32], ago_ms: i64) -> Turn {
    Turn::Firing {
        firing: format!("tidy/{}", uuid::Uuid::new_v4()),
        saver: hex::encode(cert.user_public_key),
        starter,
        came_up_ms: tilo_clock::unix_now_ms_i64().saturating_sub(ago_ms),
    }
}

/// `device`'s claim on `lease` of `workspace`, written as that device would, now.
async fn claim_as(workspaces: &Workspaces, workspace: &str, device: &DeviceCert, lease: &str) {
    let key = workspaces.inner.host.key.clone();
    let now = system_time(tilo_clock::unix_now_ns());
    let copy = workspaces.workspace(workspace).await.unwrap();
    let claimed = copy
        .lock()
        .await
        .change_held(
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
    let (key, cert) = governor();
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
    let theirs = Turn::Firing {
        firing: "theirs/1".into(),
        saver: hex::encode(stranger.user_public_key),
        starter: cert.device_public_key,
        came_up_ms: tilo_clock::unix_now_ms_i64(),
    };
    assert_eq!(
        takes_turn(workspaces, &workspace, &theirs).await.err(),
        Some(NotTaken::Ineligible(Ineligible::NotTheSaver)),
        "a device runs only its own person's automations"
    );
    host.shutdown().await;
}

/// The device that started a firing claimed it and went quiet: no other device
/// claims over its live claim, and once it wrote nothing for a minute the next
/// device takes the turn over.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_that_goes_quiet_loses_its_turn_to_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
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
        .change_held(
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
    use tilo_resource_store::wire::{Command, Reply, Turn as Answer};

    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
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
                )
                .await
                .expect("the service answers a turn");
            let Reply::Turn(answer) = reply else {
                panic!("a turn is answered as a turn: {reply:?}");
            };
            answer
        }
    };
    let lease = |address: &str| {
        Turn::Once {
            address: address.to_owned(),
            inputs: serde_json::Value::Null,
        }
        .lease()
    };

    // The editor's device opened the app first and took the migration of the first notebook.
    let first = format!("{app}/notebook-1#migrate");
    claim_as(workspaces, &workspace, &editor, &lease(&first)).await;
    assert_eq!(
        ask(first.clone(), None).await,
        Answer::Later,
        "another runs it"
    );
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
        Turn::Once {
            address: "so.tilo.notes/notebook-1#migrate".to_owned(),
            inputs: serde_json::from_str(inputs).unwrap(),
        }
        .lease()
    };
    assert_eq!(
        once(r#"{"run": 1, "of": {"b": [2.0], "a": "x"}}"#),
        once(r#"{"of": {"a": "x", "b": [2]}, "run": 1.0}"#)
    );
    assert_ne!(once(r#"{"run": 1}"#), once(r#"{"run": 2}"#));
    assert_ne!(once(r#"{"run": 1.5}"#), once(r#"{"run": 1}"#));
}
