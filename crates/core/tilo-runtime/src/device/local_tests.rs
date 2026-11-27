use super::*;
use crate::RuntimeServices as _;
use crate::device::services::Daemon;
use crate::device::services::tests::isolated_root;
use serde_json::Value;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use tilo_actions::remote::{CatalogueQuery, RemoteActionMessage};
use tilo_test_env::SpawnUnderEnvLock as _;

/// The account the daemon runs as in these tests: the person's own.
pub(crate) const OWN: u32 = 4242;
/// Another account of the same computer, signed in as nobody the daemon serves.
const STRANGER: u32 = 4243;

/// The account a test's next connection comes from, as the daemon reads it.
static PEER: AtomicU32 = AtomicU32::new(OWN);

/// The process a test's next connection comes from, as the daemon reads it: none when 0.
static PEER_PID: AtomicU32 = AtomicU32::new(0);

#[expect(
    clippy::unnecessary_wraps,
    reason = "it stands in for the kernel's answer, which can fail"
)]
fn test_peer(_: &UnixStream) -> io::Result<crate::seats::Peer> {
    Ok(crate::seats::Peer {
        uid: PEER.load(Ordering::SeqCst),
        pid: Some(PEER_PID.load(Ordering::SeqCst)).filter(|pid| *pid != 0),
    })
}

/// `daemon`'s native endpoint, served in `root` as the daemon process serves it once the
/// person's services run, the person's own account being [`OWN`].
pub(crate) fn native_endpoint(daemon: &Arc<Daemon>, root: &Path) -> PathBuf {
    let keys = tilo_api::calls::device_keys();
    let key = tilo_device_identity::persist::load_device_key(&keys).unwrap();
    let place = tilo_device_identity::login::Place::here(keys).unwrap();
    let admission: &'static tilo_api::daemon::admission::Admission = Box::leak(Box::new(
        tilo_api::daemon::admission::Admission::new(OWN, false),
    ));
    let socket = root.join("native.sock");
    crate::daemon::serve_for_test(
        daemon,
        DeviceOnly::new(key, place),
        test_peer,
        admission,
        &socket,
    )
    .unwrap();
    socket
}

/// A client of the daemon's frames on a connection of its own, as `uid`.
pub(crate) fn line_client(socket: &Path, uid: u32) -> Arc<tilo_action_services::LineClient> {
    PEER.store(uid, Ordering::SeqCst);
    PEER_PID.store(0, Ordering::SeqCst);
    let stream = UnixStream::connect(socket).unwrap();
    let client = Arc::new(tilo_action_services::LineClient::of_stream(stream).unwrap());
    // The daemon reads the account as it takes the connection: a catalogue answered says it did.
    let _ = listed_to(&client);
    client
}

/// The providers the catalogue lists to `client`.
fn listed_to(client: &tilo_action_services::LineClient) -> Vec<String> {
    client
        .query(&Caller::local_system(), CatalogueQuery::default())
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the catalogue was answered")
        .map(|page| {
            page.providers
                .into_iter()
                .map(|provider| provider.provider)
                .collect()
        })
        .unwrap_or_default()
}

const FRAMES_TEST: &str =
    "device::local::tests::the_native_endpoints_frames_carry_a_commands_calls_as_its_account";

/// A command of the person's calls actions in frames on the daemon's native endpoint, walked in
/// its own process as `tilo ls` walks them: the workspace the daemon serves, and what it keeps.
/// Another account of the computer, which the daemon serves no person for, reaches the device's
/// own actions alone: none of the person's workspaces, and nothing put into them.
#[test]
fn the_native_endpoints_frames_carry_a_commands_calls_as_its_account() {
    let Some(root) = isolated_root(FRAMES_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let socket = native_endpoint(&daemon, &root);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    // Whom a client names does not count: the daemon decides by the account connecting.
    let anyone = Caller::local_system();

    let person = line_client(&socket, OWN);
    let calls = runtime
        .block_on(tilo_api::calls::Calls::over(person))
        .unwrap();
    let workspace = daemon.workspace().expect("the workspace");
    assert_eq!(calls.served_workspace(), Some(workspace.as_str()));
    let listed = runtime
        .block_on(calls.list(Some(workspace.as_str()), None, None))
        .unwrap();
    assert!(
        listed.get("rows").and_then(Value::as_array).is_some(),
        "{listed}"
    );

    let account = |listed: &[String]| {
        listed
            .iter()
            .any(|provider| provider == tilo_api::account::PROVIDER)
    };
    assert!(
        account(&listed_to(&line_client(&socket, OWN))),
        "the person lists their account"
    );
    let stranger = line_client(&socket, STRANGER);
    assert!(
        !account(&listed_to(&stranger)),
        "another account lists none of the person's account"
    );
    let put = stranger
        .content_puts()
        .map(|puts| puts.put_many(&anyone, &[b"hello".to_vec()]));
    assert!(
        !matches!(put, Some(Ok(_))),
        "another account puts nothing into the person's workspace: {put:?}"
    );
    let refused = runtime
        .block_on(tilo_api::calls::Calls::over(stranger))
        .err()
        .expect("another account's frames do not reach the person's account");
    assert!(
        refused.to_string().contains("refused"),
        "it is told the daemon refused it: {refused}"
    );
}

const MIGRATE_TEST: &str =
    "device::local::tests::a_commands_migration_steps_reach_the_devices_own_migrations";

/// `tilo migrate` invokes the device's migrations as any caller does: a step it sends on the
/// daemon's native endpoint, as the person whose account connects, reaches the provider the
/// daemon registered on `devices/<this device>/migrate`, and its answer comes back on that
/// invocation.
#[test]
fn a_commands_migration_steps_reach_the_devices_own_migrations() {
    let Some(root) = isolated_root(MIGRATE_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    daemon.serve_migrations();
    let socket = native_endpoint(&daemon, &root);
    let person = line_client(&socket, OWN);
    // Whom a client names does not count: the daemon decides by the account connecting.
    let anyone = Caller::local_system();

    let request = tilo_actions::remote::InvokeRequest::plain(
        tilo_actions::remote::RemoteActionTarget {
            provider: tilo_migrate::provider::PROVIDER.into(),
            address: tilo_actions::ActionAddress::new(
                tilo_migrate::provider::resource(&daemon.device_key()),
                "sources",
            ),
        },
        serde_json::Map::new(),
    );
    let id = request.invocation;
    let receipt = person
        .invoke(&anyone, request)
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the daemon answered")
        .expect("the person's step is admitted");
    let mut status = receipt.status;
    // The connection is the person's own: the step is theirs to run, and it answers on the
    // invocation it was asked in, nothing waiting for a yes.
    let until = std::time::Instant::now()
        .checked_add(tilo_test_env::hang_bound())
        .expect("a time in reach");
    let answer = loop {
        match status {
            tilo_actions::remote::ReceiptStatus::Accepted => {}
            tilo_actions::remote::ReceiptStatus::Completed { output } => {
                break output.value.expect("sources answers a value");
            }
            other => panic!("sources did not answer: {other:?}"),
        }
        assert!(std::time::Instant::now() < until, "sources never ended");
        if let Some(change) = person.next_change(&anyone, &id) {
            let _ = change.wait_timeout(tilo_test_env::hang_bound());
        }
        status = person
            .observe(&anyone, &id)
            .expect("the person follows their own step")
            .receipt()
            .status;
    };
    let sources: Vec<&str> = answer["sources"]
        .as_array()
        .expect("every source")
        .iter()
        .filter_map(|source| source["source"].as_str())
        .collect();
    assert!(
        sources.contains(&"thunderbird") && sources.contains(&"linear"),
        "{answer:#}"
    );
}

const FILES_TEST: &str =
    "device::local::tests::a_file_put_over_the_native_endpoint_reads_back_by_its_hash";

/// `len` bytes no two runs of which repeat for a while, so a piece sent twice or out of place
/// reads back wrong.
fn file_bytes(len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state.to_be_bytes()[0]
        })
        .collect()
}

