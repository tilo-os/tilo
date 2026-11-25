//! Grayjay device sync for tilolang packages: the YouTube app's bridge to the Grayjay app a
//! person already uses.
//!
//! Grayjay keeps subscriptions and watch history on the person's phone or desktop. Its Sync
//! screen shows a device as a pairing invite; a client that presents it over Grayjay's own
//! Noise protocol (see [`protocol`] for every wire fact, and [`transport`] for the handshake)
//! reads the subscriptions and history there and sends the app's own subscriptions back. This
//! is a bridge to the other app, not a second sync of Tilo's workspaces: nothing here writes a
//! Tilo resource, and the app records the devices it paired.
//!
//! The device's Noise static key is derived from the key this device holds for the app's own
//! purpose ([`PURPOSE`]), which the device's daemon derives from the device's own key and hands
//! the app's own process alone: never a record and never a vault item, since the key is this
//! device's identity to Grayjay, and a copy of it on another device would be the same device
//! twice. It is kept nowhere: asked for again each time it is needed. A pairing code arrives in the invite, is used by the one handshake, and is stored
//! nowhere.
//!
//! [`Engine`] is what `engine.call` and `engine.start` answer: `device` says the key this
//! device presents, `pair` runs one pairing, and `sync` exchanges with every approved device
//! the app names. Pairing and syncing touch the network, so they are jobs the app follows by
//! id, never work a view waits on.

#![allow(
    clippy::doc_markdown,
    reason = "YouTube and Grayjay are names, not code: they read as written in prose"
)]
#![allow(
    clippy::arithmetic_side_effects,
    reason = "wire lengths are bounded by the protocol's own packet and payload limits, checked \
              where bytes enter"
)]

pub mod protocol;
#[cfg(feature = "testing")]
pub mod testing;
pub mod transport;

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::transport::{DeviceKey, SyncClient};

/// The one app this bridge is the YouTube app's.
pub const APP: &str = "so.tilo.tube";

/// What the app is called on Grayjay's side, as the device it syncs with.
pub const DEVICE_NAME: &str = "Tilo Tube";

/// What the app asks the device's key for: its Noise static key to Grayjay.
pub const PURPOSE: &str = "grayjay-noise";

/// Why a sync could not go through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The device cannot do this now: no network.
    Unavailable(String),
    /// A pairing invite or handshake did not hold up.
    Pairing(String),
    /// The connection failed or said something a frame cannot hold.
    Transport(String),
    /// The device key's secret could not be had.
    KeyUnavailable(String),
    /// The app's record of a device cannot be used as a route.
    Persistence(String),
    /// The device was revoked here: its key pairs no more.
    Revoked,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(why) => write!(f, "Grayjay sync is unavailable: {why}"),
            Self::Pairing(why) => write!(f, "Pairing failed: {why}"),
            Self::Transport(why) => write!(f, "Grayjay could not be reached: {why}"),
            Self::KeyUnavailable(why) => {
                write!(f, "This device's Grayjay key is unavailable: {why}")
            }
            Self::Persistence(why) => write!(f, "The paired device cannot be used: {why}"),
            Self::Revoked => write!(f, "This device was revoked here"),
        }
    }
}

impl std::error::Error for Error {}

/// Where a device's Grayjay key comes from: the key the device holds for the app ([`PURPOSE`]),
/// as its engine host hands it ([`FixedKey`]).
pub trait KeySource: Send + Sync {
    /// The seed the device key is derived from, wiped from memory when dropped.
    fn seed(&self) -> Result<Zeroizing<Vec<u8>>, Error>;
}

/// A seed the caller holds: the key the device holds for the app, asked for the call that runs
/// now, or a test's.
#[derive(Clone)]
pub struct FixedKey(Zeroizing<Vec<u8>>);

impl FixedKey {
    #[must_use]
    pub fn new(seed: impl Into<Vec<u8>>) -> Self {
        Self(Zeroizing::new(seed.into()))
    }

