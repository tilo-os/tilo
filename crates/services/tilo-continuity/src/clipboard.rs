//! The clipboard across the person's devices, published as [`PROVIDER`]: the copies this device
//! offers its others, theirs joining its history. A copy is content-addressed and made by one
//! device ([`ClipboardEntry`]); a device that leaves takes its copies with it. When the copies the
//! others offer change, the session here is given the history it shows (`clipboard#set`).

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tilo_actions::remote::{ActionCallError, ActionProvider};
use tilo_fabric::actions::{Asker, Provider, asking_device};
use tilo_fabric::{DeviceId, Fabric, FabricError, FabricStamp, spend, valid_mime};
use tilo_lock::MutexRecover as _;

/// The provider the clipboard's resources and actions are published under.
pub const PROVIDER: &str = "so.tilo.clipboard";
pub const MAX_CLIPBOARD_ITEMS: usize = 50;
pub const MAX_CLIPBOARD_ITEM_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardPayload {
    Text(String),
    Binary { mime: String, bytes: Vec<u8> },
}

impl ClipboardPayload {
    #[must_use]
    pub fn byte_len(&self) -> usize {
        match self {
            Self::Text(value) => value.len(),
            Self::Binary { bytes, .. } => bytes.len(),
        }
    }

    fn validate(&self) -> Result<(), FabricError> {
        if self.byte_len() > MAX_CLIPBOARD_ITEM_BYTES {
            return Err(FabricError::ClipboardItemTooLarge);
        }
        if let Self::Binary { mime, .. } = self
            && (!valid_mime(mime) || mime == tilo_clipboard::SECRET_MIME)
        {
            return Err(FabricError::InvalidClipboardMime);
        }
        Ok(())
    }
}

/// Content-addressed clipboard item. Secret fields and primary-selection churn
/// do not construct this type; a caller must explicitly classify a copy as
/// shareable before it reaches the person's other devices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardEntry {
    pub id: [u8; 32],
    pub payload: ClipboardPayload,
    pub origin: DeviceId,
    pub created_unix_ms: u64,
    pub pinned: bool,
    pub stamp: FabricStamp,
}

impl ClipboardEntry {
    /// Construct a content-addressed item the caller has classified as safe to leave the
    /// originating device.
    /// # Errors
    /// An oversized or invalid payload, or a foreign stamp.
    pub fn shareable(
        payload: ClipboardPayload,
        origin: DeviceId,
        created_unix_ms: u64,
        stamp: FabricStamp,
    ) -> Result<Self, FabricError> {
        payload.validate()?;
        if stamp.device != origin {
            return Err(FabricError::ForeignAuthorship);
        }
        let mut material = Vec::with_capacity(payload.byte_len().saturating_add(128));
        material.extend_from_slice(origin.as_str().as_bytes());
        match &payload {
            ClipboardPayload::Text(value) => {
                material.extend_from_slice(b"text\0");
                material.extend_from_slice(value.as_bytes());
            }
            ClipboardPayload::Binary { mime, bytes } => {
                material.extend_from_slice(mime.as_bytes());
                material.push(0);
                material.extend_from_slice(bytes);
            }
        }
        Ok(Self {
            id: *blake3::hash(&material).as_bytes(),
            payload,
            origin,
            created_unix_ms,
            pinned: false,
            stamp,
        })
    }

