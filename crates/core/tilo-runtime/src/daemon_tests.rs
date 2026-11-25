use super::{Mode, parse_args};
use crate::device::services::Daemon;
use std::{ffi::OsString, io, time::Duration};
use tilo_mcp::lines::{
    Frame, MAX_REQUEST_BYTES, mcp_answer, read_frame, serve_lines, spawn_reader, write_reply,
};
use tilo_test_env::SpawnUnderEnvLock as _;

tilo_test_env::scenario! {
    fn the_daemons_sockets_activate_open_and_answer_as_requests_finish;

    phase default_admission_activates_a_live_owner_and_retries_after_its_exit;
    {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        struct Endpoint {
            active: Arc<AtomicBool>,
            stop: Arc<AtomicBool>,
            thread: Option<std::thread::JoinHandle<()>>,
        }
        impl Drop for Endpoint {
            fn drop(&mut self) {
                self.stop.store(true, Ordering::SeqCst);
                if let Some(thread) = self.thread.take() {
                    thread.join().unwrap();
                }
            }
        }
        let directory = tempfile::tempdir().unwrap();
        // The endpoint the daemon serving this profile asks: a named profile's sockets are the
        // account's own, never inside the profile.
        let socket = super::native_socket(directory.path()).unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let active = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (serving, stopping) = (active.clone(), stop.clone());
        let endpoint = Endpoint {
            active,
            stop,
            thread: Some(std::thread::spawn(move || {
                while !stopping.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream
                                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                                .unwrap();
                            let mut request = String::new();
                            BufReader::new(stream.try_clone().unwrap())
                                .read_line(&mut request)
                                .unwrap();
                            let request: serde_json::Value = serde_json::from_str(&request).unwrap();
                            assert_eq!(
                                request,
                                serde_json::json!({"native":"activate", "app_id":"so.tilo.fixture"})
                            );
                            writeln!(
                                stream,
                                "{}",
                                serde_json::json!({"activated": serving.load(Ordering::SeqCst)})
                            )
                            .unwrap();
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("activation endpoint failed: {error}"),
                    }
                }
            })),
        };
        let grace = Duration::from_millis(60);
        let first = super::default_launch_guard(directory.path(), "so.tilo.fixture", grace)
            .unwrap()
            .unwrap();
        endpoint.active.store(true, Ordering::SeqCst);
        assert!(
            super::default_launch_guard(directory.path(), "so.tilo.fixture", grace)
                .unwrap()
                .is_none(),
            "a second healthy launch attaches without replacing its owner"
        );
        endpoint.active.store(false, Ordering::SeqCst);
        let refusal =
            super::default_launch_guard(directory.path(), "so.tilo.fixture", grace).unwrap_err();
        assert_eq!(refusal.kind(), io::ErrorKind::WouldBlock);
        assert!(refusal.to_string().contains("owner was retained"));
        drop(first);
        // The stale PID left in the lock file never counts as a live owner.
        let retry = super::default_launch_guard(directory.path(), "so.tilo.fixture", grace)
            .unwrap()
            .expect("owner exit releases the kernel lease for a fresh launch");
        drop(retry);
        drop(endpoint);
    }

    phase a_result_another_process_was_handed_opens_through_the_action_provider;
    {
        let host = PageHost::default();
        host.offered
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let search = std::sync::Arc::new(tilo_os_search::OsSearch::new(
            std::sync::Arc::new(host.clone()),
            tilo_actions::remote::Caller::local_person("owner").in_workspace("workspace"),
        ));
        let runtime = tilo_rt::try_app().unwrap();
        runtime.handle().block_on(search.refresh()).unwrap();
        let entry = search.search("travel", 1)[0].id;

        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("native.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let serving = {
            let (host, search) = (host.clone(), std::sync::Arc::clone(&search));
            std::thread::spawn(move || {
                for _ in 0..2 {
                    let (stream, _) = listener.accept().unwrap();
                    let server = std::sync::Arc::new(tilo_mcp::McpServer::for_person(
                        host.clone(),
                        "workspace".into(),
                        "owner",
                    ));
                    super::serve_connection(
                        &stream.try_clone().unwrap(),
                        stream,
                        &|| Ok::<_, std::io::Error>(std::sync::Arc::clone(&server)),
                        &super::Status {
                            version: 1,
                            pid: 1,
                            workspace: "workspace".into(),
                        },
                        &std::sync::OnceLock::from(std::sync::Arc::clone(&search)),
                        None,
                        None,
                        None,
                    )
                    .unwrap();
                }
            })
        };

        // What the app a Spotlight click woke sends: the daemon opens it.
        tilo_host_search::open(&socket, &entry.to_string()).unwrap();
        let invoked = host.invoked.lock().unwrap().clone();
        assert_eq!(invoked.len(), 1);
        assert_eq!(
            invoked[0].target.address,
            tilo_actions::ActionAddress::new("page:1", "open")
        );

        // Access is revoked before the next walk: the daemon re-checks and refuses.
        host.offered
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let refused = tilo_host_search::open(&socket, &entry.to_string()).unwrap_err();
        assert!(refused.to_string().contains("NotOffered"), "{refused}");
        assert_eq!(host.invoked.lock().unwrap().len(), 1, "nothing more ran");
        serving.join().unwrap();
    }

    phase native_socket_serves_parallel_requests;
    // The daemon's native socket answers one session's MCP requests as they
    // finish, not in the order asked: a `tilo_status` and one that cancels
    // answer while two invokes of the same connection wait.
    {
        use tilo_mcp::test_hosts::{HeldHost, overlapping_lines};
        let host = HeldHost::default();
        let search = std::sync::Arc::new(tilo_os_search::OsSearch::new(
            std::sync::Arc::new(host.clone()),
            tilo_actions::remote::Caller::local_person("owner").in_workspace("workspace"),
        ));
        let (client, served) = std::os::unix::net::UnixStream::pair().unwrap();
        let serving = {
            let host = host.clone();
            std::thread::spawn(move || {
                let server = std::sync::Arc::new(tilo_mcp::McpServer::for_person(
                    host,
                    "workspace".into(),
                    "owner",
                ));
                super::serve_connection(
                    &served.try_clone().unwrap(),
                    served,
                    &|| Ok::<_, std::io::Error>(std::sync::Arc::clone(&server)),
                    &super::Status {
                        version: 1,
                        pid: 1,
                        workspace: "workspace".into(),
                    },
                    &std::sync::OnceLock::from(std::sync::Arc::clone(&search)),
                    None,
                    None,
                    None,
                )
            })
        };
        overlapping_lines(&host, &client);
        serving.join().unwrap().unwrap();
    }

    phase the_proxy_delivers_each_reply_before_the_stream_ends;
    // The `--connect` proxy hands each reply to the client as it arrives, while
    // the connection stays open: a client that waits for one answer before it
    // asks the next must not wait for a reply that is sitting in a buffer.
    {
        /// A writer that records what reached its destination: only flushed bytes.
        #[derive(Default)]
        struct Client {
            pending: Vec<u8>,
            delivered: Vec<u8>,
        }
        impl io::Write for Client {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.pending.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                self.delivered.append(&mut self.pending);
                Ok(())
            }
        }
        /// The daemon's side: one reply, then a stream that is still open.
        struct OpenStream(Vec<Vec<u8>>);
        impl io::Read for OpenStream {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let Some(chunk) = self.0.pop() else {
                    return Err(io::Error::new(io::ErrorKind::WouldBlock, "still open"));
                };
                buffer[..chunk.len()].copy_from_slice(&chunk);
                Ok(chunk.len())
            }
        }
        let mut client = Client::default();
        let reply = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n".to_vec();
        let mut daemon = OpenStream(vec![reply[16..].to_vec(), reply[..16].to_vec()]);
        let still_open = tilo_mcp::session_api::relay(&mut daemon, &mut client).unwrap_err();
        assert_eq!(still_open.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(
            client.delivered, reply,
            "the whole reply reached the client while the stream is open"
        );
    }
}

tilo_test_env::scenario! {
    #[expect(
        clippy::naive_bytecount,
        reason = "counts the newlines of one short test reply"
    )]
    fn the_stdio_host_frames_bounded_lines_and_answers_as_requests_finish;

    phase bounded_framing_recovers_after_an_oversized_line;
    {
        let mut bytes = vec![b'x'; MAX_REQUEST_BYTES + 1];
        bytes.extend_from_slice(b"\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n");
        let mut input = std::io::BufReader::new(bytes.as_slice());

        assert_eq!(read_frame(&mut input).unwrap(), Frame::TooLong);
        let Frame::Line(next) = read_frame(&mut input).unwrap() else {
            panic!("expected the complete request after the oversized line");
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&next).unwrap()["method"],
            "ping"
        );
        assert_eq!(read_frame(&mut input).unwrap(), Frame::Eof);
    }

    phase line_at_the_one_mib_limit_is_accepted;
    {
        let mut bytes = vec![b' '; MAX_REQUEST_BYTES];
        bytes.push(b'\n');
        let mut input = std::io::BufReader::new(bytes.as_slice());
        let Frame::Line(line) = read_frame(&mut input).unwrap() else {
            panic!("line at limit must be accepted");
        };
        assert_eq!(line.len(), MAX_REQUEST_BYTES);
    }

    phase framing_joins_lines_split_across_buffer_refills;
    {
        // A three-byte buffer forces every line to span several `fill_buf` calls,
        // including a refill that starts exactly at a newline.
        let bytes = b"abcdef\r\n\nxy\nlast";
        let mut input = std::io::BufReader::with_capacity(3, bytes.as_slice());
        assert_eq!(
            read_frame(&mut input).unwrap(),
            Frame::Line(b"abcdef".to_vec())
        );
        assert_eq!(read_frame(&mut input).unwrap(), Frame::Line(Vec::new()));
        assert_eq!(read_frame(&mut input).unwrap(), Frame::Line(b"xy".to_vec()));
        assert_eq!(
            read_frame(&mut input).unwrap(),
            Frame::Line(b"last".to_vec())
        );
        assert_eq!(read_frame(&mut input).unwrap(), Frame::Eof);
    }

    phase protocol_writer_emits_one_json_line_and_flushes;
    {
        #[derive(Default)]
        struct Output {
            bytes: Vec<u8>,
            flushes: usize,
        }
        impl std::io::Write for Output {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.flushes += 1;
                Ok(())
            }
        }
        let mut output = Output::default();
        write_reply(
            &mut output,
            &serde_json::json!({"jsonrpc":"2.0","id":1,"result":{}}),
        )
        .unwrap();
        assert_eq!(output.flushes, 1);
        assert_eq!(output.bytes.last(), Some(&b'\n'));
        assert_eq!(
            output.bytes.iter().filter(|byte| **byte == b'\n').count(),
            1
        );
        serde_json::from_slice::<serde_json::Value>(&output.bytes).unwrap();
    }

    phase parallel_requests_complete_out_of_order;
    // The stdio host answers one session's requests as they finish, not in
    // the order asked: a `tilo_status` and one that cancels answer while two
    // invokes wait, and every reply is one whole line.
    {
        use tilo_mcp::test_hosts::{HeldHost, overlapping_lines};
        let host = HeldHost::default();
        let server = std::sync::Arc::new(tilo_mcp::McpServer::for_person(
            host.clone(),
            "workspace".into(),
            "owner",
        ));
        let (client, end) = std::os::unix::net::UnixStream::pair().unwrap();
        let input = spawn_reader(end.try_clone().unwrap()).unwrap();
        let serving = std::thread::spawn(move || {
            let mut writer = end;
            serve_lines(
                tilo_rt::try_app().unwrap(),
                input,
                &mut writer,
                |request| mcp_answer(&server, request),
                Some(&mut || Ok(())),
            )
        });
        overlapping_lines(&host, &client);
        serving.join().unwrap().unwrap();
    }
}

