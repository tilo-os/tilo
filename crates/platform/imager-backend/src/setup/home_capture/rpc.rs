//! Private app/broker RPC; file authority travels only as a held directory FD.
use super::super::portable_transfer::ArchiveProgress;
use super::super::{migration::PinnedDirectory, migration_store::NativePath};
use super::{
    Command, Response, failure,
    transport::{HostChannel, WorkerChannel},
    worker,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io,
    os::{fd::AsRawFd, unix::net::UnixStream},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};
use zeroize::Zeroizing;
const RPC_PROTOCOL: u32 = 2;
fn validate_protocol(schema: u32) -> io::Result<()> {
    if schema != RPC_PROTOCOL {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "capture RPC protocol version is unsupported",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Rendezvous(runtime_portal::request_rendezvous::Rendezvous);

pub struct Listener {
    inner: runtime_portal::request_rendezvous::Listener,
}

pub struct RequestDirectoryPin(pub(in crate::setup) Arc<PinnedDirectory>);
impl runtime_portal::request_rendezvous::DirectoryPin for RequestDirectoryPin {
    fn file(&self) -> &File {
        &self.0.file
    }
    fn readable_file(&self) -> io::Result<File> {
        super::super::migration_durability::readable_directory(&self.0.file)
    }
    fn validate(&self) -> io::Result<()> {
        self.0.validate()
    }
    fn validate_destination(&self) -> io::Result<()> {
        self.0.validate_destination()
    }
}
impl Rendezvous {
    pub fn create(directory: &Path) -> io::Result<(Self, Listener)> {
        let parent = Arc::new(RequestDirectoryPin(PinnedDirectory::open_absolute(
            directory,
        )?));
        let (rendezvous, inner) =
            runtime_portal::request_rendezvous::Rendezvous::create(directory, parent)?;
        Ok((Self(rendezvous), Listener { inner }))
    }
    fn connect(&self, directory: &Path, caller: u32) -> io::Result<UnixStream> {
        let parent = RequestDirectoryPin(PinnedDirectory::open_absolute(directory)?);
        self.0.connect(directory, &parent, caller)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MountedInput {
    placement: NativePath,
    kind: super::super::migration_handoff::EntryKind,
}
impl MountedInput {
    fn validate_all(entries: &[Self], generation: uuid::Uuid) -> io::Result<()> {
        if generation.is_nil() || entries.is_empty() || entries.len() > 256 {
            return Err(failure("Mounted input generation/count invalid"));
        }
        let mut prior: Option<std::path::PathBuf> = None;
        for entry in entries {
            let path = entry.placement.decode().map_err(io::Error::other)?;
            super::super::migration_handoff::relative(&path)?;
            use std::os::unix::ffi::OsStrExt;
            if path
                .as_os_str()
                .as_bytes()
                .split(|b| *b == b'/')
                .any(|part| part.is_empty() || part == b"." || part == b".." || part.contains(&0))
            {
                return Err(failure("Mounted input path is not canonical"));
            }
            if path.as_os_str().len() > 4096
                || prior
                    .as_ref()
                    .is_some_and(|previous| previous >= &path || path.starts_with(previous))
            {
                return Err(failure(
                    "Mounted input placement order/traversal/overlap invalid",
                ));
            }
            prior = Some(path);
        }
        Ok(())
    }
    pub(super) fn from_granted(root: &super::super::GrantedRoot<'_>) -> Self {
        Self {
            placement: NativePath::encode(root.placement()),
            kind: match root.kind() {
                super::super::RootKind::Directory => {
                    super::super::migration_handoff::EntryKind::Directory
                }
                super::super::RootKind::File => super::super::migration_handoff::EntryKind::File,
            },
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Action {
    Run {
        command: Command,
        has_output: bool,
    },
    Validate,
    RegisterOriginal,
    RegisterRetainedCaller,
    RegisterOriginalMounted {
        generation: uuid::Uuid,
        entries: Vec<MountedInput>,
    },
    Cancel,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    schema: u32,
    pub(super) operation: String,
    sequence: u64,
    pub action: Action,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Reply {
    Response {
        response: Response,
    },
    Validated,
    OriginalRegistered {
        witness: super::SourceWitness,
        original_caller: super::OriginalCallerFacts,
    },
    RetainedCallerRegistered {
        declaration: super::RetainedCallerDeclaration,
    },
    MountedOriginalRegistered {
        generation: uuid::Uuid,
        witness: super::store::MountedWitness,
        original_caller: super::OriginalCallerFacts,
    },
    Released,
    Failed,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case", deny_unknown_fields)]
enum Message {
    Progress { progress: ArchiveProgress },
    Reply { reply: Reply },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    schema: u32,
    operation: String,
    sequence: u64,
    message: Message,
}

pub struct Incoming {
    pub call: Call,
    pub output: Option<File>,
    pub retained_declaration: Option<super::RetainedCallerDeclaration>,
    pub mounted: Option<(uuid::Uuid, super::store::MountedWitness)>,
    registration: Option<super::store::original::mounted::MountedRegistration>,
}
pub struct RootSession {
    writer: UnixStream,
    pub incoming: mpsc::Receiver<io::Result<Incoming>>,
    active: Option<(String, u64)>,
    registration: Option<(
        uuid::Uuid,
        super::store::original::mounted::MountedRegistration,
    )>,
    cancel: Arc<AtomicBool>,
}
impl RootSession {
    pub(super) fn connect(
        rendezvous: &Rendezvous,
        directory: &Path,
        caller: u32,
        operation: &str,
        cancel: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        Self::connect_admitted(
            rendezvous,
            directory,
            caller,
            operation,
            cancel,
            None,
            None,
            |_| Ok(()),
        )
        .map(|(session, ())| session)
    }
    pub(in crate::setup) fn connect_original(
        rendezvous: &Rendezvous,
        directory: &Path,
        caller: u32,
        selection: &super::super::home_capture_helper::OriginalRequest,
        cancel: Arc<AtomicBool>,
    ) -> io::Result<(Self, (super::SourceWitness, super::OriginalCallerFacts))> {
        let home = selection.home.decode().map_err(io::Error::other)?;
        if selection.plan.home_uid() != caller {
            return Err(failure(
                "Original source owner differs from admitted caller",
            ));
        }
        Self::connect_admitted(
            rendezvous,
            directory,
            caller,
            &selection.plan.operation_id,
            cancel,
            Some((home.clone(), selection.plan.clone())),
            None,
            |peer| {
                let store = super::Store::system(&selection.plan.operation_id)?;
                let inventory = selection.admitted_inventory(directory, caller)?;
                let registered = if let Some(inventory) = inventory.as_ref() {
                    store.register_original_selection_with_inventory(
                        &home,
                        &selection.plan,
                        peer,
                        selection.preparation.as_ref(),
                        Some(inventory),
                    )?
                } else {
                    store.register_original_selection_with_preparation(
                        &home,
                        &selection.plan,
                        peer,
                        selection.preparation.as_ref(),
                    )?
                };
                Ok((registered.witness()?, registered.original_caller().clone()))
            },
        )
    }
    pub(in crate::setup) fn connect_retained(
        selection: &super::super::home_capture_helper::RetainedRequest,
        directory: &Path,
        caller: u32,
        cancel: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        selection.validate()?;
        if selection.plan.home_uid() != caller {
            return Err(failure(
                "Retained caller differs from selected original Home",
            ));
        }
        Self::connect_admitted(
            &selection.channel,
            directory,
            caller,
            &selection.plan.operation_id,
            cancel,
            None,
            Some(selection.clone()),
            |_| Ok(()),
        )
        .map(|(session, ())| session)
    }
    fn connect_admitted<T>(
        rendezvous: &Rendezvous,
        directory: &Path,
        caller: u32,
        operation: &str,
        cancel: Arc<AtomicBool>,
        original: Option<(std::path::PathBuf, super::super::migration_handoff::Plan)>,
        retained: Option<super::super::home_capture_helper::RetainedRequest>,
        admission: impl FnOnce(&HostChannel) -> io::Result<T>,
    ) -> io::Result<(Self, T)> {
        let socket = rendezvous.connect(directory, caller)?;
        let mut channel = HostChannel::authenticate(socket, rendezvous.0.caller_pid(), caller)?;
        channel.authenticate_as_root()?;
        let admitted = admission(&channel)?;
        let writer = channel.stream().try_clone()?;
        channel.stream().set_read_timeout(None)?;
        let (send, incoming) = mpsc::sync_channel(1);
        let operation = operation.to_owned();
        let session_cancel = cancel.clone();
        std::thread::spawn(move || {
            let mut sequence = 0u64;
            loop {
                let event = (|| {
                    let bytes = Zeroizing::new(worker::read_frame(channel.stream(), 64 * 1024)?);
                    let call: Call = serde_json::from_slice(&bytes)?;
                    validate_protocol(call.schema)?;
                    if call.operation != operation || call.sequence != sequence {
                        return Err(failure("capture RPC session binding changed"));
                    }
                    sequence = sequence
                        .checked_add(1)
                        .ok_or_else(|| failure("capture RPC sequence overflow"))?;
                    let output = if matches!(
                        &call.action,
                        Action::Run {
                            has_output: true,
                            ..
                        }
                    ) {
                        Some(channel.receive(2)?)
                    } else {
                        None
                    };
                    let retained_declaration =
                        if matches!(&call.action, Action::RegisterRetainedCaller) {
                            let selected = retained.as_ref().ok_or_else(|| {
                                failure("This RPC route has no retained source grant")
                            })?;
                            let input = channel.receive_retained_home().map_err(|error| {
                                io::Error::new(
                                    error.kind(),
                                    format!("retained source descriptor: {error}"),
                                )
                            })?;
                            let home = selected.home.decode().map_err(io::Error::other)?;
                            Some(
                                super::Store::system(&selected.plan.operation_id)?
                                    .register_retained_caller(
                                        (&home, &input),
                                        &selected.plan,
                                        &channel,
                                        &selected.setup_binding,
                                        &selected.identity_public_key,
                                    )
                                    .map_err(|error| {
                                        io::Error::new(
                                            error.kind(),
                                            format!("retained source registration: {error}"),
                                        )
                                    })?,
                            )
                        } else {
                            None
                        };
                    let mut registration = None;
                    let mounted = if let Action::RegisterOriginalMounted {
                        generation,
                        entries,
                    } = &call.action
                    {
                        let (home, plan) = original.as_ref().ok_or_else(|| {
                            failure("Captured route refuses mounted registration")
                        })?;
                        MountedInput::validate_all(entries, *generation)?;
                        let mut inputs = Vec::with_capacity(entries.len());
                        for entry in entries {
                            super::super::migration::check_cancel(&cancel)?;
                            let directory =
                                entry.kind == super::super::migration_handoff::EntryKind::Directory;
                            inputs.push((
                                entry.placement.clone(),
                                entry.kind,
                                channel.receive_mounted_input(3, directory)?,
                            ));
                        }
                        let store = Arc::new(super::Store::system(&plan.operation_id)?);
                        let held = store.register_transferred_mounted_roots(
                            home, plan, inputs, &channel, &cancel,
                        )?;
                        super::super::migration::check_cancel(&cancel)?;
                        let witness = held.witness().clone();
                        registration = Some(held);
                        Some((*generation, witness))
                    } else {
                        None
                    };
                    Ok(Incoming {
                        call,
                        output,
                        retained_declaration,
                        mounted,
                        registration,
                    })
                })();
                let failed = event.is_err();
                if send.try_send(event).is_err() || failed {
                    break;
                }
            }
            cancel.store(true, Ordering::Release);
        });
        Ok((
            Self {
                writer,
                incoming,
                active: None,
                registration: None,
                cancel: session_cancel,
            },
            admitted,
        ))
    }
    pub(in crate::setup) fn begin(&mut self, incoming: &mut Incoming) -> io::Result<()> {
        let call = &incoming.call;
        if self.active.is_some() {
            return Err(failure("capture RPC command already active"));
        }
        if let Action::RegisterOriginalMounted { generation, .. } = &call.action {
            let held = incoming
                .registration
                .take()
                .ok_or_else(|| failure("Mounted RPC custody missing"))?;
            let expected = incoming
                .mounted
                .as_ref()
                .ok_or_else(|| failure("Mounted RPC witness missing"))?;
            if expected.0 != *generation
                || expected.1 != *held.witness()
                || held.witness().operation() != call.operation
            {
                return Err(failure(
                    "Mounted RPC witness differs from retained authority",
                ));
            }
            held.validate_ack(&self.cancel)?;
            self.registration = Some((*generation, held));
        } else if incoming.registration.is_some() || incoming.mounted.is_some() {
            return Err(failure("Unexpected mounted RPC custody"));
        }
        self.active = Some((call.operation.clone(), call.sequence));
        Ok(())
    }
    fn send(&mut self, message: Message) -> io::Result<()> {
        let (operation, sequence) = self
            .active
            .as_ref()
            .ok_or_else(|| failure("capture RPC has no active command"))?;
        worker::write_frame(
            &mut self.writer,
            &serde_json::to_vec(&Frame {
                schema: RPC_PROTOCOL,
                operation: operation.clone(),
                sequence: *sequence,
                message,
            })?,
            16 * 1024,
        )
    }
    pub(in crate::setup) fn progress(&mut self, progress: ArchiveProgress) -> io::Result<()> {
        self.send(Message::Progress { progress })
    }
    pub(in crate::setup) fn reply(&mut self, reply: &Reply) -> io::Result<()> {
        let result = (|| {
            if let Some((generation, held)) = &self.registration {
                match reply {
                    Reply::MountedOriginalRegistered {
                        generation: returned,
                        witness,
                        original_caller,
                    } if returned == generation
                        && witness == held.witness()
                        && original_caller == &held.original_caller()? =>
                    {
                        held.validate_ack(&self.cancel)?
                    }
                    Reply::Failed => {}
                    _ => return Err(failure("RPC reply differs from held registration")),
                }
            } else if matches!(reply, Reply::MountedOriginalRegistered { .. }) {
                return Err(failure("Mounted reply lacks private lease authority"));
            }
            // Borrowed replies are serialized once without copying secret-bearing commands.
            let (operation, sequence) = self
                .active
                .as_ref()
                .ok_or_else(|| failure("capture RPC has no active command"))?;
            let frame = serde_json::json!({"schema":RPC_PROTOCOL,"operation":operation,"sequence":sequence,"message":{"event":"reply","reply":reply}});
            let maximum = if matches!(reply, Reply::OriginalRegistered { .. }) {
                96 * 1024
            } else {
                16 * 1024
            };
            worker::write_frame(&mut self.writer, &serde_json::to_vec(&frame)?, maximum)?;
            Ok(())
        })();
        // Keep every actual lease through the final write attempt, then retire
        // on BOTH success and failure; failure cannot retain a reusable ACK.
        self.registration = None;
        self.active = None;
        if result.is_err() {
            let _ = self.writer.shutdown(std::net::Shutdown::Both);
        }
        result
    }
}
impl Drop for RootSession {
    fn drop(&mut self) {
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
    }
}

pub(super) struct AppSession {
    channel: WorkerChannel,
    operation: String,
    sequence: u64,
}
impl Listener {
    pub(super) fn accept(
        self,
        root_pid: u32,
        operation: &str,
        cancel: &AtomicBool,
    ) -> io::Result<AppSession> {
        self.inner.socket().set_nonblocking(true)?;
        let socket = loop {
            if cancel.load(Ordering::Acquire) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            match self.inner.socket().accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(20))
                }
                Err(error) => return Err(error),
            }
        };
        let mut channel = WorkerChannel::authenticate(socket)?;
        channel.authenticate_root(root_pid)?;
        channel.stream().set_read_timeout(None)?;
        Ok(AppSession {
            channel,
            operation: operation.into(),
            sequence: 0,
        })
    }
}
impl AppSession {
    pub(super) fn call_with_mounted(
        &mut self,
        action: Action,
        output: Option<File>,
        mounted: Option<&super::super::GrantedHomeRoots>,
        cancel: Option<&AtomicBool>,
        progress: &mut dyn FnMut(ArchiveProgress) -> io::Result<()>,
    ) -> io::Result<Reply> {
        self.call_with_source(action, output, mounted, None, cancel, progress)
    }
    pub(super) fn call_with_retained(
        &mut self,
        source: &PinnedDirectory,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(ArchiveProgress) -> io::Result<()>,
    ) -> io::Result<Reply> {
        source.validate()?;
        let result = self.call_with_source(
            Action::RegisterRetainedCaller,
            None,
            None,
            Some(source),
            Some(cancel),
            progress,
        );
        source.validate()?;
        result
    }
    fn call_with_source(
        &mut self,
        action: Action,
        output: Option<File>,
        mounted: Option<&super::super::GrantedHomeRoots>,
        retained: Option<&PinnedDirectory>,
        cancel: Option<&AtomicBool>,
        progress: &mut dyn FnMut(ArchiveProgress) -> io::Result<()>,
    ) -> io::Result<Reply> {
        if matches!(&action, Action::RegisterRetainedCaller) != retained.is_some() {
            return Err(failure(
                "Retained action differs from exact source descriptor",
            ));
        }
        if matches!(&action, Action::RegisterOriginalMounted { .. }) != mounted.is_some() {
            return Err(failure("Mounted action and held grant bundle differ"));
        }
        let expected = matches!(
            &action,
            Action::Run {
                has_output: true,
                ..
            }
        );
        if expected != output.is_some() {
            return Err(failure("capture RPC output authority differs"));
        }
        let call = Call {
            schema: RPC_PROTOCOL,
            operation: self.operation.clone(),
            sequence: self.sequence,
            action,
        };
        let bytes = Zeroizing::new(serde_json::to_vec(&call)?);
        worker::write_frame(self.channel.stream(), &bytes, 64 * 1024)?;
        if let Some(output) = output {
            self.channel.send(2, &output)?;
        }
        if let Some(retained) = retained {
            retained.validate()?;
            self.channel.send(4, &retained.file)?;
        }
        let reply = if let Some(roots) = mounted {
            let Action::RegisterOriginalMounted {
                generation,
                entries,
            } = &call.action
            else {
                return Err(failure("Mounted grant sent without mounted action"));
            };
            MountedInput::validate_all(entries, *generation)?;
            if *generation != roots.generation() {
                return Err(failure("Mounted grant generation changed"));
            }
            roots.with_roots(
                cancel.ok_or_else(|| failure("Mounted transfer cancellation owner missing"))?,
                |granted| {
                    if granted.len() != entries.len() + 1 {
                        return Err(failure("Mounted grant count changed"));
                    }
                    for (root, entry) in granted[1..].iter().zip(entries) {
                        let observed = MountedInput::from_granted(root);
                        if observed.placement != entry.placement || observed.kind != entry.kind {
                            return Err(failure("Mounted grant declarations changed"));
                        }
                        self.channel.send(3, root.file())?;
                    }
                    read_reply(self.channel.stream(), &call, progress)
                },
            )?
        } else {
            read_reply(self.channel.stream(), &call, progress)?
        };
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| failure("capture RPC sequence overflow"))?;
        Ok(reply)
    }
    pub(super) fn shutdown(&mut self) {
        let _ = self.channel.stream().shutdown(std::net::Shutdown::Both);
    }
}

fn read_reply(
    input: &mut impl std::io::Read,
    call: &Call,
    progress: &mut dyn FnMut(ArchiveProgress) -> io::Result<()>,
) -> io::Result<Reply> {
    let mut exchange = match &call.action {
        Action::Run { command, .. } => Some(worker::Exchange::new(&call.operation, command)),
        _ => None,
    };
    let export_operation = match &call.action {
        Action::Run {
            command:
                Command::Export {
                    export_operation, ..
                }
                | Command::ValidateExisting {
                    export_operation, ..
                },
            ..
        } => Some(export_operation.as_str()),
        _ => None,
    };
    loop {
        let maximum = if matches!(call.action, Action::RegisterOriginal) {
            96 * 1024
        } else {
            16 * 1024
        };
        let bytes = worker::read_frame(input, maximum)?;
        let frame: Frame = serde_json::from_slice(&bytes)?;
        validate_protocol(frame.schema)?;
        if frame.operation != call.operation || frame.sequence != call.sequence {
            return Err(failure("capture RPC reply binding changed"));
        }
        match frame.message {
            Message::Progress { progress: value } => {
                exchange
                    .as_mut()
                    .ok_or_else(|| {
                        failure("capture RPC control command reported archive progress")
                    })?
                    .progress(
                        &frame.operation,
                        export_operation
                            .ok_or_else(|| failure("capture review reported archive progress"))?,
                        value,
                    )?;
                progress(value)?;
            }
            Message::Reply { reply } => {
                match (&call.action, &reply) {
                    (
                        Action::RegisterRetainedCaller,
                        Reply::RetainedCallerRegistered { declaration },
                    ) if declaration.operation() == call.operation => {}
                    (
                        Action::RegisterRetainedCaller
                        | Action::RegisterOriginal
                        | Action::RegisterOriginalMounted { .. },
                        Reply::Failed | Reply::Released,
                    ) => {}
                    (Action::RegisterRetainedCaller, _)
                    | (_, Reply::RetainedCallerRegistered { .. }) => {
                        return Err(failure("Retained registration response binding differs"));
                    }
                    (Action::RegisterOriginal, Reply::OriginalRegistered { witness, .. })
                        if witness.operation == call.operation => {}
                    (
                        Action::RegisterOriginalMounted { generation, .. },
                        Reply::MountedOriginalRegistered {
                            generation: returned,
                            witness,
                            ..
                        },
                    ) if generation == returned && witness.operation() == call.operation => {}
                    (Action::RegisterOriginalMounted { .. }, _)
                    | (_, Reply::MountedOriginalRegistered { .. }) => {
                        return Err(failure("Mounted registration response generation differs"));
                    }
                    (Action::RegisterOriginal, _) | (_, Reply::OriginalRegistered { .. }) => {
                        return Err(failure("Original registration response binding differs"));
                    }
                    _ => {}
                }
                if let Some(exchange) = &mut exchange {
                    exchange.finish(&frame.operation, export_operation)?;
                }
                return Ok(reply);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::portable_transfer::ArchivePhase;
    #[test]
    fn mounted_input_codec_refuses_unbounded_traversal_duplicate_and_unknown_claims() {
        let entry = |path: &str| MountedInput {
            placement: NativePath::encode(Path::new(path)),
            kind: super::super::super::migration_handoff::EntryKind::Directory,
        };
        let generation = uuid::Uuid::new_v4();
        MountedInput::validate_all(
            &[entry("mounted-directory"), entry("mounted-file")],
            generation,
        )
        .unwrap();
        for entries in [
            vec![],
            vec![entry("../escape")],
            vec![entry("/absolute")],
            vec![entry("a"), entry("a")],
            vec![entry("a"), entry("a/b")],
            vec![entry("z"), entry("a")],
            vec![entry("a//b")],
        ] {
            assert!(MountedInput::validate_all(&entries, generation).is_err());
        }
        assert!(MountedInput::validate_all(&[entry("a")], uuid::Uuid::nil()).is_err());
        let mut raw = serde_json::to_value(entry("a")).unwrap();
        raw["groups"] = serde_json::json!([0]);
        assert!(serde_json::from_value::<MountedInput>(raw).is_err());
    }
    fn call() -> Call {
        Call {
            schema: RPC_PROTOCOL,
            operation: "capture".into(),
            sequence: 7,
            action: Action::Run {
                has_output: true,
                command: Command::Export {
                    export_operation: "package".into(),
                    setup_binding: "binding".into(),
                    password: worker::Secret::new(Zeroizing::new(b"password".to_vec())),
                },
            },
        }
    }
    fn progress_frame(completed: u64) -> Frame {
        Frame {
            schema: RPC_PROTOCOL,
            operation: "capture".into(),
            sequence: 7,
            message: Message::Progress {
                progress: ArchiveProgress {
                    phase: ArchivePhase::Archiving,
                    completed,
                    total: None,
                },
            },
        }
    }
    fn framed(frames: impl IntoIterator<Item = Frame>) -> Vec<u8> {
        let mut bytes = Vec::new();
        for frame in frames {
            worker::write_frame(&mut bytes, &serde_json::to_vec(&frame).unwrap(), 16 * 1024)
                .unwrap();
        }
        bytes
    }
    #[test]
    fn rpc_stream_forwards_bounded_progress_then_one_distinct_final_reply() {
        let frames = (0..1000).map(progress_frame).chain([Frame {
            schema: RPC_PROTOCOL,
            operation: "capture".into(),
            sequence: 7,
            message: Message::Reply {
                reply: Reply::Validated,
            },
        }]);
        let bytes = framed(frames);
        let mut latest = None;
        assert!(matches!(
            read_reply(&mut bytes.as_slice(), &call(), &mut |value| {
                latest = Some(value);
                Ok(())
            })
            .unwrap(),
            Reply::Validated
        ));
        assert_eq!(latest.unwrap().completed, 999);
    }
    #[test]
    fn rpc_refuses_wrong_call_sequence_capture_and_regression_before_final() {
        {
            let mut wrong = progress_frame(0);
            wrong.sequence = 8;
            let bytes = framed([wrong]);
            let mut observed = false;
            assert!(
                read_reply(&mut bytes.as_slice(), &call(), &mut |_| {
                    observed = true;
                    Ok(())
                })
                .is_err()
            );
            assert!(!observed);
        }
        let mut wrong = progress_frame(0);
        wrong.operation = "other-capture".into();
        assert!(read_reply(&mut framed([wrong]).as_slice(), &call(), &mut |_| Ok(())).is_err());
        assert!(
            read_reply(
                &mut framed([progress_frame(3), progress_frame(2)]).as_slice(),
                &call(),
                &mut |_| Ok(())
            )
            .is_err()
        );
        assert!(
            read_reply(
                &mut framed([progress_frame(3)]).as_slice(),
                &call(),
                &mut |_| Err(io::ErrorKind::Interrupted.into())
            )
            .is_err()
        );
    }
    #[test]
    fn rpc_rejects_unknown_progress_fields_and_truncated_final_reply() {
        let frame = br#"{"schema":2,"operation":"capture","sequence":7,"message":{"event":"progress","progress":{"phase":"archiving","completed":0,"total":null,"path":"/private/home"}}}"#;
        let mut bytes = Vec::new();
        worker::write_frame(&mut bytes, frame, 16 * 1024).unwrap();
        assert!(read_reply(&mut bytes.as_slice(), &call(), &mut |_| Ok(())).is_err());
        let mut bytes = framed([Frame {
            schema: RPC_PROTOCOL,
            operation: "capture".into(),
            sequence: 7,
            message: Message::Reply {
                reply: Reply::Validated,
            },
        }]);
        bytes.pop();
        assert!(read_reply(&mut bytes.as_slice(), &call(), &mut |_| Ok(())).is_err());
    }
    #[test]
    fn rpc_rejects_old_protocol_before_delivering_progress_or_reply() {
        assert_eq!(
            validate_protocol(1).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        validate_protocol(RPC_PROTOCOL).unwrap();
        let mut frame = progress_frame(0);
        frame.schema = 1;
        let mut observed = false;
        let error = read_reply(&mut framed([frame]).as_slice(), &call(), &mut |_| {
            observed = true;
            Ok(())
        })
        .err()
        .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(!observed);
    }
}

/// Read-only block-device control uses the existing kernel authenticated private
/// rendezvous, rather than another socket, caller principal, or PID authority.
pub struct MediaReadSession {
    channel: WorkerChannel,
}
pub struct RootMediaReadSession {
    channel: HostChannel,
    deadline: std::time::Instant,
    boot_active: bool,
    boot_cancelled: bool,
}
impl Listener {
    pub fn accept_read_media(
        self,
        root_pid: u32,
        cancel: &AtomicBool,
        mut alive: impl FnMut() -> io::Result<bool>,
    ) -> io::Result<MediaReadSession> {
        self.inner.socket().set_nonblocking(true)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let socket = loop {
            super::super::migration::check_cancel(cancel)?;
            self.inner.validate()?;
            if !alive()? {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            if std::time::Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match self.inner.socket().accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(20))
                }
                Err(error) => return Err(error),
            }
        };
        let mut channel = WorkerChannel::authenticate(socket)?;
        channel.authenticate_root(root_pid)?;
        self.inner.validate()?;
        super::super::migration::check_cancel(cancel)?;
        Ok(MediaReadSession { channel })
    }
}
impl MediaReadSession {
    pub fn acquire_inbox(
        &mut self,
        layout: &crate::setup::OfflineLayout,
    ) -> io::Result<(File, Vec<u8>)> {
        use std::io::Write;
        self.validate()?;
        self.channel.stream().write_all(&[83])?;
        worker::write_frame(
            self.channel.stream(),
            &serde_json::to_vec(layout)?,
            1024 * 1024,
        )?;
        self.channel.validate_root()?;
        let file = self.channel.receive(82, true)?;
        let bytes = worker::read_frame(self.channel.stream(), 4096)?;
        self.validate()?;
        Ok((file, bytes))
    }
    pub fn finish_inbox(&mut self, successful: bool) -> io::Result<()> {
        use std::io::{Read, Write};
        self.validate()?;
        self.channel
            .stream()
            .write_all(&[if successful { 84 } else { 85 }])?;
        self.channel
            .stream()
            .set_read_timeout(Some(std::time::Duration::from_secs(120)))?;
        let mut reply = [0; 2];
        self.channel.stream().read_exact(&mut reply)?;
        self.validate()?;
        if reply != [86, 1] {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        self.channel
            .stream()
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
        Ok(())
    }
    pub fn validate(&self) -> io::Result<()> {
        self.channel.validate_root()
    }
    pub fn receive(&mut self) -> io::Result<File> {
        self.channel.receive_read_only_media()
    }
}
impl RootMediaReadSession {
    pub(in crate::setup) fn connect(
        rendezvous: &Rendezvous,
        directory: &Path,
        caller: u32,
    ) -> io::Result<Self> {
        let socket = rendezvous.connect(directory, caller)?;
        let mut channel = HostChannel::authenticate(socket, rendezvous.0.caller_pid(), caller)?;
        channel.authenticate_as_root()?;
        Ok(Self {
            channel,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(3600),
            boot_active: false,
            boot_cancelled: false,
        })
    }
    pub(in crate::setup) fn send(&mut self, file: &File) -> io::Result<()> {
        super::transport::validate_read_only_block_device(file)?;
        self.channel.send(81, file)
    }
    fn remaining(&self) -> io::Result<std::time::Duration> {
        self.deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|time| !time.is_zero())
            .ok_or(io::ErrorKind::TimedOut.into())
    }
    pub(in crate::setup) fn next_inbox_request(
        &mut self,
    ) -> io::Result<Option<crate::setup::OfflineLayout>> {
        self.channel.validate_peer()?;
        let remaining = self.remaining()?;
        self.channel.stream().set_read_timeout(Some(remaining))?;
        let result = self
            .next_media_command(super::super::read_media::ReadMediaPurpose::OriginalRegistration)?;
        self.channel.validate_peer()?;
        self.remaining()?;
        match result {
            Some(MediaReadCommand::Inbox(layout)) => Ok(Some(layout)),
            None => Ok(None),
            _ => Err(io::ErrorKind::PermissionDenied.into()),
        }
    }
    pub(in crate::setup) fn send_inbox(&mut self, file: &File, receipt: &[u8]) -> io::Result<()> {
        self.remaining()?;
        self.channel.validate_peer()?;
        self.channel.send(82, file)?;
        worker::write_frame(self.channel.stream(), receipt, 4096)
    }
    pub(in crate::setup) fn await_inbox_completion(&mut self) -> io::Result<bool> {
        use std::io::Read;
        let remaining = self.remaining()?;
        self.channel.stream().set_read_timeout(Some(remaining))?;
        let mut tag = [0];
        self.channel.stream().read_exact(&mut tag)?;
        self.remaining()?;
        self.channel.validate_peer()?;
        match tag {
            [84] => Ok(true),
            [85] => Ok(false),
            _ => Err(io::ErrorKind::InvalidData.into()),
        }
    }
    pub(in crate::setup) fn release_inbox(&mut self, accepted: bool) -> io::Result<()> {
        use std::io::Write;
        self.channel.validate_peer()?;
        self.channel.stream().write_all(&[86, u8::from(accepted)])
    }
}

const ORIGINAL_SECRET_BOUND: usize = 16 * 1024;
const ORIGINAL_COMPLETE: u8 = 87;
const ORIGINAL_CANCEL: u8 = 88;
const ORIGINAL_STATUS: u8 = 89;
const ORIGINAL_STATUS_ACK: u8 = 90;
const ORIGINAL_EXTRACT_COMPLETED: u8 = 91;
// No Debug/Serialize implementation: credentials stay in this private binary
// frame and are zeroized even when a partial read or callback fails.
enum MediaReadCommand {
    Inbox(crate::setup::OfflineLayout),
    Boot(
        super::super::read_media::BootMediaOperation,
        Zeroizing<Vec<u8>>,
    ),
    Cancel,
}
#[cfg(test)]
fn read_media_command(
    input: &mut impl std::io::Read,
    purpose: super::super::read_media::ReadMediaPurpose,
) -> io::Result<Option<MediaReadCommand>> {
    let mut tag = [0];
    if input.read(&mut tag)? == 0 {
        return Ok(None);
    }
    read_media_payload(input, purpose, tag[0])
}
fn read_media_payload(
    input: &mut impl std::io::Read,
    purpose: super::super::read_media::ReadMediaPurpose,
    tag: u8,
) -> io::Result<Option<MediaReadCommand>> {
    use super::super::read_media::ReadMediaPurpose;
    match (purpose, tag) {
        (ReadMediaPurpose::OriginalRegistration, 83) => {
            let bytes = worker::read_frame(input, 1024 * 1024)?;
            Ok(Some(MediaReadCommand::Inbox(serde_json::from_slice(
                &bytes,
            )?)))
        }
        (ReadMediaPurpose::BootPending, ORIGINAL_COMPLETE | ORIGINAL_EXTRACT_COMPLETED) => {
            let mut length = [0; 4];
            input.read_exact(&mut length)?;
            let length = usize::try_from(u32::from_be_bytes(length))
                .map_err(|_| io::ErrorKind::InvalidData)?;
            if length == 0 || length > ORIGINAL_SECRET_BOUND {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let mut secret = Zeroizing::new(vec![0; length]);
            input.read_exact(&mut secret)?;
            let operation = if tag == ORIGINAL_COMPLETE {
                super::super::read_media::BootMediaOperation::CompleteOriginal
            } else {
                super::super::read_media::BootMediaOperation::ExtractCompleted
            };
            Ok(Some(MediaReadCommand::Boot(operation, secret)))
        }
        (ReadMediaPurpose::BootPending, ORIGINAL_CANCEL) => Ok(Some(MediaReadCommand::Cancel)),
        _ => Err(io::ErrorKind::PermissionDenied.into()),
    }
}
impl MediaReadSession {
    pub fn complete_original(
        &mut self,
        secret: Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.run_boot_operation(
            crate::setup::read_media::BootMediaOperation::CompleteOriginal,
            secret,
            cancel,
        )
    }
    pub fn extract_completed(
        &mut self,
        secret: Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.run_boot_operation(
            super::super::read_media::BootMediaOperation::ExtractCompleted,
            secret,
            cancel,
        )
    }
    fn run_boot_operation(
        &mut self,
        operation: super::super::read_media::BootMediaOperation,
        secret: Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        use std::io::{Read, Write};
        if secret.is_empty() || secret.len() > ORIGINAL_SECRET_BOUND {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        super::super::migration::check_cancel(cancel)?;
        self.validate()?;
        let tag = match operation {
            super::super::read_media::BootMediaOperation::CompleteOriginal => ORIGINAL_COMPLETE,
            super::super::read_media::BootMediaOperation::ExtractCompleted => {
                ORIGINAL_EXTRACT_COMPLETED
            }
        };
        self.channel.stream().write_all(&[tag])?;
        worker::write_frame(self.channel.stream(), &secret, ORIGINAL_SECRET_BOUND)?;
        drop(secret);
        self.channel
            .stream()
            .set_read_timeout(Some(std::time::Duration::from_millis(100)))?;
        let mut reply = [0; 2];
        let mut received = 0;
        let mut reply_deadline = None;
        let mut sent_cancel = false;
        while received < reply.len() {
            self.validate()?;
            if cancel.load(Ordering::Acquire) && !sent_cancel {
                self.channel.stream().write_all(&[ORIGINAL_CANCEL])?;
                sent_cancel = true;
            }
            if reply_deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match self.channel.stream().read(&mut reply[received..]) {
                Ok(0) => return Err(io::ErrorKind::BrokenPipe.into()),
                Ok(n) => {
                    if received == 0 {
                        reply_deadline =
                            Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
                    }
                    received += n;
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => return Err(e),
            }
        }
        self.validate()?;
        if reply[0] != ORIGINAL_STATUS {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if !matches!(reply[1], 1..=4) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        // Observe cancellation that arrived with the final reply as well;
        // root drains this one late first cancellation before the bounded ACK.
        if cancel.load(Ordering::Acquire) && !sent_cancel {
            self.channel.stream().write_all(&[ORIGINAL_CANCEL])?;
            sent_cancel = true;
        }
        // Root stays alive until the authenticated caller has consumed the
        // final status; process retirement cannot race its receipt validation.
        self.channel.stream().write_all(&[ORIGINAL_STATUS_ACK])?;
        match reply[1] {
            1 if !sent_cancel => Ok(()),
            1 | 2 => Err(io::ErrorKind::Interrupted.into()),
            3 => Err(io::ErrorKind::Unsupported.into()),
            4 => Err(io::ErrorKind::PermissionDenied.into()),
            _ => Err(io::ErrorKind::InvalidData.into()),
        }
    }
}
impl RootMediaReadSession {
    fn next_media_command(
        &mut self,
        purpose: super::super::read_media::ReadMediaPurpose,
    ) -> io::Result<Option<MediaReadCommand>> {
        use std::io::Read;
        self.channel.validate_peer()?;
        let remaining = self.remaining()?;
        self.channel.stream().set_read_timeout(Some(remaining))?;
        let mut tag = [0];
        match self.channel.stream().read(&mut tag) {
            Ok(0) => return Ok(None),
            Ok(1) => {}
            Ok(_) => return Err(io::ErrorKind::InvalidData.into()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
        self.channel.validate_peer()?;
        self.channel
            .stream()
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
        read_media_payload(self.channel.stream(), purpose, tag[0])
    }
    pub(in crate::setup) fn next_boot_operation(
        &mut self,
    ) -> io::Result<
        Option<(
            super::super::read_media::BootMediaOperation,
            Zeroizing<Vec<u8>>,
        )>,
    > {
        if self.boot_active {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        self.channel.validate_peer()?;
        let result =
            self.next_media_command(super::super::read_media::ReadMediaPurpose::BootPending)?;
        self.channel.validate_peer()?;
        self.remaining()?;
        match result {
            Some(MediaReadCommand::Boot(operation, secret)) => {
                self.boot_active = true;
                Ok(Some((operation, secret)))
            }
            Some(MediaReadCommand::Cancel) | None => Ok(None),
            Some(MediaReadCommand::Inbox(_)) => Err(io::ErrorKind::PermissionDenied.into()),
        }
    }
    // Active work has no elapsed-total cutoff: the owned transaction polls the
    // authenticated peer and cancellation while retaining its cleanup custody.
    pub(in crate::setup) fn check_boot_cancel(&mut self, cancel: &AtomicBool) -> io::Result<()> {
        if let Err(error) = self.channel.validate_peer().and_then(|()| {
            if self.boot_active {
                Ok(())
            } else {
                self.remaining().map(|_| ())
            }
        }) {
            cancel.store(true, Ordering::Release);
            return Err(error);
        }
        poll_original_cancel(self.channel.stream(), &mut self.boot_cancelled, cancel)
    }
    pub(in crate::setup) fn finish_original_completion(
        &mut self,
        result: &io::Result<()>,
    ) -> io::Result<()> {
        self.channel.validate_peer()?;
        if !self.boot_active {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let status = match result {
            Ok(()) => 1,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => 2,
            Err(error) if error.kind() == io::ErrorKind::Unsupported => 3,
            Err(_) => 4,
        };
        finish_original_status(self.channel.stream(), &mut self.boot_cancelled, status)?;
        self.channel.validate_peer()
    }
}
// Consume exactly one cancellation command. No blocking read and no peeked
// command remains queued to poison the final acknowledgment.
fn poll_original_cancel(
    stream: &mut std::os::unix::net::UnixStream,
    cancelled: &mut bool,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let mut tag = [0u8; 1];
    // SAFETY: live owned stream FD, one writable byte, nonblocking receive.
    let result = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            tag.as_mut_ptr().cast(),
            1,
            libc::MSG_DONTWAIT,
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::WouldBlock {
            cancel.store(true, Ordering::Release);
            return Err(error);
        }
    } else {
        cancel.store(true, Ordering::Release);
        if result == 0 {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if tag[0] != ORIGINAL_CANCEL || *cancelled {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        *cancelled = true;
    }
    if *cancelled || cancel.load(Ordering::Acquire) {
        Err(io::ErrorKind::Interrupted.into())
    } else {
        Ok(())
    }
}
fn finish_original_status(
    stream: &mut std::os::unix::net::UnixStream,
    cancelled: &mut bool,
    status: u8,
) -> io::Result<()> {
    use std::io::{Read, Write};
    stream.write_all(&[ORIGINAL_STATUS, status])?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|time| !time.is_zero())
            .ok_or(io::ErrorKind::TimedOut)?;
        stream.set_read_timeout(Some(remaining))?;
        let mut tag = [0];
        stream.read_exact(&mut tag)?;
        match tag[0] {
            ORIGINAL_STATUS_ACK => return Ok(()),
            ORIGINAL_CANCEL if !*cancelled => *cancelled = true,
            _ => return Err(io::ErrorKind::InvalidData.into()),
        }
    }
}
#[cfg(test)]
mod media_purpose_tests {
    use super::super::super::read_media::ReadMediaPurpose;
    use super::*;
    #[test]
    fn paired_cancel_is_consumed_before_final_status_and_ack() {
        use std::io::{Read, Write};
        let (mut root, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
        let cancel = AtomicBool::new(false);
        let mut cancelled = false;
        client.write_all(&[ORIGINAL_CANCEL]).unwrap();
        assert_eq!(
            poll_original_cancel(&mut root, &mut cancelled, &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(cancelled && cancel.load(Ordering::Acquire));
        // A second poll retains cancellation without needing another byte.
        assert_eq!(
            poll_original_cancel(&mut root, &mut cancelled, &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        let caller = std::thread::spawn(move || {
            client
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut status = [0; 2];
            client.read_exact(&mut status).unwrap();
            assert_eq!(status, [ORIGINAL_STATUS, 2]);
            client.write_all(&[ORIGINAL_STATUS_ACK]).unwrap();
        });
        finish_original_status(&mut root, &mut cancelled, 2).unwrap();
        caller.join().unwrap();
    }
    #[test]
    fn paired_late_cancel_drains_once_before_ack_and_refuses_other_traffic() {
        use std::io::{Read, Write};
        for (traffic, accepted) in [
            (vec![ORIGINAL_CANCEL, ORIGINAL_STATUS_ACK], true),
            (
                vec![ORIGINAL_CANCEL, ORIGINAL_CANCEL, ORIGINAL_STATUS_ACK],
                false,
            ),
            (vec![ORIGINAL_COMPLETE, ORIGINAL_STATUS_ACK], false),
        ] {
            let (mut root, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
            let caller = std::thread::spawn(move || {
                client
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut status = [0; 2];
                client.read_exact(&mut status).unwrap();
                assert_eq!(status, [ORIGINAL_STATUS, 2]);
                client.write_all(&traffic).unwrap();
            });
            let mut cancelled = false;
            let result = finish_original_status(&mut root, &mut cancelled, 2);
            if accepted {
                result.unwrap();
                assert!(cancelled);
            } else {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
            }
            caller.join().unwrap();
        }
    }
    #[test]
    fn paired_cancel_poll_refuses_duplicates_foreign_commands_and_eof() {
        use std::io::Write;
        for tag in [ORIGINAL_CANCEL, ORIGINAL_COMPLETE] {
            let (mut root, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
            client.write_all(&[tag]).unwrap();
            let cancel = AtomicBool::new(true);
            let mut cancelled = tag == ORIGINAL_CANCEL;
            assert_eq!(
                poll_original_cancel(&mut root, &mut cancelled, &cancel)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        let (mut root, client) = std::os::unix::net::UnixStream::pair().unwrap();
        drop(client);
        let cancel = AtomicBool::new(false);
        assert_eq!(
            poll_original_cancel(&mut root, &mut false, &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
        assert!(cancel.load(Ordering::Acquire));
        // Local cancellation must not leave a first wire cancellation queued.
        let (mut root, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
        client.write_all(&[ORIGINAL_CANCEL]).unwrap();
        let mut cancelled = false;
        assert_eq!(
            poll_original_cancel(&mut root, &mut cancelled, &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(cancelled);
    }
    #[test]
    fn completed_extraction_is_secret_bounded_and_boot_purpose_only() {
        use super::super::super::read_media::BootMediaOperation;
        let mut encoded = vec![ORIGINAL_EXTRACT_COMPLETED];
        worker::write_frame(&mut encoded, b"synthetic-extraction", ORIGINAL_SECRET_BOUND).unwrap();
        let Some(MediaReadCommand::Boot(operation, secret)) =
            read_media_command(&mut encoded.as_slice(), ReadMediaPurpose::BootPending).unwrap()
        else {
            panic!("extraction command absent");
        };
        assert!(operation == BootMediaOperation::ExtractCompleted);
        assert_eq!(secret.as_slice(), b"synthetic-extraction");
        let mut refused = encoded.as_slice();
        assert_eq!(
            read_media_command(&mut refused, ReadMediaPurpose::OriginalRegistration)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(refused.len(), encoded.len() - 1);
        for length in [0u32, ORIGINAL_SECRET_BOUND as u32 + 1] {
            let mut invalid = vec![ORIGINAL_EXTRACT_COMPLETED];
            invalid.extend_from_slice(&length.to_be_bytes());
            assert_eq!(
                read_media_command(&mut invalid.as_slice(), ReadMediaPurpose::BootPending)
                    .err()
                    .unwrap()
                    .kind(),
                io::ErrorKind::InvalidData
            );
        }
        encoded.pop();
        assert_eq!(
            read_media_command(&mut encoded.as_slice(), ReadMediaPurpose::BootPending)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
    #[test]
    fn production_codec_refuses_wrong_purpose_before_reading_secret_or_layout() {
        let mut secret = &[ORIGINAL_COMPLETE, 0xff, 0xff, 0xff, 0xff][..];
        assert!(read_media_command(&mut secret, ReadMediaPurpose::OriginalRegistration).is_err());
        assert_eq!(secret.len(), 4);
        let mut inbox = &[83, 0xff, 0xff, 0xff, 0xff][..];
        assert!(read_media_command(&mut inbox, ReadMediaPurpose::BootPending).is_err());
        assert_eq!(inbox.len(), 4);
    }
    #[test]
    fn production_secret_codec_is_bounded_exact_and_cancel_has_no_payload() {
        let mut bytes = vec![ORIGINAL_COMPLETE];
        worker::write_frame(&mut bytes, b"synthetic-only", ORIGINAL_SECRET_BOUND).unwrap();
        let Some(MediaReadCommand::Boot(
            crate::setup::read_media::BootMediaOperation::CompleteOriginal,
            secret,
        )) = read_media_command(&mut bytes.as_slice(), ReadMediaPurpose::BootPending).unwrap()
        else {
            panic!("secret command absent");
        };
        assert_eq!(&*secret, b"synthetic-only");
        assert!(matches!(
            read_media_command(&mut &[ORIGINAL_CANCEL][..], ReadMediaPurpose::BootPending).unwrap(),
            Some(MediaReadCommand::Cancel)
        ));
        for length in [0u32, (ORIGINAL_SECRET_BOUND + 1) as u32] {
            let mut invalid = vec![ORIGINAL_COMPLETE];
            invalid.extend_from_slice(&length.to_be_bytes());
            assert!(
                read_media_command(&mut invalid.as_slice(), ReadMediaPurpose::BootPending).is_err()
            );
        }
        bytes.pop();
        assert!(read_media_command(&mut bytes.as_slice(), ReadMediaPurpose::BootPending).is_err());
    }
}

#[cfg(all(test, feature = "native-capture-qualification"))]
pub(in crate::setup::home_capture) mod mounted_native;
