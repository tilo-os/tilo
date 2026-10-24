//! The Discord connector's outside worker (discord-bridge.md §3).
//!
//! The host starts it and talks the connector protocol over stdin and
//! stdout: ops come in as `Perform`, gateway activity goes out as `Event`.
//! The worker keeps one gateway session to the bridged guild, makes the REST
//! calls the ops need, and reaches nothing but Discord's API, gateway and
//! CDN, and Discord's voice servers when it carries a call (see
//! [`egress`] and [`voice`]). The bot token and webhook tokens arrive only
//! as `Secret` messages and stay in memory.

pub mod config;
pub mod egress;
pub mod events;
pub mod gateway;
pub mod perform;
pub mod ratelimit;
pub mod rest;
pub mod secrets;
pub mod voice;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_connector::framing::{read_frame_async, write_frame_async};
use tilo_connector::protocol::{Bytes, FromWorker, PROTOCOL_VERSION, ToWorker};
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::time::Instant;

pub use config::Config;

use crate::gateway::Gateway;
use crate::perform::Performer;
use crate::rest::Rest;
use crate::secrets::Secrets;
use crate::voice::CallCommand;

/// What the gateway and the performs share.
pub struct Ctx {
    pub config: Config,
    pub rest: Arc<Rest>,
    pub secrets: Arc<Secrets>,
    pub out: mpsc::UnboundedSender<FromWorker>,
    guild: Mutex<Option<String>>,
    /// The threads in the one channel bridged, when there is one.
    threads: Mutex<BTreeSet<String>>,
    /// Performs not answered yet, by request, with when they arrived.
    in_flight: Mutex<BTreeMap<u64, Instant>>,
    /// The host's audio for the voice connection, which holds the lock
    /// while it is up.
    pub audio: Arc<tokio::sync::Mutex<mpsc::Receiver<(String, Bytes)>>>,
}

/// How often the host hears how far behind the worker is.
const METRICS_EVERY: Duration = Duration::from_secs(5);

impl Ctx {
    /// Tell the host how the connection is.
    pub fn status(&self, healthy: bool, message: &str) {
        let _ = self.out.send(FromWorker::Status {
            healthy,
            message: message.to_owned(),
        });
    }

    pub fn connected(&self) {
        self.status(
            true,
            if self.config.read_only {
                "connected to Discord (read-only)"
            } else {
                "connected to Discord"
            },
        );
    }

    pub fn set_guild(&self, guild: Option<String>) {
        if let Ok(mut held) = self.guild.lock() {
            *held = guild;
        }
    }

    fn begin(&self, request: u64) {
        if let Ok(mut in_flight) = self.in_flight.lock() {
            in_flight.insert(request, Instant::now());
        }
    }

    fn end(&self, request: u64) {
        if let Ok(mut in_flight) = self.in_flight.lock() {
            in_flight.remove(&request);
        }
    }

    /// Ops in flight, the age of the oldest, and the rate-limit count.
    fn metrics(&self) -> FromWorker {
        let (queue_depth, oldest) = self.in_flight.lock().map_or((0, None), |in_flight| {
            (
                u64::try_from(in_flight.len()).unwrap_or(u64::MAX),
                in_flight.values().min().copied(),
            )
        });
        FromWorker::Metrics {
            queue_depth,
            lag_seconds: oldest.map_or(0.0, |at| at.elapsed().as_secs_f64()),
            rate_limited_total: self.rest.rate_limited_total(),
        }
    }

    /// Whether `channel` is bridged: any is, unless the bridge is confined
    /// to one channel, then only it and the threads in it are.
    #[must_use]
    pub fn in_scope(&self, channel: &str) -> bool {
        self.config.only_channel.as_ref().is_none_or(|only| {
            only == channel
                || self
                    .threads
                    .lock()
                    .is_ok_and(|threads| threads.contains(channel))
        })
    }

    /// Note threads that grow in the one channel bridged.
    pub fn threads_in(&self, threads: impl IntoIterator<Item = (String, String)>) {
        let Some(only) = &self.config.only_channel else {
            return;
        };
        if let Ok(mut held) = self.threads.lock() {
            held.extend(
                threads
                    .into_iter()
                    .filter(|(_, parent)| parent == only)
                    .map(|(thread, _)| thread),
            );
        }
    }

