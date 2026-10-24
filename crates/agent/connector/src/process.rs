//! Start a connector's outside worker, keep it running, and carry ops,
//! blobs, secrets, events and checkpoints between it and the host.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::BufReader;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, broadcast, oneshot, watch};

use crate::EventLog;
use crate::framing::{read_frame_async, write_frame_async};
use crate::protocol::{
    ATTACH_FIELD, Bytes, CallLink, FromWorker, Outcome, OutsideCall, PROTOCOL_VERSION,
    PUBLISH_FIELD, ToWorker,
};

/// The host's content store, as far as a worker's ops need it. The worker
/// never reads the store itself; the host hands it bytes and keeps what it
/// downloads.
pub trait BlobAccess: Send + Sync + 'static {
    /// The bytes of the blob with this BLAKE3 hex hash, if the store has it.
    fn read(&self, blake3: &str) -> Option<Vec<u8>>;
    /// Keep `bytes` and return their BLAKE3 hex hash.
    ///
    /// # Errors
    /// The store refused or failed, as a message for the host's log.
    fn store(&self, bytes: &[u8]) -> Result<String, String>;
    /// Make the blob fetchable at its public URL, so the service can load it.
    ///
    /// # Errors
    /// The blob cannot be published, as a message for the host's log.
    fn publish(&self, blake3: &str) -> Result<(), String>;
}

#[derive(Clone, Debug)]
pub struct WorkerCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Where to keep the worker's `Metrics` as Prometheus text (a
    /// `metrics.d/<name>.prom` a metrics endpoint reads); `None` for no
    /// file.
    pub metrics_file: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PerformError {
    #[error("refused: {0}")]
    Refused(String),
    #[error("unavailable: {0}")]
    Unavailable(String),
}

/// Field of a queued entry holding the op the worker performs.
const OP_FIELD: &str = "op";
const START_TIMEOUT: Duration = Duration::from_secs(10);
const PERFORM_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

type Answer = oneshot::Sender<Result<serde_json::Value, PerformError>>;

/// Audio frames from the worker a slow listener may fall behind by before
/// it misses the oldest: live audio is dropped, never queued.
const AUDIO_BACKLOG: usize = 256;

/// The service's calls and the worker's place in them, as it last said.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallsReport {
    pub calls: Vec<OutsideCall>,
    pub link: CallLink,
}

/// One Opus frame a person said in the outside call the worker is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutsideAudio {
    /// Their account on the service.
    pub speaker: String,
    pub opus: Vec<u8>,
}

struct Shared {
    state_dir: PathBuf,
    metrics_file: Option<PathBuf>,
    secrets: tilo_secrets::SecretStore,
    blobs: Arc<dyn BlobAccess>,
    events: Arc<Mutex<EventLog>>,
    seq: watch::Sender<u64>,
    status: std::sync::Mutex<(bool, String)>,
    stdin: Mutex<Option<ChildStdin>>,
    pending: Mutex<HashMap<u64, Answer>>,
    next_request: AtomicU64,
    /// The outside call the host wants the worker in, said again to every
    /// worker it starts.
    wanted_call: std::sync::Mutex<Option<(Option<String>, Option<String>)>>,
    calls: watch::Sender<CallsReport>,
    audio: broadcast::Sender<OutsideAudio>,
}

pub struct ConnectorProcess {
    shared: Arc<Shared>,
    supervisor: tokio::task::JoinHandle<()>,
}

impl ConnectorProcess {
    /// Start the worker and keep it running until [`Self::stop`].
    ///
    /// # Errors
    /// The state directory cannot be prepared or the first start fails.
    pub async fn start(
        _app_id: &str,
        command: WorkerCommand,
        state_dir: &Path,
        secrets: tilo_secrets::SecretStore,
        blobs: Arc<dyn BlobAccess>,
    ) -> std::io::Result<Self> {
        let events = EventLog::open(&state_dir.join("events"))?;
        let (seq, _) = watch::channel(events.last_seq());
        let shared = Arc::new(Shared {
            state_dir: state_dir.to_owned(),
            metrics_file: command.metrics_file.clone(),
            secrets,
            blobs,
            events: Arc::new(Mutex::new(events)),
            seq,
            status: std::sync::Mutex::new((false, String::from("starting"))),
            stdin: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            next_request: AtomicU64::new(1),
            wanted_call: std::sync::Mutex::new(None),
            calls: watch::channel(CallsReport::default()).0,
            audio: broadcast::channel(AUDIO_BACKLOG).0,
        });
        let child = launch(&shared, &command).await?;
        let supervisor = tokio::spawn(supervise(Arc::clone(&shared), command, child));
        Ok(Self { shared, supervisor })
    }