/// `tilo files put` on this computer: a file larger than one piece goes over the daemon's
/// socket into the workspace it serves, piece by piece, and reads back by its hash, the same
/// bytes. An upload whose connection is lost goes on from where it stood on the next, for the
/// person who began it and nobody else.
#[test]
fn a_file_put_over_the_native_endpoint_reads_back_by_its_hash() {
    use tilo_actions::remote::ContentPut as _;
    let Some(root) = isolated_root(FILES_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let socket = native_endpoint(&daemon, &root);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let workspace = daemon.workspace().expect("the workspace");
    let calls = runtime
        .block_on(tilo_api::calls::Calls::over(line_client(&socket, OWN)))
        .unwrap();

    // A file of a megabyte and a bit: more than one piece of a put.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scan.pdf");
    let bytes = file_bytes(1024 * 1024 + 17, 7);
    std::fs::write(&path, &bytes).unwrap();
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let have = |hash: &str| {
        runtime
            .block_on(calls.have_contents(&workspace, &[hash.to_owned()]))
            .unwrap()
    };
    assert_eq!(have(&hash), [false], "the workspace does not have it yet");
    let content = runtime
        .block_on(calls.upload_file(&workspace, &path))
        .unwrap();
    assert_eq!(content, format!("tilo.content:{hash}"));
    assert_eq!(have(&hash), [true], "the workspace has it now");
    let mut read = Vec::new();
    let mut told = Vec::new();
    let written = runtime
        .block_on(
            calls.download(&workspace, &content, &mut read, &mut |at, size| {
                told.push((at, size));
            }),
        )
        .unwrap();
    assert_eq!(written, bytes.len() as u64);
    assert!(read == bytes, "the bytes read back are the bytes put");
    assert!(told.len() > 1, "it reads back a piece at a time: {told:?}");

    // An upload whose connection is lost halfway goes on, on a new connection, from the bytes
    // the daemon has of it.
    let second = file_bytes(600 * 1024, 11);
    let second_hash = blake3::hash(&second).to_hex().to_string();
    let half = second.len() / 2;
    let upload = {
        let lost = line_client(&socket, OWN);
        let mut put = lost.begin_put(second.len() as u64).unwrap();
        put.append(0, &second[..half]).unwrap();
        put.upload()
    };
    assert_eq!(have(&second_hash), [false], "half a file is no content yet");
    let mut put = line_client(&socket, OWN)
        .resume_put(upload, second.len() as u64)
        .unwrap();
    assert_eq!(put.written(), half as u64, "it goes on where it stood");
    // A piece sent again from the start is refused with where the upload stands.
    let refused = put.append(0, &second[..half]).unwrap_err();
    assert!(
        refused.to_string().contains(&format!("offset {half}")),
        "{refused}"
    );
    put.append(half as u64, &second[half..]).unwrap();
    let second_content = Box::new(put).finish().unwrap();
    assert_eq!(second_content, format!("tilo.content:{second_hash}"));
    let mut read = Vec::new();
    runtime
        .block_on(calls.download(&workspace, &second_content, &mut read, &mut |_, _| {}))
        .unwrap();
    assert!(read == second, "the resumed file reads back whole");

    // Nobody else takes it up: another account of the same computer finds no such upload.
    let lost = line_client(&socket, OWN);
    let upload = {
        let mut put = lost.begin_put(second.len() as u64).unwrap();
        put.append(0, &second[..half]).unwrap();
        put.upload()
    };
    assert!(
        line_client(&socket, STRANGER)
            .resume_put(upload, second.len() as u64)
            .is_err(),
        "another account takes up no upload of the person's"
    );
}

const FOLDER_TEST: &str = "device::local::tests::a_folder_put_again_sends_none_of_its_bytes";

/// A client of the person's at the daemon's `socket` whose every frame toward the daemon is kept
/// in `sent`, as the daemon receives it.
fn watched(
    socket: &Path,
    sent: &Arc<std::sync::Mutex<Vec<RemoteActionMessage>>>,
) -> Arc<tilo_action_services::LineClient> {
    PEER.store(OWN, Ordering::SeqCst);
    let daemon = UnixStream::connect(socket).unwrap();
    let (ours, theirs) = UnixStream::pair().unwrap();
    let (mut answers, mut to_us) = (daemon.try_clone().unwrap(), theirs.try_clone().unwrap());
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut answers, &mut to_us);
        let _ = to_us.shutdown(std::net::Shutdown::Both);
    });
    let sent = Arc::clone(sent);
    std::thread::spawn(move || {
        let mut daemon = daemon;
        for line in BufReader::new(theirs).split(b'\n') {
            let Ok(line) = line else { break };
            if let Ok(frame) = RemoteActionFrame::decode(&line) {
                sent.lock().unwrap().push(frame.message);
            }
            if daemon
                .write_all(&line)
                .and_then(|()| daemon.write_all(b"\n"))
                .is_err()
            {
                break;
            }
        }
        let _ = daemon.shutdown(std::net::Shutdown::Write);
    });
    Arc::new(tilo_action_services::LineClient::of_stream(ours).unwrap())
}

/// The content put steps among `sent`, in the order the daemon received them.
fn put_steps(
    sent: &std::sync::Mutex<Vec<RemoteActionMessage>>,
) -> Vec<tilo_actions::remote::ContentPutStep> {
    sent.lock()
        .unwrap()
        .iter()
        .filter_map(|message| match message {
            RemoteActionMessage::ContentPutRequest { step, .. } => Some(step.clone()),
            _ => None,
        })
        .collect()
}