tilo_test_env::scenario! {
    fn the_command_line_names_one_mode_and_reads_the_names_it_had;

    phase run_app_reads_the_workspace_flag_it_had_another_name_for;
    // A launcher outside this repository passes the flag by the name it had
    // before the workspace was called a workspace.
    {
        let args = |list: &[&str]| parse_args(list.iter().map(OsString::from));
        let workspace = "6f1b4a7e-0b2f-4d3a-9a1e-2c3d4e5f6a7b";
        let named = args(&[
            "--profile",
            "/tmp/profile",
            "--run-app",
            "so.tilo.photos",
            "--workspace",
            workspace,
        ])
        .unwrap();
        let Mode::RunApp {
            workspace: Some(id),
            ..
        } = &named.mode
        else {
            panic!("the launch was parsed as {:?}", named.mode);
        };
        assert_eq!(id, workspace);

        let old = args(&[
            "--profile",
            "/tmp/profile",
            "--run-app",
            "so.tilo.photos",
            "--workspace",
            workspace,
        ])
        .unwrap();
        assert_eq!(old.mode, named.mode);

        assert!(
            args(&[
                "--profile",
                "/tmp/profile",
                "--run-app",
                "so.tilo.photos",
                "--workspace",
                "not-a-workspace",
            ])
            .is_err()
        );

        // A program named after the app is refused: what runs is the app this computer has
        // installed, never one the command line names.
        assert!(
            args(&[
                "--profile",
                "/tmp/profile",
                "--run-app",
                "so.tilo.photos",
                "--",
                "/bin/true",
            ])
            .is_err()
        );
    }

    phase the_status_probe_reads_the_field_name_it_had;
    // The status probe answers the served workspace as `workspace` and reads
    // `workspace` as the same.
    {
        let workspace = "6f1b4a7e-0b2f-4d3a-9a1e-2c3d4e5f6a7b";
        let old: super::Status = serde_json::from_str(&format!(
            "{{\"version\":1,\"pid\":42,\"workspace\":\"{workspace}\"}}"
        ))
        .unwrap();
        assert_eq!(old.workspace, workspace);
        let new: super::Status = serde_json::from_str(&format!(
            "{{\"version\":1,\"pid\":42,\"workspace\":\"{workspace}\"}}"
        ))
        .unwrap();
        assert_eq!(new.workspace, old.workspace);
        // Launchers and scripts read the answer's `workspace`.
        assert_eq!(
            serde_json::to_value(&new).unwrap()["workspace"],
            serde_json::json!(workspace)
        );
    }

    phase the_daemon_is_plain_tilo_runtime;
    {
        let args = |list: &[&str]| parse_args(list.iter().map(OsString::from));
        let dev = args(&["--profile", "/tmp/profile", "--dev"]).unwrap();
        assert_eq!((dev.mode, dev.dev), (Mode::Daemon, true));
        assert!(!args(&["--profile", "/tmp/profile"]).unwrap().dev);
        assert_eq!(args(&[]).unwrap().mode, Mode::Daemon, "plain tilo-runtime");
        assert!(args(&["--profile", "/tmp/profile", "--connect", "--dev"]).is_err());
        assert!(args(&["--profile", "/tmp/profile", "--dev", "--dev"]).is_err());
    }

    phase connect_asks_for_a_workspace_by_its_id;
    // `--connect --workspace ID` asks the desktop's API for that workspace: it
    // is a client's request of the person, so it goes with `--connect` alone,
    // never with a standalone runtime of the client's own.
    {
        let args = |list: &[&str]| parse_args(list.iter().map(OsString::from));
        let workspace = "6f1b4a7e-0b2f-4d3a-9a1e-2c3d4e5f6a7b";
        let asked = args(&["--connect", "--workspace", workspace]).unwrap();
        assert_eq!(asked.mode, Mode::Connect);
        assert_eq!(asked.workspace.as_deref(), Some(workspace));
        assert_eq!(args(&["--connect"]).unwrap().workspace, None);
        for invalid in [
            vec!["--connect", "--workspace"],
            vec!["--connect", "--workspace", "not-a-workspace"],
            vec![
                "--connect",
                "--workspace",
                workspace,
                "--workspace",
                workspace,
            ],
            vec!["--connect", "--standalone", "--workspace", workspace],
            vec![
                "--profile",
                "/tmp/profile",
                "--connect",
                "--workspace",
                workspace,
            ],
            vec![
                "--profile",
                "/tmp/profile",
                "--workspace",
                workspace,
            ],
        ] {
            assert!(args(&invalid).is_err(), "{invalid:?}");
        }
    }
}

/// A profile's daemon is looked for as long as the daemon itself gives a caller: a probe that
/// missed in its short read is asked again, so a machine busy with other work does not make the
/// client say no daemon serves the profile while one is starting.
#[test]
fn a_probe_that_missed_is_asked_again_within_the_daemons_bound() {
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    let directory = tempfile::tempdir().unwrap();
    let socket = super::native_socket(directory.path()).unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let answering = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            // The first look is out before this daemon writes its answer, as a busy machine's
            // is: every look that reaches it from then on is answered.
            std::thread::sleep(Duration::from_millis(600));
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = String::new();
                        let _ = BufReader::new(stream.try_clone().unwrap()).read_line(&mut request);
                        let _ = writeln!(
                            stream,
                            "{}",
                            serde_json::json!({"version": 1, "pid": 4242, "workspace": ""})
                        );
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("the profile's endpoint failed: {error}"),
                }
            }
        })
    };
    let status = super::running(directory.path()).expect("the daemon answers a second look");
    assert_eq!(status.pid, 4242);
    stop.store(true, Ordering::SeqCst);
    answering.join().unwrap();
}

/// A client that names a profile chose a standalone runtime itself; one that
/// names none is routed by its login (`session_api::route`).
#[test]
fn standalone_is_the_clients_own_explicit_choice_for_connect() {
    let args = |list: &[&str]| parse_args(list.iter().map(OsString::from));
    assert!(args(&["--connect", "--standalone"]).unwrap().standalone);
    assert!(
        args(&["--profile", "/tmp/profile", "--connect"])
            .unwrap()
            .standalone,
        "a named profile is a standalone runtime's"
    );
    assert!(args(&["--profile", "/tmp/profile", "--standalone"]).is_err());
    assert!(args(&["--connect", "--standalone", "--standalone"]).is_err());
}

/// Host search asks the host the person's part admitted only while that part lives: when the
/// part ends (a restart through another server, a stop), its admission is retired and every
/// later ask would be refused, so the feed ends with it and lets the host go.
#[test]
fn host_search_ends_with_the_part_it_was_admitted_under() {
    let host = PageHost::default();
    host.offered
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let search = std::sync::Arc::new(tilo_os_search::OsSearch::new(
        std::sync::Arc::new(host.clone()),
        tilo_actions::remote::Caller::local_person("owner").in_workspace("workspace"),
    ));
    let part = std::sync::Arc::new(());
    let runtime = tilo_rt::try_app().unwrap();
    let feeding = {
        let (search, part) = (
            std::sync::Arc::clone(&search),
            std::sync::Arc::downgrade(&part),
        );
        runtime.handle().spawn(async move {
            crate::os_search::feed(&search, &part, std::time::Duration::from_millis(5)).await;
        })
    };
    runtime.handle().block_on(tilo_test_env::within(
        "the feed walks the catalogue",
        async {
            while search.search("travel", 1).is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        },
    ));
    drop(part);
    runtime
        .handle()
        .block_on(tilo_test_env::within(
            "the feed ends with its part",
            feeding,
        ))
        .unwrap();
    drop(search);
    assert_eq!(
        std::sync::Arc::strong_count(&host.offered),
        1,
        "the ended feed holds the part's host no longer"
    );
}

/// One page resource with an `open` view action, offered by publisher `7`,
/// and every invocation the host was asked to run.
#[derive(Clone, Default)]
struct PageHost {
    offered: std::sync::Arc<std::sync::atomic::AtomicBool>,
    invoked: std::sync::Arc<std::sync::Mutex<Vec<tilo_actions::remote::InvokeRequest>>>,
}

impl tilo_actions::remote::ActionProvider for PageHost {
    fn query(
        &self,
        _caller: &tilo_actions::remote::Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>,
    > {
        use tilo_actions::remote::{CatalogueProvider, CatalogueResource, CatalogueRow};
        let rows: Vec<CatalogueRow> = if self.offered.load(std::sync::atomic::Ordering::SeqCst) {
            vec![CatalogueRow {
                offered: None,
                provider: "7".into(),
                app_id: "so.tilo.notes".into(),
                parent_resource: String::new(),
                resource: CatalogueResource {
                    id: "page:1".into(),
                    kind: "notes.page".into(),
                    label: "Travel plans".into(),
                    presentation: None,
                },
                action: Some(tilo_actions::ActionDescriptor {
                    did: None,
                    state: tilo_actions::ActionState::default(),
                    semantic_identity: None,
                    address: tilo_actions::ActionAddress::new("page:1", "open"),
                    resource_kind: "notes.page".into(),
                    resource_label: "Travel plans".into(),
                    action_label: "Open note".into(),
                    keywords: Vec::new(),
                    icon: None,
                    description: None,
                    locus: tilo_actions::ExecutionLocus::ClientLocal,
                    kind: tilo_actions::OperationKind::View,
                    contract: tilo_actions::ActionContract::view(),
                    input: tilo_actions::InputSpec::None,
                    params: Vec::new(),
                    view: None,
                }),
                has_children: false,
                visibility: None,
                surface: None,
            }]
        } else {
            Vec::new()
        };
        let mut page = tilo_actions::remote::CataloguePage::from_rows("1".into(), rows, &query);
        if query.provider.is_none() {
            page.providers = vec![CatalogueProvider {
                provider: "7".into(),
                app_id: "so.tilo.notes".into(),
                workspace: "workspace".into(),
                document_id: String::new(),
                browsable: true,
            }];
        }
        tilo_actions::remote::Pending::ready(Ok(page))
    }

    fn invoke(
        &self,
        _caller: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        let invocation = request.invocation;
        self.invoked.lock().unwrap().push(request);
        tilo_actions::remote::Pending::ready(Ok(tilo_actions::remote::InvocationReceipt {
            invocation,
            status: tilo_actions::remote::ReceiptStatus::Accepted,
        }))
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
            tilo_actions::remote::ActionCallError::UnknownInvocation,
        ))
    }
}

/// The server writes the workspace and the agent key into the client's `initialize` and nothing
/// else: a probe before it and every line after it reach the API as sent.
#[test]
fn the_server_writes_the_workspace_and_key_into_the_initialize_alone() {
    let sent = concat!(
        r#"{"jsonrpc":"2.0","id":0,"method":"server/discover"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{"experimental":{"so.tilo":{"actsAs":"agent"}}},"clientInfo":{"name":"Claude Code"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"workspace":"x"}}"#,
        "\n",
    );
    let mut relayed = Vec::new();
    let asking = tilo_mcp::session_api::Asking {
        workspace: Some("w-1".into()),
        key: Some("tilo_ak_0123".into()),
        session_name: Some("Office Hours".into()),
    };
    let total =
        tilo_mcp::session_api::relay_asking(&mut sent.as_bytes(), &mut relayed, &asking).unwrap();
    assert_eq!(total, sent.len() as u64, "every byte read is relayed");
    let lines: Vec<serde_json::Value> = relayed
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["method"], "server/discover");
    assert_eq!(
        lines[1].pointer(tilo_mcp::session_api::KEY),
        Some(&serde_json::json!("tilo_ak_0123")),
        "the agent key rides in the initialize"
    );
    assert_eq!(
        lines[1].pointer(tilo_mcp::session_api::WORKSPACE),
        Some(&serde_json::json!("w-1"))
    );
    assert_eq!(
        lines[1].pointer("/params/capabilities/experimental/so.tilo/actsAs"),
        Some(&serde_json::json!("agent")),
        "what the client asked is kept"
    );
    assert_eq!(
        lines[2],
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"workspace":"x"}})
    );
    let hello =
        tilo_mcp::session_api::parse_hello(&serde_json::to_vec(&lines[1]).unwrap()).unwrap();
    assert_eq!(hello.workspace.as_deref(), Some("w-1"), "the API reads it");
    assert_eq!(
        hello.key.as_deref(),
        Some("tilo_ak_0123"),
        "the API reads the key"
    );
    assert_eq!(
        hello.request.pointer(tilo_mcp::session_api::KEY),
        None,
        "the key is kept out of the request the API goes on to answer"
    );
    assert_eq!(
        hello.session_name.as_deref(),
        Some("Office Hours"),
        "the session the client named rides in the initialize, and the API reads it"
    );
}

/// A device no person's certificate names yet answers on its native endpoint at once: its own
/// actions are answered, and one that needs a person is refused saying the device has none yet.
/// Nothing waits for a claim.
#[test]
fn an_unclaimed_device_answers_its_own_actions_at_once_and_says_it_has_no_person_yet() {
    use io::{BufRead as _, Write as _};
    let dir = tempfile::tempdir().unwrap();
    let keys = tilo_device_identity::IdentityPaths::under(dir.path().join("identity"));
    let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
    let place = tilo_device_identity::login::Place::at(dir.path().join("login"), keys);
    let native = std::sync::Arc::new(super::Native::new(
        crate::device::unclaimed::DeviceOnly::new(key.clone(), place),
        crate::seats::peer_of,
        tilo_api::daemon::admission::here(|| rustix::process::geteuid().as_raw()),
    ));
    let listener =
        tilo_files::native::private::bind_socket(&dir.path().join("native.sock")).unwrap();
    {
        let native = std::sync::Arc::clone(&native);
        std::thread::spawn(move || native.accept(&listener));
    }
    let stream = std::os::unix::net::UnixStream::connect(dir.path().join("native.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = io::BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    let mut ask = |request: serde_json::Value| {
        writeln!(writer, "{request}").unwrap();
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .expect("the device answers at once");
        serde_json::from_str::<serde_json::Value>(&line).unwrap()
    };

    let status = ask(serde_json::json!({"native": "status"}));
    assert_eq!(
        status["workspace"], "",
        "no workspace is served yet: {status}"
    );

    let info = ask(
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "tilo_do", "arguments": {"path": "devices#info"}}}),
    );
    let text = info["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert_ne!(info["result"]["isError"], true, "{info}");
    assert!(
        text.contains(&hex::encode(key.verifying_key().to_bytes())),
        "the device says what it is: {info}"
    );

    let person = ask(
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "tilo_do", "arguments": {"path": "notes#create"}}}),
    );
    let text = person["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        text.contains("this device has no person yet"),
        "what needs a person says the device has none: {person}"
    );
}

