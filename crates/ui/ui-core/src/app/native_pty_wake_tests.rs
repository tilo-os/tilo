use super::*;
use automerge::AutoCommit;
use std::cell::Cell;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::sync::{Arc, Mutex, mpsc};
use tilo_test_env::SpawnUnderEnvLock as _;

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
    runs: Rc<Cell<usize>>,
    updates: Rc<Cell<usize>>,
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
        observation: Arc<Mutex<Vec<u8>>>,
        runs: Rc<Cell<usize>>,
        updates: Rc<Cell<usize>>,
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
        let child = command.spawn_locked().unwrap();
        let mut input = writer.try_clone().unwrap();
        let wake = crate::WakeHandle::new();
        let (sender, chunks) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut bytes = [0; 4096];
            while let Ok(count) = input.read(&mut bytes) {
                if count == 0 {
                    break;
                }
                observation
                    .lock()
                    .unwrap()
                    .extend_from_slice(&bytes[..count]);
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
    tilo_ui_base::provides!(
        dyn tilo_ui_base::widget::resources::PublishesResources,
        dyn tilo_ui_base::widget::ResourceOperations
    );
    fn sync(&mut self, _: &AppModel, _: &ModelPath) {}
    fn update(&mut self, _: &AppModel, _: &ModelPath) -> bool {
        self.updates.set(self.updates.get() + 1);
        let prior = self.queued.len();
        for bytes in self.chunks.try_iter() {
            self.queued.push_str(std::str::from_utf8(&bytes).unwrap());
        }
        self.queued.len() != prior
    }
    fn settle(&mut self, doc: &mut tilo_ui_base::widget::Edits<'_>, _: &automerge::ObjId) {
        doc.document_mut()
            .put(ROOT, "pty-output", self.queued.as_str())
            .unwrap();
    }
    crate::testing::inert_widget_methods!(unregistered_leaf_layout);
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (_, _, _, _, mut env) = ctx.parts();
        panic!("hidden PTY progress must not paint");
    }
    fn name(&self) -> &'static str {
        "native-pty"
    }
}

impl tilo_ui_base::widget::ResourceOperations for PtyRoot {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[TYPE]
    }

    fn start_operation(
        &mut self,
        op: &tilo_ui_base::widget::ResourceOperation<'_>,
        doc: &mut tilo_ui_base::widget::Edits<'_>,
        _: &automerge::ObjId,
    ) -> Result<Option<tilo_ui_base::widget::ResourceOperationStart>, String> {
        let request = op.request();
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
        Ok(Some(
            tilo_ui_base::widget::ResourceOperationStart::Completed(tilo_actions::ActionOutput {
                value,
                resources: vec![],
            }),
        ))
    }
}
impl tilo_ui_base::widget::resources::PublishesResources for PtyRoot {
    fn resources(&self, _: &AutoCommit) -> Vec<ResourceSpec> {
        vec![
            ResourceSpec::new(TYPE, TYPE, "PTY")
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
    let observation = Arc::new(Mutex::new(Vec::new()));
    let runs = Rc::new(Cell::new(0));
    let updates = Rc::new(Cell::new(0));
    let mut app = App::from_factory_with_controlled_inputs(State::new(), || {
        Box::new(PtyRoot::open(
            observation.clone(),
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
    let actor = tilo_actions::Actor::Member(app.local_author.clone());
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
    tilo_test_env::wait_until("actual Bash PTY bytes", || {
        String::from_utf8_lossy(&observation.lock().unwrap())
            .contains("\r\nNATIVE_PTY_MARKER\r\nNATIVE_PTY> ")
    });
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