/// `tilo files put` of a folder: the daemon is asked which of its files the workspace has
/// before any bytes go, the small files go many to a step and the large one in pieces. Put
/// again, the folder sends none of its bytes: the daemon receives only the question.
#[test]
fn a_folder_put_again_sends_none_of_its_bytes() {
    use tilo_actions::remote::ContentPutStep;
    let Some(root) = isolated_root(FOLDER_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let socket = native_endpoint(&daemon, &root);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let workspace = daemon.workspace().expect("the workspace");
    let dir = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    let mut expected = Vec::new();
    for (index, len) in (0..40_u32)
        .map(|index| (index, 1024 + index as usize * 97))
        .chain([(40, 900 * 1024)])
    {
        let bytes = file_bytes(len, index + 1);
        let path = dir.path().join(format!("page-{index:02}.txt"));
        std::fs::write(&path, &bytes).unwrap();
        expected.push(format!("tilo.content:{}", blake3::hash(&bytes).to_hex()));
        paths.push(path);
    }
    let put = |sent: &Arc<std::sync::Mutex<Vec<RemoteActionMessage>>>| {
        let calls = runtime
            .block_on(tilo_api::calls::Calls::over(watched(&socket, sent)))
            .unwrap();
        let mut told = 0_u64;
        let contents = runtime
            .block_on(calls.put_files(&workspace, &paths, &mut |bytes| told += bytes))
            .unwrap();
        let total: u64 = paths
            .iter()
            .map(|path| std::fs::metadata(path).unwrap().len())
            .sum();
        assert_eq!(told, total, "every file's bytes are told once");
        contents
    };

    let first = Arc::default();
    assert_eq!(put(&first), expected);
    let steps = put_steps(&first);
    assert!(
        matches!(steps.first(), Some(ContentPutStep::Have { hashes }) if hashes.len() == paths.len()),
        "the workspace is asked about every file before any bytes go: {steps:?}"
    );
    let many: Vec<usize> = steps
        .iter()
        .filter_map(|step| match step {
            ContentPutStep::Many { files, .. } => Some(files.len()),
            _ => None,
        })
        .collect();
    assert_eq!(
        many.iter().sum::<usize>(),
        40,
        "the small files go many to a step"
    );
    assert!(many.len() < 40, "{many:?}");
    let begun = steps
        .iter()
        .filter(|step| matches!(step, ContentPutStep::Begin { .. }))
        .count();
    assert_eq!(begun, 1, "the large file goes in pieces, alone");

    let again = Arc::default();
    assert_eq!(
        put(&again),
        expected,
        "the same folder keeps the same references"
    );
    let steps = put_steps(&again);
    assert!(
        !steps.is_empty()
            && steps
                .iter()
                .all(|step| matches!(step, ContentPutStep::Have { .. })),
        "a folder the workspace has sends none of its bytes: {steps:?}"
    );
}

const FIRST_CONNECT_TEST: &str = "device::local::tests::a_command_right_after_the_daemon_starts_is_answered_in_a_workspace_it_opens";

/// `tilo ls` right after the daemon starts, on a computer whose person has no workspace yet: the
/// daemon opens it as the command connects, through the one way it opens it for anyone, and the
/// command is answered there.
#[test]
fn a_command_right_after_the_daemon_starts_is_answered_in_a_workspace_it_opens() {
    let Some(root) = isolated_root(FIRST_CONNECT_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let own = crate::device::first_workspace::own_workspace(
        &daemon.serving().device_cert().user_public_key,
    );
    let held = |serving: Arc<crate::device::workspaces::ServiceWorkspaces>| {
        tilo_rt::app()
            .block_on(async move { serving.keeps(own).await })
            .expect("the runtime asks")
    };
    assert!(
        !held(Arc::clone(daemon.serving())),
        "nothing founded the person's workspace before the command"
    );
    let socket = native_endpoint(&daemon, &root);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let calls = runtime
        .block_on(tilo_api::calls::Calls::over(line_client(&socket, OWN)))
        .expect("the command reaches the daemon");
    let listed = runtime
        .block_on(calls.list(None, None, None))
        .expect("the command is answered");
    assert!(
        listed.get("rows").and_then(Value::as_array).is_some(),
        "{listed}"
    );
    assert!(
        held(Arc::clone(daemon.serving())),
        "the command's connection opened the person's workspace"
    );
    assert_eq!(
        daemon.workspace().expect("the workspace"),
        own.to_string(),
        "it is the person's own, the one the daemon opens for anyone"
    );
}

const WORKSPACES_TEST: &str = "device::local::tests::the_native_endpoint_serves_every_workspace_the_person_is_in_and_no_other";

/// The provider whose one action asks its person first, as its contract says.
const ASKS: &str = "so.tilo.probe";

/// A provider offering `probe#publish`, which asks its person before it runs.
fn asking() -> Arc<tilo_action_services::TestProvider> {
    probe(tilo_actions::ConfirmationPolicy::Confirm)
}

/// A provider offering `probe#publish`, confirmed as `confirmation` says.
fn probe(
    confirmation: tilo_actions::ConfirmationPolicy,
) -> Arc<tilo_action_services::TestProvider> {
    probe_named(ASKS, confirmation)
}

/// [`probe`], offered as the provider `named`.
fn probe_named(
    named: &'static str,
    confirmation: tilo_actions::ConfirmationPolicy,
) -> Arc<tilo_action_services::TestProvider> {
    use tilo_action_services::TestProvider;
    use tilo_actions::remote::{CataloguePage, CatalogueProvider, CatalogueRow};
    Arc::new(TestProvider::new().with_catalogue(move |caller, query| {
        let row = CatalogueRow::for_action(
            named,
            named,
            tilo_actions::ActionDescriptor {
                did: None,
                state: tilo_actions::ActionState::default(),
                semantic_identity: None,
                address: tilo_actions::ActionAddress::new("probe", "publish"),
                resource_kind: "probe".into(),
                resource_label: "Probe".into(),
                action_label: "Publish".into(),
                keywords: Vec::new(),
                icon: None,
                description: None,
                locus: tilo_actions::ExecutionLocus::ClientLocal,
                kind: tilo_actions::OperationKind::Effect,
                contract: tilo_actions::ActionContract {
                    confirmation: confirmation.clone(),
                    ..tilo_actions::ActionContract::default()
                },
                input: tilo_actions::InputSpec::None,
                params: Vec::new(),
                view: None,
            },
        );
        let mut page = CataloguePage::from_rows("1".into(), vec![row], &query);
        if query.provider.is_none() {
            page.providers.push(CatalogueProvider {
                provider: named.into(),
                app_id: named.into(),
                workspace: caller.workspace().unwrap_or_default().to_owned(),
                document_id: String::new(),
                browsable: true,
            });
        }
        Ok(page)
    }))
}

/// What `pending` answers, within the hang bound.
fn answered<T: Send + 'static>(pending: tilo_actions::remote::Pending<T>) -> T {
    pending
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the daemon answered")
}

/// A person in two workspaces calls and reads in the second one through the daemon's one native
/// endpoint, each frame naming its workspace: the call their account's connection makes there
/// waits for their yes as its asked record, the connection reads that record among the second
/// workspace's invocations, and the answer is theirs alone, as in their own workspace: the
/// connection's resolve is refused, theirs through their own dispatch there answers it. A
/// workspace they are not in refuses both the read and the call.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "two workspaces, one connection and the writes each reaches, read top to bottom"
)]
fn the_native_endpoint_serves_every_workspace_the_person_is_in_and_no_other() {
    use tilo_actions::remote::{InvokeRequest, ReceiptStatus, RemoteActionTarget};
    let Some(root) = isolated_root(WORKSPACES_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let own = daemon.workspace().unwrap();
    let studio = tilo_desktop_model::WorkspaceId::new();
    let serving = Arc::clone(daemon.serving());
    tilo_rt::app()
        .block_on(async move { serving.found(studio, "Studio").await })
        .expect("the runtime runs the founding")
        .expect("the device founds Studio");
    let studio = studio.to_string();
    let probe = asking();
    daemon.provide(ASKS, Arc::clone(&probe) as _);
    let socket = native_endpoint(&daemon, &root);
    let person = line_client(&socket, OWN);
    // Whom a client names does not count, only the workspace it names: the daemon decides who by
    // the account connecting.
    let in_studio = Caller::local_person("").in_workspace(studio.clone());
    let publish = || {
        InvokeRequest::plain(
            RemoteActionTarget {
                provider: ASKS.into(),
                address: tilo_actions::ActionAddress::new("probe", "publish"),
            },
            serde_json::Map::new(),
        )
    };

    // The person's call in the second workspace waits for their yes, nothing of it run.
    let receipt = answered(person.invoke(&in_studio, publish())).expect("the call is admitted");
    assert!(
        matches!(receipt.status, ReceiptStatus::Accepted),
        "the call waits for its person: {receipt:?}"
    );
    assert!(probe.asked().is_empty(), "nothing of it ran yet");

    // They read its asked record among the second workspace's invocations, as `tilo migrate`
    // reads the questions that wait for them.
    let steps = |workspace: &str| {
        tilo_migrate::provider::caller::Steps::new(
            Arc::clone(&person) as _,
            Caller::local_person("").in_workspace(workspace.to_owned()),
            &daemon.device_key(),
            workspace.to_owned(),
            None,
        )
    };
    let questions = tilo_rt::app()
        .block_on(steps(&studio).questions())
        .expect("the app runtime")
        .expect("the questions read");
    let question = questions
        .iter()
        .find(|question| question.action.ends_with("#publish"))
        .unwrap_or_else(|| panic!("the call's question reads there: {questions:?}"))
        .clone();
    assert_eq!(question.workspace, studio, "{question:?}");
    assert!(
        tilo_rt::app()
            .block_on(steps(&own).questions())
            .expect("the app runtime")
            .unwrap()
            .iter()
            .all(|asked| asked.workspace != studio),
        "the device's own workspace lists none of the second one's questions"
    );

    // The connection is the person's own, so it answers the question there as their shell does:
    // their yes runs the call it answers, once it is admitted again for its caller.
    let resolve = || {
        let mut approve = serde_json::Map::new();
        approve.insert("resolution".into(), serde_json::json!("approve"));
        InvokeRequest::plain(
            RemoteActionTarget {
                provider: tilo_action_services::workspace_invocations::PROVIDER.into(),
                address: tilo_actions::ActionAddress::new(
                    question.record.record.clone(),
                    tilo_actions::remote::asked::RESOLVE,
                ),
            },
            approve,
        )
    };
    let resolved = answered(person.invoke(&in_studio, resolve())).expect("the answer is admitted");
    assert!(
        matches!(resolved.status, ReceiptStatus::Completed { .. }),
        "the answer goes through: {resolved:?}"
    );
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    while probe.asked().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(probe.asked().len(), 1, "their yes runs the call it answers");
    assert!(
        tilo_rt::app()
            .block_on(steps(&studio).questions())
            .expect("the app runtime")
            .unwrap()
            .iter()
            .all(|asked| asked.record.record != question.record.record),
        "the answered question waits no more"
    );

    // A workspace the person is not in refuses their read and their call alike.
    let elsewhere = tilo_desktop_model::WorkspaceId::new().to_string();
    let refused = tilo_rt::app()
        .block_on(steps(&elsewhere).questions())
        .expect("the app runtime")
        .expect_err("a workspace they are not in reads nothing");
    assert!(
        refused
            .to_string()
            .contains(tilo_actions::remote::RESOURCE_READ_REFUSED),
        "{refused:#}"
    );
    let ran = probe.asked().len();
    let refused = answered(person.invoke(
        &Caller::local_person("").in_workspace(elsewhere.clone()),
        publish(),
    ));
    let refusal = match refused {
        Err(error)
        | Ok(tilo_actions::remote::InvocationReceipt {
            status: ReceiptStatus::Rejected { error },
            ..
        }) => error.to_string(),
        Ok(receipt) => panic!("a workspace they are not in runs nothing: {receipt:?}"),
    };
    assert!(
        refusal.contains(&elsewhere) && refusal.contains("does not let you act"),
        "{refusal}"
    );
    assert_eq!(probe.asked().len(), ran, "nothing ran in it");
}

const CLI_TEST: &str =
    "device::local::tests::the_clis_calls_reach_a_second_workspace_through_the_daemon_and_no_other";

/// `tilo ls -w <workspace>` and `tilo do -w <workspace>` on a computer whose person is in two
/// workspaces: the command's calls go to the daemon as every command's do, each naming the
/// workspace it was given, and the second workspace answers them as it admits the person's
/// account. A workspace the person is not in refuses the command, saying so, and runs nothing.
#[test]
fn the_clis_calls_reach_a_second_workspace_through_the_daemon_and_no_other() {
    let Some(root) = isolated_root(CLI_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let own = daemon
        .workspace()
        .expect("the device founds its person's workspace");
    let studio = tilo_desktop_model::WorkspaceId::new();
    let serving = Arc::clone(daemon.serving());
    tilo_rt::app()
        .block_on(async move { serving.found(studio, "Studio").await })
        .expect("the runtime runs the founding")
        .expect("the device founds Studio");
    let studio = studio.to_string();
    let probe = probe(tilo_actions::ConfirmationPolicy::default());
    daemon.provide(ASKS, Arc::clone(&probe) as _);
    let socket = native_endpoint(&daemon, &root);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    // The command's calls, as `tilo` opens them on this computer: over the daemon's socket.
    let calls = runtime
        .block_on(tilo_api::calls::Calls::over(line_client(&socket, OWN)))
        .unwrap();
    assert_eq!(calls.served_workspace(), Some(own.as_str()));
    let ls = |workspace: &str| {
        runtime.block_on(async {
            let workspace = calls.workspace_id(workspace).await?;
            calls.list(Some(&workspace), Some("probe"), None).await
        })
    };

    // `tilo ls probe -w <studio>`: the second workspace lists what the probe offers there, asked
    // of the probe in that workspace.
    let listed = ls(&studio).expect("the second workspace lists what it holds");
    assert!(
        listed.to_string().contains("publish"),
        "the probe's action is listed: {listed:#}"
    );
    let asked_in: Vec<Option<String>> = probe
        .callers()
        .iter()
        .map(|caller| caller.workspace().map(str::to_owned))
        .collect();
    assert!(
        asked_in.contains(&Some(studio.clone())),
        "the listing was asked in the second workspace: {asked_in:?}"
    );

    // `tilo do -w <studio> probe#publish`: the action runs there, once, and the command reads
    // its answer.
    let act = |workspace: &str| {
        runtime.block_on(async {
            let workspace = calls.workspace_id(workspace).await?;
            let started = calls
                .start(
                    Some(&workspace),
                    "probe",
                    "publish",
                    &serde_json::json!({}),
                    "probe#publish",
                )
                .await?;
            calls.follow(Some(&workspace), started).await
        })
    };
    let answer = act(&studio).expect("the action runs in the second workspace");
    assert_eq!(
        answer.get("status").and_then(Value::as_str),
        Some("ok"),
        "{answer:#}"
    );
    assert_eq!(probe.asked().len(), 1, "the action ran once");
    assert!(
        probe
            .callers()
            .last()
            .is_some_and(|caller| caller.workspace() == Some(studio.as_str())),
        "it ran in the second workspace: {:?}",
        probe.callers()
    );

    // `tilo ls probe -w <elsewhere>` and `tilo do -w <elsewhere> probe#publish`, in a workspace the
    // person is not in: each refused, saying which workspace refused it.
    let elsewhere = tilo_desktop_model::WorkspaceId::new().to_string();
    let refused_by = |said: String| {
        assert!(
            said.contains(&elsewhere) && said.contains("does not let you act"),
            "the command says the workspace refused it: {said}"
        );
    };
    let asked_before = probe.callers().len();
    refused_by(format!(
        "{:#}",
        ls(&elsewhere).expect_err("a workspace they are not in lists nothing")
    ));
    refused_by(format!(
        "{:#}",
        act(&elsewhere).expect_err("a workspace they are not in runs nothing")
    ));
    assert_eq!(
        probe.callers().len(),
        asked_before,
        "nothing reached the provider from it"
    );
    assert_eq!(probe.asked().len(), 1, "nothing ran in it");
}

const LAUNCH_TEST: &str =
    "device::local::tests::an_app_starts_in_any_workspace_the_computer_holds_and_no_other";

/// The app a launch test asks for.
const APP: &str = "so.tilo.chat";

/// The daemon's answer to `tilo-runtime --run-app <app> --workspace <workspace>`, as the person's
/// account asks it over the native endpoint at `socket`.
fn launched(socket: &Path, app: &str, workspace: &str) -> Value {
    use std::io::{BufRead as _, Write as _};
    PEER.store(OWN, Ordering::SeqCst);
    PEER_PID.store(0, Ordering::SeqCst);
    let mut stream = UnixStream::connect(socket).unwrap();
    let request = runtime_portal::native::LaunchRequest::new(app, workspace, false);
    writeln!(stream, "{}", serde_json::to_string(&request).unwrap()).unwrap();
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).unwrap();
    serde_json::from_str(&answer).unwrap()
}

/// A computer holding two workspaces of its person's starts an app in the one its daemon does
/// not show now, as `--run-app APP --workspace W` asks, and that leaves the computer's own
/// workspace where it was. A workspace the computer does not hold is refused, named.
#[test]
fn an_app_starts_in_any_workspace_the_computer_holds_and_no_other() {
    let Some(root) = isolated_root(LAUNCH_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let own = daemon.workspace().unwrap();
    let studio = tilo_desktop_model::WorkspaceId::new();
    let serving = Arc::clone(daemon.serving());
    tilo_rt::app()
        .block_on(async move { serving.found(studio, "Studio").await })
        .expect("the runtime runs the founding")
        .expect("the device founds Studio");
    let studio = studio.to_string();
    let socket = native_endpoint(&daemon, &root);
    // No app is installed here, so a launch the workspace admits ends at the installed package:
    // what it says names the app, never the workspace.
    let not_installed = |answer: &Value| {
        answer
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|error| error.contains(&format!("{APP} is not installed")))
    };

    let answer = launched(&socket, APP, &studio);
    assert!(
        not_installed(&answer),
        "the launch reaches the app in the second workspace: {answer}"
    );
    assert_eq!(
        daemon.workspace().unwrap(),
        own,
        "the computer's own workspace stays where it was"
    );
    let answer = launched(&socket, APP, &own);
    assert!(
        not_installed(&answer),
        "the launch reaches the app in the computer's own workspace: {answer}"
    );

    let elsewhere = tilo_desktop_model::WorkspaceId::new().to_string();
    let answer = launched(&socket, APP, &elsewhere);
    let refusal = answer
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        refusal.contains(&format!("holds no workspace {elsewhere}")),
        "a workspace the computer does not hold is refused, named: {answer}"
    );
}