/// Before anyone logs a device in, an account on it changes its package cache over the native
/// endpoint the way it would after: it puts the host files a runtime is built from, then asks the
/// cache to build the runtime from them, and the runtime is installed. A host path that leaves
/// the root it names is refused before anything is copied.
#[cfg(target_os = "linux")]
#[test]
fn before_a_login_an_account_builds_a_runtime_from_files_it_puts() {
    use io::{BufRead as _, Write as _};
    use serde_json::json;
    let home = tilo_test_env::TestHome::new().unwrap();
    let bundles = home.data().join("bundles");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_RUNTIME_BUNDLE_DIR", &bundles);
    let dir = tempfile::tempdir().unwrap();
    let keys = tilo_device_identity::IdentityPaths::under(dir.path().join("identity"));
    let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
    let place = tilo_device_identity::login::Place::at(dir.path().join("login"), keys);
    let cache = crate::device::packages::PackageCache::new(
        &key,
        true,
        &dir.path().join("packages"),
        std::sync::Arc::new(crate::device::control::DeviceContent::at(
            &dir.path().join("content"),
        )),
    );
    let native = std::sync::Arc::new(super::Native::new(
        crate::device::unclaimed::DeviceOnly::new(key, place)
            .with_packages(std::sync::Arc::new(cache)),
        crate::seats::peer_of,
        tilo_api::daemon::admission::here(|| rustix::process::geteuid().as_raw()),
    ));
    let listener =
        tilo_files::native::private::bind_socket(&dir.path().join("native.sock")).unwrap();
    {
        let native = std::sync::Arc::clone(&native);
        std::thread::spawn(move || native.accept(&listener));
    }
    let stream = std::os::unix::net::UnixStream::connect(dir.path().join("native.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .unwrap();
    let mut reader = io::BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    let mut id = 0;
    let mut call = |tool: &str, arguments: serde_json::Value| {
        id += 1;
        let request = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": tool, "arguments": arguments}});
        writeln!(writer, "{request}").unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).expect("the device answers");
        serde_json::from_str::<serde_json::Value>(&line).unwrap()
    };

    let host = [
        "/lib64/ld-linux-x86-64.so.2",
        "/lib64/libc.so.6",
        "/lib64/libm.so.6",
        "/lib64/libdl.so.2",
        "/etc/ld.so.cache",
    ];
    let bytes = |path: &str| format!("stub: {path}\n").into_bytes();
    let put = call(
        "tilo_put_many",
        json!({"files": host.iter().map(|path| base64(&bytes(path))).collect::<Vec<_>>()}),
    );
    assert_ne!(put["result"]["isError"], true, "{put}");
    let from: Vec<serde_json::Value> = host
        .iter()
        .map(|path| {
            let source = format!("tilo.content:{}", blake3::hash(&bytes(path)).to_hex());
            assert!(put.to_string().contains(&source), "{path} is put: {put}");
            json!({"path": path, "file": {"source": source}})
        })
        .collect();

    let escaping = call(
        "tilo_do",
        json!({"path": "packages#runtime-install", "wait_ms": 60_000, "args": {
            "version": "escaped", "arch": "x86_64",
            "from": [{"path": "/lib64/../../outside", "file": from[1]["file"]}]}}),
    );
    assert!(
        escaping
            .to_string()
            .contains("a host path names only directories and a file"),
        "a host path that leaves its root is refused: {escaping}"
    );
    assert!(!bundles.join("escaped").exists(), "{escaping}");

    let installed = call(
        "tilo_do",
        json!({"path": "packages#runtime-install", "wait_ms": 60_000, "args": {
            "version": "vv", "arch": "x86_64", "from": from}}),
    );
    assert_ne!(installed["result"]["isError"], true, "{installed}");
    assert!(
        installed
            .to_string()
            .contains("installed runtime bundle: tilo-runtime/vv"),
        "the cache says what it installed: {installed}"
    );
    assert_eq!(
        std::fs::read(bundles.join("vv/lib/lib64/libc.so.6")).unwrap(),
        bytes("/lib64/libc.so.6"),
        "the runtime is built from the files put"
    );
}

/// `bytes` as standard base64, the way a client puts a file.
#[cfg(target_os = "linux")]
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let word = chunk
            .iter()
            .zip([16, 8, 0])
            .fold(0u32, |word, (byte, shift)| word | u32::from(*byte) << shift);
        for (at, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            text.push(if at <= chunk.len() {
                char::from(ALPHABET[(word >> shift & 63) as usize])
            } else {
                '='
            });
        }
    }
    text
}

/// A device nobody set up founds no person by itself: it stays its device alone until its own
/// account starts one (`devices#found`), as a laptop's first command does through the place every
/// client finds the daemon at. An account that is a stranger to this daemon may not, and a device
/// with a person starts no other.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one account founding a person, its refusals and the device it leaves alone, read top to bottom"
)]
fn a_device_founds_its_person_only_when_its_own_account_asks() {
    use io::{BufRead as _, Write as _};
    use serde_json::{Value, json};
    use std::sync::atomic::Ordering;
    const OWN: u32 = 990;
    const OTHER: u32 = 1000;

    let mut home = tilo_test_env::TestHome::new().unwrap();
    home.env()
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
    let root = home.root().to_owned();
    // This computer's one device key, which also seals the root it keeps for an account.
    let keys = tilo_api::calls::device_keys();
    let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
    let device = hex::encode(key.verifying_key().to_bytes());
    let place = tilo_device_identity::login::Place::at(root.join("login"), keys);
    let admission: &'static tilo_api::daemon::admission::Admission = Box::leak(Box::new(
        tilo_api::daemon::admission::Admission::new(OWN, false),
    ));
    let answers = crate::device::unclaimed::DeviceOnly::at(key, place, root.clone(), OWN);
    let native = std::sync::Arc::new(super::Native::new(
        answers.clone(),
        founding_peer,
        admission,
    ));
    let sockets = root.join("sockets");
    std::fs::create_dir_all(&sockets).unwrap();
    let listener =
        tilo_files::native::private::bind_socket(&sockets.join(tilo_paths::machine::DAEMON_NATIVE))
            .unwrap();
    {
        let native = std::sync::Arc::clone(&native);
        std::thread::spawn(move || native.accept(&listener));
    }
    let daemon = tilo_api::daemon::DaemonPlace::at(&sockets, &root.join("login"));
    let ask = |uid: u32, request: Value| {
        FOUNDING_PEER.store(uid, Ordering::SeqCst);
        let stream = std::os::unix::net::UnixStream::connect(daemon.native()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let mut writer = stream.try_clone().unwrap();
        writeln!(writer, "{request}").unwrap();
        let mut line = String::new();
        io::BufReader::new(stream)
            .read_line(&mut line)
            .expect("the device answers");
        serde_json::from_str::<Value>(&line).unwrap()
    };
    let found = |uid: u32| {
        let answer = ask(
            uid,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "tilo_do", "arguments": {"path": "devices#found"}}}),
        );
        let text = answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        (answer["result"]["isError"] == true, text)
    };

    let fresh = ask(OWN, json!({"native": "account#who"}));
    assert_eq!(
        fresh["person"],
        Value::Null,
        "nothing founds a person by itself: {fresh}"
    );
    assert_eq!(fresh["claimed"], false, "{fresh}");

    let (refused, why) = found(OTHER);
    assert!(refused, "another account started a person: {why}");
    assert!(why.contains("only an account signed in"), "{why}");
    assert_eq!(ask(OWN, json!({"native": "account#who"}))["claimed"], false);

    // The laptop's first command: through the place every client finds the daemon at.
    FOUNDING_PEER.store(OWN, Ordering::SeqCst);
    daemon
        .found(&tilo_api::daemon::Founding {
            name: Some("Ada".to_owned()),
            ..tilo_api::daemon::Founding::default()
        })
        .unwrap();
    // The person's certificate names the device now, so the daemon starts their services; the
    // device's own actions stand in for them here.
    let frames = crate::device::local::Frames::of_the_device(answers.clone());
    native.serve_the_person(super::Endpoint {
        session_factory: std::sync::Arc::new(move || {
            Ok(tilo_mcp::McpServer::for_agent(
                answers.clone(),
                String::new(),
                "agent",
                "",
            ))
        }),
        status: std::sync::Arc::new(|| super::Status {
            version: 1,
            pid: std::process::id(),
            workspace: "home".into(),
        }),
        search: std::sync::Arc::new(std::sync::OnceLock::new()),
        media: None,
        other_devices: None,
        agents: None,
        frames,
        to_agents: None,
        launch: None,
        connections: None,
    });
    let who = ask(OWN, json!({"native": "account#who"}));
    let person = who["person"].as_str().expect("a person now").to_owned();
    assert_eq!(who["claimed"], true, "{who}");
    assert_eq!(who["device"], device, "{who}");
    // The person is made where a session on this account reads them, named as asked and holding
    // their root here: what `tilo login --new-identity` and first-run setup ask for. The root is
    // kept for the account that asked alone.
    let identity_dir = crate::device::services::account_identity_dir(OWN).unwrap();
    let active = tilo_identity::store::resolve_active_identity(&identity_dir)
        .expect("the person's record, where the session reads it");
    let metadata = tilo_identity::store::load_metadata_from_path(&active).unwrap();
    assert_eq!(hex::encode(metadata.group_public_key), person);
    assert_eq!(metadata.display_name, "Ada");
    assert!(metadata.single_device, "this computer holds the root");

    let (refused, why) = found(OWN);
    assert!(refused, "a device with a person started another: {why}");
    assert!(
        why.contains(tilo_device_identity::login::ALREADY_A_PERSONS),
        "{why}"
    );
    let again = daemon
        .found(&tilo_api::daemon::Founding {
            name: Some("Someone else".to_owned()),
            tickets: vec![tilo_api::daemon::TicketKind::Server],
        })
        .expect("a command on a device with a person goes on as that person");
    assert!(
        again.is_none(),
        "a person made already is founded no second time, and issues no first ticket: {again:?}"
    );
    assert_eq!(
        ask(OWN, json!({"native": "account#who"}))["person"],
        person,
        "the person stays the one first started"
    );
    assert_eq!(
        tilo_identity::store::load_metadata_from_path(&active)
            .unwrap()
            .display_name,
        "Ada",
        "a second ask names nobody again"
    );
}

/// The account the founding test's next native connection comes from, apart from [`PEER`]: the
/// tests run side by side.
static FOUNDING_PEER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[expect(
    clippy::unnecessary_wraps,
    reason = "it stands in for the kernel's answer, which can fail"
)]
fn founding_peer(_: &std::os::unix::net::UnixStream) -> io::Result<crate::seats::Peer> {
    Ok(crate::seats::Peer {
        uid: FOUNDING_PEER.load(std::sync::atomic::Ordering::SeqCst),
        pid: Some(std::process::id()),
    })
}

/// The account a test's next native connection comes from, as the daemon reads it.
static PEER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[expect(
    clippy::unnecessary_wraps,
    reason = "it stands in for the kernel's answer, which can fail"
)]
fn test_peer(_: &std::os::unix::net::UnixStream) -> io::Result<crate::seats::Peer> {
    Ok(crate::seats::Peer {
        uid: PEER.load(std::sync::atomic::Ordering::SeqCst),
        pid: Some(std::process::id()),
    })
}