    /// Whether this is a copy `owner` made, as it made it.
    /// # Errors
    /// A copy another device made, or one whose content is not what its id names.
    pub fn validate(&self, owner: &DeviceId) -> Result<(), FabricError> {
        if &self.origin != owner || &self.stamp.device != owner {
            return Err(FabricError::ForeignAuthorship);
        }
        let rebuilt = Self::shareable(
            self.payload.clone(),
            self.origin.clone(),
            self.created_unix_ms,
            self.stamp.clone(),
        )?;
        if rebuilt.id != self.id {
            return Err(FabricError::ClipboardHashMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClipboardHistory {
    entries: VecDeque<ClipboardEntry>,
}

impl ClipboardHistory {
    pub fn entries(&self) -> impl Iterator<Item = &ClipboardEntry> {
        self.entries.iter()
    }

    /// Add or promote an item, enforcing the bounded history policy.
    /// # Errors
    /// An invalid item, or every history slot pinned.
    pub fn push(&mut self, entry: ClipboardEntry) -> Result<bool, FabricError> {
        entry.payload.validate()?;
        if let Some(index) = self.entries.iter().position(|item| item.id == entry.id) {
            let Some(mut prior) = self.entries.remove(index) else {
                return Ok(false);
            };
            if entry.stamp.is_newer_than(&prior.stamp) {
                prior.stamp = entry.stamp;
                prior.pinned = entry.pinned;
            }
            self.entries.push_front(prior);
            return Ok(false);
        }
        while self.entries.len() >= MAX_CLIPBOARD_ITEMS {
            let Some(index) = self.entries.iter().rposition(|item| !item.pinned) else {
                return Err(FabricError::ClipboardHistoryFull);
            };
            self.entries.remove(index);
        }
        self.entries.push_front(entry);
        Ok(true)
    }

    pub fn remove(&mut self, id: [u8; 32]) -> bool {
        let Some(index) = self.entries.iter().position(|entry| entry.id == id) else {
            return false;
        };
        self.entries.remove(index);
        true
    }
}

/// Every copy the person's devices offer, newest first, as one bounded history.
#[must_use]
pub fn merged(copies: impl IntoIterator<Item = ClipboardEntry>) -> ClipboardHistory {
    let mut items: Vec<_> = copies.into_iter().collect();
    // Oldest first: each copy goes in front of the ones before it, and a full history lets
    // the oldest go.
    items.sort_by(|a, b| (a.created_unix_ms, &a.stamp).cmp(&(b.created_unix_ms, &b.stamp)));
    let mut history = ClipboardHistory::default();
    for item in items {
        if history.push(item).is_err() {
            break;
        }
    }
    history
}

/// The copies `device` offers, checked: each its own and as it made it, and no more than a
/// history holds.
///
/// # Errors
/// The first copy that is not one.
pub fn checked(device: &DeviceId, entries: &[ClipboardEntry]) -> Result<(), FabricError> {
    if entries.len() > MAX_CLIPBOARD_ITEMS {
        return Err(FabricError::TooLarge);
    }
    entries.iter().try_for_each(|entry| entry.validate(device))
}

/// `clipboard#offer` and `clipboard#offered`: a device's shareable copies, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Copies {
    pub entries: Vec<ClipboardEntry>,
}

#[derive(Default)]
struct State {
    own: Vec<ClipboardEntry>,
    theirs: BTreeMap<DeviceId, Vec<ClipboardEntry>>,
}

/// The clipboard across the person's devices, on this one. Cloning shares it.
#[derive(Clone)]
pub struct Clipboard {
    fabric: Fabric,
    state: Arc<Mutex<State>>,
}

impl Clipboard {
    /// The clipboard of the device `fabric` is: it offers its copies to each of the person's
    /// devices as it arrives.
    #[must_use]
    pub fn new(fabric: &Fabric) -> Self {
        let clipboard = Self {
            fabric: fabric.clone(),
            state: Arc::default(),
        };
        let offering = clipboard.clone();
        fabric.on_arrival(move |device| {
            let own = offering.state.lock_recover().own.clone();
            if !own.is_empty() {
                offering.tell(device, own);
            }
        });
        clipboard
    }

    /// Every copy the person's devices here offer, this one's among them.
    #[must_use]
    pub fn history(&self) -> ClipboardHistory {
        let online = self.fabric.online();
        let state = self.state.lock_recover();
        merged(
            state.own.iter().cloned().chain(
                state
                    .theirs
                    .iter()
                    .filter(|(device, _)| online.contains(device))
                    .flat_map(|(_, entries)| entries.iter().cloned()),
            ),
        )
    }

    fn tell(&self, device: &DeviceId, entries: Vec<ClipboardEntry>) {
        spend(
            self.fabric
                .call(device, PROVIDER, "clipboard", "offered", Copies { entries }),
            "this device's clipboard",
        );
    }

    /// The session here offers its shareable copies: the person's devices here are told when
    /// they changed.
    fn offer(&self, entries: &[ClipboardEntry]) -> Result<(), ActionCallError> {
        checked(self.fabric.local(), entries).map_err(|error| invalid(&error))?;
        {
            let mut state = self.state.lock_recover();
            if state.own == entries {
                return Ok(());
            }
            state.own = entries.to_vec();
        }
        for device in self.fabric.online() {
            self.tell(&device, entries.to_vec());
        }
        Ok(())
    }

    /// Another device offers its copies: the session here shows them with its own.
    fn offered(
        &self,
        from: &DeviceId,
        entries: Vec<ClipboardEntry>,
    ) -> Result<(), ActionCallError> {
        checked(from, &entries).map_err(|error| invalid(&error))?;
        let changed = {
            let mut state = self.state.lock_recover();
            let kept = state.theirs.entry(from.clone()).or_default();
            let changed = *kept != entries;
            *kept = entries;
            drop(state);
            changed
        };
        if changed {
            let history = Copies {
                entries: self.history().entries().cloned().collect(),
            };
            spend(
                self.fabric.seat().call(
                    "clipboard",
                    "set",
                    serde_json::to_value(history).unwrap_or_default(),
                ),
                "the clipboard history here",
            );
        }
        Ok(())
    }

    /// The clipboard's resources and actions, as the device's action host mounts them under
    /// [`PROVIDER`].
    #[must_use]
    pub fn provider(&self) -> Arc<dyn ActionProvider + Send + Sync> {
        let at = || self.clone();
        Provider::new(PROVIDER, self.fabric.local().clone(), *self.fabric.person())
            .read("history", "The clipboard across the person's devices", {
                let clipboard = at();
                move || Copies {
                    entries: clipboard.history().entries().cloned().collect(),
                }
            })
            .act(
                "clipboard",
                "offer",
                "Offer this device's copies to the person's other devices",
                Asker::Here,
                {
                    let clipboard = at();
                    move |_, copies: Copies| clipboard.offer(&copies.entries)
                },
            )
            .act(
                "clipboard",
                "offered",
                "The copies another device offers",
                Asker::Device,
                {
                    let clipboard = at();
                    move |who, copies: Copies| {
                        clipboard.offered(asking_device(who)?, copies.entries)
                    }
                },
            )
            .shared()
    }
}

fn invalid(error: &FabricError) -> ActionCallError {
    ActionCallError::InvalidRequest {
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(name: &str) -> DeviceId {
        DeviceId::new(hex::encode(blake3::hash(name.as_bytes()).as_bytes())).unwrap()
    }

    fn stamp(device: &str, counter: u64) -> FabricStamp {
        FabricStamp {
            counter,
            device: id(device),
        }
    }

    #[test]
    fn clipboard_is_deduplicated_and_bounded() {
        let mut history = ClipboardHistory::default();
        for n in 0..=MAX_CLIPBOARD_ITEMS {
            let n = n as u64;
            history
                .push(
                    ClipboardEntry::shareable(
                        ClipboardPayload::Text(format!("item-{n}")),
                        id("laptop"),
                        n,
                        stamp("laptop", n),
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        assert_eq!(history.entries().count(), MAX_CLIPBOARD_ITEMS);
        let oversized = ClipboardEntry::shareable(
            ClipboardPayload::Text("x".repeat(MAX_CLIPBOARD_ITEM_BYTES + 1)),
            id("laptop"),
            1,
            stamp("laptop", 1),
        );
        assert_eq!(oversized, Err(FabricError::ClipboardItemTooLarge));
    }

    #[test]
    fn clipboard_hash_tampering_is_rejected() {
        let mut entry = ClipboardEntry::shareable(
            ClipboardPayload::Text("safe".into()),
            id("phone"),
            1,
            stamp("phone", 1),
        )
        .unwrap();
        entry.payload = ClipboardPayload::Text("changed".into());
        assert_eq!(
            entry.validate(&id("phone")),
            Err(FabricError::ClipboardHashMismatch)
        );
        assert_eq!(
            entry.validate(&id("laptop")),
            Err(FabricError::ForeignAuthorship),
            "another device's copy is not this one's to validate"
        );
    }
}