const DESCENDANT_TEST: &str =
    "device::local::tests::a_process_an_app_started_is_judged_as_that_app_and_never_its_person";

/// A process an app started (the shell in a terminal, a helper it runs) connects to the daemon as
/// a process of the person's account, and is judged as the app it descends from: an action the
/// person may run is refused it as that app's, which holds no grant for it. The person's own
/// process beside it is answered.
#[test]
fn a_process_an_app_started_is_judged_as_that_app_and_never_its_person() {
    use std::io::BufRead as _;
    use tilo_actions::remote::{InvokeRequest, ReceiptStatus, RemoteActionTarget};
    let Some(root) = isolated_root(DESCENDANT_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let workspace = daemon.workspace().unwrap();
    let probe = probe(tilo_actions::ConfirmationPolicy::default());
    daemon.provide(ASKS, Arc::clone(&probe) as _);
    let socket = native_endpoint(&daemon, &root);
    let publish = || {
        InvokeRequest::plain(
            RemoteActionTarget {
                provider: ASKS.into(),
                address: tilo_actions::ActionAddress::new("probe", "publish"),
            },
            serde_json::Map::new(),
        )
    };
    let in_workspace = Caller::local_person("").in_workspace(workspace.clone());

    // The app the daemon started, and a helper the app started in turn.
    let mut app = std::process::Command::new("/bin/sh")
        .args(["-c", "sleep 60 & echo $!; wait"])
        .stdout(std::process::Stdio::piped())
        .spawn_locked()
        .unwrap();
    let mut said = String::new();
    BufReader::new(app.stdout.take().unwrap())
        .read_line(&mut said)
        .unwrap();
    let helper: u32 = said.trim().parse().unwrap();
    daemon.apps().record_started(app.id(), APP, &workspace);
    assert_eq!(
        daemon.apps().attested(helper),
        Some((APP.to_owned(), workspace)),
        "the helper runs as the app it descends from"
    );

    // The person's own process runs the action.
    let person = line_client(&socket, OWN);
    let receipt = answered(person.invoke(&in_workspace, publish())).expect("the person's call");
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "the person's call runs: {receipt:?}"
    );
    assert_eq!(probe.asked().len(), 1);

    // The helper's call is the app's, which holds no grant for it: nothing runs.
    PEER.store(OWN, Ordering::SeqCst);
    PEER_PID.store(helper, Ordering::SeqCst);
    let stream = UnixStream::connect(&socket).unwrap();
    let helpers = tilo_action_services::LineClient::of_stream(stream).unwrap();
    let refusal = match answered(helpers.invoke(&in_workspace, publish())) {
        Err(error)
        | Ok(tilo_actions::remote::InvocationReceipt {
            status: ReceiptStatus::Rejected { error },
            ..
        }) => error,
        Ok(receipt) => panic!("the app's helper acts on its person's authority: {receipt:?}"),
    };
    assert!(
        matches!(refusal, tilo_actions::remote::ActionCallError::NotPermitted),
        "the helper is refused as the app it descends from, which holds no grant: {refusal}"
    );
    assert_eq!(probe.asked().len(), 1, "nothing ran for the helper");

    app.kill().unwrap();
    app.wait().unwrap();
    // The helper outlives the shell that started it; it is ended by its pid.
    let _ = std::process::Command::new("kill")
        .arg(helper.to_string())
        .status();
}