    /// The key the device holds for the app, taken: the array it came in is wiped.
    #[must_use]
    pub fn taking(mut key: [u8; 32]) -> Self {
        let seed = Self::new(key.to_vec());
        zeroize::Zeroize::zeroize(&mut key);
        seed
    }
}

impl KeySource for FixedKey {
    fn seed(&self) -> Result<Zeroizing<Vec<u8>>, Error> {
        Ok(self.0.clone())
    }
}

/// Where a paired Grayjay device answers: what the app records of it, and all a sync needs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerRoute {
    /// The device's Noise static key, base64: what Grayjay shows as the fingerprint, and what
    /// identifies the device from then on. Not `id`: the app reads it as a field, and a field
    /// called `id` is a resource's own id in the language.
    pub key: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub addresses: Vec<String>,
    pub port: i32,
}

/// One channel as both apps name it: the URL Grayjay's subscription carries, and the name the
/// app knows it by.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedSubscription {
    pub url: String,
    #[serde(default)]
    pub name: String,
}

/// What one device answered when asked for everything it holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pulled {
    pub subscriptions: Vec<SharedSubscription>,
    /// The videos it watched, as the URLs Grayjay records them by.
    pub history: Vec<String>,
}

/// What a pairing asks: the invite Grayjay showed.
#[derive(Clone, Debug, Deserialize)]
pub struct PairAsk {
    pub invite: String,
}

/// What a sync asks: the approved devices, and the subscriptions to send them.
#[derive(Clone, Debug, Deserialize)]
pub struct SyncAsk {
    pub peers: Vec<PeerRoute>,
    #[serde(default)]
    pub subscriptions: Vec<SharedSubscription>,
}

/// How one device's exchange went.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PeerOutcome {
    pub key: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
}

/// What a sync came to: how each device went, and everything read from the ones that answered.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SyncAnswer {
    pub peers: Vec<PeerOutcome>,
    pub subscriptions: Vec<SharedSubscription>,
    pub history: Vec<String>,
}

impl SyncAnswer {
    /// The devices that failed, for the app to say which did.
    #[must_use]
    pub fn failures(&self) -> usize {
        self.peers.iter().filter(|peer| !peer.ok).count()
    }
}

/// Merge `incoming` into `merged`, keeping the first sight of each subscription and video.
fn merge(merged: &mut SyncAnswer, incoming: Pulled) {
    for subscription in incoming.subscriptions {
        if !merged
            .subscriptions
            .iter()
            .any(|seen| seen.url == subscription.url)
        {
            merged.subscriptions.push(subscription);
        }
    }
    for video in incoming.history {
        if !merged.history.contains(&video) {
            merged.history.push(video);
        }
    }
}

/// The bridge itself: this device's key, and the network the exchanges run over.
#[derive(Clone)]
pub struct Engine {
    key: Arc<dyn KeySource>,
    device_name: String,
}

impl Engine {
    /// The engine over `key`.
    #[must_use]
    pub fn with_key_source(key: Arc<dyn KeySource>) -> Self {
        Self {
            key,
            device_name: DEVICE_NAME.to_owned(),
        }
    }

    /// This device's key and the client that speaks with it.
    fn client(&self) -> Result<SyncClient, Error> {
        let seed = self.key.seed()?;
        let mut digest = Sha256::new();
        digest.update(b"tilo.tube.grayjay-noise-static-v1\0");
        digest.update(seed.as_slice());
        let private = Zeroizing::new(digest.finalize().to_vec());
        let key = DeviceKey::from_private(private.as_slice())?;
        Ok(SyncClient::new(key, self.device_name.clone()))
    }

    /// The base64 key this device presents to Grayjay: the fingerprint Grayjay shows.
    pub fn device_public_key(&self) -> Result<String, Error> {
        Ok(self.client()?.public_key())
    }

    /// Add the device whose invite this is. The answer is the route to record; the person
    /// approves it afterwards, which is the app's own record change.
    pub fn pair(&self, invite: &str) -> Result<PeerRoute, Error> {
        self.client()?.pair(invite)
    }