/// Under the system unit every account reaches the native endpoint, and the account is what it is
/// answered as: root says who signed in (and nobody else may), an account signed in is its person
/// and is served, and any other account is answered the device's own actions alone. `account#who`
/// tells each which it is.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one sign-in and sign-out, read top to bottom"
)]
fn the_system_daemon_answers_each_account_as_who_signed_in() {
    use io::{BufRead as _, Write as _};
    use serde_json::{Value, json};
    use std::sync::atomic::Ordering;
    use tilo_api::daemon::admission::{Admission, ROOT};
    const DAEMON: u32 = 990;
    const ADA: u32 = 1000;
    const BOB: u32 = 1001;

    // A device bound to Ada: her root certified its key.
    let home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let keys = tilo_device_identity::IdentityPaths::under(dir.path().join("identity"));
    keys.ensure_directories().unwrap();
    let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
    let _key_taken = tilo_device_identity::persist::take_device_key(&keys).unwrap();
    let ada = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
    let person = ada.verifying_key().to_bytes();
    let device = key.verifying_key().to_bytes();
    tilo_device_identity::persist::write_public_key(&keys.user_pubkey, &person).unwrap();
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: person,
        device_public_key: device,
        device_label: "desk".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1_000,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &ada).unwrap();
    tilo_device_identity::persist::write_device_cert(&keys.device_cert, &cert).unwrap();
    // Ada's account keeps what its identity store names, and the profile records that account as
    // the one the device was set up from: whose identity the device's own actions start as.
    let profile = home.root().to_owned();
    tilo_identity::provisioning::complete_enrolled_device(
        &crate::device::services::account_identity_dir(ADA).unwrap(),
        &keys,
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    let record = tilo_api::daemon::admission::person_record(&profile, ADA);
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    std::fs::write(
        &record,
        serde_json::json!({"person": hex::encode(person), "device": true}).to_string(),
    )
    .unwrap();

    let answers = crate::device::unclaimed::DeviceOnly::at(
        key,
        tilo_device_identity::login::Place::at(dir.path().join("login"), keys),
        profile,
        DAEMON,
    );
    // The profile records that account for the identity check; this daemon's own admission
    // has not recorded it yet, which the sign-in section below asks about.
    let admission: &'static Admission = Box::leak(Box::new(Admission::new(DAEMON, true)));
    let native = std::sync::Arc::new(super::Native::new(answers.clone(), test_peer, admission));
    // The person's services run, answering as the workspace `home`.
    let frames = crate::device::local::Frames::of_the_device(answers.clone());
    native.serve_the_person(super::Endpoint {
        session_factory: std::sync::Arc::new(move || {
            Ok(tilo_mcp::McpServer::for_agent(
                answers.clone(),
                String::new(),
                "agent",
                "",
            ))
        }),
        status: std::sync::Arc::new(|| super::Status {
            version: 1,
            pid: std::process::id(),
            workspace: "home".into(),
        }),
        search: std::sync::Arc::new(std::sync::OnceLock::new()),
        media: None,
        other_devices: None,
        agents: None,
        frames,
        to_agents: None,
        launch: None,
        connections: None,
    });
    let socket = dir.path().join("native.sock");
    let listener = tilo_files::native::private::bind_socket(&socket).unwrap();
    {
        let native = std::sync::Arc::clone(&native);
        std::thread::spawn(move || native.accept(&listener));
    }
    // One request on a connection of `uid`'s, and its answer.
    let ask = |uid: u32, request: Value| {
        PEER.store(uid, Ordering::SeqCst);
        let stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut writer = stream.try_clone().unwrap();
        writeln!(writer, "{request}").unwrap();
        let mut line = String::new();
        io::BufReader::new(stream)
            .read_line(&mut line)
            .expect("the daemon answers");
        serde_json::from_str::<Value>(&line).unwrap()
    };
    let who = |uid: u32| ask(uid, json!({"native": "account#who"}));
    // A seat action called as any action is: whether it failed, and what it answered.
    let act = |uid: u32, path: &str, args: Value| {
        let answer = ask(
            uid,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "tilo_do", "arguments": {"path": path, "args": args}}}),
        );
        let text = answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        (answer["result"]["isError"] == true, text)
    };
    let status = |uid: u32| ask(uid, json!({"native": "status"}));
    // A seat action whose answer may carry a descriptor: its text, and the descriptor.
    let act_handed = |uid: u32, path: &str, args: Value| {
        PEER.store(uid, Ordering::SeqCst);
        let stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "tilo_do", "arguments": {"path": path, "args": args}}});
        writeln!(&stream, "{request}").unwrap();
        let (mut line, fd) = runtime_portal::fd_transport::recv_with_fd(&stream).unwrap();
        while line.last() != Some(&b'\n') {
            let mut rest = Vec::new();
            io::BufReader::new(&stream)
                .read_until(b'\n', &mut rest)
                .unwrap();
            line.extend(rest);
        }
        let answer: Value = serde_json::from_slice(&line).unwrap();
        assert_ne!(answer["result"]["isError"], true, "{answer}");
        let text = answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        (text, fd)
    };

    // Before anyone signed in, Ada's account is a stranger: no person, and the device alone.
    let unknown = who(ADA);
    assert_eq!(unknown["person"], Value::Null, "{unknown}");
    assert_eq!(unknown["device"], hex::encode(device), "{unknown}");
    assert_eq!(unknown["claimed"], true, "{unknown}");
    assert_eq!(
        status(ADA)["workspace"],
        "",
        "a stranger is not served the person"
    );

    // An account that only names the device's person is not them: the daemon has no record of
    // it as theirs.
    let signing_in = json!({"person": hex::encode(person), "uid": ADA});
    let (failed, why) = act(ROOT, "seats#signed-in", signing_in.clone());
    assert!(failed && why.contains("set up from"), "{why}");
    // Ada's account set the person up.
    admission.founded_by(ADA, person).unwrap();
    // Only root says who signed in: neither Ada herself nor the daemon's own account may.
    for by in [ADA, DAEMON] {
        let (failed, why) = act(by, "seats#signed-in", signing_in.clone());
        assert!(failed, "{by} said who signed in: {why}");
    }
    // Bob naming her says nothing about him.
    let (failed, why) = act(
        ROOT,
        "seats#signed-in",
        json!({"person": hex::encode(person), "uid": BOB}),
    );
    assert!(failed, "{why}");
    // A person who arrives has a workspace found for them, unless their session hands over the
    // ones their desktop kept, and root is handed the link her shell is admitted by, with its
    // answer, for the session it starts.
    let shells = std::sync::Arc::new(Linked::default());
    shells.person(person);
    native.seats.serve(std::sync::Arc::clone(&shells) as _);
    // The session her sign-in started redeems the answer's one-use token for her link, in her
    // own account.
    let redeem = |token: String| {
        PEER.store(ADA, Ordering::SeqCst);
        let stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(
            &stream,
            "{}",
            json!({"native": "shells#link", "token": token})
        )
        .unwrap();
        let (line, fd) = runtime_portal::fd_transport::recv_with_fd(&stream).unwrap();
        let answer: Value = serde_json::from_slice(&line).unwrap();
        assert!(answer["error"].is_null(), "{answer}");
        fd.expect("the link is handed with its answer")
    };
    let token_of = |text: &str| {
        // `tilo_do` says `ok` above the action's own answer.
        let text = text.strip_prefix("ok\n").unwrap_or(text);
        let answer: Value = serde_json::from_str(text).unwrap();
        answer["token"].as_str().expect("a token").to_owned()
    };
    let mut importing = signing_in.clone();
    importing["importing"] = json!(true);
    let (signed_in, _) = act_handed(ROOT, "seats#signed-in", importing);
    assert!(signed_in.contains(&hex::encode(person)), "{signed_in}");
    assert_eq!(
        shells.arrivals.load(Ordering::SeqCst),
        0,
        "the desktop's workspace is hers"
    );
    shells.reaches(redeem(token_of(&signed_in)));
    let (signed_in, _) = act_handed(ROOT, "seats#signed-in", signing_in);
    assert!(signed_in.contains(&hex::encode(person)), "{signed_in}");
    assert_eq!(
        shells.arrivals.load(Ordering::SeqCst),
        1,
        "she is found a workspace"
    );
    shells.reaches(redeem(token_of(&signed_in)));
    // A link is only asked for by the seat it was admitted for: another account reaches none.
    let refused = ask(BOB, json!({"native": "shells#link"}));
    assert!(refused["error"].is_string(), "{refused}");
    // A daemon of the system's starts no shell itself: root starts each with the link it got.
    let started = ask(
        ADA,
        json!({"native": "shells#start", "args": ["-c", "exit 0"], "env": {}}),
    );
    assert!(started["error"].is_string(), "{started}");

    // Ada is her person now, and served; Bob still a stranger; root the device, no person.
    assert_eq!(who(ADA)["person"], hex::encode(person));
    assert_eq!(
        status(ADA)["workspace"],
        "home",
        "a signed-in account is served"
    );
    assert_eq!(who(BOB)["person"], Value::Null);
    assert_eq!(status(BOB)["workspace"], "");
    assert_eq!(who(ROOT)["person"], Value::Null);

    // She leaves: only root says so. Her person's root is opened only while she is signed in, so
    // her account is served no more; it stays the account that set the device's person up.
    let leaving = json!({"uid": ADA});
    assert!(act(ADA, "seats#left", leaving.clone()).0);
    let (failed, left) = act(ROOT, "seats#left", leaving);
    assert!(!failed && left.contains("true"), "{left}");
    assert_eq!(who(ADA)["person"], Value::Null);
    assert_eq!(
        status(ADA)["workspace"],
        "",
        "an account that left is served"
    );
    assert_eq!(
        admission.recorded_person(ADA),
        Some(tilo_api::daemon::admission::AccountPerson {
            person,
            device: true
        })
    );
    assert_eq!(who(BOB)["person"], Value::Null);
}

/// Two people signed in on one computer each act as themselves: the device's own person as
/// before, and another person as a key of theirs this daemon made at their sign-in, once their
/// root certified it. Neither is served as the other, an account that holds no certificate for
/// its key is served the device's own actions alone, and a key stops acting when its account
/// left.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "two sign-ins, a certificate and a sign-out, read top to bottom"
)]
fn a_second_person_on_the_computer_acts_as_their_own_certified_key() {
    use io::{BufRead as _, Write as _};
    use serde_json::{Value, json};
    use std::sync::atomic::Ordering;
    use tilo_api::daemon::admission::{Admission, ROOT};
    const DAEMON: u32 = 990;
    const ADA: u32 = 1000;
    const BO: u32 = 1001;

    // A device bound to Ada: her root certified its key. Bo is another person, with a root of
    // their own.
    let dir = tempfile::tempdir().unwrap();
    let keys = tilo_device_identity::IdentityPaths::under(dir.path().join("identity"));
    keys.ensure_directories().unwrap();
    let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
    let ada = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
    let bo = ed25519_dalek::SigningKey::from_bytes(&[11; 32]);
    let device = key.verifying_key().to_bytes();
    let certified = |root: &ed25519_dalek::SigningKey, key: [u8; 32]| {
        let now = tilo_clock::unix_now_ns();
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key,
            device_label: "desk".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: now.saturating_sub(60_000_000_000),
            expires_at_ns: Some(now.saturating_add(3_600_000_000_000)),
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
        cert
    };
    let person = |root: &ed25519_dalek::SigningKey| hex::encode(root.verifying_key().to_bytes());
    tilo_device_identity::persist::write_public_key(
        &keys.user_pubkey,
        &ada.verifying_key().to_bytes(),
    )
    .unwrap();
    tilo_device_identity::persist::write_device_cert(&keys.device_cert, &certified(&ada, device))
        .unwrap();

    let answers = crate::device::unclaimed::DeviceOnly::new(
        key,
        tilo_device_identity::login::Place::at(dir.path().join("login"), keys),
    );
    let admission: &'static Admission = Box::leak(Box::new(Admission::new(DAEMON, true)));
    // Ada's account set the device's person up.
    admission
        .founded_by(ADA, ada.verifying_key().to_bytes())
        .unwrap();
    let native = std::sync::Arc::new(super::Native::new(answers.clone(), test_peer, admission));
    // The device's person's services run, answering as the workspace `home`.
    let own = answers.clone();
    let frames = crate::device::local::Frames::of_the_device(answers.clone());
    native.serve_the_person(super::Endpoint {
        session_factory: std::sync::Arc::new(move || {
            Ok(tilo_mcp::McpServer::for_agent(
                answers.clone(),
                String::new(),
                "agent",
                "",
            ))
        }),
        status: std::sync::Arc::new(|| super::Status {
            version: 1,
            pid: std::process::id(),
            workspace: "home".into(),
        }),
        search: std::sync::Arc::new(std::sync::OnceLock::new()),
        media: None,
        other_devices: None,
        agents: None,
        frames,
        to_agents: None,
        launch: None,
        connections: None,
    });
    // Another person's connection is served by an endpoint of the key it acts as, which says
    // that key and its person as the workspace it serves.
    native.serve_keys(Box::new(
        move |stream: std::os::unix::net::UnixStream,
              slot: super::ConnectionSlot,
              seated: crate::seats::Seated,
              key: ed25519_dalek::SigningKey,
              cert: tilo_user_identity::DeviceCert| {
            assert_eq!(key.verifying_key().to_bytes(), cert.device_public_key);
            let answers = own.seated(seated.clone());
            let acting = format!(
                "{} as {}",
                hex::encode(cert.user_public_key),
                hex::encode(cert.device_public_key)
            );
            super::Endpoint {
                session_factory: {
                    let answers = answers.clone();
                    std::sync::Arc::new(move || {
                        Ok(tilo_mcp::McpServer::for_agent(
                            answers.clone(),
                            String::new(),
                            "agent",
                            "",
                        ))
                    })
                },
                status: std::sync::Arc::new(move || super::Status {
                    version: 1,
                    pid: std::process::id(),
                    workspace: acting.clone(),
                }),
                search: std::sync::Arc::new(std::sync::OnceLock::new()),
                media: None,
                other_devices: None,
                agents: None,
                frames: crate::device::local::Frames::of_the_device(answers),
                to_agents: None,
                launch: None,
                connections: None,
            }
            .take(stream, slot, Some(seated))
        },
    ));
    native.seats.serve(std::sync::Arc::new(Linked::default()));
    let socket = dir.path().join("native.sock");
    let listener = tilo_files::native::private::bind_socket(&socket).unwrap();
    {
        let native = std::sync::Arc::clone(&native);
        std::thread::spawn(move || native.accept(&listener));
    }
    // One request on a connection of `uid`'s, and its answer.
    let ask = |uid: u32, request: Value| {
        PEER.store(uid, Ordering::SeqCst);
        let stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(&stream, "{request}").unwrap();
        let mut line = String::new();
        io::BufReader::new(stream)
            .read_line(&mut line)
            .expect("the daemon answers");
        serde_json::from_str::<Value>(&line).unwrap()
    };
    let who = |uid: u32| ask(uid, json!({"native": "account#who"}));
    let status = |uid: u32| ask(uid, json!({"native": "status"}));
    // A seat action: whether it failed, and what it answered.
    let act = |uid: u32, path: &str, args: Value| {
        let answer = ask(
            uid,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "tilo_do", "arguments": {"path": path, "args": args}}}),
        );
        let text = answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        (answer["result"]["isError"] == true, text)
    };
    let sign_in = |root: &ed25519_dalek::SigningKey, uid: u32| {
        let (failed, said) = act(
            ROOT,
            "seats#signed-in",
            json!({"person": person(root), "uid": uid}),
        );
        assert!(!failed, "{said}");
        said
    };
    let certify = |uid: u32, cert: &tilo_user_identity::DeviceCert| {
        act(
            uid,
            "seats#certified",
            json!({"cert": hex::encode(
                tilo_user_identity::canonical::canonical_bytes(cert).unwrap()
            )}),
        )
    };
    // The key `uid`'s account is to certify, as it reads it from who it is.
    let to_certify = |uid: u32| {
        let who = who(uid);
        assert_eq!(who["certify"]["person"], person(&bo), "{who}");
        let key: [u8; 32] = hex::decode(who["certify"]["key"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        key
    };

    // Ada signs in as the device's person, as before.
    sign_in(&ada, ADA);
    assert_eq!(who(ADA)["person"], person(&ada));
    assert_eq!(status(ADA)["workspace"], "home");

    // Bo signs in too: not taken as Ada when naming her, and naming themselves they are to act
    // as a key this daemon made them, which they have yet to certify.
    let (failed, why) = act(
        ROOT,
        "seats#signed-in",
        json!({"person": person(&ada), "uid": BO}),
    );
    assert!(failed, "{why}");
    let said = sign_in(&bo, BO);
    let first = to_certify(BO);
    assert!(said.contains(&hex::encode(first)), "{said}");
    // Until they certified it, their account is served the device's own actions alone.
    assert_eq!(who(BO)["person"], Value::Null);
    assert_eq!(status(BO)["workspace"], "", "an uncertified key acts");

    // Only Bo's own root certifies Bo's key, only for the key made for them, and only their own
    // account hands it.
    let (failed, why) = certify(BO, &certified(&ada, first));
    assert!(failed, "Ada certified Bo's key: {why}");
    let (failed, why) = certify(BO, &certified(&bo, [4; 32]));
    assert!(failed, "a key the daemon did not make acts: {why}");
    for other in [ADA, ROOT, DAEMON] {
        let (failed, why) = certify(other, &certified(&bo, first));
        assert!(failed, "{other} handed Bo's certificate: {why}");
    }
    assert_eq!(status(BO)["workspace"], "");
    let (failed, why) = certify(BO, &certified(&bo, first));
    assert!(!failed, "{why}");

    // Each acts as themselves now: Bo as their key, never the device's person's endpoint; Ada
    // as before, never Bo's key.
    let bo_who = who(BO);
    assert_eq!(bo_who["person"], person(&bo), "{bo_who}");
    assert_eq!(bo_who["key"], hex::encode(first), "{bo_who}");
    assert_eq!(bo_who["home"], Value::Null, "{bo_who}");
    assert_eq!(
        status(BO)["workspace"],
        format!("{} as {}", person(&bo), hex::encode(first)),
        "Bo is not served as their key"
    );
    assert_eq!(status(ADA)["workspace"], "home");
    assert_eq!(who(ADA)["key"], Value::Null);

    // Bo leaves: their key acts no more, and a new sign-in makes a new one, which the old
    // certificate does not certify.
    let (failed, left) = act(ROOT, "seats#left", json!({"uid": BO}));
    assert!(!failed && left.contains("true"), "{left}");
    assert_eq!(who(BO)["person"], Value::Null);
    assert_eq!(
        status(BO)["workspace"],
        "",
        "a key acts after its account left"
    );
    sign_in(&bo, BO);
    let second = to_certify(BO);
    assert_ne!(first, second, "a new sign-in acts as a new key");
    let (failed, why) = certify(BO, &certified(&bo, first));
    assert!(failed, "the key of an account that left acts again: {why}");
    assert_eq!(status(BO)["workspace"], "");
    // Ada was served throughout.
    assert_eq!(status(ADA)["workspace"], "home");
}

/// An app that is its own computer's daemon runs it in its process, and may stop it and start it
/// again there: each start answers on its native endpoint at once, and each stop closes it and
/// lets go of the device, so the next start takes the device again.
#[test]
fn a_daemon_in_this_process_stops_and_starts_again_on_its_device() {
    use io::{BufRead as _, Write as _};
    use std::os::unix::fs::DirBuilderExt as _;
    let dir = tempfile::tempdir().unwrap();
    let private = |path: &std::path::Path| {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .unwrap();
    };
    let (home, run, profile) = (
        dir.path().join("home"),
        dir.path().join("run"),
        dir.path().join("profile"),
    );
    for path in [&home, &run, &profile] {
        private(path);
    }
    // A device nobody set up founds no person by itself: it waits for one, answering its own
    // actions.
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("HOME", &home)
        .set("XDG_RUNTIME_DIR", &run)
        .set("TILO_IDENTITY_ROOT", dir.path().join("identity"))
        .remove("CREDENTIALS_DIRECTORY")
        .remove("TILO_RUNTIME_PROFILE");
    for start in ["first", "again"] {
        let running = super::Running::start(&profile).unwrap();
        let socket = running.native_socket().to_owned();
        let stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = io::BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        writeln!(
            writer,
            "{}",
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "tilo_do", "arguments": {"path": "devices#info"}}})
        )
        .unwrap();
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .expect("the device answers at once");
        let info: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_ne!(info["result"]["isError"], true, "{start} start: {info}");
        writeln!(writer, "{}", serde_json::json!({"native": "account#who"})).unwrap();
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .expect("the device says who asks");
        let who: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            who["person"],
            serde_json::Value::Null,
            "{start} start: {who}"
        );
        assert_eq!(who["claimed"], false, "{start} start: {who}");
        // `tilo status` asks the same endpoint what the device holds, as a connection's first
        // line too.
        let status = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        status
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(
            &status,
            "{}",
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": tilo_api::daemon::PEER_STATUS})
        )
        .unwrap();
        let mut line = String::new();
        io::BufReader::new(&status)
            .read_line(&mut line)
            .expect("the device says what it holds");
        let peer: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            peer["result"]["pid"],
            std::process::id(),
            "{start} start: {peer}"
        );
        running.stop().unwrap();
        assert!(
            !socket.exists(),
            "the {start} start's endpoint is gone once it stopped"
        );
        assert!(
            std::os::unix::net::UnixStream::connect(&socket).is_err(),
            "nothing answers there after the {start} start stopped"
        );
    }
}