const SHELL_TEST: &str = "device::local::tests::a_shell_the_runtime_tells_of_acts_as_terminal_and_a_forged_record_is_refused";

/// What the daemon answers the shell runtime's record `record`, told on a connection from `pid`
/// of the person's account: the answer's line, `Null` when it is none.
fn told(socket: &Path, pid: u32, record: &runtime_portal::native::ShellRecord) -> Value {
    use std::io::{BufRead as _, Write as _};
    PEER.store(OWN, Ordering::SeqCst);
    PEER_PID.store(pid, Ordering::SeqCst);
    let mut stream = UnixStream::connect(socket).unwrap();
    stream.write_all(&record.line().unwrap()).unwrap();
    let mut answer = String::new();
    let _ = BufReader::new(stream).read_line(&mut answer);
    serde_json::from_str(answer.trim()).unwrap_or(Value::Null)
}

/// The shell runtime tells the daemon of a shell it started for Terminal, as `ush` does for every
/// shell: the shell, and a program typed in it, are judged as Terminal in the workspace from then
/// on. An action the person runs is refused them as Terminal's, which holds no grant for it, and
/// so is what the person's seat keeps (the account this computer is signed in to). A record whose
/// start time is not the shell's, one told by a process that did not start it, and one told by a
/// process in the shell are refused. The person's own programs, the runtime's among them, stay
/// the person's.
#[cfg(target_os = "linux")]
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the record, its forgeries and what each process reaches, read top to bottom"
)]
fn a_shell_the_runtime_tells_of_acts_as_terminal_and_a_forged_record_is_refused() {
    use runtime_portal::native::ShellRecord;
    use std::io::BufRead as _;
    use tilo_actions::remote::{
        ActionCallError, InvocationReceipt, InvokeRequest, ReceiptStatus, RemoteActionTarget,
        SERVER_ACCOUNT_PROVIDER,
    };
    const TERMINAL: &str = tilo_app_id::apps::TERMINAL;
    let Some(root) = isolated_root(SHELL_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let workspace = daemon.workspace().unwrap();
    let probe = probe(tilo_actions::ConfirmationPolicy::default());
    daemon.provide(ASKS, Arc::clone(&probe) as _);
    let account = probe_named(
        SERVER_ACCOUNT_PROVIDER,
        tilo_actions::ConfirmationPolicy::default(),
    );
    daemon.provide(SERVER_ACCOUNT_PROVIDER, Arc::clone(&account) as _);
    let socket = native_endpoint(&daemon, &root);
    let publish = |provider: &str| {
        InvokeRequest::plain(
            RemoteActionTarget {
                provider: provider.into(),
                address: tilo_actions::ActionAddress::new("probe", "publish"),
            },
            serde_json::Map::new(),
        )
    };
    let in_workspace = Caller::local_person("").in_workspace(workspace.clone());
    let from = |pid: u32| {
        PEER.store(OWN, Ordering::SeqCst);
        PEER_PID.store(pid, Ordering::SeqCst);
        tilo_action_services::LineClient::of_stream(UnixStream::connect(&socket).unwrap()).unwrap()
    };
    let refusal = |answer: Result<InvocationReceipt, ActionCallError>| match answer {
        Err(error)
        | Ok(InvocationReceipt {
            status: ReceiptStatus::Rejected { error },
            ..
        }) => error,
        Ok(receipt) => panic!("a program in the shell acts on its person's authority: {receipt:?}"),
    };

    // This test stands in for the runtime: the shell is its child, and a program typed in the
    // shell its grandchild.
    let runtime = std::process::id();
    let mut shell = std::process::Command::new("/bin/sh")
        .args(["-c", "sleep 60 & echo $!; wait"])
        .stdout(std::process::Stdio::piped())
        .spawn_locked()
        .unwrap();
    let mut said = String::new();
    BufReader::new(shell.stdout.take().unwrap())
        .read_line(&mut said)
        .unwrap();
    let typed: u32 = said.trim().parse().unwrap();
    let started = crate::apps::process_start_time(shell.id()).unwrap();

    // A record whose start time is not the shell's is refused: a process that took its pid since
    // is never the shell.
    let forged = told(
        &socket,
        runtime,
        &ShellRecord::new(shell.id(), started + 1, TERMINAL, ""),
    );
    assert!(
        forged.get("error").is_some(),
        "a record with another start time is refused: {forged}"
    );
    // A process that did not start the shell tells of nothing.
    let mut beside = a_process();
    let not_its = told(
        &socket,
        beside.id(),
        &ShellRecord::new(shell.id(), started, TERMINAL, ""),
    );
    assert!(
        not_its.get("error").is_some(),
        "a record of a process the teller did not start is refused: {not_its}"
    );
    assert_eq!(
        daemon.apps().attested(typed),
        None,
        "a refused record judges nothing"
    );

    // The runtime tells of its shell, in the primary workspace, as it does when it starts one.
    PEER.store(OWN, Ordering::SeqCst);
    PEER_PID.store(runtime, Ordering::SeqCst);
    assert_eq!(
        ush::shell_record::record_at(&socket, shell.id(), TERMINAL, ""),
        Ok(ush::shell_record::Recorded::In(workspace.clone())),
        "the runtime's primary workspace is the daemon's own"
    );
    let as_terminal = Some((TERMINAL.to_owned(), workspace.clone()));
    assert_eq!(daemon.apps().attested(shell.id()), as_terminal);
    assert_eq!(
        daemon.apps().attested(typed),
        as_terminal,
        "a program typed in the shell acts as Terminal"
    );
    assert_eq!(
        daemon.apps().process_of(typed),
        Some(crate::apps::ProcessOf::Started {
            app: TERMINAL.to_owned(),
            workspace,
            own: false,
        }),
        "with none of the standing of a process the daemon started for Terminal itself"
    );
    assert_eq!(
        daemon.apps().attested(runtime),
        None,
        "the runtime that told of it stays the person's"
    );

    // The person's own program runs the action.
    let person = from(beside.id());
    let receipt = answered(person.invoke(&in_workspace, publish(ASKS))).expect("the person's call");
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "the person's call runs: {receipt:?}"
    );
    assert_eq!(probe.asked().len(), 1);

    // The program typed in the shell is Terminal, which holds no grant for it: nothing runs.
    let in_shell = from(typed);
    assert!(
        matches!(
            refusal(answered(in_shell.invoke(&in_workspace, publish(ASKS)))),
            ActionCallError::NotPermitted
        ),
        "a program in the shell is refused as Terminal"
    );
    assert_eq!(
        probe.asked().len(),
        1,
        "nothing ran for the program in the shell"
    );
    // Nor does it reach the account this computer is signed in to.
    let _ = refusal(answered(
        in_shell.invoke(&in_workspace, publish(SERVER_ACCOUNT_PROVIDER)),
    ));
    assert!(
        account.asked().is_empty(),
        "the seat's account is not reached from the shell"
    );

    // A process in the shell cannot tell of another app's shell below it: it is Terminal's.
    let typed_started = crate::apps::process_start_time(typed).unwrap();
    let from_the_shell = told(
        &socket,
        shell.id(),
        &ShellRecord::new(typed, typed_started, "so.tilo.files", ""),
    );
    assert!(
        from_the_shell.get("workspace").is_none(),
        "a process in the shell tells of no shell: {from_the_shell}"
    );
    assert_eq!(daemon.apps().attested(typed), as_terminal);

    for process in [&mut shell, &mut beside] {
        process.kill().unwrap();
        process.wait().unwrap();
    }
    // The program outlives the shell that started it; it is ended by its pid.
    let _ = std::process::Command::new("kill")
        .arg(typed.to_string())
        .status();
}

