//! Owned speech/capture actor. Every probe, pipe and process wait stays off the
//! compositor thread. A model Ready reply alone never opens the microphone.
use std::collections::VecDeque;
use std::os::fd::AsRawFd as _;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use agent_provider_config::speech::{
    FRAME, MAX_TEXT_BYTES, RequestKind, SpeechConfig, SpeechError, SpeechReply, request,
};
use zeroize::Zeroizing;

use super::super::audio_capture::AudioCapture;

const START_WINDOW: Duration = Duration::from_secs(15);
const RECORD_WINDOW: Duration = Duration::from_secs(105);
const FINISH_WINDOW: Duration = Duration::from_secs(15);
const DRAFT_INTERVAL: Duration = Duration::from_secs(1);
const QUEUE_BYTES: usize = 256 * 1024;
const MAX_REPLY: usize = 64 * 1024 + 9;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeError {
    Worker(SpeechError),
    MicrophoneUnavailable,
    AudioDiscontinuity,
    TimedOut,
    Protocol,
    Busy,
    Canceled,
}

/// Private recorded words have no Debug implementation and never enter logs.
pub enum RuntimeEvent {
    Ready,
    Listening,
    Finishing,
    Draft(String),
    Done(String),
    Stopped(RuntimeError),
}

enum Control {
    Capture,
    Finish,
}

/// One actor owns one worker and at most one capture for its entire lifetime.
/// Keep this handle until `is_stopped`: cancellation is asynchronous, and a
/// successor must wait for the old actor's actual microphone/worker teardown.
pub struct Runtime {
    commands: SyncSender<Control>,
    events: Receiver<RuntimeEvent>,
    cancel: Arc<AtomicBool>,
    owner: Option<JoinHandle<()>>,
}
impl Runtime {
    /// Spawn only the immutable image worker. No paths, accounts or network
    /// endpoints can be supplied by a collaborator or model.
    /// # Errors
    /// Rejects invalid languages or an unavailable actor thread.
    pub fn start(language: &str) -> Result<Self, RuntimeError> {
        let mut command = Command::new("/usr/bin/tilo-speech-worker");
        command.env_clear();
        // These select the same per-user store as the model manager; trust
        // keys and catalogue remain fixed image paths inside the worker.
        for key in ["HOME", "XDG_DATA_HOME"] {
            if let Some(value) = std::env::var_os(key)
                && std::path::Path::new(&value).is_absolute()
            {
                command.env(key, value);
            }
        }
        Self::spawn(command, language)
    }
    fn spawn(mut command: Command, language: &str) -> Result<Self, RuntimeError> {
        if language.is_empty()
            || language.len() > 16
            || !language
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'-')
        {
            return Err(RuntimeError::Worker(SpeechError::UnsupportedLanguage));
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        bounded_command::own_process_tree(&mut command);
        let config = serde_json::to_vec(&SpeechConfig {
            language: language.into(),
        })
        .map_err(|_| RuntimeError::Protocol)?;
        let (commands, receiver) = sync_channel(2);
        let (sender, events) = sync_channel(8);
        let cancel = Arc::new(AtomicBool::new(false));
        let canceled = Arc::clone(&cancel);
        let owner = std::thread::Builder::new()
            .name("tilo-dictation".into())
            .spawn(move || {
                let result = run(&mut command, &config, &receiver, &sender, &canceled);
                if let Err(error) = result {
                    let _ = sender.try_send(RuntimeEvent::Stopped(error));
                }
            })
            .map_err(|_| RuntimeError::Worker(SpeechError::WorkerStopped))?;
        Ok(Self {
            commands,
            events,
            cancel,
            owner: Some(owner),
        })
    }
    /// Call after local user consent and rechecking the original editable field
    /// against the Ready reply. A remote action alone is never consent.
    /// # Errors
    /// Fails closed when the actor stopped or its control queue is full.
    pub fn capture(&self) -> Result<(), RuntimeError> {
        self.commands
            .try_send(Control::Capture)
            .map_err(|_| RuntimeError::Busy)
    }
    /// Stop capture before requesting the final corrected transcript.
    /// # Errors
    /// Fails closed when the actor stopped or its control queue is full.
    pub fn finish(&self) -> Result<(), RuntimeError> {
        self.commands
            .try_send(Control::Finish)
            .map_err(|_| RuntimeError::Busy)
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn poll(&self) -> Option<RuntimeEvent> {
        self.events.try_recv().ok()
    }
    pub fn is_stopped(&mut self) -> bool {
        if self.owner.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(owner) = self.owner.take()
        {
            let _ = owner.join();
        }
        self.owner.is_none()
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.cancel();
        // The detached owner wakes within one poll or the bounded source probe
        // and tears down/reaps its owned processes; the renderer never waits.
    }
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        bounded_command::terminate_tree(&mut self.0, Duration::ZERO);
    }
}

