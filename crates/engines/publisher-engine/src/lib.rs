//! The App Publisher's device work, as a package reaches it: this machine's Ed25519 publisher key,
//! and the applications that key makes to the registry.
//!
//! An app runs sandboxed and may neither read the secret store nor sign, so both are here. The app
//! asks `so.tilo.publisher`'s engine; this crate answers only that app's asks, and every answer is
//! JSON the package reads into its own records.
//!
//! The key is the one `tilolang keys` and `tilolang apply` use: the same app id's entry in this
//! device's secret store ([`key::SECRETS_APP`]), with the same key file format for a copy or an
//! export. It is this device's, not the workspace's: the key file is how it travels to another
//! machine or to CI, which is not a member of any workspace.
//!
//! Requests answered now (`engine.call`):
//!
//! | op | asks | answers |
//! |---|---|---|
//! | `key-state` | none | `{state, public}`: `missing`, `present` or `unreadable` |
//! | `create` | none | `{state, public}`, refusing when the machine already has a key |
//! | `key-file` | none | `{text}`: the key file, for the person to copy or export |
//! | `sign` | `{message_hex}` | `{signature_hex}`: the message, signed by this machine's key |
//!
//! Jobs (`engine.start`, asked about by `engine.status`, stopped by `engine.cancel`):
//!
//! | op | asks | answers |
//! |---|---|---|
//! | `apply` | `{registry, domain, forge_account, contact}` | `{job}` |
//! | `status` | `{registry}` | `{job}` |
//!
//! A job's status answers `{state, summary, error}`: `running`, `done`, `failed` or `cancelled`,
//! the registry's own summary of the application in `done`, and why it failed otherwise. An
//! application with no complaint on the registry answers an empty summary.
//!
//! Nothing here publishes a package: `apply` asks the registry for the namespaces an app's ids
//! live in, and `status` reads the registry's answer about this machine's key.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use app_registry::publisher::{RegistryClient, key};
use ed25519_dalek::Signer as _;
use serde::Deserialize;
use serde_json::json;
use tilo_secrets::SecretStore;
use tilolang_tilo::engine::Provider;

/// The app whose engine this is: the one that asks these ops, and no other.
pub const APP: &str = key::SECRETS_APP;

/// Register this crate as the App Publisher's engine: what the app host calls once at its start,
/// before any app asks.
pub fn register() {
    tilolang_tilo::engine::register(APP, Arc::new(Publisher::machine()));
}

/// The key and the registry applications of one device.
#[derive(Clone)]
pub struct Publisher {
    store: SecretStore,
}

impl Publisher {
    /// The publisher key this machine's secret store holds.
    #[must_use]
    pub fn machine() -> Self {
        Self {
            store: SecretStore::for_app(APP),
        }
    }

    /// A publisher over `store`: the device's, or a test's own.
    #[must_use]
    pub fn with_store(store: SecretStore) -> Self {
        Self { store }
    }
}

impl Provider for Publisher {
    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        match op {
            "key-state" => Ok(key_state(&self.store)),
            "create" => create(&self.store),
            "key-file" => key_file(&self.store),
            "sign" => sign(&self.store, input),
            other => Err(format!("`{other}` is no op this device runs for {APP}")),
        }
    }

    fn start(&self, op: &str, input: &str) -> Result<String, String> {
        jobs::start(&self.store, op, input)
    }

    fn status(&self, job: &str) -> Result<String, String> {
        jobs::status(job)
    }

    fn cancel(&self, job: &str) -> bool {
        jobs::cancel(job)
    }
}

// ---------------------------------------------------------------- the key on this machine

/// Whether this machine has a key, and its public half: never the secret.
fn key_state(store: &SecretStore) -> String {
    let (state, public) = match key::discover(store) {
        Ok(Some(key)) => ("present", key::public_hex(&key)),
        Ok(None) => ("missing", String::new()),
        Err(_) => ("unreadable", String::new()),
    };
    json!({ "state": state, "public": public }).to_string()
}