const UNSEEN_TEST: &str = "device::local::tests::a_shells_endpoint_acts_as_terminal_where_the_daemon_sees_no_process_of_the_person";

/// On Tilo OS the daemon runs as an account of its own and sees none of the person's processes,
/// so it cannot judge a program by the shell above it. The shell runtime's record of a shell is
/// answered as unseen and judges nothing; the shell's own endpoint, which the runtime serves it in
/// place of the daemon's, carries each connection over one the daemon made for Terminal: a call
/// there is refused as Terminal's, the seat's account included, while the person's own program
/// runs it. A connection judged as an app asks for no such connection.
#[cfg(target_os = "linux")]
#[test]
fn a_shells_endpoint_acts_as_terminal_where_the_daemon_sees_no_process_of_the_person() {
    use runtime_portal::native::{ShellLine, ShellRecord};
    use tilo_actions::remote::{
        ActionCallError, InvocationReceipt, InvokeRequest, ReceiptStatus, RemoteActionTarget,
        SERVER_ACCOUNT_PROVIDER,
    };
    const TERMINAL: &str = tilo_app_id::apps::TERMINAL;
    /// A process no daemon sees: above the kernel's largest pid.
    const UNSEEN: u32 = 1 << 30;
    let Some(root) = isolated_root(UNSEEN_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let workspace = daemon.workspace().unwrap();
    let probe = probe(tilo_actions::ConfirmationPolicy::default());
    daemon.provide(ASKS, Arc::clone(&probe) as _);
    let account = probe_named(
        SERVER_ACCOUNT_PROVIDER,
        tilo_actions::ConfirmationPolicy::default(),
    );
    daemon.provide(SERVER_ACCOUNT_PROVIDER, Arc::clone(&account) as _);
    let socket = native_endpoint(&daemon, &root);
    let publish = |provider: &str| {
        InvokeRequest::plain(
            RemoteActionTarget {
                provider: provider.into(),
                address: tilo_actions::ActionAddress::new("probe", "publish"),
            },
            serde_json::Map::new(),
        )
    };
    let in_workspace = Caller::local_person("").in_workspace(workspace.clone());
    let refusal = |answer: Result<InvocationReceipt, ActionCallError>| match answer {
        Err(error)
        | Ok(InvocationReceipt {
            status: ReceiptStatus::Rejected { error },
            ..
        }) => error,
        Ok(receipt) => panic!("a program in the shell acts on its person's authority: {receipt:?}"),
    };

    // The person's own program runs the action.
    let person = line_client(&socket, OWN);
    let receipt = answered(person.invoke(&in_workspace, publish(ASKS))).expect("the person's call");
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "the person's call runs: {receipt:?}"
    );
    assert_eq!(probe.asked().len(), 1);

    // From here on the daemon sees no process of the person's: the runtime's included.
    let mut shell = a_process();
    let started = crate::apps::process_start_time(shell.id()).unwrap();
    assert_eq!(
        told(
            &socket,
            UNSEEN,
            &ShellRecord::new(shell.id(), started, TERMINAL, "")
        ),
        serde_json::json!({ "unseen": true }),
        "a daemon that sees no process of the person's judges nothing by them"
    );
    assert_eq!(daemon.apps().attested(shell.id()), None);

    // The shell's endpoint, as the runtime serves it: a program's connection there is Terminal's.
    let endpoint = ush::shell_record::ShellEndpoint::serve(
        &root.join("shells"),
        "s",
        socket.clone(),
        (TERMINAL, ""),
    )
    .unwrap();
    PEER.store(OWN, Ordering::SeqCst);
    PEER_PID.store(UNSEEN, Ordering::SeqCst);
    let in_shell =
        tilo_action_services::LineClient::of_stream(UnixStream::connect(endpoint.path()).unwrap())
            .unwrap();
    assert!(
        matches!(
            refusal(answered(in_shell.invoke(&in_workspace, publish(ASKS)))),
            ActionCallError::NotPermitted
        ),
        "a program in the shell is refused as Terminal"
    );
    let _ = refusal(answered(
        in_shell.invoke(&in_workspace, publish(SERVER_ACCOUNT_PROVIDER)),
    ));
    assert_eq!(
        probe.asked().len(),
        1,
        "nothing ran for the program in the shell"
    );
    assert!(
        account.asked().is_empty(),
        "the seat's account is not reached from the shell"
    );

    // A connection the daemon judges as an app asks for no connection of a shell.
    daemon
        .apps()
        .record_started(shell.id(), TERMINAL, &workspace);
    let from_an_app = told_line(&socket, shell.id(), &ShellLine::new(TERMINAL, ""));
    assert!(
        from_an_app.get("workspace").is_none(),
        "an app asks for no connection of a shell: {from_an_app}"
    );

    drop(endpoint);
    shell.kill().unwrap();
    shell.wait().unwrap();
}

