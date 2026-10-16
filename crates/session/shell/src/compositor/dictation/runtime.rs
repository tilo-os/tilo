//! Owned speech/capture actor. Every probe, pipe and process wait stays off the
//! compositor thread. A model Ready reply alone never opens the microphone.
use std::collections::VecDeque;
use std::os::fd::AsRawFd as _;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use agent_provider_config::speech::{
    DraftPolicy, FRAME, MAX_TEXT_BYTES, RequestKind, SpeechConfig, SpeechError, SpeechReply,
    request,
};
use zeroize::{Zeroize as _, Zeroizing};

use super::super::audio_capture::AudioCapture;

pub mod vocabulary;

const START_WINDOW: Duration = Duration::from_secs(15);
const RECORD_WINDOW: Duration = Duration::from_secs(105);
// Public CPU qualification takes about nineteen seconds for a seventy-second
// automatic-language final. Keep capture off while a bounded slower pass finishes.
const FINISH_WINDOW: Duration = Duration::from_secs(60);
// A slow draft must not discard live speech. This covers the entire bounded
// PCM16 session plus framing; it grows only as the owned worker backpressures.
const QUEUE_BYTES: usize = 4 * 1024 * 1024;
const MAX_REPLY: usize = 64 * 1024 + 9;

pub use crate::compositor::dictation_status::RuntimeError;

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
    // Unit tests feed framed PCM into the real actor without opening a
    // microphone. This variant and its caller do not exist in an image build.
    #[cfg(test)]
    FixtureAudio {
        bytes: Vec<u8>,
        preview: bool,
    },
}