/// A person's shells as a test serves them: each sign-in counted unless it imports, and each
/// link a pair whose other end the test keeps; a shell is started as `/bin/sh` with what it asks.
#[derive(Default)]
struct Linked {
    arrivals: std::sync::atomic::AtomicUsize,
    ends: std::sync::Mutex<Vec<std::os::unix::net::UnixStream>>,
    /// The person the tokens are made for, once the test knows them.
    person: std::sync::Mutex<Option<[u8; 32]>>,
    tokens: std::sync::Mutex<Vec<[u8; 32]>>,
}

impl Linked {
    fn person(&self, person: [u8; 32]) {
        *self.person.lock().unwrap() = Some(person);
    }

    /// Whether `link` is the other end of the newest link made.
    fn reaches(&self, link: std::os::fd::OwnedFd) {
        use io::{Read as _, Write as _};
        let mut link = std::os::unix::net::UnixStream::from(link);
        let mut end = self.ends.lock().unwrap().pop().unwrap();
        end.write_all(b"shell").unwrap();
        let mut read = [0; 5];
        link.read_exact(&mut read).unwrap();
        assert_eq!(
            &read, b"shell",
            "the handed descriptor is the link the daemon made"
        );
    }

    fn made(&self) -> std::os::unix::net::UnixStream {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        self.ends.lock().unwrap().push(ours);
        theirs
    }
}

impl crate::seats::Shells for Linked {
    fn token(&self, importing: bool) -> io::Result<[u8; 32]> {
        if !importing {
            self.arrivals
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        let token = [7; 32];
        self.tokens.lock().unwrap().push(token);
        Ok(token)
    }

    fn token_holder(&self, token: &[u8; 32]) -> Option<[u8; 32]> {
        self.tokens
            .lock()
            .unwrap()
            .contains(token)
            .then(|| *self.person.lock().unwrap())
            .flatten()
    }

    fn redeem(&self, _secret: [u8; 32], _seat: u64) -> io::Result<std::os::unix::net::UnixStream> {
        Ok(self.made())
    }

    fn link_for(
        &self,
        _person: [u8; 32],
        _seat: u64,
    ) -> io::Result<std::os::unix::net::UnixStream> {
        Ok(self.made())
    }

    fn live(&self, _seat: u64) -> bool {
        false
    }

    fn start(
        &self,
        args: Vec<std::ffi::OsString>,
        env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
        _: Option<std::os::fd::OwnedFd>,
    ) -> io::Result<std::process::Child> {
        std::process::Command::new("/bin/sh")
            .args(args)
            .env_clear()
            .envs(env)
            .spawn_locked()
    }
}

/// A daemon that runs as its person's own account starts their shell when that account asks, and
/// answers how it ended, the shell asked for before the person's services run once they do; no
/// other account starts one, and a request that asks for one founds nothing.
#[test]
fn the_persons_own_daemon_starts_their_shell_when_their_account_asks() {
    use io::{BufRead as _, Write as _};
    use serde_json::{Value, json};
    use std::sync::atomic::Ordering;
    use tilo_api::daemon::admission::Admission;
    const DAEMON: u32 = 1000;
    const OTHER: u32 = 1001;

    let dir = tempfile::tempdir().unwrap();
    let keys = tilo_device_identity::IdentityPaths::under(dir.path().join("identity"));
    let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
    let answers = crate::device::unclaimed::DeviceOnly::new(
        key,
        tilo_device_identity::login::Place::at(dir.path().join("login"), keys),
    );
    let admission: &'static Admission = Box::leak(Box::new(Admission::new(DAEMON, false)));
    let frames = crate::device::local::Frames::of_the_device(answers.clone());
    let native = std::sync::Arc::new(super::Native::new(answers, test_peer, admission));
    // The person's endpoint founds their workspace as a connection is admitted to it.
    let founded = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    native.serve_the_person(super::Endpoint::<crate::device::unclaimed::DeviceOnly> {
        session_factory: {
            let founded = std::sync::Arc::clone(&founded);
            std::sync::Arc::new(move || {
                founded.fetch_add(1, Ordering::SeqCst);
                Err(io::Error::other("no workspace in this test"))
            })
        },
        status: std::sync::Arc::new(|| super::Status {
            version: 1,
            pid: std::process::id(),
            workspace: String::new(),
        }),
        search: std::sync::Arc::new(std::sync::OnceLock::new()),
        media: None,
        other_devices: None,
        agents: None,
        frames,
        to_agents: None,
        launch: None,
        connections: None,
    });
    let socket = dir.path().join("native.sock");
    let listener = tilo_files::native::private::bind_socket(&socket).unwrap();
    {
        let native = std::sync::Arc::clone(&native);
        std::thread::spawn(move || native.accept(&listener));
    }
    // A connection of `uid`'s.
    let connect = |uid: u32| {
        PEER.store(uid, Ordering::SeqCst);
        let stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        (io::BufReader::new(stream.try_clone().unwrap()), stream)
    };
    // One request on `connection`, and its answer.
    let ask = |connection: &mut (
        io::BufReader<std::os::unix::net::UnixStream>,
        std::os::unix::net::UnixStream,
    ),
               request: &Value| {
        writeln!(connection.1, "{request}").unwrap();
        let mut line = String::new();
        connection
            .0
            .read_line(&mut line)
            .expect("the daemon answers");
        serde_json::from_str::<Value>(&line).unwrap()
    };

    let start =
        json!({"native": "shells#start", "args": ["-c", "exit \"$CODE\""], "env": {"CODE": "7"}});
    // Asked while the person's services still start (a sign-in as the computer boots): the shell
    // starts once they run.
    let mut person = connect(DAEMON);
    writeln!(person.1, "{start}").unwrap();
    native.seats.serve(std::sync::Arc::new(Linked::default()));
    let mut line = String::new();
    person.0.read_line(&mut line).expect("the daemon answers");
    let started: Value = serde_json::from_str(&line).unwrap();
    let pid = started["pid"].clone();
    assert!(pid.is_u64(), "{started}");
    let ended = ask(&mut person, &json!({"native": "shells#wait", "pid": pid}));
    assert_eq!(
        ended["code"], 7,
        "the shell ran as asked, and its end is told: {ended}"
    );
    assert_eq!(
        founded.load(Ordering::SeqCst),
        0,
        "starting a shell founds nothing"
    );

    // A shell is waited for once, and only one this daemon started.
    let again = ask(
        &mut connect(DAEMON),
        &json!({"native": "shells#wait", "pid": pid}),
    );
    assert!(again["error"].is_string(), "{again}");
    // Another account starts no shell of this person's.
    let refused = ask(&mut connect(OTHER), &start);
    assert!(refused["error"].is_string(), "{refused}");
}

/// Names the test below when this executable runs as the shell a session asked the daemon to
/// start with it.
const NESTED_SHELL: &str = "TILO_NESTED_SHELL_HELPER";
/// Where the shell the daemon started asks for its link: the daemon's own socket.
const NESTED_SHELL_SOCKET: &str = "TILO_NESTED_SHELL_SOCKET";
/// Where that shell says its link reached the daemon, once it has: the session that started it
/// reads it, so a test that selected nothing cannot pass as one.
const NESTED_SHELL_REACHED: &str = "TILO_NESTED_SHELL_REACHED";
/// The test the started shell runs: this one.
const NESTED_SHELL_TEST: &str = "daemon::tests::a_session_started_with_a_link_starts_a_nested_shell_with_its_own_link_and_window";

/// A session that holds a link of its own asks this computer's daemon to start another shell
/// (`shells#start`), handing it the connection the new shell draws its window on as a descriptor
/// of the call. The daemon starts that shell with the connection and a link of its own, which
/// reaches the daemon's workspaces: the link passes from the daemon to the shell it started.
#[cfg(target_os = "linux")]
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one sign-in, the shell it starts and the link that crosses the daemon, read top to bottom"
)]
fn a_session_started_with_a_link_starts_a_nested_shell_with_its_own_link_and_window() {
    use crate::RuntimeServices as _;
    use std::io::{BufRead as _, Write as _};
    use tilo_api::daemon::admission::Admission;

    if std::env::var_os(NESTED_SHELL).is_some() {
        // The shell the daemon started: it redeems the one-use token it was given for a link of
        // its own, which reaches the daemon, and the window connection the asking session sent
        // came with its start.
        let token = std::env::var(runtime_collab::wire::shell::SHELL_TOKEN_ENV)
            .expect("the daemon gives its shell a token");
        let socket = std::env::var(NESTED_SHELL_SOCKET).expect("the daemon's own socket");
        let stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        writeln!(
            &stream,
            "{}",
            serde_json::json!({"native": "shells#link", "token": token})
        )
        .unwrap();
        let (framed, handed) = runtime_portal::fd_transport::recv_with_fd(&stream).unwrap();
        let answer: serde_json::Value = serde_json::from_slice(&framed).unwrap();
        assert!(answer["error"].is_null(), "{answer}");
        let link = std::os::unix::net::UnixStream::from(
            handed.expect("the daemon answers its shell's token with a link"),
        );
        link.set_nonblocking(true).unwrap();
        assert_eq!(
            std::env::var("WAYLAND_SOCKET").as_deref(),
            Ok("4"),
            "the window the asking session sent is handed at its own descriptor"
        );
        let mut window = [0_u8; 6];
        // SAFETY: fd 4 is this process's window connection, handed to it as `WAYLAND_SOCKET`.
        let read = unsafe { libc::read(4, window.as_mut_ptr().cast(), window.len()) };
        assert_eq!(read, 6, "the window connection reaches the shell");
        assert_eq!(
            &window, b"window",
            "and it is the connection the session sent"
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let link = tokio::net::UnixStream::from_std(link).unwrap();
            let (shell, _) = runtime_collab::ShellClient::open(link);
            let answer = shell
                .call(runtime_collab::wire::shell::ShellCall::Workspaces)
                .await;
            assert!(
                matches!(
                    &answer,
                    Ok(runtime_collab::wire::shell::ShellReply::Workspaces(held)) if !held.is_empty()
                ),
                "the nested shell reaches the daemon's workspaces over its own link: {answer:?}"
            );
        });
        std::fs::write(
            std::env::var_os(NESTED_SHELL_REACHED).expect("the session names where to say so"),
            b"reached",
        )
        .unwrap();
        return;
    }
    let Some(root) = crate::device::services::tests::isolated_root(NESTED_SHELL_TEST) else {
        return;
    };
    let session = std::sync::Arc::new(Daemon::start_in_dev_mode(&root, &mut |_| {}).unwrap());
    // The person's workspace is founded the first time it is asked for, as their session does.
    session.workspace().unwrap();
    // The session that asks was started with a link of its own.
    let parent = session.shell_link().unwrap();
    // This computer's daemon as a person's own service: its seats answer the account that runs
    // it, and start the person's shell when that account asks.
    let keys = tilo_device_identity::IdentityPaths::under(root.join("identity"));
    let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
    let answers = crate::device::unclaimed::DeviceOnly::new(
        key,
        tilo_device_identity::login::Place::at(root.join("login"), keys),
    );
    let own = rustix::process::geteuid().as_raw();
    let admission: &'static Admission = Box::leak(Box::new(Admission::new(own, false)));
    let native = std::sync::Arc::new(super::Native::new(answers, test_peer, admission));
    native.seats.serve(std::sync::Arc::new(NestedShells(
        std::sync::Arc::downgrade(&session),
    )));
    let socket = root.join("native.sock");
    let listener = tilo_files::native::private::bind_socket(&socket).unwrap();
    {
        let native = std::sync::Arc::clone(&native);
        std::thread::spawn(move || native.accept(&listener));
    }
    // The window connection the asking session keeps: the shell it starts is handed the other
    // end.
    let (mut window_ours, window_theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    let reached = root.join("nested-shell.reached");
    let mut env: serde_json::Map<String, serde_json::Value> = std::env::vars_os()
        .filter_map(|(name, value)| {
            Some((name.into_string().ok()?, value.into_string().ok()?.into()))
        })
        .collect();
    env.insert(NESTED_SHELL.to_owned(), "1".into());
    env.insert(
        NESTED_SHELL_REACHED.to_owned(),
        reached.display().to_string().into(),
    );
    env.insert(
        NESTED_SHELL_SOCKET.to_owned(),
        socket.display().to_string().into(),
    );
    let start = serde_json::json!({
        "native": "shells#start",
        "args": [
            std::env::current_exe().unwrap().display().to_string(),
            "--exact",
            NESTED_SHELL_TEST,
            "--nocapture",
        ],
        "env": env,
    });
    // The start goes as a session in a window sends it: the request line and the window
    // connection in one message, over the connection the session already has.
    PEER.store(own, std::sync::atomic::Ordering::SeqCst);
    let stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    runtime_portal::fd_transport::send_with_fd(
        &stream,
        format!("{start}\n").as_bytes(),
        &window_theirs,
    )
    .unwrap();
    drop(window_theirs);
    window_ours.write_all(b"window").unwrap();
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let started: serde_json::Value = serde_json::from_str(&line).unwrap();
    let pid = started["pid"]
        .as_u64()
        .unwrap_or_else(|| panic!("the daemon starts the shell: {started}"));
    writeln!(
        &stream,
        "{}",
        serde_json::json!({"native": "shells#wait", "pid": pid})
    )
    .unwrap();
    let mut ended = String::new();
    reader.read_line(&mut ended).unwrap();
    let ended: serde_json::Value = serde_json::from_str(&ended).unwrap();
    assert_eq!(ended["code"], 0, "the nested shell passed: {ended}");
    assert_eq!(
        std::fs::read_to_string(&reached).unwrap_or_default(),
        "reached",
        "the nested shell reached the daemon's workspaces over its own link"
    );
    // The link the asking session holds answers too: the nested shell has a link of its own.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        parent.set_nonblocking(true).unwrap();
        let (shell, _) =
            runtime_collab::ShellClient::open(tokio::net::UnixStream::from_std(parent).unwrap());
        let answer = shell
            .call(runtime_collab::wire::shell::ShellCall::Workspaces)
            .await;
        assert!(
            matches!(
                &answer,
                Ok(runtime_collab::wire::shell::ShellReply::Workspaces(held)) if !held.is_empty()
            ),
            "the session's own link still answers: {answer:?}"
        );
    });
}