/// What the daemon answers `ask`, made on a connection from `pid` of the person's account: the
/// answer's line, `Null` when it is none.
fn told_line(socket: &Path, pid: u32, ask: &runtime_portal::native::ShellLine) -> Value {
    use std::io::{BufRead as _, Write as _};
    PEER.store(OWN, Ordering::SeqCst);
    PEER_PID.store(pid, Ordering::SeqCst);
    let mut stream = UnixStream::connect(socket).unwrap();
    stream.write_all(&ask.line().unwrap()).unwrap();
    let mut answer = String::new();
    let _ = BufReader::new(stream).read_line(&mut answer);
    serde_json::from_str(answer.trim()).unwrap_or(Value::Null)
}

const OWN_LINE_TEST: &str =
    "device::local::tests::an_apps_own_process_keeps_reads_back_and_is_keyed_on_its_daemon_line";

/// A process standing in for one the daemon started, alive until it is ended.
fn a_process() -> std::process::Child {
    std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn_locked()
        .unwrap()
}

/// `client`'s call of `target` with `argument`: the value answered, or the reason refused.
fn called(
    client: &tilo_action_services::LineClient,
    target: tilo_actions::remote::RemoteActionTarget,
    argument: Value,
) -> Result<Value, String> {
    use tilo_actions::remote::{ActionCallError, InvokeRequest, ReceiptStatus};
    let argument = match argument {
        Value::Object(fields) => fields,
        _ => serde_json::Map::new(),
    };
    let answer = client
        .invoke(
            &Caller::local_person(""),
            InvokeRequest::plain(target, argument),
        )
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the daemon answers on the line");
    match answer {
        Err(ActionCallError::Refused { reason }) => Err(reason),
        Err(other) => Err(other.to_string()),
        Ok(receipt) => match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
            ReceiptStatus::Rejected {
                error: ActionCallError::Refused { reason },
            } => Err(reason),
            other => Err(format!("{other:?}")),
        },
    }
}