/// One actor owns one worker and at most one capture for its entire lifetime.
/// Keep this handle until `is_stopped`: cancellation is asynchronous, and a
/// successor must wait for the old actor's actual microphone/worker teardown.
pub struct Runtime {
    commands: SyncSender<Control>,
    events: EventReceiver,
    cancel: Arc<AtomicBool>,
    owner: Option<JoinHandle<()>>,
}
impl Runtime {
    /// Spawn only the immutable image worker. No paths, accounts or network
    /// endpoints can be supplied by a collaborator or model.
    /// # Errors
    /// Rejects invalid languages or an unavailable actor thread.
    pub fn start(language: &str) -> Result<Self, RuntimeError> {
        Self::start_with_vocabulary(language, None)
    }
    /// Use only a host-delivered reader for the authenticated local person.
    /// Binding replacement must cancel the existing compositor session too.
    /// # Errors
    /// Invalid language or unavailable actor; reader failures arrive asynchronously.
    pub fn start_with_vocabulary(
        language: &str,
        vocabulary: Option<Arc<dyn settings_store::personal_dictionary::Vocabulary>>,
    ) -> Result<Self, RuntimeError> {
        Self::spawn_with_vocabulary(Self::image_command(), language, vocabulary)
    }
    /// Field policy comes only from the compositor's original active target.
    /// It grants neither microphone consent nor a different insertion target.
    pub(super) fn start_for_field(
        language: &str,
        vocabulary: Option<Arc<dyn settings_store::personal_dictionary::Vocabulary>>,
        multiline: bool,
    ) -> Result<Self, RuntimeError> {
        Self::spawn_for_field(Self::image_command(), language, vocabulary, multiline)
    }
    fn image_command() -> Command {
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
        command
    }
    #[cfg(test)]
    fn spawn(command: Command, language: &str) -> Result<Self, RuntimeError> {
        Self::spawn_with_vocabulary(command, language, None)
    }
    fn spawn_with_vocabulary(
        command: Command,
        language: &str,
        vocabulary: Option<Arc<dyn settings_store::personal_dictionary::Vocabulary>>,
    ) -> Result<Self, RuntimeError> {
        Self::spawn_for_field(command, language, vocabulary, false)
    }
    fn spawn_for_field(
        mut command: Command,
        language: &str,
        vocabulary: Option<Arc<dyn settings_store::personal_dictionary::Vocabulary>>,
        multiline: bool,
    ) -> Result<Self, RuntimeError> {
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
        let language = language.to_owned();
        let (commands, receiver) = sync_channel(2);
        let (sender, events) = event_channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let canceled = Arc::clone(&cancel);
        let owner = std::thread::Builder::new()
            .name("tilo-dictation".into())
            .spawn(move || {
                let result = (|| {
                    let mut vocabulary = vocabulary
                        .map(|reader| vocabulary::VocabularyLease::capture(reader, &language))
                        .transpose()
                        .map_err(|_| RuntimeError::VocabularyUnavailable)?;
                    let config = SpeechConfig {
                        language,
                        vocabulary: vocabulary
                            .as_ref()
                            .map_or_else(Vec::new, |lease| lease.hints().to_vec()),
                    };
                    let bytes = serde_json::to_vec(&config).map_err(|_| RuntimeError::Protocol)?;
                    drop(config);
                    let mut config = Configuration {
                        bytes: Zeroizing::new(bytes),
                        multiline,
                    };
                    run(
                        &mut command,
                        &mut config,
                        &receiver,
                        &sender,
                        &canceled,
                        &mut vocabulary,
                    )
                })();
                if let Err(error) = result {
                    let _ = event(&sender, RuntimeEvent::Stopped(error));
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
        self.events.poll()
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
    multiline: bool,
    queue: VecDeque<Zeroizing<Vec<u8>>>,
    offset: usize,
    queued: usize,
    incoming: Zeroizing<Vec<u8>>,
    sequence: u64,
    waiting: Option<(RequestKind, u64)>,
}
impl Pipe {
    fn new(multiline: bool) -> Self {
        Self {
            multiline,
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
        let encoded = Zeroizing::new(request(kind, sequence, payload));
        FRAME
            .write(&mut *frame, &encoded)
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
        let mut chunk = Zeroizing::new([0_u8; 4096]);
        loop {
            match input.read(&mut *chunk) {
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
                ..
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
            } => kind == RequestKind::Draft && *got == sequence && valid_text(text, self.multiline),
            SpeechReply::Done {
                sequence: got,
                text,
            } => {
                kind == RequestKind::Finish && *got == sequence && valid_text(text, self.multiline)
            }
            SpeechReply::Error { error, .. } => return Err(RuntimeError::Worker(*error)),
        };
        if !matches {
            return Err(RuntimeError::Protocol);
        }
        self.waiting = None;
        Ok(())
    }
}
fn valid_text(text: &str, multiline: bool) -> bool {
    text.len() <= MAX_TEXT_BYTES
        && !text
            .chars()
            .any(|character| character.is_control() && !(multiline && character == '\n'))
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
// Previews are replaceable render state, not an event history. Keep one
// zeroizing slot so an occupied compositor cannot backpressure recognition.
// Lifecycle events remain ordered in a separate bounded channel.
struct EventSender {
    lifecycle: SyncSender<RuntimeEvent>,
    preview: Arc<Mutex<Option<Zeroizing<String>>>>,
}
struct EventReceiver {
    lifecycle: Receiver<RuntimeEvent>,
    preview: Arc<Mutex<Option<Zeroizing<String>>>>,
}
impl EventReceiver {
    fn poll(&self) -> Option<RuntimeEvent> {
        if let Ok(event) = self.lifecycle.try_recv() {
            return Some(event);
        }
        // Never wait on the compositor thread. The actor holds this lock only
        // while replacing or retiring one bounded preview.
        let mut slot = self.preview.try_lock().ok()?;
        slot.take()
            .map(|mut text| RuntimeEvent::Draft(std::mem::take(&mut *text)))
    }
}
fn event_channel() -> (EventSender, EventReceiver) {
    let (sender, receiver) = sync_channel(8);
    let preview = Arc::new(Mutex::new(None));
    (
        EventSender {
            lifecycle: sender,
            preview: Arc::clone(&preview),
        },
        EventReceiver {
            lifecycle: receiver,
            preview,
        },
    )
}
fn event(sender: &EventSender, event: RuntimeEvent) -> Result<(), RuntimeError> {
    match event {
        RuntimeEvent::Draft(text) => {
            let mut preview = sender.preview.lock().map_err(|_| RuntimeError::Protocol)?;
            *preview = Some(Zeroizing::new(text));
            drop(preview);
            Ok(())
        }
        terminal @ (RuntimeEvent::Done(_) | RuntimeEvent::Stopped(_)) => {
            // A final or failure supersedes every pending preview. Replacement,
            // cancellation and drop all wipe the retired private text.
            sender
                .preview
                .lock()
                .map_err(|_| RuntimeError::Protocol)?
                .take();
            sender
                .lifecycle
                .try_send(terminal)
                .map_err(|_| RuntimeError::Protocol)
        }
        lifecycle => sender
            .lifecycle
            .try_send(lifecycle)
            .map_err(|_| RuntimeError::Protocol),
    }
}
struct Configuration {
    bytes: Zeroizing<Vec<u8>>,
    // Local original-field metadata, never a model-supplied configuration key.
    multiline: bool,
}
fn run(
    command: &mut Command,
    config: &mut Configuration,
    controls: &Receiver<Control>,
    events: &EventSender,
    cancel: &AtomicBool,
    vocabulary: &mut Option<vocabulary::VocabularyLease>,
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
        let mut pipe = Pipe::new(config.multiline);
        pipe.enqueue(RequestKind::Configure, &config.bytes)?;
        config.bytes.zeroize();
        result = pump_io(
            &mut pipe,
            &mut ActorIo {
                input: &mut input,
                output: &mut output,
                controls,
                events,
                cancel,
                vocabulary,
                worker: &worker.0,
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
    events: &'a EventSender,
    cancel: &'a AtomicBool,
    vocabulary: &'a mut Option<vocabulary::VocabularyLease>,
    worker: &'a Child,
}
fn interrupt_draft(worker: &Child, supported: bool, pipe: &Pipe) -> Result<(), RuntimeError> {
    if !supported
        || !matches!(pipe.waiting, Some((RequestKind::Draft, sequence)) if pipe.sequence > sequence)
    {
        return Ok(());
    }
    let pid = i32::try_from(worker.id()).map_err(|_| RuntimeError::Protocol)?;
    // SAFETY: this actor exclusively owns the unreaped Child until pump exits,
    // so its PID cannot be reused. Only that child's advertised handler gets
    // the hint; legacy/AOT workers never receive a terminating SIGUSR1.
    if unsafe { libc::kill(pid, libc::SIGUSR1) } == 0 {
        Ok(())
    } else {
        Err(RuntimeError::Worker(SpeechError::WorkerStopped))
    }
}
fn check_vocabulary(io: &mut ActorIo<'_>) -> Result<(), RuntimeError> {
    if let Some(vocabulary) = io.vocabulary.as_mut() {
        vocabulary.check().map_err(|error| match error {
            vocabulary::LeaseError::Changed => RuntimeError::VocabularyChanged,
            vocabulary::LeaseError::Unavailable => RuntimeError::VocabularyUnavailable,
        })?;
    }
    Ok(())
}
fn finish_capture(
    pipe: &mut Pipe,
    microphone: &mut Option<AudioCapture>,
    io: &ActorIo<'_>,
    finish_interrupts_draft: bool,
) -> Result<(), RuntimeError> {
    if let Some(capture) = microphone.as_mut() {
        audio(pipe, capture)?;
    }
    microphone.take();
    interrupt_draft(io.worker, finish_interrupts_draft, pipe)?;
    event(io.events, RuntimeEvent::Finishing)
}
fn pump_io(pipe: &mut Pipe, io: &mut ActorIo<'_>) -> Result<String, RuntimeError> {
    let mut microphone: Option<AudioCapture> = None;
    let mut ready = false;
    let mut finishing = false;
    let mut deadline = later(START_WINDOW);
    let session_end = later(
        START_WINDOW
            .saturating_add(RECORD_WINDOW)
            .saturating_add(FINISH_WINDOW),
    );
    let mut draft_policy = DraftPolicy::FullContext;
    let mut finish_interrupts_draft = false;
    let mut next_draft = later(draft_policy.interval());
    let mut next_vocabulary_check = Instant::now();
    loop {
        if io.cancel.load(Ordering::Acquire) {
            return Err(RuntimeError::Canceled);
        }
        if Instant::now() >= next_vocabulary_check {
            check_vocabulary(io)?;
            next_vocabulary_check = later(Duration::from_millis(250));
        }
        if Instant::now() >= deadline && microphone.is_some() && !finishing {
            finish_capture(pipe, &mut microphone, io, finish_interrupts_draft)?;
            finishing = true;
            deadline = session_end;
        }
        if Instant::now() >= deadline {
            return Err(RuntimeError::TimedOut);
        }
        match io.controls.try_recv() {
            Ok(Control::Capture) if ready && microphone.is_none() && !finishing => {
                check_vocabulary(io)?;
                microphone = Some(
                    AudioCapture::start_dictation_if_active(io.cancel)
                        .map_err(|_| RuntimeError::MicrophoneUnavailable)?,
                );
                if io.cancel.load(Ordering::Acquire) {
                    return Err(RuntimeError::Canceled);
                }
                deadline = later(RECORD_WINDOW).min(
                    session_end
                        .checked_sub(FINISH_WINDOW)
                        .unwrap_or_else(Instant::now),
                );
                // Anchor context accumulation to actual capture, not model
                // startup or time spent awaiting the user's consent.
                next_draft = later(draft_policy.initial_delay());
                event(io.events, RuntimeEvent::Listening)?;
            }
            #[cfg(test)]
            Ok(Control::FixtureAudio { bytes, preview }) if ready && !finishing => {
                pipe.enqueue(RequestKind::Audio, &bytes)?;
                if preview {
                    pipe.enqueue(RequestKind::Draft, &[])?;
                }
            }
            Ok(Control::Finish) if ready && !finishing => {
                check_vocabulary(io)?;
                // Drain the complete final chunk before stopping capture.
                finish_capture(pipe, &mut microphone, io, finish_interrupts_draft)?;
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
                next_draft = later(draft_policy.interval());
            }
        }
        pipe.flush(io.output)?;
        if let Some(reply) = pipe.read(io.input)? {
            pipe.accept(&reply)?;
            check_vocabulary(io)?;
            match reply {
                SpeechReply::Ready {
                    draft_policy: policy,
                    finish_interrupts_draft: interruptible,
                    ..
                } => {
                    draft_policy = policy;
                    finish_interrupts_draft = interruptible;
                    ready = true;
                    event(io.events, RuntimeEvent::Ready)?;
                }
                SpeechReply::Draft { text, .. } if !finishing => {
                    event(io.events, RuntimeEvent::Draft(text))?;
                }
                SpeechReply::Draft { .. } => {}
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
pub(super) fn dictionary_test_peer(
    reader: Arc<dyn settings_store::personal_dictionary::Vocabulary>,
    pid: &std::path::Path,
    completes: bool,
) -> Runtime {
    let mode = if completes {
        "dictionary_repeated"
    } else {
        "dictionary_slow"
    };
    let peer =
        Runtime::spawn_with_vocabulary(tests::peer_command(mode, pid), "en", Some(reader)).unwrap();
    assert!(matches!(tests::next(&peer), RuntimeEvent::Ready));
    peer
}

#[cfg(test)]
pub(super) fn finishing_test_peer(pid: &std::path::Path) -> Runtime {
    let peer = tests::peer("repeated", pid);
    assert!(matches!(tests::next(&peer), RuntimeEvent::Ready));
    peer
}

#[cfg(test)]
pub(super) fn repair_test_peer(pid: &std::path::Path, language: &str) -> Runtime {
    let mode = match language {
        "de" => "repair_de",
        "fr" => "repair_fr",
        "es" => "repair_es",
        _ => "repair",
    };
    let peer = Runtime::spawn(tests::peer_command(mode, pid), language).unwrap();
    assert!(matches!(tests::next(&peer), RuntimeEvent::Ready));
    peer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_interrupts_stale_worker_draft_without_leaking_preview() {
        let directory = tempfile::tempdir().unwrap();
        let pid = directory.path().join("interrupt");
        let mut runtime = peer("interrupt", &pid);
        assert!(matches!(next(&runtime), RuntimeEvent::Ready));
        assert!(
            runtime
                .commands
                .try_send(Control::FixtureAudio {
                    bytes: vec![0, 1],
                    preview: true,
                })
                .is_ok()
        );
        let deadline = later(Duration::from_secs(3));
        while !pid.with_extension("draft").exists() {
            assert!(Instant::now() < deadline, "real actor never sent the draft");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            runtime
                .commands
                .try_send(Control::FixtureAudio {
                    bytes: vec![0, 2],
                    preview: false,
                })
                .is_ok()
        );
        runtime.finish().unwrap();
        assert!(matches!(next(&runtime), RuntimeEvent::Finishing));
        while !pid.with_extension("final").exists() {
            assert!(
                Instant::now() < deadline,
                "Finish never interrupted the owned draft and drained newer PCM"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            runtime.poll().is_none(),
            "a stale preview escaped while finishing"
        );
        std::fs::write(pid.with_extension("release"), b"release final").unwrap();
        assert!(matches!(next(&runtime), RuntimeEvent::Done(text) if text == "safe final"));
        reaped(&mut runtime, &pid);
    }

    #[test]
    fn finish_interrupt_targets_only_the_owned_supported_draft() {
        use std::io::Read as _;
        let mut command = Command::new("python3");
        command.args([
            "-c",
            r"import os, signal, time
signal.signal(signal.SIGUSR1, lambda *_: os.write(1, b'I'))
os.write(1, b'R')
while True: time.sleep(1)",
        ]);
        command.stdout(Stdio::piped()).stderr(Stdio::null());
        bounded_command::own_process_tree(&mut command);
        let mut worker = Process(command.spawn().unwrap());
        let mut output = worker.0.stdout.take().unwrap();
        let mut byte = [0];
        output.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [b'R']);
        nonblocking(output.as_raw_fd()).unwrap();
        let mut pipe = Pipe::new(false);
        pipe.enqueue(RequestKind::Draft, &[]).unwrap();
        pipe.enqueue(RequestKind::Audio, &[0, 1]).unwrap();
        interrupt_draft(&worker.0, false, &pipe).unwrap();
        assert_eq!(
            output.read(&mut byte).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        pipe.accept(&SpeechReply::Draft {
            sequence: 1,
            text: String::new(),
        })
        .unwrap();
        interrupt_draft(&worker.0, true, &pipe).unwrap();
        assert_eq!(
            output.read(&mut byte).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        pipe.enqueue(RequestKind::Draft, &[]).unwrap();
        // No newer PCM: the in-flight result can be reused as final, so do
        // not throw away that work merely because Finish was requested.
        interrupt_draft(&worker.0, true, &pipe).unwrap();
        assert_eq!(
            output.read(&mut byte).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        pipe.enqueue(RequestKind::Audio, &[0, 1]).unwrap();
        interrupt_draft(&worker.0, true, &pipe).unwrap();
        let deadline = later(Duration::from_secs(2));
        loop {
            match output.read(&mut byte) {
                Ok(1) => break,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "owned handler never observed Finish"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                result => panic!("owned signal peer failed: {result:?}"),
            }
        }
        assert_eq!(byte, [b'I']);
        assert!(worker.0.try_wait().unwrap().is_none());
        // Process owns teardown/reaping even if any assertion above fails.
    }

    // A real private-pipe peer exercises the production actor's ownership,
    // framing and cancellation. Recognition accuracy is qualified separately
    // against the real confined engine and pinned prerecorded speech.
    const PEER: &str = r"
import json, os, signal, struct, sys, time
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
expected_language=sys.argv[1].removeprefix('repair_') if sys.argv[1].startswith('repair_') else 'en'
assert k == 0 and s == 1 and json.loads(b)['language'] == expected_language
if sys.argv[1].startswith('dictionary_'): assert json.loads(b)['vocabulary'] == ['TiLo']
if sys.argv[1] == 'interrupt':
    interrupted = False
    def stop_draft(*_):
        global interrupted
        interrupted = True
    signal.signal(signal.SIGUSR1, stop_draft)
    reply(dict(kind='ready', sequence=s, model='fixture', revision='1', finish_interrupts_draft=True))
    k, s, b = read()
    assert k == 1 and s == 2 and b == bytes([0, 1])
    k, s, b = read()
    assert k == 2 and s == 3 and not b
    open(sys.argv[2] + '.draft', 'w').write('in flight')
    deadline = time.monotonic() + 5
    while not interrupted:
        assert time.monotonic() < deadline
        time.sleep(.005)
    reply(dict(kind='draft', sequence=s, text='stale preview'))
    k, s, b = read()
    assert k == 1 and s == 4 and b == bytes([0, 2])
    k, s, b = read()
    assert k == 3 and s == 5 and not b
    open(sys.argv[2] + '.final', 'w').write('all final PCM received')
    while not os.path.exists(sys.argv[2] + '.release'):
        assert time.monotonic() < deadline
        time.sleep(.005)
    reply(dict(kind='done', sequence=s, text='safe final'))
    raise SystemExit(0)
reply(dict(kind='ready', sequence=2 if sys.argv[1] == 'replay' else s, model='fixture', revision='1'))
if sys.argv[1] == 'backpressure':
    total = 0
    for index in range(60):
        k, s, b = read()
        assert k == 1 and s == index + 2
        assert len(b) == 64000 and b == struct.pack('<h', index) * 32000
        total += len(b)
    k, s, b = read()
    assert k == 3 and s == 62 and b == b''
    reply(dict(kind='done', sequence=s, text=str(total)))
    raise SystemExit(0)
k, s, b = read()
assert k == 3 and b == b''
if sys.argv[1] in ('slow', 'dictionary_slow'): time.sleep(60)
if sys.argv[1] == 'delayed': time.sleep(18)
if sys.argv[1] in ('repeated', 'repair', 'dictionary_repeated') or sys.argv[1].startswith('repair_'):
    deadline = time.monotonic() + 10
    while not os.path.exists(sys.argv[2] + '.release'):
        assert time.monotonic() < deadline
        time.sleep(0.005)
repairs={'repair':'Let me grab two, uh, no, three of these','repair_de':'TiLo braucht zwei, äh, nein, drei Stück.','repair_fr':'Prendre deux, euh, non, trois exemplaires.','repair_es':'TiLo necesita dos, eh, no, tres unidades.'}
paragraphs={'paragraph':'First paragraph.\n\nSecond paragraph.','paragraph_cr':'First\rSecond','paragraph_escape':'First\x1bSecond','paragraph_nul':'First\0Second'}
reply(dict(kind='done', sequence=s, text=repairs[sys.argv[1]] if sys.argv[1] in repairs else paragraphs[sys.argv[1]] if sys.argv[1] in paragraphs else 'unsafe\n' if sys.argv[1] == 'control' else 'Corrected 👋'))
";

    pub(super) fn peer_command(mode: &str, pid: &std::path::Path) -> Command {
        let mut command = Command::new("python3");
        command.args(["-c", PEER, mode]);
        command.arg(pid);
        command
    }
    pub(super) fn peer(mode: &str, pid: &std::path::Path) -> Runtime {
        Runtime::spawn(peer_command(mode, pid), "en").unwrap()
    }
    pub(super) fn next(runtime: &Runtime) -> RuntimeEvent {
        next_within(runtime, Duration::from_secs(5))
    }
    fn next_within(runtime: &Runtime, wait: Duration) -> RuntimeEvent {
        let deadline = later(wait);
        loop {
            if let Some(event) = runtime.poll() {
                return event;
            }
            assert!(Instant::now() < deadline, "actor must report a result");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn stalled_compositor_keeps_latest_preview_and_terminal_result() {
        let (sender, events) = event_channel();
        let (commands, _controls) = sync_channel(2);
        let runtime = Runtime {
            commands,
            events,
            cancel: Arc::new(AtomicBool::new(false)),
            owner: None,
        };
        event(&sender, RuntimeEvent::Ready).unwrap();
        event(&sender, RuntimeEvent::Listening).unwrap();
        // The production publisher keeps decoding while the compositor is
        // busy. A preview is replaceable; lifecycle and final events are not.
        for index in 0..64 {
            event(&sender, RuntimeEvent::Draft(format!("preview {index}"))).unwrap();
        }
        assert!(matches!(runtime.poll(), Some(RuntimeEvent::Ready)));
        assert!(matches!(runtime.poll(), Some(RuntimeEvent::Listening)));
        assert!(matches!(runtime.poll(), Some(RuntimeEvent::Draft(text)) if text == "preview 63"));
        assert!(runtime.poll().is_none(), "old previews must not replay");
        for index in 0..64 {
            event(&sender, RuntimeEvent::Draft(format!("pending {index}"))).unwrap();
        }
        event(&sender, RuntimeEvent::Finishing).unwrap();
        event(&sender, RuntimeEvent::Done("final 👋".into())).unwrap();
        assert!(matches!(runtime.poll(), Some(RuntimeEvent::Finishing)));
        assert!(matches!(runtime.poll(), Some(RuntimeEvent::Done(text)) if text == "final 👋"));
        assert!(runtime.poll().is_none(), "no preview may follow the final");

        event(
            &sender,
            RuntimeEvent::Draft("private canceled preview".into()),
        )
        .unwrap();
        event(&sender, RuntimeEvent::Stopped(RuntimeError::Canceled)).unwrap();
        assert!(matches!(
            runtime.poll(),
            Some(RuntimeEvent::Stopped(RuntimeError::Canceled))
        ));
        assert!(
            runtime.poll().is_none(),
            "cancellation retires the pending preview"
        );
    }

    #[test]
    fn actual_pipe_dictionary_removal_revocation_and_service_drop_reap_busy_worker() {
        use settings_store::personal_dictionary::{Edit, Service, Word};
        let directory = tempfile::tempdir().unwrap();
        for mode in ["remove", "rebind", "drop"] {
            let service = Service::new(directory.path().join(mode)).unwrap();
            let reader = service.bind(Some([7; 32])).unwrap();
            let word = Word::new("TiLo", None).unwrap();
            let snapshot = service.edit(&reader, 0, Edit::Add(word.clone())).unwrap();
            let pid = directory.path().join(format!("{mode}.pid"));
            let mut runtime = Runtime::spawn_with_vocabulary(
                peer_command("dictionary_slow", &pid),
                "en",
                Some(Arc::new(reader.clone())),
            )
            .unwrap();
            assert!(matches!(next(&runtime), RuntimeEvent::Ready));
            runtime.finish().unwrap();
            assert!(matches!(next(&runtime), RuntimeEvent::Finishing));
            match mode {
                "remove" => {
                    service
                        .edit(&reader, snapshot.revision, Edit::Remove(word))
                        .unwrap();
                }
                "rebind" => {
                    service.bind(None);
                    service.bind(Some([7; 32]));
                }
                _ => drop(service),
            }
            let expected = if mode == "remove" {
                RuntimeError::VocabularyChanged
            } else {
                RuntimeError::VocabularyUnavailable
            };
            assert!(matches!(next(&runtime), RuntimeEvent::Stopped(error) if error == expected));
            reaped(&mut runtime, &pid);
            assert!(
                runtime.poll().is_none(),
                "retired vocabulary cannot deliver late words"
            );
        }
    }

    #[test]
    fn actual_pipe_actor_keeps_a_slow_final_without_opening_a_microphone() {
        let directory = tempfile::tempdir().unwrap();
        let pid = directory.path().join("delayed");
        let mut runtime = peer("delayed", &pid);
        assert!(matches!(next(&runtime), RuntimeEvent::Ready));
        runtime.finish().unwrap();
        assert!(matches!(next(&runtime), RuntimeEvent::Finishing));
        assert!(matches!(
            next_within(&runtime, Duration::from_secs(30)),
            RuntimeEvent::Done(text) if text == "Corrected 👋"
        ));
        reaped(&mut runtime, &pid);
    }

    #[test]
    fn actual_private_pipe_preserves_bounded_audio_under_worker_backpressure() {
        let directory = tempfile::tempdir().unwrap();
        let pid = directory.path().join("backpressure");
        let mut command = peer_command("backpressure", &pid);
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
        bounded_command::own_process_tree(&mut command);
        let mut child = Process(command.spawn().unwrap());
        let mut output = child.0.stdin.take().unwrap();
        let mut input = child.0.stdout.take().unwrap();
        nonblocking(input.as_raw_fd()).unwrap();
        nonblocking(output.as_raw_fd()).unwrap();
        let mut pipe = Pipe::new(false);
        pipe.enqueue(RequestKind::Configure, br#"{"language":"en"}"#)
            .unwrap();
        let deadline = later(Duration::from_secs(10));
        loop {
            pipe.flush(&mut output).unwrap();
            if let Some(reply) = pipe.read(&mut input).unwrap() {
                pipe.accept(&reply).unwrap();
                assert!(matches!(reply, SpeechReply::Ready { .. }));
                break;
            }
            assert!(Instant::now() < deadline, "private peer must become ready");
            std::thread::sleep(Duration::from_millis(1));
        }
        // Queue before flushing so the actual OS pipe must backpressure it.
        for index in 0_i16..60 {
            let samples = index.to_le_bytes().repeat(32000);
            pipe.enqueue(RequestKind::Audio, &samples).unwrap();
        }
        pipe.enqueue(RequestKind::Finish, &[]).unwrap();
        loop {
            pipe.flush(&mut output).unwrap();
            if let Some(reply) = pipe.read(&mut input).unwrap() {
                pipe.accept(&reply).unwrap();
                assert!(matches!(reply, SpeechReply::Done { text, .. } if text == "3840000"));
                break;
            }
            assert!(Instant::now() < deadline, "all bounded audio must arrive");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(child.0.wait().unwrap().success());
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

    #[test]
    fn actual_pipe_actor_accepts_only_field_bound_paragraphs_without_opening_a_microphone() {
        let directory = tempfile::tempdir().unwrap();
        for (index, (mode, multiline, accepted)) in [
            ("paragraph", false, false),
            ("paragraph", true, true),
            ("paragraph_cr", true, false),
            ("paragraph_escape", true, false),
            ("paragraph_nul", true, false),
        ]
        .into_iter()
        .enumerate()
        {
            let pid = directory.path().join(index.to_string());
            let mut runtime =
                Runtime::spawn_for_field(peer_command(mode, &pid), "en", None, multiline).unwrap();
            assert!(matches!(next(&runtime), RuntimeEvent::Ready));
            runtime.finish().unwrap();
            assert!(matches!(next(&runtime), RuntimeEvent::Finishing));
            if accepted {
                assert!(matches!(next(&runtime), RuntimeEvent::Done(text)
                    if text == "First paragraph.\n\nSecond paragraph."));
            } else {
                assert!(matches!(
                    next(&runtime),
                    RuntimeEvent::Stopped(RuntimeError::Protocol)
                ));
            }
            reaped(&mut runtime, &pid);
        }
    }
}
