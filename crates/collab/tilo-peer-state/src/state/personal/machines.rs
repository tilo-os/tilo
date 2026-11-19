//! The disk recovery keys of the person's machines, in their own store.
//!
//! A machine that installs without a person at its screen has no screen to
//! show its disk's recovery key on, and the installed system keeps no copy:
//! the key reaches the person's devices with the enrollment that admits the
//! machine, and each of them keeps it here, beside their own list of their
//! devices. It is the person's own store, handed to none but devices their
//! root certified, so a recovery key in it is readable by exactly their
//! devices.
//!
//! One entry per machine, under the machine's device key in hex: the name the
//! person gave it, the key, and when it was taken. A machine that installs
//! again replaces its own entry.

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ReadDoc as _};
use collab_types::CommitNamed as _;
use serde::{Deserialize, Serialize};

/// The kind of the person's resource that holds their machines' recovery
/// keys: one entry per machine, under its device key in hex.
pub const KIND: &str = "identity.machines";

/// The most machines that resource names to this device.
const MAX_MACHINES: usize = 256;
/// The longest recovery key one entry carries: systemd's is far shorter.
pub const MAX_KEY_BYTES: usize = 256;
/// The longest name one entry carries: the same bound a server's label has.
pub const MAX_LABEL_BYTES: usize = 128;

/// One machine's disk recovery key, as the person's devices keep it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineRecovery {
    /// The machine's device key, hex: what its entry in the Servers workspace
    /// is known by.
    pub machine: String,
    /// The machine's name, as the person planned it.
    pub label: String,
    /// The disk's recovery key: the factor that opens it with nothing else.
    pub key: String,
    /// When the person's device took it, nanoseconds since the epoch.
    pub at_ns: u64,
}

impl MachineRecovery {
    /// Whether `entry` is one this store will keep: a device key in hex, a
    /// bounded name and key, and a time.
    ///
    /// # Errors
    ///
    /// It is not: why, for the caller to report.
    pub fn check(&self) -> Result<(), String> {
        if self.machine.len() != 64
            || !self
                .machine
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("a machine's recovery key names no machine key".into());
        }
        if self.label.trim().is_empty() || self.label.len() > MAX_LABEL_BYTES {
            return Err("a machine's recovery key needs a name of 1 to 128 bytes".into());
        }
        if self.key.trim().is_empty() || self.key.len() > MAX_KEY_BYTES {
            return Err("a recovery key is 1 to 256 bytes".into());
        }
        if self.key.chars().any(char::is_control) {
            return Err("a recovery key is text, not control bytes".into());
        }
        Ok(())
    }
}

/// The key of `entry`'s machine in the document.
#[must_use]
pub fn entry_key(machine: &str) -> String {
    format!("machine:{machine}")
}

/// Write `entry` into the store's document, replacing what the machine had.
///
/// # Errors
///
/// `entry` is not one the store keeps, or the document refused it.
pub fn put(doc: &mut AutoCommit, entry: &MachineRecovery) -> Result<(), String> {
    entry.check()?;
    let value = serde_json::to_string(entry).map_err(|error| error.to_string())?;
    let key = entry_key(&entry.machine);
    doc.put(automerge::ROOT, &key, value)
        .map_err(|error| error.to_string())?;
    // Every change a signed op carries names what it did: this one wrote the
    // machine's entry.
    doc.commit_edit("machine", &key);
    Ok(())
}

/// Every machine's recovery key the document holds, oldest first.
///
/// A value this build cannot read is no machine's key and is left out.
#[must_use]
pub fn read(bytes: &[u8]) -> Vec<MachineRecovery> {
    let Ok(doc) = AutoCommit::load(bytes) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for key in doc.keys(automerge::ROOT) {
        if !key.starts_with("machine:") {
            continue;
        }
        if entries.len() >= MAX_MACHINES {
            break;
        }
        let Some((value, _)) = doc.get(automerge::ROOT, &key).ok().flatten() else {
            continue;
        };
        let Ok(text) = value.into_string() else {
            continue;
        };
        let Some(entry) = tilo_failure::ignored!(
            "an entry this build cannot read is no machine's key",
            serde_json::from_str::<MachineRecovery>(&text).ok()
        ) else {
            continue;
        };
        if entry.check().is_ok() && entry_key(&entry.machine) == key {
            entries.push(entry);
        }
    }
    entries.sort_by(|left, right| {
        left.at_ns
            .cmp(&right.at_ns)
            .then_with(|| left.machine.cmp(&right.machine))
    });
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(machine: &str, label: &str, key: &str) -> MachineRecovery {
        MachineRecovery {
            machine: machine.to_owned(),
            label: label.to_owned(),
            key: key.to_owned(),
            at_ns: 7,
        }
    }

    #[test]
    fn a_machine_replaces_its_own_key_and_others_stay() {
        let mut doc = AutoCommit::new();
        put(&mut doc, &entry(&"a".repeat(64), "ryzen", "1111-2222")).unwrap();
        put(&mut doc, &entry(&"b".repeat(64), "cupboard", "3333-4444")).unwrap();
        put(&mut doc, &entry(&"a".repeat(64), "ryzen", "5555-6666")).unwrap();
        let held = read(&doc.save());
        assert_eq!(held.len(), 2);
        let ryzen = held
            .iter()
            .find(|entry| entry.machine == "a".repeat(64))
            .unwrap();
        assert_eq!(ryzen.key, "5555-6666");
        assert_eq!(ryzen.label, "ryzen");
    }

    #[test]
    fn a_value_this_build_cannot_read_is_no_key() {
        let mut doc = AutoCommit::new();
        put(&mut doc, &entry(&"a".repeat(64), "ryzen", "1111-2222")).unwrap();
        doc.put(automerge::ROOT, entry_key(&"b".repeat(64)), "not json")
            .unwrap();
        doc.put(automerge::ROOT, "unrelated", "{}").unwrap();
        let held = read(&doc.save());
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].label, "ryzen");
    }

    #[test]
    fn an_entry_names_a_machine_key_and_bounded_text() {
        let mut doc = AutoCommit::new();
        assert!(put(&mut doc, &entry("A".repeat(64).as_str(), "ryzen", "k")).is_err());
        assert!(put(&mut doc, &entry(&"a".repeat(64), "", "k")).is_err());
        assert!(put(&mut doc, &entry(&"a".repeat(64), "ryzen", "")).is_err());
        assert!(put(&mut doc, &entry(&"a".repeat(64), "ryzen", "\u{7}")).is_err());
        assert!(read(&doc.save()).is_empty());
    }
}
