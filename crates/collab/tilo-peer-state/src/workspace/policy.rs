//! One logical current-policy projection; destination storage owns its budget.
use super::read::{self, GovernanceEntry, ReadError, WorkspaceRead};
use crate::cached_document::PolicyData;

pub(crate) trait Destination {
    type Error;
    fn owner(&mut self, value: &str) -> Result<(), Self::Error>;
    fn identity(&mut self, key: &str, bytes: &[u8]) -> Result<(), Self::Error>;
    /// The display name beside a device row; only a destination that shows
    /// people keeps it.
    fn peer(&mut self, _key: &str, _name: &str) -> Result<(), Self::Error> {
        Ok(())
    }
}
pub(crate) fn project<R: WorkspaceRead, D: Destination>(
    read: &R,
    destination: &mut D,
) -> Result<(), ReadError<D::Error>> {
    read::visit_governance_entries(read, |entry| match entry {
        GovernanceEntry::Owner(value) => destination.owner(value),
        GovernanceEntry::Identity { key, bytes } => destination.identity(key, bytes),
        GovernanceEntry::Peer { key, name } => destination.peer(key, name),
        GovernanceEntry::Sandbox { .. } => Ok(()),
    })
}
impl Destination for PolicyData {
    type Error = std::convert::Infallible;
    fn owner(&mut self, value: &str) -> Result<(), Self::Error> {
        self.owner = Some(value.to_owned());
        Ok(())
    }
    fn identity(&mut self, key: &str, bytes: &[u8]) -> Result<(), Self::Error> {
        self.identities.insert(key.to_owned(), bytes.to_vec());
        Ok(())
    }
    fn peer(&mut self, key: &str, name: &str) -> Result<(), Self::Error> {
        self.names.insert(key.to_owned(), name.to_owned());
        Ok(())
    }
}
/// `document`'s governance as [`legacy`] reads it, derived where it can be
/// from a state read before.
///
/// Admitting one person writes a row or two of a workspace of hundreds, and
/// every reading of the next state read every member's row again. The states
/// read last are remembered by their heads; a state whose history reaches one
/// of them within [`DERIVE_WALK`] changes is that state with the entries those
/// changes wrote read again, and nothing else
/// ([`crate::workspace::rows::touched`]). A state that cannot be placed so (a
/// change still open, a long or unplaced history, a governance map written
/// whole) is read whole, and counted as such ([`crate::governance_work`]).
pub(crate) fn read(document: &automerge::AutoCommit) -> std::sync::Arc<PolicyData> {
    use automerge::transaction::Transactable as _;
    if document.pending_ops() > 0 {
        return std::sync::Arc::new(legacy(document));
    }
    let heads = document.base_heads();
    let workspace = governed_workspace(document);
    if let Some(known) = remembered::recall(&workspace, &heads) {
        return known;
    }
    let read = std::sync::Arc::new(
        derive(document, &workspace, &heads).unwrap_or_else(|| legacy(document)),
    );
    // Every derived reading a test makes is checked against the whole one.
    #[cfg(test)]
    assert_eq!(
        *read,
        legacy_uncounted(document),
        "a governance reading derived from an earlier state is the whole reading"
    );
    remembered::keep(&workspace, heads, std::sync::Arc::clone(&read));
    read
}

/// The workspace `document` governs, as its `room` row names it: whose
/// states a reading is remembered among.
fn governed_workspace(document: &automerge::AutoCommit) -> String {
    use automerge::ReadDoc as _;
    document
        .get(automerge::ROOT, "room")
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
        .unwrap_or_default()
}

/// The most changes a derived reading walks back to a state read before.
const DERIVE_WALK: usize = 64;

/// `document`, at `heads`, read as a remembered state with what the changes
/// since it wrote read again; `None` when no remembered state is within
/// [`DERIVE_WALK`] changes or the changes cannot be placed.
fn derive(
    document: &automerge::AutoCommit,
    workspace: &str,
    heads: &[automerge::ChangeHash],
) -> Option<PolicyData> {
    use automerge::ReadDoc as _;
    for (base_heads, base) in remembered::all(workspace) {
        if !base_heads
            .iter()
            .all(|head| document.get_change_by_hash(head).is_some())
        {
            continue;
        }
        // Every change of `document` that is not in the remembered state's
        // history is reached from its heads before a head of that state is;
        // a change reached that the state does hold only reads its entries
        // again.
        let stop: std::collections::BTreeSet<_> = base_heads.iter().copied().collect();
        let mut seen = std::collections::BTreeSet::new();
        let mut pending: Vec<automerge::ChangeHash> = heads.to_vec();
        let mut changes = Vec::new();
        let mut placed = true;
        while let Some(hash) = pending.pop() {
            if stop.contains(&hash) || !seen.insert(hash) {
                continue;
            }
            if changes.len() >= DERIVE_WALK {
                placed = false;
                break;
            }
            let Some(change) = document.get_change_by_hash(&hash) else {
                placed = false;
                break;
            };
            pending.extend(change.deps().iter().copied());
            changes.push(change);
        }
        if !placed {
            continue;
        }
        let touched = super::rows::touched(document, &changes)?;
        return patched((*base).clone(), document, &touched);
    }
    None
}

