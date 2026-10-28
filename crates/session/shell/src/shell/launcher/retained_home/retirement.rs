//! Process-local observations of genuinely owned helper cleanup, not capabilities.
use runtime_collab::retained_retirement::Subject;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    io,
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};
#[derive(Clone, Serialize)]
struct Entry {
    id: uuid::Uuid,
    source_operation: String,
    context_sha256: String,
    root_pid: Option<u32>,
    state: &'static str,
    child_reaped: bool,
    proof_terminations: Vec<String>,
    pending_proofs: usize,
    #[serde(skip)]
    invalid_proof: bool,
    #[serde(skip)]
    retired_at: Option<Instant>,
}
static ENTRIES: OnceLock<Mutex<BTreeMap<uuid::Uuid, Entry>>> = OnceLock::new();
fn entries() -> &'static Mutex<BTreeMap<uuid::Uuid, Entry>> {
    ENTRIES.get_or_init(Mutex::default)
}
/// Run `f` on entry `id` under the registry's lock.
fn with_entry<R>(id: &uuid::Uuid, f: impl FnOnce(&mut Entry) -> io::Result<R>) -> io::Result<R> {
    let mut entries = entries()
        .lock()
        .map_err(|_| io::Error::other("retirement registry poisoned"))?;
    f(entries.get_mut(id).ok_or(io::ErrorKind::NotFound)?)
}
/// Run `f` on entry `id` if the registry and the entry are there, and say
/// whether it ran; an observation going away records what it can and
/// nothing more.
fn if_entry(id: &uuid::Uuid, f: impl FnOnce(&mut Entry)) -> bool {
    let Ok(mut entries) = entries().lock() else {
        return false;
    };
    entries.get_mut(id).map(f).is_some()
}
pub(super) struct Observation {
    id: uuid::Uuid,
}
impl Observation {
    pub(super) fn reserve(operation: &str, digest: &str) -> io::Result<Self> {
        runtime_collab::retained_retirement::validate_labels(operation, digest)?;
        let mut entries = entries()
            .lock()
            .map_err(|_| io::Error::other("retirement registry poisoned"))?;
        if entries.len() >= 32 {
            let retired = entries
                .iter()
                .filter(|(_, entry)| entry.state == "retired")
                .min_by_key(|(_, entry)| entry.retired_at)
                .map(|(id, _)| *id);
            if let Some(id) = retired {
                entries.remove(&id);
            } else {
                return Err(io::ErrorKind::WouldBlock.into());
            }
        }
        let id = uuid::Uuid::new_v4();
        entries.insert(
            id,
            Entry {
                id,
                source_operation: operation.into(),
                context_sha256: digest.into(),
                root_pid: None,
                state: "preparing",
                child_reaped: false,
                proof_terminations: Vec::new(),
                pending_proofs: 0,
                invalid_proof: false,
                retired_at: None,
            },
        );
        drop(entries);
        Ok(Self { id })
    }
    pub(super) fn bind(
        &self,
        peer: &runtime_portal::process_binding::ExecutingPeer,
    ) -> io::Result<Subject> {
        if !peer.current() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        with_entry(&self.id, |entry| {
            if entry.root_pid.is_some() {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            entry.root_pid = Some(peer.pid());
            entry.state = "running";
            let subject = Subject {
                source_operation: entry.source_operation.clone(),
                context_sha256: entry.context_sha256.clone(),
                root_pid: peer.pid(),
            };
            subject.validate()?;
            Ok(subject)
        })
    }
    pub(super) fn track(self: &Arc<Self>) -> io::Result<ProofTicket> {
        with_entry(&self.id, |entry| {
            if entry.state != "running" || entry.pending_proofs >= 256 {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            entry.pending_proofs = entry.pending_proofs.saturating_add(1);
            Ok(())
        })?;
        Ok(ProofTicket {
            observation: self.clone(),
            recorded: false,
        })
    }
    pub(super) fn unknown(&self) {
        if_entry(&self.id, |entry| entry.state = "unknown");
    }
    pub(super) fn reaped(&self) {
        if_entry(&self.id, |entry| {
            entry.child_reaped = true;
            entry.state = "retiring";
        });
    }
    pub(super) fn retired(&self, outcomes: Vec<String>) -> io::Result<()> {
        with_entry(&self.id, |entry| {
            if !entry.child_reaped || entry.root_pid.is_none() {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            if entry.invalid_proof {
                return Err(io::Error::other("unobserved selected proof termination"));
            }
            if entry.pending_proofs != 0 {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            entry.proof_terminations.extend(outcomes);
            entry.state = "retired";
            entry.retired_at = Some(Instant::now());
            Ok(())
        })
    }
}
pub(super) struct ProofTicket {
    observation: Arc<Observation>,
    recorded: bool,
}
impl ProofTicket {
    pub(super) fn record(mut self, outcome: &io::Result<String>) {
        self.recorded = if_entry(&self.observation.id, |entry| {
            entry.pending_proofs = entry.pending_proofs.saturating_sub(1);
            if let Ok(value) = outcome {
                if entry.proof_terminations.len() >= 256 {
                    entry.proof_terminations.remove(0);
                }
                entry.proof_terminations.push(value.clone());
            } else {
                entry.invalid_proof = true;
                entry.state = "unknown";
            }
        });
    }
}
impl Drop for ProofTicket {
    fn drop(&mut self) {
        if !self.recorded {
            if_entry(&self.observation.id, |entry| {
                entry.invalid_proof = true;
                entry.state = "unknown";
            });
        }
    }
}
impl Drop for Observation {
    fn drop(&mut self) {
        if_entry(&self.id, |entry| {
            if entry.state != "retired" {
                entry.state = "unknown";
            }
        });
    }
}
pub(crate) fn snapshot() -> io::Result<serde_json::Value> {
    let entries = entries()
        .lock()
        .map_err(|_| io::Error::other("retirement registry poisoned"))?;
    Ok(serde_json::json!({"schema":1,"entries":entries.values().collect::<Vec<_>>() }))
}