/// The person's shells as a test runs them: [`super::PersonShells`] but for the shell being the
/// program the asking session names first in its start, where a computer starts the installed
/// `tilo-session` beside its daemon.
#[cfg(target_os = "linux")]
struct NestedShells(std::sync::Weak<Daemon>);

#[cfg(target_os = "linux")]
impl crate::seats::Shells for NestedShells {
    fn token(&self, _: bool) -> io::Result<[u8; 32]> {
        Err(io::Error::other("a test starts no shell at a sign-in"))
    }

    fn token_holder(&self, token: &[u8; 32]) -> Option<[u8; 32]> {
        self.0.upgrade()?.shell_token_holder(token)
    }

    fn redeem(&self, secret: [u8; 32], seat: u64) -> io::Result<std::os::unix::net::UnixStream> {
        self.0
            .upgrade()
            .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?
            .shell_redeem(secret, seat)
    }

    fn link_for(&self, person: [u8; 32], seat: u64) -> io::Result<std::os::unix::net::UnixStream> {
        let daemon = self
            .0
            .upgrade()
            .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
        let secret = daemon.shell_token_for(person, seat)?;
        daemon.shell_link_of(secret)
    }

    fn live(&self, seat: u64) -> bool {
        self.0
            .upgrade()
            .is_some_and(|daemon| daemon.shell_live(seat))
    }

    fn start(
        &self,
        args: Vec<std::ffi::OsString>,
        env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
        window: Option<std::os::fd::OwnedFd>,
    ) -> io::Result<std::process::Child> {
        let daemon = self
            .0
            .upgrade()
            .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
        let program = args
            .first()
            .ok_or_else(|| io::Error::other("a start names the program to run"))?;
        let mut command = std::process::Command::new(program);
        command
            .args(args.get(1..).unwrap_or_default())
            .stdin(std::process::Stdio::null());
        super::the_shells_environment(&mut command, env);
        daemon.start_shell(command, window.as_ref())
    }
}

/// The person's services start when this device's own account founds them (`devices#found`),
/// without the daemon restarting: before founding they are not there and a call as the person is
/// refused, and after it answers the person's key they come up and answer a call as them.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the whole first start: before, founding, and the services it brings up"
)]
fn the_persons_services_start_when_this_device_founds_them() {
    use io::{BufRead as _, Write as _};
    use std::os::unix::fs::DirBuilderExt as _;
    use tilo_actions::remote::{
        InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
        RemoteActionTarget,
    };
    use tilo_actions::{ActionAddress, InvocationId};

    let dir = tempfile::tempdir().unwrap();
    let private = |path: &std::path::Path| {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .unwrap();
    };
    let (home, run, profile) = (
        dir.path().join("home"),
        dir.path().join("run"),
        dir.path().join("profile"),
    );
    for path in [
        home.clone(),
        run.clone(),
        profile.clone(),
        home.join(".local/state"),
        home.join(".config"),
        home.join(".local/share"),
    ] {
        private(&path);
    }
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("HOME", &home)
        .set("XDG_RUNTIME_DIR", &run)
        .set("XDG_STATE_HOME", home.join(".local/state"))
        .set("XDG_CONFIG_HOME", home.join(".config"))
        .set("XDG_DATA_HOME", home.join(".local/share"))
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1")
        .set("TILO_IDENTITY_ROOT", dir.path().join("identity"))
        .remove("CREDENTIALS_DIRECTORY")
        .remove("TILO_RUNTIME_PROFILE");
    let running = super::Running::start(&profile).unwrap();
    let socket = running.native_socket().to_owned();

    // One call to the device's actions, over the frame wire every client reaches it with.
    let call = |action: &str| -> ReceiptStatus {
        let request = RemoteActionMessage::Invoke(InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: tilo_device_identity::login::PROVIDER.to_owned(),
                address: ActionAddress::new(tilo_device_identity::login::RESOURCE, action),
            },
            argument: InvokeArgument::Json(serde_json::Map::new()),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        });
        let stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        let mut writer = stream.try_clone().unwrap();
        writer
            .write_all(&RemoteActionFrame::new(request).encode().unwrap())
            .unwrap();
        writer.write_all(b"\n").unwrap();
        let mut line = String::new();
        io::BufReader::new(stream)
            .read_line(&mut line)
            .expect("the daemon answers");
        match RemoteActionFrame::decode(line.trim_end().as_bytes())
            .expect("the answer is a frame")
            .message
        {
            RemoteActionMessage::Receipt(receipt) => receipt.status,
            other => panic!("a call is answered with a receipt, not {other:?}"),
        }
    };

    // Before founding, the person's services are not there and a call as them is refused.
    assert!(
        running.person.within(Duration::from_millis(200)).is_err(),
        "nothing serves a person before one is founded"
    );
    let before = call(tilo_device_identity::login::IDENTITIES);
    assert!(
        matches!(before, ReceiptStatus::Rejected { .. }),
        "a call as the person is refused before one is founded: {before:?}"
    );

    // This device's own account founds them; the action answers the person's key, in hex.
    let found = call(tilo_device_identity::login::FOUND);
    let ReceiptStatus::Completed { output } = found else {
        panic!("the device founds a person: {found:?}");
    };
    let person = output
        .value
        .as_ref()
        .and_then(|value| value.get("person"))
        .and_then(serde_json::Value::as_str)
        .expect("the found action answers the person")
        .to_owned();
    assert_eq!(
        person.len(),
        64,
        "the person is their root key, in hex: {person}"
    );

    // Without a restart, their services come up, and a frame now answers as them.
    running
        .person
        .within(Duration::from_secs(120))
        .expect("the person's services start once this device founds them");
    let after = call(tilo_device_identity::login::IDENTITIES);
    assert!(
        matches!(after, ReceiptStatus::Completed { .. }),
        "the person's services answer a call as them: {after:?}"
    );
    running.stop().unwrap();
}

/// A process the daemon started is the app it runs, with that app's grants, and any other
/// process of the same account is no app: its pid names no launch record, so it reaches only
/// what a plain connection of the account reaches.
#[test]
fn a_process_the_daemon_started_is_its_app_and_another_process_of_the_account_is_no_app() {
    use crate::seats::{AppAt, Peer, Seats};
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tilo_api::daemon::admission::Admission;

    /// The pid a test's next connection comes from, as the daemon reads it.
    static PID: AtomicU32 = AtomicU32::new(0);

    fn peer(_: &UnixStream) -> Peer {
        Peer {
            uid: rustix::process::geteuid().as_raw(),
            pid: Some(PID.load(Ordering::SeqCst)),
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let keys = tilo_device_identity::IdentityPaths::under(dir.path().join("identity"));
    let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
    let place = tilo_device_identity::login::Place::at(dir.path().join("login"), keys);
    let own = rustix::process::geteuid().as_raw();
    let admission: &'static Admission = Box::leak(Box::new(Admission::new(own, false)));
    let seats = Seats::new(
        admission,
        crate::device::unclaimed::DeviceOnly::new(key, place),
        |stream| Ok(peer(stream)),
    );
    let started = 424_242;
    seats.follow_launches(std::sync::Arc::new(move |pid| {
        (pid == started).then(|| ("so.tilo.notes".to_owned(), "a-workspace".to_owned()))
    }));
    let (_ours, theirs) = UnixStream::pair().unwrap();

    PID.store(started, Ordering::SeqCst);
    let seat = seats.seat(&theirs);
    assert_eq!(
        seat.app,
        Some(AppAt {
            app: "so.tilo.notes".to_owned(),
            workspace: "a-workspace".to_owned(),
            pid: started,
        }),
        "the process the daemon started is the app it runs"
    );

    PID.store(started + 1, Ordering::SeqCst);
    let seat = seats.seat(&theirs);
    assert!(
        seat.app.is_none(),
        "another process of the same account is no app: it reaches only what a plain connection does"
    );
    assert_eq!(
        seat.admitted,
        tilo_api::daemon::admission::Admitted::Device,
        "the account is still the device's own"
    );
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "it stands in for the kernel's answer, which can fail"
)]
fn root_peer(_: &std::os::unix::net::UnixStream) -> io::Result<crate::seats::Peer> {
    Ok(crate::seats::Peer {
        uid: tilo_api::daemon::admission::ROOT,
        pid: None,
    })
}

/// Root acts as the device only under the system unit, where it says who signed in. A person's
/// own daemon that runs as root (a container's, a test sandbox's) is root's own: its connections
/// reach the person's services once they run, as its account's do anywhere.
#[test]
fn root_reaches_the_person_in_their_own_daemon_and_the_device_under_the_system_unit() {
    use io::{BufRead as _, Write as _};
    use tilo_api::daemon::admission::{Admission, ROOT};

    let served_as_root = |system: bool| -> String {
        let dir = tempfile::tempdir().unwrap();
        let keys = tilo_device_identity::IdentityPaths::under(dir.path().join("identity"));
        keys.ensure_directories().unwrap();
        let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
        let answers = crate::device::unclaimed::DeviceOnly::new(
            key,
            tilo_device_identity::login::Place::at(dir.path().join("login"), keys),
        );
        let admission: &'static Admission = Box::leak(Box::new(Admission::new(ROOT, system)));
        let native = std::sync::Arc::new(super::Native::new(answers.clone(), root_peer, admission));
        // The person's services run, answering as the workspace `home`.
        let frames = crate::device::local::Frames::of_the_device(answers.clone());
        native.serve_the_person(super::Endpoint {
            session_factory: std::sync::Arc::new(move || {
                Ok(tilo_mcp::McpServer::for_agent(
                    answers.clone(),
                    String::new(),
                    "agent",
                    "",
                ))
            }),
            status: std::sync::Arc::new(|| super::Status {
                version: 1,
                pid: std::process::id(),
                workspace: "home".into(),
            }),
            search: std::sync::Arc::new(std::sync::OnceLock::new()),
            media: None,
            other_devices: None,
            agents: None,
            frames,
            to_agents: None,
            launch: None,
            connections: None,
        });
        let socket = dir.path().join("native.sock");
        let listener = tilo_files::native::private::bind_socket(&socket).unwrap();
        {
            let native = std::sync::Arc::clone(&native);
            std::thread::spawn(move || native.accept(&listener));
        }
        let stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(&stream, "{}", serde_json::json!({"native": "status"})).unwrap();
        let mut line = String::new();
        io::BufReader::new(stream)
            .read_line(&mut line)
            .expect("the daemon answers");
        let status: serde_json::Value = serde_json::from_str(&line).unwrap();
        status["workspace"].as_str().unwrap_or_default().to_owned()
    };

    assert_eq!(
        served_as_root(false),
        "home",
        "a person's own daemon running as root serves root the person's services"
    );
    assert_eq!(
        served_as_root(true),
        "",
        "under the system unit root is answered as the device, never the person"
    );
}

/// The account and process a test's next connection comes from, as the kernel reads them.
static KERNEL_UID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static KERNEL_PID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The kernel's answer for a connection's other end, as [`KERNEL_UID`] and [`KERNEL_PID`] stand
/// in for it: a test's next connection is the account and process they name.
#[expect(
    clippy::unnecessary_wraps,
    reason = "it stands in for the kernel's answer, which can fail"
)]
fn kernel_peer(_: &std::os::unix::net::UnixStream) -> io::Result<crate::seats::Peer> {
    use std::sync::atomic::Ordering;
    Ok(crate::seats::Peer {
        uid: KERNEL_UID.load(Ordering::SeqCst),
        pid: Some(KERNEL_PID.load(Ordering::SeqCst)),
    })
}

