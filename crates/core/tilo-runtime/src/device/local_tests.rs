use super::*;
use crate::RuntimeServices as _;
use crate::device::services::Daemon;
use crate::device::services::tests::isolated_root;
use serde_json::Value;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use tilo_actions::remote::{CatalogueQuery, RemoteActionMessage};

/// The account the daemon runs as in these tests: the person's own.
const OWN: u32 = 4242;
/// Another account of the same computer, signed in as nobody the daemon serves.
const STRANGER: u32 = 4243;

/// The account a test's next connection comes from, as the daemon reads it.
static PEER: AtomicU32 = AtomicU32::new(OWN);

#[expect(
    clippy::unnecessary_wraps,
    reason = "it stands in for the kernel's answer, which can fail"
)]
fn test_peer(_: &UnixStream) -> io::Result<crate::seats::Peer> {
    Ok(crate::seats::Peer {
        uid: PEER.load(Ordering::SeqCst),
        pid: None,
    })
}

/// `daemon`'s native endpoint, served in `root` as the daemon process serves it once the
/// person's services run, the person's own account being [`OWN`].
fn native_endpoint(daemon: &Arc<Daemon>, root: &Path) -> PathBuf {
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
fn line_client(socket: &Path, uid: u32) -> Arc<tilo_actions::remote::LineClient> {
    PEER.store(uid, Ordering::SeqCst);
    let stream = UnixStream::connect(socket).unwrap();
    let client = Arc::new(tilo_actions::remote::LineClient::of_stream(stream).unwrap());
    // The daemon reads the account as it takes the connection: a catalogue answered says it did.
    let _ = listed_to(&client);
    client
}

/// The providers the catalogue lists to `client`.
fn listed_to(client: &tilo_actions::remote::LineClient) -> Vec<String> {
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
    // The person's workspace is founded the first time it is asked for: before a command reaches
    // it, as `tilo login` and `tilo status` have by then.
    let workspace = daemon.workspace().expect("the workspace");
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
/// daemon's native endpoint, as the agent of the person whose account connects, waits for the
/// person's yes, then reaches the provider the daemon registered on
/// `devices/<this device>/migrate`, and its answer comes back on that invocation.
#[test]
fn a_commands_migration_steps_reach_the_devices_own_migrations() {
    let Some(root) = isolated_root(MIGRATE_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    // The person's workspace is founded before a command reaches it, as `tilo login` and
    // `tilo status` have by then.
    let workspace = daemon
        .workspace()
        .expect("the device founds its person's workspace");
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
    // The connection is an agent of the person's: its step waits for their yes, which they give
    // through their own dispatch, as their shell does.
    let questions = tilo_migrate::provider::caller::Steps::new(
        Arc::clone(&person) as _,
        Caller::local_person("").in_workspace(workspace.clone()),
        &daemon.device_key(),
        workspace.clone(),
        None,
    )
    .questions()
    .expect("the questions read");
    let question = questions
        .iter()
        .find(|question| question.action.ends_with("#sources"))
        .unwrap_or_else(|| panic!("the step's question reads: {questions:?}"));
    let mut approve = serde_json::Map::new();
    approve.insert("resolution".into(), serde_json::json!("approve"));
    let resolve = tilo_actions::remote::InvokeRequest::plain(
        tilo_actions::remote::RemoteActionTarget {
            provider: tilo_actions::remote::workspace_invocations::PROVIDER.into(),
            address: tilo_actions::ActionAddress::new(
                question.record.record.clone(),
                tilo_actions::remote::asked::RESOLVE,
            ),
        },
        approve,
    );
    let (theirs, as_them) = daemon
        .person_tree_in(&workspace)
        .expect("the person's own dispatch serves their workspace");
    let allowed = theirs
        .invoke(&as_them, resolve)
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the daemon answered")
        .expect("the person's yes is admitted");
    assert!(
        matches!(
            allowed.status,
            tilo_actions::remote::ReceiptStatus::Completed { .. }
        ),
        "the person allows the step: {allowed:?}"
    );
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
) -> Arc<tilo_actions::remote::LineClient> {
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
    Arc::new(tilo_actions::remote::LineClient::of_stream(ours).unwrap())
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
            ContentPutStep::Many { files } => Some(files.len()),
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

const WORKSPACES_TEST: &str =
    "device::local::tests::the_native_endpoint_serves_every_workspace_the_person_is_in_and_no_other";

/// The provider whose one action asks its person first, as its contract says.
const ASKS: &str = "so.tilo.probe";

/// A provider offering `probe#publish`, which asks its person before it runs.
fn asking() -> Arc<tilo_actions::remote::TestProvider> {
    probe(tilo_actions::ConfirmationPolicy::Confirm)
}

/// A provider offering `probe#publish`, confirmed as `confirmation` says.
fn probe(
    confirmation: tilo_actions::ConfirmationPolicy,
) -> Arc<tilo_actions::remote::TestProvider> {
    use tilo_actions::remote::{CataloguePage, CatalogueProvider, CatalogueRow, TestProvider};
    Arc::new(TestProvider::new().with_catalogue(move |caller, query| {
        let row = CatalogueRow::for_action(
            ASKS,
            ASKS,
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
                provider: ASKS.into(),
                app_id: ASKS.into(),
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
    let questions = steps(&studio).questions().expect("the questions read");
    let question = questions
        .iter()
        .find(|question| question.action.ends_with("#publish"))
        .unwrap_or_else(|| panic!("the call's question reads there: {questions:?}"))
        .clone();
    assert_eq!(question.workspace, studio, "{question:?}");
    assert!(
        steps(&own)
            .questions()
            .unwrap()
            .iter()
            .all(|asked| asked.workspace != studio),
        "the device's own workspace lists none of the second one's questions"
    );

    // The connection is an agent acting for them, which answers no question, there as in their own
    // workspace; they answer it with its record's own resolve, and it waits no more.
    let resolve = || {
        let mut approve = serde_json::Map::new();
        approve.insert("resolution".into(), serde_json::json!("approve"));
        InvokeRequest::plain(
            RemoteActionTarget {
                provider: tilo_actions::remote::workspace_invocations::PROVIDER.into(),
                address: tilo_actions::ActionAddress::new(
                    question.record.record.clone(),
                    tilo_actions::remote::asked::RESOLVE,
                ),
            },
            approve,
        )
    };
    let refusal = match answered(person.invoke(&in_studio, resolve())) {
        Err(error)
        | Ok(tilo_actions::remote::InvocationReceipt {
            status: ReceiptStatus::Rejected { error },
            ..
        }) => error.to_string(),
        Ok(receipt) => panic!("an agent answers no question: {receipt:?}"),
    };
    assert!(
        refusal.contains(tilo_actions::remote::asked::NOT_YOURS),
        "{refusal}"
    );
    assert!(probe.asked().is_empty(), "nothing of it ran on the refusal");
    let (theirs, as_them) = daemon
        .person_tree_in(&studio)
        .expect("the person's own dispatch serves the second workspace");
    let resolved = answered(theirs.invoke(&as_them, resolve())).expect("the answer is admitted");
    assert!(
        matches!(resolved.status, ReceiptStatus::Completed { .. }),
        "the answer goes through: {resolved:?}"
    );
    assert!(
        steps(&studio)
            .questions()
            .unwrap()
            .iter()
            .all(|asked| asked.record.record != question.record.record),
        "the answered question waits no more"
    );

    // A workspace the person is not in refuses their read and their call alike.
    let elsewhere = tilo_desktop_model::WorkspaceId::new().to_string();
    let refused = steps(&elsewhere)
        .questions()
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
    let done = runtime.block_on(async {
        let workspace = calls.workspace_id(&elsewhere).await?;
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
    });
    refused_by(format!(
        "{:#}",
        done.expect_err("a workspace they are not in runs nothing")
    ));
    assert_eq!(
        probe.callers().len(),
        asked_before,
        "nothing reached the provider from it"
    );
    assert!(probe.asked().is_empty(), "nothing ran in it");
}
