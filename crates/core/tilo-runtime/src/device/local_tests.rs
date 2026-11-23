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
fn test_peer(_: &UnixStream) -> io::Result<u32> {
    Ok(PEER.load(Ordering::SeqCst))
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
    assert_eq!(calls.served_workspace(), Some(daemon.workspace()));
    let listed = runtime
        .block_on(calls.list(daemon.workspace(), None, None))
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
    let workspace = daemon.workspace().to_owned();
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
    let workspace = daemon.workspace().to_owned();
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

/// An agent's `tilo mcp` session on the daemon's native endpoint: its connection, the first line
/// said, as `tilo mcp` connects.
struct Agent {
    stream: UnixStream,
    answers: std::io::Lines<BufReader<UnixStream>>,
    asked: u64,
}

impl Agent {
    /// A new agent at `socket`, and the instructions its `initialize` was answered with.
    fn connect(socket: &Path) -> (Self, String) {
        PEER.store(OWN, Ordering::SeqCst);
        let mut stream = UnixStream::connect(socket).unwrap();
        stream
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        writeln!(stream, "{}", tilo_api::daemon::AGENT_LINE).unwrap();
        let answers = BufReader::new(stream.try_clone().unwrap()).lines();
        let mut agent = Self {
            stream,
            answers,
            asked: 0,
        };
        let started = agent.ask(
            "initialize",
            &serde_json::json!({"protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "Claude Code", "version": "1"}}),
        );
        let instructions = started["instructions"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        (agent, instructions)
    }

    /// The result of `method` with `params`.
    fn ask(&mut self, method: &str, params: &Value) -> Value {
        self.asked = self.asked.wrapping_add(1);
        let id = self.asked;
        writeln!(
            self.stream,
            "{}",
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
        .unwrap();
        loop {
            let line = self.answers.next().expect("the daemon answers").unwrap();
            let answer: Value = serde_json::from_str(&line).unwrap();
            if answer["id"] == id {
                return answer
                    .get("result")
                    .cloned()
                    .unwrap_or_else(|| panic!("{method} was not answered: {answer}"));
            }
        }
    }

    /// What the tool `name` said with `arguments`, and whether it failed.
    fn call(&mut self, name: &str, arguments: &Value) -> (String, bool) {
        let result = self.ask(
            "tools/call",
            &serde_json::json!({"name": name, "arguments": arguments}),
        );
        let text = result["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        (text, result["isError"] == Value::Bool(true))
    }
}

const AGENT_TEST: &str =
    "device::local::tests::an_agent_acts_under_its_own_key_and_a_revoked_one_is_refused";

/// How the workspace's host refuses an agent from another of the person's devices: only a call
/// it verified as signed by the agent's own key, certified for that agent, is refused so.
const AN_AGENT: &str = "Agents from your other devices aren't accepted on this computer";
/// What an agent whose key its person revoked is told.
const REVOKED: &str = "the agent's person revoked its key";

/// An agent's `tilo mcp` reaches the daemon at its native endpoint and acts in the person's
/// workspace under a key of its own, which the person's root certified for that agent in that
/// workspace: not as the person, nor as the computer. Once the person revokes that key, an agent
/// that connects is told so and reaches nothing.
#[test]
fn an_agent_acts_under_its_own_key_and_a_revoked_one_is_refused() {
    let Some(root) = isolated_root(AGENT_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let socket = native_endpoint(&daemon, &root);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (person, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let state = tempfile::tempdir().unwrap();
    let studio = runtime.block_on(super::super::ticket_tests::studio(&person, state.path()));
    // This computer holds the person's root, as the one they set Tilo up on does: what makes its
    // agents keys of their own.
    let identities = tilo_identity::store::identity_dir_from_env().unwrap();
    tilo_identity::provisioning::import_single_device_root_as(
        &identities,
        "ada",
        &person,
        "Ada",
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let identity = hex::encode(person.verifying_key().to_bytes());

    // The holder of the device's one session, logged in to the person's workspace.
    let place = tilo_api::daemon::DaemonPlace::at(state.path(), state.path(), None);
    let login = studio.login.clone();
    let shared = Arc::new(super::super::Shared::new(
        super::super::LoggedIn {
            device: Ok((studio.laptop_key.clone(), login.clone())),
            own: None,
            local: None,
        },
        Ok(login),
    ));
    let holder = {
        let agents = daemon.agents().clone();
        std::thread::spawn(move || {
            super::super::serve::hold_for(
                &place,
                shared,
                super::super::serve::Holding::Linger(Duration::from_secs(1)),
                Duration::ZERO,
                &agents,
            )
            .unwrap()
        })
    };

    // The agent connects, and the person's root certifies it a key of its own, for this agent in
    // this workspace alone.
    let (mut agent, _) = Agent::connect(&socket);
    let fleets = tilo_identity::device_fleet::DeviceFleetStore::from_env().unwrap();
    let grants: Vec<_> = fleets
        .load(&identity)
        .unwrap()
        .certificates
        .into_iter()
        .filter_map(|cert| Some((cert.device_public_key, cert.capabilities.agent?)))
        .collect();
    let [(key, scope)] = grants.as_slice() else {
        panic!("the person's root certified the agent one key: {grants:?}");
    };
    let workspace = tilo_server_protocol::WorkspaceId::parse(&studio.workspace).unwrap();
    assert_eq!(scope.workspace(), Some(*workspace.as_uuid().as_bytes()));
    assert!(scope.is_agent(&tilo_actions::remote::device_agent_id(
        &person.verifying_key().to_bytes(),
        "Claude Code"
    )));

    // Its call reaches the workspace's host signed by that key: the host takes it as an agent
    // from another of the person's devices, which the person has not let in there.
    let publication = || serde_json::json!({"path": "workspace#publication"});
    let (said, failed) = agent.call("tilo_do", &publication());
    assert!(failed && said.contains(AN_AGENT), "{said}");

    // The person revokes the agent's key: an agent that connects now is told so, and reaches
    // nothing.
    fleets
        .revoke(
            &identity,
            *key,
            tilo_user_identity::RevocationReason::Retired,
            tilo_clock::unix_now_ns(),
        )
        .unwrap();
    let (mut later, said) = Agent::connect(&socket);
    assert!(said.contains(REVOKED), "{said}");
    let (said, failed) = later.call("tilo_do", &publication());
    assert!(failed && said.contains(REVOKED), "{said}");

    drop((later, agent));
    holder.join().unwrap();
    runtime.block_on(studio.server.shutdown());
}
