//! Files' clipboard, as the session serves it to the app: the person copies a file in Files and
//! pastes it into another folder, the copy also lands on the desktop's clipboard so any other app
//! can take it, and a cut moves the file out of the folder it came from.
//! The app speaks exactly as Files does: one launcher-attested connection to the session's app wire,
//! its selected objects handed over on that connection's descriptor stream, and the commands it sends
//! naming them by descriptor number.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              lives until the app it serves is done"
)]

use std::os::fd::AsRawFd as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use runtime_collab::CollabClient;
use runtime_collab::RecordHandle;
use runtime_collab::file_clipboard::{
    Command, Conflict, ObjectIdentity, Offer, Reply, SelectedDescriptor,
};
use runtime_portal::DescriptorStreamSource as _;
use tilo_compositor::{OutputConfig, Selection};
use tilo_service::LoopWake;
use tilo_session::{AppSocket, Launches, Lists, Options, Session};

use crate::common::held::{Fixture, WORKSPACE, actor, fixture, layout, opened};
use ui::dnd::COPIED_FILES_MIME;

/// The app whose clipboard this is.
const FILES: &str = "so.tilo.file-explorer";
/// The file the person selects in Files.
const NAME: &str = "notes.txt";
/// Files' record in the workspace: the resource its folder actions name.
const RESOURCE: &str = "files.folder";
/// The screen the session draws its shell on.
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
const BODY: &[u8] = b"the file the person copied";
/// The session serving the apps it starts on `socket` , holding the fixture's workspace from its
/// state and showing its shell.
fn session(socket: &Path, fixture: &Fixture) -> crate::common::held::Hosted {
    // The app speaks from this test's own process, so the launch the session would have recorded
    // for it names that process.
    let launches = std::sync::Arc::new(Launches::default());
    launches.started(std::process::id(), FILES, WORKSPACE, true);
    let mut hosted = crate::common::held::hosted(
        Options {
            collab: false,
            wake: LoopWake::default(),
            app_socket: AppSocket::At(socket.to_path_buf()),
            person: "Ada".to_owned(),
            launches,
            state: Some(fixture.state.clone()),
            lists: Lists::default(),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    );
    hosted
        .session
        .handle()
        .replace_tree(layout(WORKSPACE, &actor(&fixture.cert.user_public_key)));
    for _ in 0..4 {
        hosted.session.step(Duration::from_millis(1)).expect("step");
    }
    hosted
}

/// A connection to the session's app wire as Files in the workspace, as the launcher hands an app one:
/// its writes carry the descriptors of the objects it selected.
fn connect(socket: &Path) -> CollabClient {
    let mut stream =
        std::os::unix::net::UnixStream::connect(socket).expect("the app wire's socket");
    launcher::services::write_collab_handoff(&mut stream, FILES, WORKSPACE)
        .expect("the launcher names the app on the connection");
    stream
        .set_nonblocking(true)
        .expect("the connection is ready for the runtime");
    let stream = tokio::net::UnixStream::from_std(stream).expect("a runtime stream");
    let (stream, sender) = stream.with_descriptors();
    let (reader, writer) = tokio::io::split(stream);
    let (client, carrying) = CollabClient::over(reader, writer);
    tokio::spawn(carrying);
    client.with_optional_descriptors(sender)
}

/// The channel key of Files' own route in the workspace.
fn channel() -> String {
    tilo_workspace_service::apps::route::CrdtRoute::app(FILES, WORKSPACE).channel_key()
}

/// What a person selected in Files, as the app hands it over: the path it is shown at, the
/// descriptor open in this process, and the object both name.
fn selected(path: &Path) -> (SelectedDescriptor, std::fs::File) {
    let file = std::fs::File::open(path).expect("the shown object");
    let metadata = std::fs::symlink_metadata(path).expect("the shown object's identity");
    let descriptor = SelectedDescriptor {
        path: path.as_os_str().as_bytes().to_vec(),
        descriptor: u32::try_from(file.as_raw_fd()).expect("a descriptor number"),
        expected: ObjectIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
    };
    (descriptor, file)
}

/// The app as the session knows it: its channel into the workspace, and the invocations the session
/// addresses back to it.
struct Files {
    mine: RecordHandle,
    invocations: runtime_collab::InvocationStream,
}

impl Files {
    async fn join(socket: &Path) -> Self {
        let client = connect(socket);
        let mine = opened(&client, &channel())
            .await
            .expect("the app the session launched joins the workspace's channel");
        let invocations = mine
            .subscribe_stable_invocations()
            .await
            .expect("the app listens for the invocations it runs");
        mine.publish_actions(&surface())
            .await
            .expect("Files publishes the actions it runs");
        Self { mine, invocations }
    }

    /// Ask the session for `command` , under the invocation it runs for: what Files passes on every
    /// clipboard command it makes.
    async fn command(&self, command: &Command, invocation: [u8; 16]) -> Reply {
        self.mine
            .local_file_clipboard_command(command, Some(invocation))
            .await
            .expect("the session serves Files' clipboard")
    }

    /// Have the session admit `action` of Files' own surface, as Files asks for every effect before
    /// it runs it: the invocation whose run the clipboard commands then continue.
    async fn admit(&self, action: &str) -> [u8; 16] {
        use tilo_actions::remote::{
            InvokeArgument, InvokeRequest, OWN_PROVIDER, ReceiptStatus, RemoteActionFrame,
            RemoteActionMessage, RemoteActionTarget,
        };
        let invocation = tilo_actions::InvocationId::new();
        let frame = RemoteActionFrame::new(RemoteActionMessage::Invoke(InvokeRequest {
            on_behalf_of: None,
            invocation,
            target: RemoteActionTarget {
                provider: OWN_PROVIDER.into(),
                address: tilo_actions::ActionAddress::new(RESOURCE, action),
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }))
        .encode()
        .expect("an invocation encodes");
        let answer = self
            .mine
            .remote_action(&frame)
            .await
            .expect("the session answers Files' invocation");
        let answer = RemoteActionFrame::decode(&answer).expect("the session answers a frame");
        let RemoteActionMessage::Receipt(receipt) = answer.message else {
            panic!("the session answers the invocation it was asked: {answer:?}");
        };
        assert_eq!(receipt.invocation, invocation, "the receipt names it");
        assert!(
            matches!(receipt.status, ReceiptStatus::Accepted),
            "the session admits the action Files runs: {:?}",
            receipt.status
        );
        // The session hands the app the run to carry out; it stays running while the app reports
        // what it does as that invocation.
        let run = self
            .invocations
            .read()
            .await
            .expect("the session addresses the run to the app");
        assert_eq!(run.action, action, "the run is the action asked for");
        run.invocation_uuid
    }
}

/// The surface Files publishes: the three actions whose effects paste and copy, each with the reach
/// its kind carries.
fn surface() -> Vec<runtime_collab::ExportedAction> {
    use runtime_collab::{ExportedAction, ExportedInput};
    use tilo_actions::{ActionContract, ActionKind, OperationKind};
    let action = |action: &str, kind: ActionKind| ExportedAction {
        did: None,
        resource: RESOURCE.into(),
        semantic_identity: None,
        action: action.into(),
        resource_kind: RESOURCE.into(),
        resource_label: "Folder".into(),
        action_label: action.into(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        state: tilo_actions::ActionState::default(),
        operation_kind: OperationKind::Effect,
        params: Vec::new(),
        contract: ActionContract {
            kind,
            ..ActionContract::default()
        },
        view: None,
        input: ExportedInput::None,
    };
    vec![
        action("copy-selection", ActionKind::Read),
        action("cut-selection", ActionKind::Outside),
        action("paste", ActionKind::Outside),
    ]
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Copy or cut the file in `source` , as Files publishes what the person selected: capture it,
/// publish the tickets, release them.
async fn publish(files: &Files, source: &Path, cut: bool) -> Offer {
    let invocation = files
        .admit(if cut {
            "cut-selection"
        } else {
            "copy-selection"
        })
        .await;
    let (selected, _open) = selected(source);
    let captured = files
        .command(
            &Command::Capture {
                sources: vec![selected],
                destination: None,
            },
            invocation,
        )
        .await;
    let Reply::Captured {
        sources: tickets,
        destination: None,
    } = captured
    else {
        panic!("the host captures the selected object: {captured:?}")
    };
    let reply = files
        .command(
            &Command::Publish {
                sources: tickets.clone(),
                cut,
            },
            invocation,
        )
        .await;
    let Reply::Published { offer } = reply else {
        panic!("the host publishes the selection: {reply:?}")
    };
    let released = files
        .command(&Command::Release { tickets }, invocation)
        .await;
    assert!(
        matches!(released, Reply::Released),
        "the host released its capture: {released:?}"
    );
    offer
}

/// Paste the offer `offer` into `folder` , as Files does: ask what the clipboard holds, hold the
/// folder, paste, and read the receipt.
async fn paste(files: &Files, offer: &str, folder: &Path) -> (usize, Option<String>) {
    let invocation = files.admit("paste").await;
    let snapshot = files.command(&Command::Snapshot, invocation).await;
    let Reply::Snapshot { offer: Some(held) } = snapshot else {
        panic!("the host reports the clipboard: {snapshot:?}")
    };
    assert_eq!(held.id, offer, "the clipboard holds the copy Files made");
    let (destination, _open) = selected(folder);
    let captured = files
        .command(
            &Command::Capture {
                sources: Vec::new(),
                destination: Some(destination),
            },
            invocation,
        )
        .await;
    let Reply::Captured {
        sources,
        destination: Some(ticket),
    } = captured
    else {
        panic!("the host holds the folder the paste goes into: {captured:?}")
    };
    assert!(sources.is_empty(), "a paste captures no new source");
    let operation = uuid::Uuid::new_v4().to_string();
    let started = files
        .command(
            &Command::Paste {
                offer: held.id.clone(),
                destination: ticket.clone(),
                operation: operation.clone(),
                conflict: Conflict::KeepBoth,
            },
            invocation,
        )
        .await;
    let Reply::Running { operation: running } = started else {
        panic!("the host starts the paste: {started:?}")
    };
    assert_eq!(
        running, operation,
        "the paste keeps the operation it was given"
    );
    let started_at = Instant::now();
    let settled = loop {
        match files
            .command(
                &Command::Observe {
                    operation: operation.clone(),
                },
                invocation,
            )
            .await
        {
            Reply::Running { .. } => {
                assert!(
                    started_at.elapsed() < tilo_test_env::hang_bound(),
                    "the paste never reported what it did"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Reply::Settled {
                operation: done,
                completed,
                error,
                undone,
            } => {
                assert_eq!(done, operation, "the receipt names the transfer");
                assert!(!undone, "a paste is not an undone transfer");
                break (completed, error);
            }
            other => panic!("the host reports the transfer: {other:?}"),
        }
    };
    let released = files
        .command(
            &Command::Release {
                tickets: vec![ticket],
            },
            invocation,
        )
        .await;
    assert!(
        matches!(released, Reply::Released),
        "the host released the folder: {released:?}"
    );
    settled
}

/// What Files' own calls produced, for the session to check against the disk.
struct Proof {
    /// The copy Files published.
    offer: Offer,
    /// The receipt of pasting it into another folder.
    pasted: (usize, Option<String>),
    /// The file in that folder afterwards.
    copied: Vec<u8>,
    /// Whether the file the person copied is still where they copied it from.
    source_after_copy: bool,
    /// The offer a cut published.
    cut_offer: Offer,
    /// The receipt of pasting the cut into a third folder.
    cut_pasted: (usize, Option<String>),
    /// The file in that folder afterwards.
    moved: Vec<u8>,
    /// Whether the file the person cut is still where it was.
    source_after_cut: bool,
}

/// What the app reports while it works: a copy it published (so the session can be asked what the
/// desktop's clipboard then holds), or everything it saw.
enum Note {
    /// A copy or cut went on the desktop's clipboard, naming `path` .
    Published {
        path: PathBuf,
        cut: bool,
    },
    Done(Proof),
}

/// Files' own calls: copy the file and paste it into another folder, then cut it and paste it into
/// a third.
fn files_calls(
    socket: &Path,
    folders: &(PathBuf, PathBuf, PathBuf),
    notes: &std::sync::mpsc::Sender<Note>,
    checked: &std::sync::mpsc::Receiver<()>,
) {
    let (documents, copied_folder, cut_folder) = folders;
    let source = documents.join(NAME);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    let proof = runtime.block_on(async {
        let files = Files::join(socket).await;
        let offer = publish(&files, &source, false).await;
        assert_eq!(offer.count, 1, "the copy holds the one file");
        assert!(!offer.cut, "a copy is not a cut");
        let _ = notes.send(Note::Published {
            path: source.clone(),
            cut: false,
        });
        // The next copy replaces this one on the clipboard: only once the session has read it does
        // the app go on.
        wait_for_check(checked);
        let pasted = paste(&files, &offer.id, copied_folder).await;
        let copied = read(&copied_folder.join(NAME));
        let source_after_copy = source.exists();
        let cut_offer = publish(&files, &source, true).await;
        assert!(cut_offer.cut, "a cut is published as one");
        let _ = notes.send(Note::Published {
            path: source.clone(),
            cut: true,
        });
        wait_for_check(checked);
        let cut_pasted = paste(&files, &cut_offer.id, cut_folder).await;
        let moved = read(&cut_folder.join(NAME));
        let source_after_cut = source.exists();
        Proof {
            offer,
            pasted,
            copied,
            source_after_copy,
            cut_offer,
            cut_pasted,
            moved,
            source_after_cut,
        }
    });
    let _ = notes.send(Note::Done(proof));
}

/// Hold the app at `check` until the session has read the clipboard it wrote.
fn wait_for_check(check: &std::sync::mpsc::Receiver<()>) {
    check
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("the session read the clipboard the app put on it");
}

/// What another app reading the desktop's clipboard gets: the files the copy put there, read back
/// through the same clipboard it would read.
fn read_clipboard(session: &mut Session) -> ui::dnd::CopiedFiles {
    let mime = COPIED_FILES_MIME.to_owned();
    let mut pull = session
        .compositor()
        .pull_selection(Selection::Clipboard, &mime)
        .expect("the clipboard offers its files to any app");
    let started = Instant::now();
    loop {
        match pull.drain() {
            tilo_compositor::clipboard_history::Pulled::Ready(bytes) => {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                return ui::dnd::copied_files(&mime, &text);
            }
            tilo_compositor::clipboard_history::Pulled::Pending => {
                assert!(
                    started.elapsed() < tilo_test_env::hang_bound(),
                    "the clipboard never answered another app"
                );
                session.step(Duration::from_millis(1)).expect("step");
            }
            tilo_compositor::clipboard_history::Pulled::Lost => {
                panic!("the copy was taken off the clipboard before it was read")
            }
        }
    }
}

/// The session's pass until the app is done, as the session runs its loop: what it proved, or the
/// app's own ending tells why it left.
fn until_the_app_is_done(
    session: &mut Session,
    notes: &std::sync::mpsc::Receiver<Note>,
    checked: &std::sync::mpsc::Sender<()>,
) -> Proof {
    use std::sync::mpsc::TryRecvError;
    let started = Instant::now();
    while started.elapsed() < tilo_test_env::hang_bound() {
        session.settle_services();
        session.step(Duration::from_millis(1)).expect("step");
        match notes.try_recv() {
            Ok(Note::Published { path, cut }) => {
                // The copy reaches the desktop's clipboard on the session's next passes: what any
                // other app pastes from.
                for _ in 0..4 {
                    session.step(Duration::from_millis(1)).expect("step");
                }
                assert!(
                    session
                        .compositor()
                        .selection_formats(Selection::Clipboard)
                        .iter()
                        .any(|format| format == COPIED_FILES_MIME),
                    "the copy Files made is on the desktop's clipboard"
                );
                // Another app pasting it takes exactly what Files copied,
                // and knows whether it is a move.
                let copied = read_clipboard(session);
                assert_eq!(copied.paths, [path], "the clipboard names the file");
                assert_eq!(copied.cut, cut, "the clipboard says a cut is a move");
                let _ = checked.send(());
            }
            Ok(Note::Done(proof)) => return proof,
            Err(TryRecvError::Disconnected) => panic!("the app left without answering"),
            Err(TryRecvError::Empty) => {}
        }
    }
    panic!(
        "the app's calls were never answered within {:?}",
        tilo_test_env::hang_bound()
    );
}

#[test]
fn files_copies_and_cuts_through_the_session_and_pastes_into_another_folder() {
    let home = crate::common::private_home();
    let socket = home.path().join("collab.sock");
    let fixture = fixture();
    let mut session = session(&socket, &fixture);
    let folders = folders(&fixture);
    std::fs::write(folders.0.join(NAME), BODY).expect("the file the person selects");

    let (notes, received) = std::sync::mpsc::channel();
    let (checked, check) = std::sync::mpsc::channel();
    let app = std::thread::spawn(move || {
        files_calls(&socket, &folders, &notes, &check);
    });
    let proof = until_the_app_is_done(&mut session, &received, &checked);
    if let Err(panic) = app.join() {
        std::panic::resume_unwind(panic);
    }

    assert_eq!(
        proof.pasted,
        (1, None),
        "the copy landed whole: {:?}",
        proof.pasted.1
    );
    assert_eq!(proof.copied, BODY, "the pasted file is the file copied");
    assert!(
        proof.source_after_copy,
        "a copy leaves the file it copied where it was"
    );
    assert_eq!(
        proof.cut_pasted,
        (1, None),
        "the cut landed whole: {:?}",
        proof.cut_pasted.1
    );
    assert_eq!(proof.moved, BODY, "the moved file is the file cut");
    assert!(
        !proof.source_after_cut,
        "a cut takes the file out of the folder it came from"
    );

    // The cut's offer is its own, and a cut is not what a copy is.
    assert_ne!(proof.cut_offer.id, proof.offer.id);
    assert_eq!(proof.cut_offer.count, 1);
}

/// The folders a person works in: where the file is, and the two folders they paste it into.
fn folders(fixture: &Fixture) -> (PathBuf, PathBuf, PathBuf) {
    let root = fixture.root.path().join("files");
    let source = root.join("documents");
    let copied = root.join("archive");
    let cut = root.join("sent");
    for folder in [&source, &copied, &cut] {
        std::fs::create_dir_all(folder).expect("a folder");
    }
    (source, copied, cut)
}