    /// Exchange with every device in `ask`, reading what each holds and sending the
    /// subscriptions in return. One device failing never stops the others.
    pub fn sync(&self, ask: &SyncAsk) -> SyncAnswer {
        let mut answer = SyncAnswer::default();
        let client = self.client();
        for peer in &ask.peers {
            let outcome = match client.as_ref() {
                Ok(client) => client.sync(peer, &ask.subscriptions),
                Err(error) => Err(error.clone()),
            };
            match outcome {
                Ok(pulled) => {
                    merge(&mut answer, pulled);
                    answer.peers.push(PeerOutcome {
                        key: peer.key.clone(),
                        ok: true,
                        error: String::new(),
                    });
                }
                Err(error) => answer.peers.push(PeerOutcome {
                    key: peer.key.clone(),
                    ok: false,
                    error: error.to_string(),
                }),
            }
        }
        answer
    }

    /// One `engine.call`: `device` answers this device's key, and nothing else runs now.
    pub fn call(&self, op: &str, _input: &str) -> Result<String, String> {
        match op {
            "device" => {
                let key = self
                    .device_public_key()
                    .map_err(|error| error.to_string())?;
                Ok(serde_json::json!({ "public_key": key }).to_string())
            }
            other => Err(format!("`{other}` is no op the Grayjay bridge answers now")),
        }
    }

    /// One `engine.start`: `pair` or `sync`, run as the job the host names.
    pub fn start(&self, job: &str, op: &str, input: &str) -> Result<(), String> {
        match op {
            "pair" => {
                let ask: PairAsk = serde_json::from_str(input)
                    .map_err(|error| format!("the invite does not read: {error}"))?;
                let engine = self.clone();
                spawn_job(job, move |job: &Job| {
                    if job.cancelled() {
                        return Ok(JobEnd::Cancelled);
                    }
                    let route = engine
                        .pair(&ask.invite)
                        .map_err(|error| error.to_string())?;
                    Ok(JobEnd::Answer(serde_json::to_string(&route).map_err(
                        |error| format!("the device's answer does not write: {error}"),
                    )?))
                })
            }
            "sync" => {
                let ask: SyncAsk = serde_json::from_str(input)
                    .map_err(|error| format!("the sync ask does not read: {error}"))?;
                let engine = self.clone();
                spawn_job(job, move |job: &Job| {
                    let mut answer = SyncAnswer::default();
                    let client = engine.client().map_err(|error| error.to_string())?;
                    for peer in &ask.peers {
                        if job.cancelled() {
                            return Ok(JobEnd::Cancelled);
                        }
                        match client.sync(peer, &ask.subscriptions) {
                            Ok(pulled) => {
                                merge(&mut answer, pulled);
                                answer.peers.push(PeerOutcome {
                                    key: peer.key.clone(),
                                    ok: true,
                                    error: String::new(),
                                });
                            }
                            Err(error) => answer.peers.push(PeerOutcome {
                                key: peer.key.clone(),
                                ok: false,
                                error: error.to_string(),
                            }),
                        }
                    }
                    Ok(JobEnd::Answer(serde_json::to_string(&answer).map_err(
                        |error| format!("the sync answer does not write: {error}"),
                    )?))
                })
            }
            other => Err(format!("`{other}` is no job the Grayjay bridge starts")),
        }
    }

    /// How a job stands, as JSON: its state, what it answered, or why it failed.
    pub fn status(job: &str) -> Result<String, String> {
        let (state, answer, error) = {
            let jobs = jobs();
            let held = jobs
                .held
                .get(job)
                .ok_or_else(|| format!("no job `{job}` runs on this device"))?;
            let read = (held.state.clone(), held.answer.clone(), held.error.clone());
            drop(jobs);
            read
        };
        // The app reads this as text: a job still running has answered nothing yet, which is
        // the empty string rather than a JSON null it would have to keep a second shape for.
        Ok(serde_json::json!({
            "state": state,
            "answer": answer.unwrap_or_default(),
            "error": error,
        })
        .to_string())
    }

