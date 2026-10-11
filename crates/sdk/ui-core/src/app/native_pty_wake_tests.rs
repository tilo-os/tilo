//! Real PTY bytes and published operations through the native idle-model seam.
use super::*;
use automerge::AutoCommit;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const TYPE: &str = "test.native-pty";
#[derive(Clone, Debug, Serialize, Deserialize)]
enum PtyAction {
    Run,
    Read,
}
impl QAction for PtyAction {
    fn apply(&self, _: &mut Doc) -> Result<(), ActionError> {
        Ok(())
    }
}

struct PtyRoot {
    child: std::process::Child,
    writer: std::fs::File,
    reader: Option<std::thread::JoinHandle<()>>,
    chunks: mpsc::Receiver<Vec<u8>>,
    queued: String,
    runs: Rc<std::cell::Cell<usize>>,
    updates: Rc<std::cell::Cell<usize>>,
}
impl Drop for PtyRoot {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.reader.take().unwrap().join().unwrap();
    }
}
impl PtyRoot {
    fn open(
        witness: Arc<Mutex<Vec<u8>>>,
        runs: Rc<std::cell::Cell<usize>>,
        updates: Rc<std::cell::Cell<usize>>,
    ) -> Self {
        use std::os::unix::process::CommandExt;
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: valid output descriptors; no termios or name buffers requested.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &raw mut master,
                    &raw mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                )
            },
            0
        );
        // SAFETY: both owned descriptors are valid and must not leak across exec.
        unsafe {
            libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(slave, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        // SAFETY: openpty returned unique descriptors whose ownership transfers here.
        let writer = unsafe { std::fs::File::from_raw_fd(master) };
        let slave = unsafe { std::fs::File::from_raw_fd(slave) };
        let mut command = std::process::Command::new("/bin/bash");
        command
            .args(["--noprofile", "--norc", "-i"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("PS1", "NATIVE_PTY> ")
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave);
        // SAFETY: only async-signal-safe syscalls run between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        let mut input = writer.try_clone().unwrap();
        let wake = crate::WakeHandle::new();
        let (sender, chunks) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut bytes = [0; 4096];
            while let Ok(count) = input.read(&mut bytes) {
                if count == 0 {
                    break;
                }
                witness.lock().unwrap().extend_from_slice(&bytes[..count]);
                if sender.send(bytes[..count].to_vec()).is_err() {
                    break;
                }
                wake.wake();
            }
        });
        Self {
            child,
            writer,
            reader: Some(reader),
            chunks,
            queued: String::new(),
            runs,
            updates,
        }
    }
}
impl Widget for PtyRoot {
    crate::provides!(dyn crate::resources::PublishesResources);
    fn sync(&mut self, _: &AppModel, _: &ModelPath) {}
    fn update(&mut self, _: &AppModel, _: &ModelPath) -> bool {
        self.updates.set(self.updates.get() + 1);
        let prior = self.queued.len();
        for bytes in self.chunks.try_iter() {
            self.queued.push_str(std::str::from_utf8(&bytes).unwrap());
        }
        self.queued.len() != prior
    }
    fn settle(&mut self, doc: &mut crate::widget::Edits<'_>, _: &automerge::ObjId) {
        doc.document_mut()
            .put(ROOT, "pty-output", self.queued.as_str())
            .unwrap();
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        _: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.new_leaf(taffy::Style::default()).unwrap()
    }
    fn draw(&mut self, _: &mut Scene, _: Affine, _: &mut TextRenderer, _: &Theme, _: &PresenceCtx) {
        panic!("hidden PTY progress must not paint");
    }
    fn name(&self) -> &'static str {
        "native-pty"
    }
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[TYPE]
    }
    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        _: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        op.check()?;
        let (request, _) = op.with_custody();
        if request.type_id != TYPE {
            return Ok(None);
        }
        let action: PtyAction =
            serde_json::from_slice(request.payload).map_err(|error| error.to_string())?;
        let value = match action {
            PtyAction::Run => {
                self.runs.set(self.runs.get() + 1);
                self.writer
                    .write_all(b"printf '\nNATIVE_PTY_MARKER\n'\r")
                    .map_err(|error| error.to_string())?;
                None
            }
            PtyAction::Read => Some(
                serde_json::json!({"text": doc.get(ROOT, "pty-output").unwrap().and_then(|(value,_)| value.as_str().map(str::to_owned)).unwrap_or_default()}),
            ),
        };
        Ok(Some(crate::widget::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput {
                value,
                resources: vec![],
            },
        )))
    }
}
impl crate::resources::PublishesResources for PtyRoot {
    fn resources(&self, _: &AutoCommit) -> Vec<Resource> {
        vec![
            Resource::new(TYPE, TYPE, "PTY")
                .with_action(
                    ActionSpec::operation(
                        "run",
                        "Run",
                        OperationKind::Effect,
                        TYPE,
                        &PtyAction::Run,
                    )
                    .unwrap(),
                )
                .with_action(
                    ActionSpec::operation(
                        "read",
                        "Read",
                        OperationKind::View,
                        TYPE,
                        &PtyAction::Read,
                    )
                    .unwrap()
                    .with_contract(tilo_actions::ActionContract::view())
                .returns(tilo_actions::ResultSpec::Json { schema: serde_json::json!({"type":"object", "required":["text"], "properties":{"text":{"type":"string"}}}) }).reads_document(),
                ),
        ]
    }
}