/// The line the daemon makes for the process `pid` of `app` in `workspace`, as a client.
fn own_line(
    daemon: &Arc<Daemon>,
    (pid, app, workspace): (u32, &str, &str),
) -> tilo_action_services::LineClient {
    let line = crate::daemon::own_lines(daemon)(pid, app, workspace).unwrap();
    tilo_action_services::LineClient::of_stream(UnixStream::from(line)).unwrap()
}

/// The tilolang process the daemon started to run an app's own instances is answered on the line
/// the daemon handed it, as that app's own connection, and as nothing else: it keeps its secret
/// and reads it back with nobody asked, is answered one key per purpose, the same on every ask,
/// and is refused another app's item. A task's host of the same app is refused all of it, and
/// a line whose process ended speaks for nobody.
#[test]
fn an_apps_own_process_keeps_reads_back_and_is_keyed_on_its_daemon_line() {
    use tilo_workspace_records::vault as workspace_vault;
    const BRIDGE: &str = "so.tilo.bridge";
    const READER: &str = "so.tilo.reader";
    const TOKEN: &str = "made-up-bridge-token-0123456789abcdef";
    let Some(root) = isolated_root(OWN_LINE_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let workspace = daemon.workspace().expect("the workspace");
    let (mut bridge, mut reader, mut task) = (a_process(), a_process(), a_process());
    daemon
        .apps()
        .record_own_tilolang(bridge.id(), BRIDGE, &workspace);
    daemon
        .apps()
        .record_own_tilolang(reader.id(), READER, &workspace);
    daemon.apps().record_started(task.id(), BRIDGE, &workspace);
    let bridges = own_line(&daemon, (bridge.id(), BRIDGE, &workspace));
    let readers = own_line(&daemon, (reader.id(), READER, &workspace));
    let tasks = own_line(&daemon, (task.id(), BRIDGE, &workspace));

    // The bridge keeps its token, and reads it back with nobody asked.
    let kept = called(
        &bridges,
        workspace_vault::target(workspace_vault::KEEP),
        serde_json::json!({"purpose": "bot-token", "value": TOKEN}),
    )
    .expect("the bridge's own process keeps its token");
    let kept: workspace_vault::Kept = serde_json::from_value(kept).unwrap();
    let released = called(
        &bridges,
        workspace_vault::ItemRef::target(&kept.reference, workspace_vault::RELEASE),
        Value::Null,
    )
    .expect("and reads it back");
    let released: workspace_vault::Released = serde_json::from_value(released).unwrap();
    assert_eq!(released.secret, TOKEN);

    // One key per purpose, the same on every ask.
    let key = |client: &tilo_action_services::LineClient, purpose: &str| {
        called(
            client,
            workspace_vault::target(workspace_vault::DEVICE_KEY),
            serde_json::json!({ "purpose": purpose }),
        )
        .map(|key| serde_json::from_value::<workspace_vault::DeviceKey>(key).unwrap())
    };
    let first = key(&bridges, "grayjay-noise").expect("the bridge's process is keyed");
    assert_eq!(key(&bridges, "grayjay-noise").unwrap(), first);
    assert_ne!(key(&bridges, "signing").unwrap(), first);
    assert_ne!(key(&readers, "grayjay-noise").unwrap(), first);

    // The reader's own process is refused the bridge's item.
    assert!(
        called(
            &readers,
            workspace_vault::ItemRef::target(&kept.reference, workspace_vault::RELEASE),
            Value::Null
        )
        .is_err_and(|refused| refused.contains(READER)),
        "another app's own process is refused the bridge's item"
    );

    // A task's host of the bridge runs code that is not the bridge's: refused all of it.
    let from_a_task = tilo_person_services::vault_release::from_a_tilolang_process(BRIDGE);
    assert_eq!(key(&tasks, "grayjay-noise"), Err(from_a_task.clone()));
    assert_eq!(
        called(
            &tasks,
            workspace_vault::ItemRef::target(&kept.reference, workspace_vault::RELEASE),
            Value::Null
        ),
        Err(from_a_task.clone())
    );
    assert_eq!(
        called(
            &tasks,
            workspace_vault::target(workspace_vault::KEEP),
            serde_json::json!({"purpose": "bot-token", "value": TOKEN}),
        ),
        Err(from_a_task)
    );

    // Once the bridge's process ended, its line speaks for nobody.
    bridge.kill().unwrap();
    bridge.wait().unwrap();
    assert!(
        key(&bridges, "grayjay-noise").is_err_and(|refused| refused.contains("no longer")),
        "a line outliving its process is refused"
    );
    for process in [&mut reader, &mut task] {
        process.kill().unwrap();
        process.wait().unwrap();
    }
}

const DEVICES_TEST: &str =
    "device::local::tests::a_daemon_founds_its_person_workspace_and_lists_itself_in_their_devices";

/// A computer whose person has no workspace yet founds theirs when its daemon first needs one,
/// and lists itself in the record of their devices in the same breath: the entry that lists this
/// device and, where it keeps their root, the mark that says so. Both are kept, the record is
/// placed, and a write of it after the founding finds it.
#[test]
fn a_daemon_founds_its_person_workspace_and_lists_itself_in_their_devices() {
    use tilo_workspace_service::workspaces::person_workspace::devices;

    let Some(root) = isolated_root(DEVICES_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let own = daemon
        .workspace()
        .expect("the device founds its person's workspace");
    let serving = Arc::clone(daemon.serving());
    let cert = serving.device_cert();
    let person = cert.user_public_key;
    let copies = serving.copies().clone();
    let keeps_root = crate::device::services::root()
        .expect("the root this device keeps reads")
        .is_some();
    let read = tilo_rt::app()
        .block_on({
            let copies = copies.clone();
            async move { devices::of(&copies, &person).await }
        })
        .expect("the runtime runs the read");
    assert!(
        read.listed(&person)
            .iter()
            .any(|listed| listed.cert.device_public_key == cert.device_public_key),
        "the device that founded {own} is listed in the record of its person's devices: {read:?}"
    );
    if keeps_root {
        assert!(
            read.root_holders(&person).contains(&cert.device_public_key),
            "the device that founded {own} and keeps its person's root marks it there: {read:?}"
        );
    }
    // The record is placed: this device's own write of it after the founding finds it.
    let kept = tilo_rt::app()
        .block_on(async move {
            devices::keep_own_fact(&copies, &cert, devices::SEEN_FACT, "0000000000000000").await
        })
        .expect("the runtime runs the write");
    assert_eq!(
        kept,
        Ok(()),
        "a later write of the record of the person's devices finds the record its founding placed"
    );
}