struct Pipe {
    queue: VecDeque<Zeroizing<Vec<u8>>>,
    offset: usize,
    queued: usize,
    incoming: Zeroizing<Vec<u8>>,
    sequence: u64,
    waiting: Option<(RequestKind, u64)>,
}
impl Pipe {
    fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            offset: 0,
            queued: 0,
            incoming: Zeroizing::new(Vec::new()),
            sequence: 0,
            waiting: None,
        }
    }
    fn enqueue(&mut self, kind: RequestKind, payload: &[u8]) -> Result<(), RuntimeError> {
        let sequence = self.sequence.checked_add(1).ok_or(RuntimeError::Protocol)?;
        let mut frame = Zeroizing::new(Vec::new());
        FRAME
            .write(&mut *frame, &request(kind, sequence, payload))
            .map_err(|_| RuntimeError::Protocol)?;
        if self.queued.saturating_add(frame.len()) > QUEUE_BYTES {
            return Err(RuntimeError::AudioDiscontinuity);
        }
        if kind != RequestKind::Audio {
            if self.waiting.is_some() {
                return Err(RuntimeError::Busy);
            }
            self.waiting = Some((kind, sequence));
        }
        self.sequence = sequence;
        self.queued = self
            .queued
            .checked_add(frame.len())
            .ok_or(RuntimeError::Protocol)?;
        self.queue.push_back(frame);
        Ok(())
    }
    fn flush(&mut self, output: &mut impl std::io::Write) -> Result<(), RuntimeError> {
        while let Some(frame) = self.queue.front() {
            match output.write(frame.get(self.offset..).ok_or(RuntimeError::Protocol)?) {
                Ok(0) => return Err(RuntimeError::Worker(SpeechError::WorkerStopped)),
                Ok(n) => {
                    self.offset = self.offset.checked_add(n).ok_or(RuntimeError::Protocol)?;
                    if self.offset == frame.len() {
                        self.queued = self
                            .queued
                            .checked_sub(frame.len())
                            .ok_or(RuntimeError::Protocol)?;
                        self.queue.pop_front();
                        self.offset = 0;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err(RuntimeError::Worker(SpeechError::WorkerStopped)),
            }
        }
        Ok(())
    }
    fn read(
        &mut self,
        input: &mut impl std::io::Read,
    ) -> Result<Option<SpeechReply>, RuntimeError> {
        let mut chunk = [0_u8; 4096];
        loop {
            match input.read(&mut chunk) {
                Ok(0) => return Err(RuntimeError::Worker(SpeechError::WorkerStopped)),
                Ok(n) => self
                    .incoming
                    .extend_from_slice(chunk.get(..n).ok_or(RuntimeError::Protocol)?),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(RuntimeError::Worker(SpeechError::WorkerStopped)),
            }
            if self.incoming.len() > MAX_REPLY + 4 {
                return Err(RuntimeError::Protocol);
            }
            if self.incoming.len() >= 4 {
                let prefix = self
                    .incoming
                    .get(..4)
                    .ok_or(RuntimeError::Protocol)?
                    .try_into()
                    .map_err(|_| RuntimeError::Protocol)?;
                let size = usize::try_from(u32::from_le_bytes(prefix))
                    .map_err(|_| RuntimeError::Protocol)?;
                if size == 0 || size > MAX_REPLY {
                    return Err(RuntimeError::Protocol);
                }
                if self.incoming.len() >= size.saturating_add(4) {
                    break;
                }
            }
        }
        if self.incoming.len() < 4 {
            return Ok(None);
        }
        let prefix = self
            .incoming
            .get(..4)
            .ok_or(RuntimeError::Protocol)?
            .try_into()
            .map_err(|_| RuntimeError::Protocol)?;
        let size =
            usize::try_from(u32::from_le_bytes(prefix)).map_err(|_| RuntimeError::Protocol)?;
        if size == 0 || size > MAX_REPLY {
            return Err(RuntimeError::Protocol);
        }
        if self.incoming.len() < size.saturating_add(4) {
            return Ok(None);
        }
        let reply = serde_json::from_slice(
            self.incoming
                .get(4..size.saturating_add(4))
                .ok_or(RuntimeError::Protocol)?,
        )
        .map_err(|_| RuntimeError::Protocol)?;
        self.incoming.drain(..size.saturating_add(4));
        Ok(Some(reply))
    }
    fn accept(&mut self, reply: &SpeechReply) -> Result<(), RuntimeError> {
        let (kind, sequence) = self.waiting.ok_or(RuntimeError::Protocol)?;
        let matches = match reply {
            SpeechReply::Ready {
                sequence: got,
                model,
                revision,
            } => {
                kind == RequestKind::Configure
                    && *got == sequence
                    && !model.is_empty()
                    && model.len() <= 256
                    && !revision.is_empty()
                    && revision.len() <= 256
            }
            SpeechReply::Draft {
                sequence: got,
                text,
            } => kind == RequestKind::Draft && *got == sequence && valid_text(text),
            SpeechReply::Done {
                sequence: got,
                text,
            } => kind == RequestKind::Finish && *got == sequence && valid_text(text),
            SpeechReply::Error { error, .. } => return Err(RuntimeError::Worker(*error)),
        };
        if !matches {
            return Err(RuntimeError::Protocol);
        }
        self.waiting = None;
        Ok(())
    }
}
fn valid_text(text: &str) -> bool {
    text.len() <= MAX_TEXT_BYTES && !text.chars().any(char::is_control)
}
fn nonblocking(fd: std::os::fd::RawFd) -> Result<(), RuntimeError> {
    // SAFETY: caller owns this live pipe and F_GETFL/F_SETFL use integer args.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(RuntimeError::Protocol);
        }
    }
    Ok(())
}
fn event(sender: &SyncSender<RuntimeEvent>, event: RuntimeEvent) -> Result<(), RuntimeError> {
    sender.try_send(event).map_err(|_| RuntimeError::Protocol)
}
fn run(
    command: &mut Command,
    config: &[u8],
    controls: &Receiver<Control>,
    events: &SyncSender<RuntimeEvent>,
    cancel: &AtomicBool,
) -> Result<(), RuntimeError> {
    let result;
    {
        let mut worker = Process(
            command
                .spawn()
                .map_err(|_| RuntimeError::Worker(SpeechError::WorkerStopped))?,
        );
        let mut input = worker.0.stdout.take().ok_or(RuntimeError::Protocol)?;
        let mut output = worker.0.stdin.take().ok_or(RuntimeError::Protocol)?;
        nonblocking(input.as_raw_fd())?;
        nonblocking(output.as_raw_fd())?;
        let mut pipe = Pipe::new();
        pipe.enqueue(RequestKind::Configure, config)?;
        result = pump_io(
            &mut pipe,
            &mut ActorIo {
                input: &mut input,
                output: &mut output,
                controls,
                events,
                cancel,
            },
        );
        // These pipe owners drop before Process kills/reaps. The capture has
        // already stopped inside pump before a terminal event is delivered.
    }
    match result {
        Ok(text) => event(events, RuntimeEvent::Done(text)),
        Err(error) => Err(error),
    }
}
struct ActorIo<'a> {
    input: &'a mut std::process::ChildStdout,
    output: &'a mut std::process::ChildStdin,
    controls: &'a Receiver<Control>,
    events: &'a SyncSender<RuntimeEvent>,
    cancel: &'a AtomicBool,
}
fn pump_io(pipe: &mut Pipe, io: &mut ActorIo<'_>) -> Result<String, RuntimeError> {
    let mut microphone: Option<AudioCapture> = None;
    let mut ready = false;
    let mut finishing = false;
    let mut deadline = later(START_WINDOW);
    let session_end = later(RECORD_WINDOW.saturating_add(FINISH_WINDOW));
    let mut next_draft = later(DRAFT_INTERVAL);
    loop {
        if io.cancel.load(Ordering::Acquire) {
            return Err(RuntimeError::Canceled);
        }
        if Instant::now() >= deadline && microphone.is_some() && !finishing {
            if let Some(capture) = microphone.as_mut() {
                audio(pipe, capture)?;
            }
            microphone.take();
            event(io.events, RuntimeEvent::Finishing)?;
            finishing = true;
            deadline = session_end;
        }
        if Instant::now() >= deadline {
            return Err(RuntimeError::TimedOut);
        }
        match io.controls.try_recv() {
            Ok(Control::Capture) if ready && microphone.is_none() && !finishing => {
                microphone = Some(
                    AudioCapture::start_dictation_if_active(io.cancel)
                        .map_err(|_| RuntimeError::MicrophoneUnavailable)?,
                );
                if io.cancel.load(Ordering::Acquire) {
                    return Err(RuntimeError::Canceled);
                }
                deadline = session_end
                    .checked_sub(FINISH_WINDOW)
                    .unwrap_or_else(Instant::now);
                event(io.events, RuntimeEvent::Listening)?;
            }
            Ok(Control::Finish) if ready && !finishing => {
                // Drain the complete final chunk before stopping capture.
                if let Some(capture) = microphone.as_mut() {
                    audio(pipe, capture)?;
                }
                microphone.take();
                event(io.events, RuntimeEvent::Finishing)?;
                finishing = true;
                deadline = (later(FINISH_WINDOW)).min(session_end);
            }
            Ok(_) => return Err(RuntimeError::Protocol),
            Err(TryRecvError::Disconnected) => return Err(RuntimeError::Canceled),
            Err(TryRecvError::Empty) => {}
        }
        if let Some(capture) = microphone.as_mut() {
            audio(pipe, capture)?;
        }
        if ready && pipe.waiting.is_none() {
            if finishing {
                pipe.enqueue(RequestKind::Finish, &[])?;
            } else if microphone.is_some() && Instant::now() >= next_draft {
                pipe.enqueue(RequestKind::Draft, &[])?;
                next_draft = later(DRAFT_INTERVAL);
            }
        }
        pipe.flush(io.output)?;
        if let Some(reply) = pipe.read(io.input)? {
            pipe.accept(&reply)?;
            match reply {
                SpeechReply::Ready { .. } => {
                    ready = true;
                    event(io.events, RuntimeEvent::Ready)?;
                }
                SpeechReply::Draft { text, .. } => event(io.events, RuntimeEvent::Draft(text))?,
                SpeechReply::Done { text, .. } => return Ok(text),
                SpeechReply::Error { .. } => return Err(RuntimeError::Protocol),
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn later(duration: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(duration).unwrap_or(now)
}
fn audio(pipe: &mut Pipe, capture: &mut AudioCapture) -> Result<(), RuntimeError> {
    let bytes = Zeroizing::new(
        capture
            .drain(true)
            .map_err(|_| RuntimeError::AudioDiscontinuity)?,
    );
    for chunk in bytes.chunks(32 * 1024) {
        pipe.enqueue(RequestKind::Audio, chunk)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real private-pipe peer exercises the production actor's ownership,
    // framing and cancellation. Recognition accuracy is qualified separately
    // against the real confined engine and pinned prerecorded speech.
    const PEER: &str = r"
import json, os, struct, sys, time
open(sys.argv[2], 'w').write(str(os.getpid()))
def read():
    p = sys.stdin.buffer.read(4)
    if len(p) != 4: raise SystemExit(2)
    n = struct.unpack('<I', p)[0]
    b = sys.stdin.buffer.read(n)
    if len(b) != n: raise SystemExit(3)
    return b[0], struct.unpack('<Q', b[1:9])[0], b[9:]
def reply(value):
    b = json.dumps(value).encode()
    sys.stdout.buffer.write(struct.pack('<I', len(b)) + b)
    sys.stdout.buffer.flush()
k, s, b = read()
assert k == 0 and s == 1 and json.loads(b)['language'] == 'en'
reply(dict(kind='ready', sequence=2 if sys.argv[1] == 'replay' else s, model='fixture', revision='1'))
k, s, b = read()
assert k == 3 and b == b''
if sys.argv[1] == 'slow': time.sleep(60)
reply(dict(kind='done', sequence=s, text='unsafe\n' if sys.argv[1] == 'control' else 'Corrected 👋'))
";

    fn peer(mode: &str, pid: &std::path::Path) -> Runtime {
        let mut command = Command::new("python3");
        command.args(["-c", PEER, mode]);
        command.arg(pid);
        Runtime::spawn(command, "en").unwrap()
    }
    fn next(runtime: &Runtime) -> RuntimeEvent {
        let deadline = later(Duration::from_secs(5));
        loop {
            if let Some(event) = runtime.poll() {
                return event;
            }
            assert!(Instant::now() < deadline, "actor must report a result");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn reaped(runtime: &mut Runtime, pid: &std::path::Path) {
        let deadline = later(Duration::from_secs(5));
        while !runtime.is_stopped() {
            assert!(Instant::now() < deadline, "owner must stop and reap");
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid = std::fs::read_to_string(pid).unwrap();
        assert!(
            !std::path::Path::new("/proc").join(pid).exists(),
            "the actor's exact child must be reaped"
        );
    }

    #[test]
    fn actual_pipe_actor_finishes_cancels_busy_worker_and_restarts_after_reaping() {
        let directory = tempfile::tempdir().unwrap();
        for (index, mode) in ["normal", "slow", "normal"].iter().enumerate() {
            let pid = directory.path().join(index.to_string());
            let mut runtime = peer(mode, &pid);
            assert!(matches!(next(&runtime), RuntimeEvent::Ready));
            runtime.finish().unwrap();
            assert!(matches!(next(&runtime), RuntimeEvent::Finishing));
            if *mode == "slow" {
                runtime.cancel();
                assert!(matches!(
                    next(&runtime),
                    RuntimeEvent::Stopped(RuntimeError::Canceled)
                ));
            } else {
                assert!(
                    matches!(next(&runtime), RuntimeEvent::Done(text) if text == "Corrected 👋")
                );
            }
            reaped(&mut runtime, &pid);
        }
    }

    #[test]
    fn actual_pipe_actor_rejects_replayed_ready_and_control_character_results() {
        let directory = tempfile::tempdir().unwrap();
        for mode in ["replay", "control"] {
            let pid = directory.path().join(mode);
            let mut runtime = peer(mode, &pid);
            if mode == "control" {
                assert!(matches!(next(&runtime), RuntimeEvent::Ready));
                runtime.finish().unwrap();
                assert!(matches!(next(&runtime), RuntimeEvent::Finishing));
            }
            assert!(matches!(
                next(&runtime),
                RuntimeEvent::Stopped(RuntimeError::Protocol)
            ));
            reaped(&mut runtime, &pid);
        }
    }
}