/// Makes this machine's key, unless it already has one.
fn create(store: &SecretStore) -> Result<String, String> {
    let key = key::create(store).map_err(|error| format!("{error:#}"))?;
    Ok(json!({ "state": "present", "public": key::public_hex(&key) }).to_string())
}

/// The key file, as `tilolang keys export` writes it: what a person copies for CI or keeps.
fn key_file(store: &SecretStore) -> Result<String, String> {
    let key = signed_with(store)?;
    Ok(json!({ "text": key::encode(&key) }).to_string())
}

/// Signs `message_hex` with this machine's key.
fn sign(store: &SecretStore, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        message_hex: String,
    }
    let ask: Ask = serde_json::from_str(input)
        .map_err(|error| format!("the message to sign does not read: {error}"))?;
    let message = hex::decode(ask.message_hex.trim())
        .map_err(|error| format!("the message is not hexadecimal: {error}"))?;
    let key = signed_with(store)?;
    let signature = key.sign(&message);
    Ok(json!({ "signature_hex": hex::encode(signature.to_bytes()) }).to_string())
}

/// This machine's key, or why there is none to use.
fn signed_with(store: &SecretStore) -> Result<key::SigningKey, String> {
    key::resolve(None, store).map_err(|error| format!("{error:#}"))
}

// ---------------------------------------------------------------- the registry, as jobs

mod jobs {
    //! The registry's calls as jobs: what a person waits for, asks about and can stop.

    use super::{
        Arc, AtomicBool, BTreeMap, Condvar, Deserialize, Mutex, OnceLock, Ordering, RegistryClient,
        SecretStore, json, key,
    };

    /// Every job this device runs, and the id the next one takes.
    #[derive(Default)]
    struct Jobs {
        next: u64,
        running: BTreeMap<String, Arc<Job>>,
    }

    /// One registry call under way.
    struct Job {
        stopped: AtomicBool,
        outcome: Mutex<Option<Result<String, String>>>,
        /// Moves when the outcome lands: what a test waits on instead of asking again.
        landed: Condvar,
    }