/// A person's own daemon in `dir`, serving their services as the workspace `home`, which serves
/// the account `own` as its person and no other: every connection is admitted as whoever
/// [`kernel_peer`] says is at its other end, and the process `launched` is followed as the app it
/// runs. The socket it answers on.
fn person_served_as_the_kernel_says(
    dir: &std::path::Path,
    launched: u32,
    own: u32,
) -> std::path::PathBuf {
    use tilo_api::daemon::admission::Admission;

    let keys = tilo_device_identity::IdentityPaths::under(dir.join("identity"));
    keys.ensure_directories().unwrap();
    let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
    let answers = crate::device::unclaimed::DeviceOnly::new(
        key,
        tilo_device_identity::login::Place::at(dir.join("login"), keys),
    );
    // A person's own daemon: it serves its own account and root, and no other.
    let admission: &'static Admission = Box::leak(Box::new(Admission::new(own, false)));
    let native = std::sync::Arc::new(super::Native::new(answers.clone(), kernel_peer, admission));
    // The person's services run, answering as the workspace `home`.
    let frames = crate::device::local::Frames::of_the_device(answers.clone());
    native.serve_the_person(super::Endpoint {
        session_factory: std::sync::Arc::new(move || {
            Ok(tilo_mcp::McpServer::for_agent(
                answers.clone(),
                String::new(),
                "agent",
                "",
            ))
        }),
        status: std::sync::Arc::new(|| super::Status {
            version: 1,
            pid: std::process::id(),
            workspace: "home".into(),
        }),
        search: std::sync::Arc::new(std::sync::OnceLock::new()),
        media: None,
        other_devices: None,
        agents: None,
        frames,
        to_agents: None,
        launch: None,
        connections: None,
    });
    // A process the daemon started is the app it runs.
    native
        .seats
        .follow_launches(std::sync::Arc::new(move |pid| {
            (pid == launched).then(|| ("so.tilo.notes".to_owned(), "a-workspace".to_owned()))
        }));
    let socket = dir.join("native.sock");
    let listener = tilo_files::native::private::bind_socket(&socket).unwrap();
    {
        let native = std::sync::Arc::clone(&native);
        std::thread::spawn(move || native.accept(&listener));
    }
    socket
}

/// One request on a connection of `uid`'s from process `pid`, and its answer.
fn asked_as(socket: &std::path::Path, uid: u32, pid: u32) -> serde_json::Value {
    use std::io::{BufRead as _, Write as _};
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::Ordering;

    KERNEL_UID.store(uid, Ordering::SeqCst);
    KERNEL_PID.store(pid, Ordering::SeqCst);
    let stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(&stream, "{}", serde_json::json!({"native": "status"})).unwrap();
    let mut line = String::new();
    io::BufReader::new(stream)
        .read_line(&mut line)
        .expect("the daemon answers");
    serde_json::from_str(&line).unwrap()
}

/// A connection to a person's own daemon is admitted as whoever the kernel says is at its other
/// end ([`crate::seats::Peer`]): the person's own account reaches their services, and another
/// account on the same computer is answered the device's own actions alone, never the person's.
/// A process the daemon started is the app it runs and is served that app's actions, never the
/// person's, whichever account it runs as.
#[test]
fn a_plain_socket_peer_is_the_person_only_from_the_persons_own_account() {
    let dir = tempfile::tempdir().unwrap();
    let launched = 424_242;
    let own = rustix::process::geteuid().as_raw();
    let socket = person_served_as_the_kernel_says(dir.path(), launched, own);
    let unrelated = 1_000_000;

    // The person's own account is the person: their services answer it.
    assert_eq!(
        asked_as(&socket, own, unrelated)["workspace"],
        "home",
        "the person's own account reaches their services"
    );
    // Another account on the same computer is answered the device's own actions alone.
    let stranger = own.wrapping_add(1);
    assert_eq!(
        asked_as(&socket, stranger, unrelated)["workspace"],
        "",
        "another account on the same computer is refused the person's services"
    );
    // A process the daemon started is the app it runs, and is served that app's actions alone.
    let app = asked_as(&socket, own, launched);
    assert_eq!(
        app["err"]["code"], "not_permitted",
        "an app's process is no person: {app}"
    );
    assert!(
        app["err"]["message"]
            .as_str()
            .is_some_and(|why| why.contains("apps' workspaces")),
        "the app's endpoint answers, not the person's: {app}"
    );
}

/// A program in the same process as a person's own daemon — a phone's app, which runs the daemon
/// beside its windows — reaches the person's account over the native endpoint as `tilo` does,
/// and is answered: the daemon founds their workspace on that first call, and the answer comes
/// back at once rather than waiting for the copy of the workspace the same process serves.
#[test]
fn a_client_in_the_persons_own_daemons_process_is_answered_at_once() {
    use std::os::unix::fs::DirBuilderExt as _;
    use std::time::Instant;

    let dir = tempfile::tempdir().unwrap();
    let private = |path: &std::path::Path| {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .unwrap();
    };
    let (home, files, run) = (
        dir.path().join("home"),
        dir.path().join("files"),
        dir.path().join("run"),
    );
    for path in [
        home.clone(),
        files.join("data"),
        files.join("config"),
        files.join("state"),
        files.join("cache"),
        run.clone(),
    ] {
        private(&path);
    }
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("HOME", &home)
        .set("XDG_DATA_HOME", files.join("data"))
        .set("XDG_CONFIG_HOME", files.join("config"))
        .set("XDG_STATE_HOME", files.join("state"))
        .set("XDG_CACHE_HOME", files.join("cache"))
        .set("XDG_RUNTIME_DIR", &run)
        .set("TILO_IDENTITY_ROOT", files.join("data/tilo"))
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1")
        .remove("TILO_RUNTIME_PROFILE")
        .remove("CREDENTIALS_DIRECTORY");
    let profile = super::default_profile().unwrap();
    private(&profile);

    // This device's own account starts its person, and the daemon starts again to serve them, as
    // the phone's does.
    let running = super::Running::start(&profile).unwrap();
    tilo_api::daemon::this_computer()
        .unwrap()
        .found(&tilo_api::daemon::Founding::default())
        .expect("the device starts its person");
    running.stop().unwrap();
    let running = super::Running::start(&profile).unwrap();
    running
        .person
        .within(Duration::from_secs(120))
        .expect("the person's services start");

    // A client of this process, as the phone's app is, calls the person's account: the first
    // call founds their workspace, and is answered.
    let asked = Instant::now();
    let listed = {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(async {
                let calls = tilo_api::calls::Calls::open(tilo_api::calls::Reach::default()).await?;
                calls.workspaces().await
            })
            .expect("the person's account is answered")
    };
    assert!(
        listed
            .get("workspaces")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|listed| !listed.is_empty()),
        "the person's workspace is listed: {listed}"
    );
    assert!(
        asked.elapsed() < Duration::from_secs(10),
        "the account is answered at once, not after a wait this process causes: {:?}",
        asked.elapsed()
    );

    running.stop().unwrap();
}

/// A profile's path is the person's state, which can be longer than a socket's address holds. A
/// named profile's daemon keeps its sockets in a short directory of its own, and the client
/// helper names that same directory, so finding a daemon and binding one agree.
#[test]
fn a_long_profile_path_keeps_its_sockets_where_every_client_finds_them() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir
        .path()
        .join("people")
        .join("a".repeat(90))
        .join("state")
        .join("server");
    std::fs::create_dir_all(&profile).unwrap();
    assert!(
        tilo_api::daemon::socket_fits(&profile.join(tilo_paths::machine::DAEMON_NATIVE)).is_err(),
        "the test's profile is long enough that a socket cannot live in it"
    );
    let sockets = super::sockets_dir(&profile).unwrap();
    assert_ne!(sockets, profile, "{}", sockets.display());
    assert!(!sockets.starts_with(&profile), "{}", sockets.display());
    tilo_api::daemon::socket_fits(&sockets.join(tilo_paths::machine::DAEMON_NATIVE)).unwrap();
    assert_eq!(
        tilo_api::daemon::profile_sockets(&profile).unwrap(),
        sockets,
        "the daemon and every client name one directory"
    );
}

/// A person's own daemon that started again, as a phone's does, answers the person's own account
/// as their device, and not as a question for another device of theirs to allow: the list of
/// their workspaces comes back from that daemon. A call that arrives before the person's part
/// runs waits for that part and is answered by the same daemon; it is never kept as an asked
/// record waiting on a device that keeps their root.
#[test]
fn a_restarted_daemons_own_account_lists_its_workspaces_without_asking() {
    use std::os::unix::fs::DirBuilderExt as _;
    use std::time::Instant;

    // The daemon's own account of what it did, when a failing run needs it.
    if let Ok(directives) = std::env::var("TILO_TEST_LOG") {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(directives))
            .with_test_writer()
            .try_init();
    }

    let dir = tempfile::tempdir().unwrap();
    let private = |path: &std::path::Path| {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .unwrap();
    };
    let (home, files, run) = (
        dir.path().join("home"),
        dir.path().join("files"),
        dir.path().join("run"),
    );
    for path in [
        home.clone(),
        files.join("data"),
        files.join("config"),
        files.join("state"),
        files.join("cache"),
        run.clone(),
    ] {
        private(&path);
    }
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("HOME", &home)
        .set("XDG_DATA_HOME", files.join("data"))
        .set("XDG_CONFIG_HOME", files.join("config"))
        .set("XDG_STATE_HOME", files.join("state"))
        .set("XDG_CACHE_HOME", files.join("cache"))
        .set("XDG_RUNTIME_DIR", &run)
        .set("TILO_IDENTITY_ROOT", files.join("data/tilo"))
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1")
        .remove("TILO_RUNTIME_PROFILE")
        .remove("CREDENTIALS_DIRECTORY");
    let profile = super::default_profile().unwrap();
    private(&profile);

    // The device starts its person, and the daemon starts again to serve them, as the phone's
    // does after its own `found`.
    let mut device = crate::in_process::Device::start(&profile).unwrap();
    device.found().expect("the device starts its person");

    // Right away: the phone's view asks the daemon that just started again for the person's
    // workspaces, before the person's part has run. A call that comes before it waits for it;
    // what comes back must never be the wait for another device's approval.
    let deadline = Instant::now() + Duration::from_secs(60);
    let listed = loop {
        match device.devices() {
            Ok(devices) if !devices.workspaces.is_empty() => break devices,
            Ok(devices) => assert!(
                Instant::now() < deadline,
                "the person's own workspace is never listed: {devices:?}"
            ),
            Err(error) => {
                let said = format!("{error:#}");
                let awaiting = tilo_api::calls::Refusal::of(&error).is_some_and(|refusal| {
                    refusal.code() == tilo_actions::remote::AWAITING_APPROVAL
                }) || said.contains("Waiting for your approval");
                assert!(
                    !awaiting,
                    "the person's own account asked their own daemon and was told it waits for an \
                     approval: {said}"
                );
                assert!(
                    Instant::now() < deadline,
                    "the person's own account is never answered: {said}"
                );
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        listed.signed_in,
        "the phone is its new person's: {listed:?}"
    );
    device.stop().expect("the phone's daemon stops");
}

/// A daemon of the computer's binds its services' sockets in a directory only it may enter,
/// named for the account the person is, while its native endpoint stays in the directory every
/// account reaches: a broker's socket refuses one another account can read.
#[test]
fn a_system_daemons_services_are_the_accounts_and_its_native_endpoint_is_everyones() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let directory = tempfile::tempdir().unwrap();
    let shared = directory.path().join("run");
    std::fs::create_dir(&shared).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
    let native = shared.join(tilo_paths::machine::DAEMON_NATIVE);

    // The daemon answers at its native endpoint before anyone signs in, and a founding then makes
    // the services' directory: the endpoint is still there, and still reached through the
    // directory every account may enter.
    let listener = std::os::unix::net::UnixListener::bind(&native).unwrap();
    let services = super::service_sockets_at(&shared, true, Some(1000)).unwrap();
    assert_eq!(
        services,
        shared.join("accounts/1000"),
        "the services are the account's own"
    );
    assert_eq!(
        std::fs::symlink_metadata(&shared).unwrap().mode() & 0o777,
        0o755,
        "making the services' directory left the native endpoint's directory as every account \
         reaches it"
    );
    std::os::unix::net::UnixStream::connect(&native).expect("an account reaches native.sock");
    drop(listener);
    let metadata = std::fs::symlink_metadata(&services).unwrap();
    assert_eq!(
        metadata.mode() & 0o777,
        0o700,
        "the services' directory is the daemon's alone: {:o}",
        metadata.mode() & 0o777
    );
    assert_eq!(
        metadata.uid(),
        rustix::process::geteuid().as_raw(),
        "the services' directory is the daemon's"
    );
    let named = super::Sockets::in_dir(&shared);
    assert_eq!(named.native, native, "every account reaches native.sock");
    for name in ["collab.sock", "identity.sock", "blobs.sock"] {
        assert_eq!(services.join(name).parent(), Some(services.as_path()));
        assert_ne!(
            shared.join(name),
            services.join(name),
            "the service socket stayed in the directory every account reaches"
        );
    }

    // One account's own daemon keeps its services beside its native endpoint, and a daemon of the
    // computer's that serves no account yet keeps them there too.
    assert_eq!(
        super::service_sockets_at(&shared, false, Some(1000)).unwrap(),
        shared
    );
    assert_eq!(
        super::service_sockets_at(&shared, true, None).unwrap(),
        shared
    );
}