/// `held` with the entries `touched` names read again from `document`;
/// `None` when a map is written whole.
fn patched(
    mut held: PolicyData,
    document: &automerge::AutoCommit,
    touched: &std::collections::BTreeMap<String, super::rows::Wrote>,
) -> Option<PolicyData> {
    use super::rows::Wrote;
    use automerge::{ROOT, ReadDoc as _, Value};
    for (key, wrote) in touched {
        let kind = match key.as_str() {
            "owner" => {
                held.owner = document
                    .get(ROOT, "owner")
                    .ok()
                    .flatten()
                    .and_then(|(value, _)| value.into_string().ok());
                continue;
            }
            "peers" | "peer_identities_v1" => key.as_str(),
            _ => continue,
        };
        let Wrote::Entries(entries) = wrote else {
            return None;
        };
        let map = map_of(document, key);
        for entry in entries {
            let at = |doc: &automerge::AutoCommit| -> Option<Value<'static>> {
                let map = map.as_ref()?;
                doc.get(map, entry.as_str())
                    .ok()
                    .flatten()
                    .map(|(value, _)| value.to_owned())
            };
            let value = at(document);
            if kind == "peers" {
                held.names.remove(entry);
                if let Some(name) = value.and_then(|value| value.into_string().ok()) {
                    let _ = tilo_failure::ignored!(
                        "the governance reading's destination cannot fail",
                        Destination::peer(&mut held, entry, &name)
                    );
                }
            } else {
                held.identities.remove(entry);
                if let Some(bytes) = value.and_then(scalar_bytes) {
                    let _ = tilo_failure::ignored!(
                        "the governance reading's destination cannot fail",
                        Destination::identity(&mut held, entry, &bytes)
                    );
                }
            }
        }
    }
    Some(held)
}

/// The map `document` holds under `key`.
fn map_of(document: &automerge::AutoCommit, key: &str) -> Option<automerge::ObjId> {
    use automerge::{ObjType, ReadDoc as _, Value};
    match document.get(automerge::ROOT, key).ok().flatten()? {
        (Value::Object(ObjType::Map | ObjType::Table), map) => Some(map),
        _ => None,
    }
}

/// `value`'s bytes, when it is bytes.
fn scalar_bytes(value: automerge::Value<'static>) -> Option<Vec<u8>> {
    match value {
        automerge::Value::Scalar(scalar) => match scalar.into_owned() {
            automerge::ScalarValue::Bytes(bytes) => Some(bytes),
            _ => None,
        },
        automerge::Value::Object(_) => None,
    }
}

/// The governance readings of each workspace's states read last, by their
/// heads ([`crate::kept_states`]).
mod remembered {
    use std::sync::{Arc, Mutex, OnceLock};

    use crate::cached_document::PolicyData;
    use crate::kept_states::KeptStates;

    /// How many states of one workspace are kept: the one a write starts
    /// from, the write's own, and the ones a few readers between them hold.
    const STATES: usize = 4;
    /// The workspaces whose states are kept at once: a host's busy ones.
    const WORKSPACES: usize = 256;

    type Kept = Mutex<KeptStates<Arc<PolicyData>>>;

    fn kept() -> &'static Kept {
        static KEPT: OnceLock<Kept> = OnceLock::new();
        KEPT.get_or_init(|| Mutex::new(KeptStates::new(STATES, WORKSPACES)))
    }

    pub(super) fn recall(
        workspace: &str,
        heads: &[automerge::ChangeHash],
    ) -> Option<Arc<PolicyData>> {
        kept().lock().ok()?.recall(workspace, heads)
    }

    /// Every kept state of `workspace`, the newest first.
    pub(super) fn all(workspace: &str) -> Vec<(Vec<automerge::ChangeHash>, Arc<PolicyData>)> {
        kept()
            .lock()
            .map(|kept| kept.all(workspace))
            .unwrap_or_default()
    }

    pub(super) fn keep(workspace: &str, heads: Vec<automerge::ChangeHash>, read: Arc<PolicyData>) {
        if let Ok(mut kept) = kept().lock() {
            kept.keep(workspace, heads, read);
        }
    }
}

/// [`legacy`], uncounted: a test's check of a derived reading.
#[cfg(test)]
fn legacy_uncounted(document: &automerge::AutoCommit) -> PolicyData {
    crate::governance_work::uncounted(|| {
        let mut value = empty();
        let _ = project(&read::LegacyRead(document), &mut value);
        value
    })
}

#[cfg(test)]
fn empty() -> PolicyData {
    PolicyData {
        owner: None,
        identities: std::collections::BTreeMap::new(),
        names: std::collections::BTreeMap::new(),
    }
}

pub(crate) fn legacy(document: &automerge::AutoCommit) -> PolicyData {
    #[cfg(test)]
    super::POLICY_READS.with(|count| count.set(count.get().saturating_add(1)));
    let mut value = PolicyData {
        owner: None,
        identities: std::collections::BTreeMap::new(),
        names: std::collections::BTreeMap::new(),
    };
    // This concrete legacy adapter has neither resource failures nor fallible
    // destination operations. Malformed native lookups retain the historical
    // ignored-value behavior inside visit_governance_entries.
    let _ = tilo_failure::ignored!(
        "the legacy adapter's destination operations are infallible",
        project(&read::LegacyRead(document), &mut value)
    );
    value
}