    /// The bridged guild, once the gateway knows it.
    #[must_use]
    pub fn guild(&self) -> Option<String> {
        self.guild.lock().ok().and_then(|g| g.clone())
    }
}

/// Serve the connector protocol on `input` and `output` until the host
/// says `Stop` or closes `input`.
///
/// # Errors
/// The host broke the protocol or the pipes failed.
pub async fn run<R, W>(config: Config, mut input: R, mut output: W) -> Result<(), String>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (out, mut outgoing) = mpsc::unbounded_channel::<FromWorker>();
    let writer = tokio::spawn(async move {
        while let Some(message) = outgoing.recv().await {
            if write_frame_async(&mut output, &message).await.is_err() {
                return;
            }
        }
        let _ = output.flush().await;
    });

    let checkpoint = match read_frame_async::<_, ToWorker>(&mut input).await {
        Ok(Some(ToWorker::Start { checkpoint, .. })) => checkpoint,
        Ok(_) => return Err("the host did not start with Start".into()),
        Err(error) => return Err(format!("reading Start: {error}")),
    };
    let _ = out.send(FromWorker::Ready {
        version: PROTOCOL_VERSION,
    });

    let secrets = Arc::new(Secrets::new(out.clone()));
    let rest = Arc::new(Rest::new(
        config.api_base.clone(),
        config.egress,
        Arc::clone(&secrets),
        out.clone(),
    )?);
    let (audio, audio_rx) = mpsc::channel(voice::AUDIO_QUEUE);
    let (calls, calls_rx) = mpsc::unbounded_channel::<CallCommand>();
    let ctx = Arc::new(Ctx {
        config,
        rest,
        secrets: Arc::clone(&secrets),
        out: out.clone(),
        guild: Mutex::new(None),
        threads: Mutex::new(BTreeSet::new()),
        in_flight: Mutex::new(BTreeMap::new()),
        audio: Arc::new(tokio::sync::Mutex::new(audio_rx)),
    });
    let performer = Arc::new(Performer::new(Arc::clone(&ctx)));
    let gateway = tokio::spawn(Gateway::new(Arc::clone(&ctx), checkpoint).run(calls_rx));
    let metrics = {
        let ctx = Arc::clone(&ctx);
        tokio::spawn(async move {
            let mut every = tokio::time::interval(METRICS_EVERY);
            every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                every.tick().await;
                if ctx.out.send(ctx.metrics()).is_err() {
                    return;
                }
            }
        })
    };

    let ended = loop {
        let message = match read_frame_async::<_, ToWorker>(&mut input).await {
            Ok(Some(message)) => message,
            Ok(None) => break Ok(()),
            Err(error) => break Err(format!("reading from the host: {error}")),
        };
        match message {
            ToWorker::Perform { request, op } => {
                ctx.begin(request);
                let performing = performer.perform(op);
                let ctx = Arc::clone(&ctx);
                tokio::spawn(async move {
                    let outcome = performing.await;
                    ctx.end(request);
                    let out = &ctx.out;
                    let _ = out.send(FromWorker::Result { request, outcome });
                });
            }
            ToWorker::Blob { blake3, bytes } => performer.blob(&blake3, &bytes),
            ToWorker::Secret {
                purpose,
                item,
                bytes,
            } => secrets.deliver(&purpose, &item, bytes.as_ref()),
            ToWorker::Call { call, status } => {
                let _ = calls.send(CallCommand { call, status });
            }
            // Dropped when the queue is full: late audio is worse than a gap.
            ToWorker::Audio { speaker, opus } => {
                let _ = audio.try_send((speaker, opus));
            }
            ToWorker::Stop => break Ok(()),
            ToWorker::Start { .. } => {
                let _ = out.send(FromWorker::Status {
                    healthy: true,
                    message: "ignored a second Start".into(),
                });
            }
        }
    };
    gateway.abort();
    metrics.abort();
    drop((ctx, performer, secrets, out, calls, audio));
    // In-flight performs still hold senders; give the writer what is
    // queued now and stop.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), writer).await;
    ended
}