#[test]
fn native_idle_wake_publishes_real_pty_output_without_any_frame() {
    let witness = Arc::new(Mutex::new(Vec::new()));
    let runs = Rc::new(std::cell::Cell::new(0));
    let updates = Rc::new(std::cell::Cell::new(0));
    let mut app = App::from_factory_with_controlled_inputs(State::new(), || {
        Box::new(PtyRoot::open(
            witness.clone(),
            runs.clone(),
            updates.clone(),
        ))
    });
    let (sender, events) = mpsc::channel();
    app.wake_scope
        .as_mut()
        .unwrap()
        .bind(move |event| sender.send(event).unwrap())
        .unwrap();
    app.action_registry_mut().register::<PtyAction>(TYPE);
    let sent = Rc::new(RefCell::new(vec![]));
    app.collaboration.manager =
        CollaborationManager::with_peer_transport(HostedModelTransport(sent.clone()));
    app.init_layout();
    app.sync_model(true);
    app.publish_action_surface();
    let actor = app.collaboration.local_author.0.clone();
    let invocation = tilo_actions::InvocationId::new();
    app.invoke_published_action(
        9601,
        invocation,
        TYPE.into(),
        "run".into(),
        None,
        &actor,
        None,
    );
    assert_eq!(
        runs.get(),
        1,
        "the published Run must reach the actual PTY once"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !String::from_utf8_lossy(&witness.lock().unwrap())
        .contains("\r\nNATIVE_PTY_MARKER\r\nNATIVE_PTY> ")
    {
        assert!(
            Instant::now() < deadline,
            "actual Bash PTY bytes did not arrive"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    // Actual reader-originated wake events, same acceptance and idle phase as native_runner.
    for event in events.try_iter() {
        assert!(app.accept_wake(&event));
    }
    let _entry = app.enter_wake_scope();
    app.service_native_idle_model();
    let read = tilo_actions::InvocationId::new();
    app.invoke_published_action(9602, read, TYPE.into(), "read".into(), None, &actor, None);
    let text = sent
        .borrow()
        .iter()
        .find_map(|request| match request {
            CollabRequest::UpdateInvocation {
                update: tilo_actions::InvocationUpdate::Succeeded { invocation, output },
                ..
            } if *invocation == read => output
                .value
                .as_ref()
                .and_then(|value| value["text"].as_str().map(str::to_owned)),
            _ => None,
        })
        .expect("the original published Read must return its actual shared output");
    assert!(
        text.lines().any(|line| line == "NATIVE_PTY_MARKER"),
        "native idle servicing left actual PTY output unrecorded while no frame was available"
    );
    assert!(
        text.ends_with("NATIVE_PTY> "),
        "actual prompt must return on the same PTY"
    );
    assert_eq!(runs.get(), 1);
    let count = updates.get();
    for _ in 0..10 {
        app.service_native_idle_model();
    }
    assert_eq!(
        updates.get(),
        count,
        "idle passes must not update without another model wake"
    );
}

struct IdleCounts {
    updates: Rc<std::cell::Cell<usize>>,
    syncs: Rc<std::cell::Cell<usize>>,
    wake_during_update: Rc<std::cell::Cell<bool>>,
    wake: crate::WakeHandle,
}
impl Widget for IdleCounts {
    crate::testing::inert_widget_methods!(
        unregistered_leaf_layout,
        apply_layout,
        draw,
        get_rect,
        name = "idle-counts"
    );
    fn update(&mut self, _: &AppModel, _: &ModelPath) -> bool {
        self.updates.set(self.updates.get() + 1);
        if self.wake_during_update.replace(false) {
            self.wake.wake();
        }
        false
    }
    fn sync(&mut self, _: &AppModel, _: &ModelPath) {
        self.syncs.set(self.syncs.get() + 1);
    }
}

#[test]
fn native_idle_services_only_unconsumed_own_model_wakes() {
    let updates = Rc::new(std::cell::Cell::new(0));
    let syncs = Rc::new(std::cell::Cell::new(0));
    let wake_during_update = Rc::new(std::cell::Cell::new(false));
    let mut app = App::from_factory_with_controlled_inputs(State::new(), || {
        Box::new(IdleCounts {
            updates: updates.clone(),
            syncs: syncs.clone(),
            wake_during_update: wake_during_update.clone(),
            wake: crate::WakeHandle::new(),
        })
    });
    let (tx, rx) = mpsc::channel();
    app.wake_scope
        .as_mut()
        .unwrap()
        .bind(move |event| {
            tx.send(event).unwrap();
        })
        .unwrap();
    let wake = app.wake_scope.as_ref().unwrap().handle();
    app.update_native_model();
    app.sync_model(false);
    let initial = (updates.get(), syncs.get());
    app.service_native_idle_model();
    assert_eq!((updates.get(), syncs.get()), initial);

    wake.notify_events();
    assert!(app.accept_wake(&rx.recv().unwrap()));
    app.service_native_idle_model();
    assert_eq!(
        (updates.get(), syncs.get()),
        initial,
        "notification-only wake must not sync"
    );

    wake.wake();
    assert!(app.accept_wake(&rx.recv().unwrap()));
    // The same production model boundary used by RedrawRequested/deadlines.
    app.update_native_model();
    app.sync_model(false);
    let visible = (updates.get(), syncs.get());
    app.service_native_idle_model();
    assert_eq!(
        (updates.get(), syncs.get()),
        visible,
        "a serviced visible frame must not sync again at idle"
    );

    wake.wake();
    assert!(app.accept_wake(&rx.recv().unwrap()));
    app.service_native_idle_model();
    assert_eq!((updates.get(), syncs.get()), (visible.0 + 1, visible.1 + 1));
    let serviced = (updates.get(), syncs.get());
    app.service_native_idle_model();
    assert_eq!((updates.get(), syncs.get()), serviced);

    let (other_tx, other_rx) = mpsc::channel();
    let mut other = crate::WakeScope::new();
    other
        .bind(move |event| {
            other_tx.send(event).unwrap();
        })
        .unwrap();
    other.handle().wake();
    assert!(!app.accept_wake(&other_rx.recv().unwrap()));
    app.service_native_idle_model();
    assert_eq!((updates.get(), syncs.get()), serviced);

    wake.wake();
    assert!(app.accept_wake(&rx.recv().unwrap()));
    wake_during_update.set(true);
    app.update_native_model();
    app.sync_model(false);
    let during = (updates.get(), syncs.get());
    assert!(app.accept_wake(&rx.recv().unwrap()));
    app.service_native_idle_model();
    assert_eq!(
        (updates.get(), syncs.get()),
        (during.0 + 1, during.1 + 1),
        "a wake raised during the visible drain must survive for idle"
    );
    let serviced = (updates.get(), syncs.get());

    wake.wake();
    let stale = rx.recv().unwrap();
    drop(app.wake_scope.take());
    assert!(!app.accept_wake(&stale));
    app.service_native_idle_model();
    assert_eq!(
        (updates.get(), syncs.get()),
        serviced,
        "closed owner must not be serviced"
    );
}