    /// Perform one queued op: an outbox entry's value, `{"op": <the op the
    /// worker performs>, "publish": [..], "attach": [..]}`. The blob lists
    /// are the host's business, so only the inner op reaches the worker.
    ///
    /// # Errors
    /// [`PerformError::Refused`] to dead-letter, [`PerformError::Unavailable`] to retry.
    pub async fn perform(
        &self,
        mut queued: serde_json::Value,
    ) -> Result<serde_json::Value, PerformError> {
        let op = queued
            .get_mut(OP_FIELD)
            .map(serde_json::Value::take)
            .ok_or_else(|| PerformError::Refused("queued entry names no op".into()))?;
        for hash in names(&queued, PUBLISH_FIELD) {
            self.shared
                .blobs
                .publish(&hash)
                .map_err(PerformError::Unavailable)?;
        }
        let mut frames = Vec::new();
        for hash in names(&queued, ATTACH_FIELD) {
            // A member's file reaches the host's store after the message
            // naming it: the host asks the poster for it once it reads the
            // message. Try again until it arrived rather than give up.
            let bytes = self.shared.blobs.read(&hash).ok_or_else(|| {
                PerformError::Unavailable("the attachment has not reached this host yet".into())
            })?;
            frames.push(ToWorker::Blob {
                blake3: hash,
                bytes: Bytes(bytes),
            });
        }
        let request = self.shared.next_request.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = oneshot::channel();
        self.shared.pending.lock().await.insert(request, answer);
        frames.push(ToWorker::Perform { request, op });
        if let Err(error) = send(&self.shared, &frames).await {
            self.shared.pending.lock().await.remove(&request);
            return Err(PerformError::Unavailable(error.to_string()));
        }
        match tokio::time::timeout(PERFORM_TIMEOUT, answered).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(PerformError::Unavailable("worker exited".into())),
            Err(_) => {
                self.shared.pending.lock().await.remove(&request);
                Err(PerformError::Unavailable("timed out".into()))
            }
        }
    }

    #[must_use]
    pub fn events(&self) -> Arc<Mutex<EventLog>> {
        Arc::clone(&self.shared.events)
    }

    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.shared.seq.subscribe()
    }

    #[must_use]
    pub fn status(&self) -> (bool, String) {
        self.shared
            .status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    /// Have the worker be in the outside call `call` from now on, showing
    /// `status` there (`None` leaves). Kept, and said again to a restarted
    /// worker.
    ///
    /// # Errors
    /// The worker is not running now; a worker that starts later is told.
    pub async fn set_call(
        &self,
        call: Option<String>,
        status: Option<String>,
    ) -> std::io::Result<()> {
        if let Ok(mut wanted) = self.shared.wanted_call.lock() {
            *wanted = Some((call.clone(), status.clone()));
        }
        send(&self.shared, &[ToWorker::Call { call, status }]).await
    }

    /// The service's calls and the worker's place in them, as they change.
    #[must_use]
    pub fn calls(&self) -> watch::Receiver<CallsReport> {
        self.shared.calls.subscribe()
    }

    /// The audio the outside call says, frame by frame, from now on.
    #[must_use]
    pub fn audio(&self) -> broadcast::Receiver<OutsideAudio> {
        self.shared.audio.subscribe()
    }

    /// Hand the worker one Opus frame the room's call heard from `speaker`.
    ///
    /// # Errors
    /// The worker is not running.
    pub async fn send_audio(&self, speaker: String, opus: Vec<u8>) -> std::io::Result<()> {
        send(
            &self.shared,
            &[ToWorker::Audio {
                speaker,
                opus: Bytes(opus),
            }],
        )
        .await
    }

    pub async fn stop(self) {
        self.supervisor.abort();
        let stdin = self.shared.stdin.lock().await.take();
        if let Some(mut stdin) = stdin {
            let _ = write_frame_async(&mut stdin, &ToWorker::Stop).await;
        }
    }
}