    /// Stop a job: false when it is no job of this bridge, or had already ended.
    #[must_use]
    pub fn cancel(job: &str) -> bool {
        let mut jobs = jobs();
        match jobs.held.get_mut(job) {
            Some(held) if held.state == RUNNING => {
                held.cancel.store(true, Ordering::Release);
                true
            }
            _ => false,
        }
    }
}

const RUNNING: &str = "running";
const DONE: &str = "done";
const FAILED: &str = "failed";
const CANCELLED: &str = "cancelled";

/// The most jobs kept: enough for a person to keep pressing Sync now without the table
/// growing for the life of the process.
const MAX_JOBS: usize = 64;

/// What a job came to.
enum JobEnd {
    Answer(String),
    Cancelled,
}

/// One job's state, read by the app by its id.
struct Job {
    state: String,
    answer: Option<String>,
    error: String,
    cancel: Arc<AtomicBool>,
}

impl Job {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
}

/// The jobs this bridge has run, oldest first.
struct Jobs {
    held: BTreeMap<String, Job>,
    order: VecDeque<String>,
}

fn jobs() -> MutexGuard<'static, Jobs> {
    static JOBS: OnceLock<Mutex<Jobs>> = OnceLock::new();
    JOBS.get_or_init(|| {
        Mutex::new(Jobs {
            held: BTreeMap::new(),
            order: VecDeque::new(),
        })
    })
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Makes sure a job that leaves its thread without saying how it ended still ends legibly.
struct Finish {
    id: String,
    said: bool,
}

impl Finish {
    fn say(&mut self, state: &str, answer: Option<String>, error: String) {
        let mut jobs = jobs();
        if let Some(held) = jobs.held.get_mut(&self.id) {
            held.state.clear();
            held.state.push_str(state);
            held.answer = answer;
            held.error = error;
        }
        drop(jobs);
        self.said = true;
    }
}

impl Drop for Finish {
    fn drop(&mut self) {
        if !self.said {
            self.say(
                FAILED,
                None,
                "The Grayjay work ended without an answer.".into(),
            );
        }
    }
}

/// Run `work` on a thread of its own as the job `job`.
fn spawn_job(
    job: &str,
    work: impl FnOnce(&Job) -> Result<JobEnd, String> + Send + 'static,
) -> Result<(), String> {
    let id = job.to_owned();
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut jobs = jobs();
        jobs.held.insert(
            id.clone(),
            Job {
                state: RUNNING.to_owned(),
                answer: None,
                error: String::new(),
                cancel: Arc::clone(&cancel),
            },
        );
        jobs.order.push_back(id.clone());
        while jobs.order.len() > MAX_JOBS {
            let Some(oldest) = jobs.order.front().cloned() else {
                break;
            };
            let finished = jobs
                .held
                .get(&oldest)
                .is_some_and(|held| held.state != RUNNING);
            if finished {
                jobs.order.pop_front();
                jobs.held.remove(&oldest);
            } else {
                break;
            }
        }
        drop(jobs);
    }
    let job = Job {
        state: RUNNING.to_owned(),
        answer: None,
        error: String::new(),
        cancel,
    };
    let finished_id = id.clone();
    let spawned = std::thread::Builder::new()
        .name("grayjay-sync".into())
        .spawn(move || {
            let mut finish = Finish {
                id: finished_id,
                said: false,
            };
            match work(&job) {
                Ok(JobEnd::Answer(answer)) => finish.say(DONE, Some(answer), String::new()),
                Ok(JobEnd::Cancelled) => finish.say(CANCELLED, None, String::new()),
                Err(error) => finish.say(FAILED, None, error),
            }
        });
    if let Err(error) = spawned {
        let mut jobs = jobs();
        if let Some(held) = jobs.held.get_mut(&id) {
            held.state.clear();
            held.state.push_str(FAILED);
            held.error = format!("the sync worker could not start: {error}");
        }
        drop(jobs);
        return Err(format!("the sync worker could not start: {error}"));
    }
    Ok(())
}