    fn jobs() -> &'static Mutex<Jobs> {
        static JOBS: OnceLock<Mutex<Jobs>> = OnceLock::new();
        JOBS.get_or_init(|| Mutex::new(Jobs::default()))
    }

    /// Starts one registry call, answering the job's id.
    ///
    /// # Errors
    ///
    /// Not this device's app, a request that does not read, a registry address the client refuses,
    /// or namespaces that could not be applied for.
    pub(super) fn start(store: &SecretStore, op: &str, input: &str) -> Result<String, String> {
        match op {
            "apply" => {
                #[derive(Deserialize)]
                struct Ask {
                    registry: String,
                    #[serde(default)]
                    domain: String,
                    #[serde(default)]
                    forge_account: String,
                    #[serde(default)]
                    contact: String,
                }
                let ask: Ask = serde_json::from_str(input)
                    .map_err(|error| format!("the application does not read: {error}"))?;
                // What the registry would be told is settled here, so a profile that names
                // nothing to claim answers before any work starts.
                let claims = app_registry::publisher::claims(
                    [ask.domain.as_str()],
                    [ask.forge_account.as_str()],
                )
                .map_err(|error| format!("{error:#}"))?;
                RegistryClient::new(&ask.registry).map_err(|error| format!("{error:#}"))?;
                let store = store.clone();
                begin("apply", move || {
                    let key = key::resolve(None, &store).map_err(|error| format!("{error:#}"))?;
                    let client =
                        RegistryClient::new(&ask.registry).map_err(|error| format!("{error:#}"))?;
                    let status = app_registry::publisher::apply(
                        &client,
                        &key,
                        claims,
                        ask.contact.trim().to_owned(),
                        String::new(),
                    )
                    .map_err(|error| format!("{error:#}"))?;
                    Ok(app_registry::publisher::status_summary(&status))
                })
            }
            "status" => {
                #[derive(Deserialize)]
                struct Ask {
                    registry: String,
                }
                let ask: Ask = serde_json::from_str(input)
                    .map_err(|error| format!("the registry does not read: {error}"))?;
                RegistryClient::new(&ask.registry).map_err(|error| format!("{error:#}"))?;
                let store = store.clone();
                begin("status", move || {
                    let key = key::resolve(None, &store).map_err(|error| format!("{error:#}"))?;
                    let client =
                        RegistryClient::new(&ask.registry).map_err(|error| format!("{error:#}"))?;
                    Ok(app_registry::publisher::application(&client, &key)
                        .map_err(|error| format!("{error:#}"))?
                        .map_or_else(String::new, |(_, summary)| summary))
                })
            }
            other => Err(format!(
                "`{other}` is no op this device runs for {}",
                super::APP
            )),
        }
    }

    /// Runs `work` on a thread of its own, answering the job's id.
    fn begin(
        name: &str,
        work: impl FnOnce() -> Result<String, String> + Send + 'static,
    ) -> Result<String, String> {
        let mut known = jobs()
            .lock()
            .map_err(|_| "the job table is unreadable".to_owned())?;
        known.next = known.next.saturating_add(1);
        let id = format!("{name}-{}", known.next);
        let job = Arc::new(Job {
            stopped: AtomicBool::new(false),
            outcome: Mutex::new(None),
            landed: Condvar::new(),
        });
        known.running.insert(id.clone(), Arc::clone(&job));
        drop(known);
        let worker = Arc::clone(&job);
        std::thread::Builder::new()
            .name(format!("publisher-{name}"))
            .spawn(move || {
                let outcome = work();
                if let Ok(mut slot) = worker.outcome.lock() {
                    *slot = Some(outcome);
                    worker.landed.notify_all();
                }
            })
            .map_err(|error| format!("the registry call did not start: {error}"))?;
        Ok(json!({ "job": id }).to_string())
    }

    /// How far a job has got, as JSON.
    ///
    /// # Errors
    ///
    /// No such job on this device.
    pub(super) fn status(job: &str) -> Result<String, String> {
        let state = {
            let known = jobs()
                .lock()
                .map_err(|_| "the job table is unreadable".to_owned())?;
            known
                .running
                .get(job)
                .cloned()
                .ok_or_else(|| format!("no job `{job}` runs on this device"))?
        };
        let outcome = state
            .outcome
            .lock()
            .map_err(|_| "the job's outcome is unreadable".to_owned())?
            .clone();
        let (state_word, summary, error) = match outcome {
            Some(Ok(summary)) => ("done", summary, String::new()),
            Some(Err(error)) => ("failed", String::new(), error),
            None if state.stopped.load(Ordering::Relaxed) => {
                ("cancelled", String::new(), String::new())
            }
            None => ("running", String::new(), String::new()),
        };
        Ok(json!({ "state": state_word, "summary": summary, "error": error }).to_string())
    }

    /// Stops a job: the registry's call cannot be called back, so a stopped one says `cancelled`
    /// once it would have ended. False for a job this device does not know or that had finished.
    pub(super) fn cancel(job: &str) -> bool {
        let Ok(known) = jobs().lock() else {
            return false;
        };
        let Some(state) = known.running.get(job) else {
            return false;
        };
        if state
            .outcome
            .lock()
            .map_or(true, |outcome| outcome.is_some())
        {
            return false;
        }
        state.stopped.store(true, Ordering::Relaxed);
        true
    }

    /// Waits for `job` to end and answers what it came to: what a test holds the registry to
    /// instead of asking again in a loop.
    #[cfg(test)]
    pub(super) fn wait(job: &str) -> Result<String, String> {
        let state = {
            let known = jobs()
                .lock()
                .map_err(|_| "the job table is unreadable".to_owned())?;
            known
                .running
                .get(job)
                .cloned()
                .ok_or_else(|| format!("no job `{job}` runs on this device"))?
        };
        let mut outcome = state
            .outcome
            .lock()
            .map_err(|_| "the job's outcome is unreadable".to_owned())?;
        while outcome.is_none() {
            outcome = state
                .landed
                .wait(outcome)
                .map_err(|_| "the job's outcome is unreadable".to_owned())?;
        }
        outcome
            .clone()
            .unwrap_or_else(|| Err("the job did not end".to_owned()))
    }
}

#[cfg(test)]
mod tests;