/// Write `frames` back to back, so no other message lands between an op's
/// blobs and the op itself.
async fn send(shared: &Shared, frames: &[ToWorker]) -> std::io::Result<()> {
    let mut stdin = shared.stdin.lock().await;
    let result = match stdin.as_mut() {
        Some(stdin) => write_all(stdin, frames).await,
        None => Err(std::io::Error::other("worker not running")),
    };
    drop(stdin);
    result
}

async fn write_all(stdin: &mut ChildStdin, frames: &[ToWorker]) -> std::io::Result<()> {
    for frame in frames {
        write_frame_async(stdin, frame).await?;
    }
    Ok(())
}

fn names(queued: &serde_json::Value, field: &str) -> Vec<String> {
    queued
        .get(field)
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

async fn launch(shared: &Arc<Shared>, command: &WorkerCommand) -> std::io::Result<Child> {
    let mut child = Command::new(&command.program)
        .args(&command.args)
        .env_clear()
        .envs(command.env.iter().cloned())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("no stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("no stdout"))?;
    let checkpoint = std::fs::read(shared.state_dir.join("checkpoint.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    write_frame_async(
        &mut stdin,
        &ToWorker::Start {
            version: PROTOCOL_VERSION,
            checkpoint,
        },
    )
    .await?;
    let mut reader = BufReader::new(stdout);
    let ready = tokio::time::timeout(
        START_TIMEOUT,
        read_frame_async::<_, FromWorker>(&mut reader),
    )
    .await
    .map_err(|_| std::io::Error::other("worker did not answer Start"))??;
    if !matches!(ready, Some(FromWorker::Ready { version }) if version == PROTOCOL_VERSION) {
        return Err(std::io::Error::other(
            "worker speaks another protocol version",
        ));
    }
    let wanted = shared
        .wanted_call
        .lock()
        .ok()
        .and_then(|wanted| wanted.clone());
    if let Some((call, status)) = wanted {
        write_frame_async(&mut stdin, &ToWorker::Call { call, status }).await?;
    }
    *shared.stdin.lock().await = Some(stdin);
    tokio::spawn(read_loop(Arc::clone(shared), reader));
    Ok(child)
}

async fn supervise(shared: Arc<Shared>, command: WorkerCommand, mut child: Child) {
    loop {
        let _ = child.wait().await;
        shared.stdin.lock().await.take();
        // A stopped worker is in no call.
        shared.calls.send_replace(CallsReport::default());
        for (_, answer) in shared.pending.lock().await.drain() {
            let _ = answer.send(Err(PerformError::Unavailable("worker exited".into())));
        }
        set_status(&shared, false, "the connector's worker stopped; restarting");
        let mut backoff = Duration::from_secs(1);
        child = loop {
            tokio::time::sleep(backoff).await;
            backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
            if let Ok(next) = launch(&shared, &command).await {
                break next;
            }
        };
    }
}

fn set_status(shared: &Shared, healthy: bool, message: &str) {
    if let Ok(mut status) = shared.status.lock() {
        *status = (healthy, message.to_owned());
    }
}

/// The secret purposes a worker may ask for; `SecretStore::item` wants a
/// `&'static str`, and a worker naming anything else gets nothing.
fn secret_purpose(purpose: &str) -> Option<&'static str> {
    match purpose {
        "bot-token" => Some("bot-token"),
        "webhook" => Some("webhook"),
        _ => None,
    }
}

/// The worker's metrics as Prometheus text.
#[must_use]
pub fn metrics_text(queue_depth: u64, lag_seconds: f64, rate_limited_total: u64) -> String {
    let lag = if lag_seconds.is_finite() {
        lag_seconds.max(0.0)
    } else {
        0.0
    };
    format!(
        "# HELP tilo_rooms_bridge_lag_seconds Age of the oldest op in flight or waiting on a rate limit.\n\
         # TYPE tilo_rooms_bridge_lag_seconds gauge\n\
         tilo_rooms_bridge_lag_seconds {lag:.3}\n\
         # HELP tilo_rooms_bridge_queue_depth Ops in flight or waiting.\n\
         # TYPE tilo_rooms_bridge_queue_depth gauge\n\
         tilo_rooms_bridge_queue_depth {queue_depth}\n\
         # HELP tilo_rooms_bridge_rate_limited_total Calls that hit a 429 or waited for a rate-limit bucket.\n\
         # TYPE tilo_rooms_bridge_rate_limited_total counter\n\
         tilo_rooms_bridge_rate_limited_total {rate_limited_total}\n"
    )
}

/// Replace `path` so a reader sees the old file or the new one, never half.
async fn read_loop(shared: Arc<Shared>, mut reader: BufReader<ChildStdout>) {
    while let Ok(Some(message)) = read_frame_async::<_, FromWorker>(&mut reader).await {
        match message {
            FromWorker::Ready { .. } => {}
            FromWorker::Result { request, outcome } => {
                let answer = match outcome {
                    Outcome::Done { result } => Ok(result),
                    Outcome::Refused { reason } => Err(PerformError::Refused(reason)),
                    Outcome::Unavailable { reason } => Err(PerformError::Unavailable(reason)),
                };
                let waiting = shared.pending.lock().await.remove(&request);
                if let Some(waiting) = waiting {
                    let _ = waiting.send(answer);
                }
            }
            FromWorker::Event {
                source,
                id,
                at_ms,
                payload,
            } => {
                let appended = shared.events.lock().await.append(
                    &source,
                    &id,
                    at_ms,
                    tilo_clock::unix_now_ms_i64(),
                    payload,
                );
                if let Ok(Some(seq)) = appended {
                    shared.seq.send_replace(seq);
                }
            }
            // Stored synchronously, so it is in the store before the
            // `Result` that names it is read off the same stream.
            FromWorker::Blob { bytes } => {
                let _ = shared.blobs.store(&bytes.0);
            }
            FromWorker::NeedSecret { purpose, item } => {
                let bytes = match secret_purpose(&purpose) {
                    Some(known) => shared.secrets.item(known, &item).get().await.ok().flatten(),
                    None => None,
                }
                .map(|secret| Bytes(secret.expose().to_vec()));
                let answer = ToWorker::Secret {
                    purpose,
                    item,
                    bytes,
                };
                let _ = send(&shared, std::slice::from_ref(&answer)).await;
            }
            FromWorker::StoreSecret {
                purpose,
                item,
                bytes,
            } => {
                if purpose == "webhook" {
                    let _ = shared.secrets.item("webhook", &item).put(&bytes.0).await;
                }
            }
            FromWorker::Checkpoint { state } => {
                if let Ok(bytes) = serde_json::to_vec(&state) {
                    let _ = tilo_files::native::atomic::write_creating_parent(
                        &shared.state_dir.join("checkpoint.json"),
                        &bytes,
                    );
                }
            }
            FromWorker::Status { healthy, message } => set_status(&shared, healthy, &message),
            FromWorker::Calls { calls, link } => {
                shared.calls.send_replace(CallsReport { calls, link });
            }
            FromWorker::Audio { speaker, mut opus } => {
                // Audio is no secret: moved out rather than copied and zeroed.
                let opus = std::mem::take(&mut opus.0);
                let _ = shared.audio.send(OutsideAudio { speaker, opus });
            }
            FromWorker::Metrics {
                queue_depth,
                lag_seconds,
                rate_limited_total,
            } => {
                if let Some(path) = &shared.metrics_file {
                    let text = metrics_text(queue_depth, lag_seconds, rate_limited_total);
                    let _ =
                        tilo_files::native::atomic::write_creating_parent(path, text.as_bytes());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_file_is_prometheus_text_replaced_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metrics.d").join("bridge.prom");
        tilo_files::native::atomic::write_creating_parent(
            &path,
            metrics_text(3, 1.25, 7).as_bytes(),
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let samples: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(
            samples,
            [
                "tilo_rooms_bridge_lag_seconds 1.250",
                "tilo_rooms_bridge_queue_depth 3",
                "tilo_rooms_bridge_rate_limited_total 7",
            ]
        );
        assert!(text.contains("# TYPE tilo_rooms_bridge_rate_limited_total counter"));
        tilo_files::native::atomic::write_creating_parent(
            &path,
            metrics_text(0, f64::NAN, 7).as_bytes(),
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("tilo_rooms_bridge_lag_seconds 0.000\n"));
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }
}