/// A connection whose session cannot be made is told so within seconds: a daemon whose person's
/// part is not up never holds a caller unanswered.
#[test]
fn a_session_that_cannot_be_made_is_refused_within_seconds() {
    let started = std::time::Instant::now();
    let refused = super::admit_within::<crate::device::unclaimed::DeviceOnly>(|| {
        std::thread::sleep(std::time::Duration::from_secs(60));
        Err(std::io::Error::other("too late"))
    });
    let Err(refused) = refused else {
        panic!("a session that is not made is refused");
    };
    assert!(
        refused.to_string().contains("no person signed in yet"),
        "the refusal says why: {refused}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(45),
        "the refusal came within its bound: {:?}",
        started.elapsed()
    );
}

/// A daemon of the computer's that no person's certificate names yet answers root's sign-in over
/// its native endpoint as one typed action, as `pam_tilo` says it; the account then founds its
/// person through the same endpoint, and root's next sign-in answers a person. An action that
/// needs a person before one exists is refused at once.
#[test]
fn an_unclaimed_system_daemon_answers_roots_sign_in_then_has_a_person() {
    use io::{BufRead as _, Write as _};
    use serde_json::{Value, json};
    use std::sync::atomic::Ordering;
    const DAEMON: u32 = 990;
    const ADA: u32 = 1000;

    let mut home = tilo_test_env::TestHome::new().unwrap();
    home.env()
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
    let root = home.root().to_owned();
    let keys = tilo_api::calls::device_keys();
    let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
    let place = tilo_device_identity::login::Place::at(root.join("login"), keys);
    let admission: &'static tilo_api::daemon::admission::Admission = Box::leak(Box::new(
        tilo_api::daemon::admission::Admission::new(DAEMON, true),
    ));
    let answers = crate::device::unclaimed::DeviceOnly::at(key, place, root.clone(), DAEMON);
    let native = std::sync::Arc::new(super::Native::new(answers, founding_peer, admission));
    let sockets = root.join("sockets");
    std::fs::create_dir_all(&sockets).unwrap();
    let listener =
        tilo_files::native::private::bind_socket(&sockets.join(tilo_paths::machine::DAEMON_NATIVE))
            .unwrap();
    {
        let native = std::sync::Arc::clone(&native);
        std::thread::spawn(move || native.accept(&listener));
    }
    let native_socket = sockets.join(tilo_paths::machine::DAEMON_NATIVE);
    let ask = |uid: u32, request: Value| {
        FOUNDING_PEER.store(uid, Ordering::SeqCst);
        let stream = std::os::unix::net::UnixStream::connect(&native_socket).unwrap();
        // A request nothing answers fails here, rather than in a wait the walk would report.
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut writer = stream.try_clone().unwrap();
        writeln!(writer, "{request}").unwrap();
        let mut line = String::new();
        io::BufReader::new(stream)
            .read_line(&mut line)
            .expect("the daemon answers at once");
        serde_json::from_str::<Value>(&line).unwrap()
    };
    let call = |uid: u32, path: &str, args: Value| {
        let answer = ask(
            uid,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "tilo_do", "arguments": {"path": path, "args": args}}}),
        );
        let text = answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        (answer["result"]["isError"] == true, text)
    };

    // An action that needs a person is refused at once, before any person exists.
    let started = std::time::Instant::now();
    let (refused, why) = call(0, "notes#create", json!({}));
    assert!(refused, "an action needing a person answered: {why}");
    assert!(
        why.contains("no person"),
        "the refusal says the device has no person: {why}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the refusal came at once: {:?}",
        started.elapsed()
    );

    // Root says the account signed in, as `pam_tilo` says it: one typed action over the native
    // endpoint, answered the same way by every caller.
    let (failed, said) = call(0, "seats#signed-in", json!({"uid": ADA}));
    assert!(!failed, "{said}");
    assert!(
        said.contains("\"founding\":true"),
        "a person is to be made: {said}"
    );

    // The account's first command founds its person through the same endpoint.
    let (failed, said) = call(ADA, "devices#found", json!({}));
    assert!(!failed, "the account founded its person: {said}");

    // Root says it signed in again, and the daemon answers the person, not a second founding.
    let (failed, said) = call(0, "seats#signed-in", json!({"uid": ADA}));
    assert!(!failed, "{said}");
    assert!(
        said.contains("\"person\""),
        "the sign-in answers the person: {said}"
    );
    assert!(
        !said.contains("\"founding\":true"),
        "the person is made already: {said}"
    );
}

/// A call a person's program made over the action wire, still running as the person's part
/// ends, is answered on the connection it was made on; a call made on it while the part changes
/// waits there and is answered by the part that starts next. Nothing the caller sent is lost,
/// and the caller never connects again.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one connection across two parts, read top to bottom"
)]
fn a_call_in_flight_as_the_persons_part_ends_is_answered_and_the_next_one_waits_for_the_new_part() {
    use io::{BufRead as _, Write as _};
    use std::sync::{Arc, Mutex, atomic::Ordering};
    use tilo_actions::remote::{
        Caller, InvocationReceipt, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame,
        RemoteActionMessage, RemoteActionTarget, TestProvider, pending,
    };
    use tilo_api::daemon::admission::Admission;
    // The person's own account, which their daemon runs as.
    const DAEMON: u32 = 990;

    let dir = tempfile::tempdir().unwrap();
    let keys = tilo_device_identity::IdentityPaths::under(dir.path().join("identity"));
    keys.ensure_directories().unwrap();
    let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
    let ada = ed25519_dalek::SigningKey::from_bytes(&[13; 32]);
    let now = tilo_clock::unix_now_ns();
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: ada.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "desk".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: now.saturating_sub(60_000_000_000),
        expires_at_ns: Some(now.saturating_add(3_600_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &ada).unwrap();
    tilo_device_identity::persist::write_public_key(
        &keys.user_pubkey,
        &ada.verifying_key().to_bytes(),
    )
    .unwrap();
    tilo_device_identity::persist::write_device_cert(&keys.device_cert, &cert).unwrap();
    let answers = crate::device::unclaimed::DeviceOnly::new(
        key,
        tilo_device_identity::login::Place::at(dir.path().join("login"), keys),
    );
    let admission: &'static Admission = Box::leak(Box::new(Admission::new(DAEMON, true)));
    let native = Arc::new(super::Native::new(answers.clone(), test_peer, admission));

    // A part of the person's, answering their calls with `provider` and holding its connections
    // in `connections`.
    let part = |provider: &Arc<TestProvider>, connections: &Arc<super::Connections>| {
        let provider = Arc::clone(provider);
        let answers = answers.clone();
        super::Endpoint {
            session_factory: Arc::new(move || {
                Ok(tilo_mcp::McpServer::for_agent(
                    answers.clone(),
                    String::new(),
                    "agent",
                    "",
                ))
            }),
            status: Arc::new(|| super::Status {
                version: 1,
                pid: std::process::id(),
                workspace: "home".into(),
            }),
            search: Arc::new(std::sync::OnceLock::new()),
            media: None,
            other_devices: None,
            agents: None,
            frames: crate::device::local::Frames::new(move |_| {
                Ok((
                    Arc::clone(&provider)
                        as Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
                    Caller::local_person("ada"),
                ))
            }),
            to_agents: None,
            launch: None,
            connections: Some(Arc::clone(connections)),
        }
    };

    // The first part's call runs until the test lets it finish.
    let (finishing, finished) = std::sync::mpsc::channel();
    let asked = Arc::new((Mutex::new(0_usize), std::sync::Condvar::new()));
    let first = Arc::new({
        let (finishing, asked) = (Mutex::new(finishing), Arc::clone(&asked));
        TestProvider::new().answering(move |_, request| {
            let (completer, answer) = pending();
            finishing
                .lock()
                .unwrap()
                .send((completer, request.invocation))
                .unwrap();
            *asked.0.lock().unwrap() += 1;
            asked.1.notify_all();
            answer
        })
    });
    let first_connections = Arc::new(super::Connections::default());
    native.serve_the_person(part(&first, &first_connections));
    native.seats.serve(Arc::new(Linked::default()));
    let socket = dir.path().join("native.sock");
    let listener = tilo_files::native::private::bind_socket(&socket).unwrap();
    {
        let native = Arc::clone(&native);
        std::thread::spawn(move || native.accept(&listener));
    }

    let call = |path: &str| {
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: "test".to_owned(),
                address: tilo_actions::ActionAddress::new("workspaces", path),
            },
            argument: InvokeArgument::default(),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: Some(30_000),
            appearance: None,
            signed: None,
        };
        let line = RemoteActionFrame::new(RemoteActionMessage::Invoke(request.clone()))
            .encode()
            .unwrap();
        (request.invocation, line)
    };
    PEER.store(DAEMON, Ordering::SeqCst);
    let stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut answers_read = io::BufReader::new(stream.try_clone().unwrap());
    let answer = |answers_read: &mut io::BufReader<std::os::unix::net::UnixStream>| {
        let mut line = String::new();
        answers_read
            .read_line(&mut line)
            .expect("the call is answered on the connection it was made on");
        match RemoteActionFrame::decode(line.as_bytes()).unwrap().message {
            RemoteActionMessage::Receipt(receipt) => (receipt.invocation, receipt.status),
            other => panic!("a receipt answers the call: {other:?}"),
        }
    };

    let (founding, line) = call("create");
    (&stream).write_all(&line).unwrap();
    (&stream).write_all(b"\n").unwrap();
    let Ok((completer, invocation)) = finished.recv_timeout(Duration::from_secs(10)) else {
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut said = String::new();
        let _ = answers_read.read_line(&mut said);
        panic!("the first part runs the call, but the connection was answered: {said:?}");
    };
    assert_eq!(invocation, founding);

    // The part ends while the call runs: no part serves the person while the next one starts.
    native.forget_the_person();
    let stop = crate::device::serve::Stop::default();
    let handing = {
        let connections = Arc::clone(&first_connections);
        std::thread::spawn(move || connections.hand_over(&stop))
    };
    while !tilo_actions::remote::Handover::handing(&*first_connections) {
        std::thread::sleep(Duration::from_millis(5));
    }
    // A call made now is not the part's that ends: it waits for the next.
    let (next_call, line) = call("list");
    (&stream).write_all(&line).unwrap();
    (&stream).write_all(b"\n").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        *asked.0.lock().unwrap(),
        1,
        "the part that ends takes no new call"
    );
    assert!(
        !handing.is_finished(),
        "the part waits for the call it took to be answered"
    );

    completer.complete(Ok(InvocationReceipt {
        invocation: founding,
        status: ReceiptStatus::Completed {
            output: tilo_actions::ActionOutput::default(),
        },
    }));
    let (answered, status) = answer(&mut answers_read);
    assert_eq!(answered, founding, "the call in flight is answered first");
    assert!(
        matches!(status, ReceiptStatus::Completed { .. }),
        "{status:?}"
    );
    let (handed, closed) = handing.join().unwrap();
    assert_eq!(
        (handed, closed),
        (1, 0),
        "the connection is handed on, never closed"
    );
    assert_eq!(first.asked().len(), 1);

    // The next part starts, and the call that waited is its.
    let second = Arc::new(TestProvider::new());
    native.serve_the_person(part(&second, &Arc::new(super::Connections::default())));
    let (answered, status) = answer(&mut answers_read);
    assert_eq!(answered, next_call, "the call that waited is answered");
    assert!(
        matches!(status, ReceiptStatus::Completed { .. }),
        "{status:?}"
    );
    assert_eq!(
        second
            .asked()
            .iter()
            .map(|asked| asked.invocation)
            .collect::<Vec<_>>(),
        vec![next_call],
        "the next part answers it"
    );
}
