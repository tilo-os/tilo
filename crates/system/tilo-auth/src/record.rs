//! A person's record in device storage: `<dir>/<uid>.json`, root 0600 in a
//! 0700 directory.

use crate::wrapper::{Entry, Kind, Wrapper};
use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tilo_files::native::migration::Format;

pub const DIR: &str = "/var/lib/tilo/auth/users";

pub const RECORD_FORMAT: Format = Format {
    name: "sign-in record",
    version: 1,
    oldest: 1,
    steps: &[],
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub version: u32,
    pub uid: u32,
    pub name: String,
    pub display_name: String,
    pub unlocks_system: bool,
    pub wrappers: Vec<Entry>,
    pub system_keyslot: Option<u32>,
    /// Something Settings should tell the person, when enrollment could not
    /// finish by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<Attention>,
    /// What the secret looks like, so a screen labels its field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<SecretKind>,
}

/// Whether a secret is a PIN (digits only) or a password. A screen asks for
/// one or the other by name and offers a number pad for a PIN.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecretKind {
    Pin,
    Password,
}

impl SecretKind {
    pub fn of(secret: &crate::secret::Secret) -> Self {
        let text = secret.as_str();
        if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
            Self::Pin
        } else {
            Self::Password
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Attention {
    /// No TPM, and the secret is too weak to stand alone: nothing was
    /// enrolled, the old password check still applies.
    WeakSecret,
    /// The person unlocks the machine, but no way to add their keyslot was
    /// available yet; the next sign-in tries again.
    SystemKeyslotPending,
}

/// What a sign-in screen can offer for a person.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Factor {
    Password,
    Recovery,
}

impl Record {
    pub fn new(uid: u32, name: &str, display_name: &str) -> Self {
        Self {
            version: RECORD_FORMAT.version,
            uid,
            name: name.to_owned(),
            display_name: display_name.to_owned(),
            unlocks_system: false,
            wrappers: Vec::new(),
            system_keyslot: None,
            attention: None,
            secret: None,
        }
    }

    pub fn known(&self) -> impl Iterator<Item = &Wrapper> {
        self.wrappers.iter().filter_map(Entry::known)
    }

    pub fn has(&self, kind: Kind) -> bool {
        self.known().any(|w| w.kind() == kind)
    }

    /// Whether a typed secret is checked against this record rather than the
    /// old password database.
    pub fn enrolled(&self) -> bool {
        self.has(Kind::Tpm) || self.has(Kind::Passphrase)
    }

    pub fn factors(&self) -> Vec<Factor> {
        let mut factors = Vec::new();
        if self.enrolled() {
            factors.push(Factor::Password);
        }
        if self.has(Kind::Recovery) {
            factors.push(Factor::Recovery);
        }
        factors
    }

    /// Replace the wrappers a secret opens with `wrapper`; the recovery
    /// wrapper and kinds this build does not know stay.
    pub fn set_secret_wrapper(&mut self, wrapper: Wrapper) {
        self.wrappers.retain(|entry| {
            !matches!(
                entry.known().map(Wrapper::kind),
                Some(Kind::Tpm | Kind::Passphrase)
            )
        });
        self.wrappers.insert(0, Entry::Known(wrapper));
    }

    pub fn set_recovery_wrapper(&mut self, wrapper: Wrapper) {
        self.wrappers
            .retain(|entry| entry.known().map(Wrapper::kind) != Some(Kind::Recovery));
        self.wrappers.push(Entry::Known(wrapper));
    }
}

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, uid: u32) -> PathBuf {
        self.dir.join(format!("{uid}.json"))
    }

    pub fn load(&self, uid: u32) -> Result<Option<Record>> {
        let path = self.path(uid);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let record = parse(&path, bytes)?;
        if record.uid != uid {
            bail!(
                "{} names uid {}, not {uid}; leaving it alone",
                path.display(),
                record.uid
            );
        }
        Ok(Some(record))
    }

    pub fn save(&self, record: &Record) -> Result<()> {
        let path = self.path(record.uid);
        let bytes = serde_json::to_vec_pretty(record).context("encoding a sign-in record")?;
        tilo_files::native::atomic::write_private(&path, &bytes)
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn all(&self) -> Result<Vec<Record>> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("listing {}", self.dir.display())),
        };
        let mut records = Vec::new();
        for entry in entries {
            let path = entry?.path();
            let Some(uid) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".json"))
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            if let Some(record) = self.load(uid)? {
                records.push(record);
            }
        }
        records.sort_by_key(|r| r.uid);
        Ok(records)
    }
}

fn parse(path: &Path, bytes: Vec<u8>) -> Result<Record> {
    #[derive(Deserialize)]
    struct Versioned {
        version: u32,
    }
    let stored = serde_json::from_slice::<Versioned>(&bytes)
        .with_context(|| format!("{} is not a sign-in record", path.display()))?
        .version;
    let current = RECORD_FORMAT
        .bring_up(path, bytes, stored)
        .with_context(|| format!("bringing {} up to date", path.display()))?;
    serde_json::from_slice(&current).with_context(|| format!("reading {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn recovery() -> Wrapper {
        Wrapper::Recovery {
            envelope: "cmVj".into(),
        }
    }

    #[test]
    fn records_round_trip_owner_only_and_by_uid() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("users"));
        assert!(store.load(1000).unwrap().is_none());
        let mut record = Record::new(1000, "ada", "Ada Lovelace");
        record.set_recovery_wrapper(recovery());
        store.save(&record).unwrap();
        assert_eq!(store.load(1000).unwrap().unwrap(), record);
        assert_eq!(store.all().unwrap(), vec![record]);
        let file = std::fs::metadata(dir.path().join("users/1000.json")).unwrap();
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
        let parent = std::fs::metadata(dir.path().join("users")).unwrap();
        assert_eq!(parent.permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn the_stored_shape_is_the_contract() {
        let mut record = Record::new(1000, "ada", "Ada");
        record.unlocks_system = true;
        record.system_keyslot = Some(3);
        let json = serde_json::to_value(&record).unwrap();
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "display_name",
                "name",
                "system_keyslot",
                "uid",
                "unlocks_system",
                "version",
                "wrappers"
            ]
        );
    }

    #[test]
    fn a_new_secret_replaces_only_the_wrappers_a_secret_opens() {
        let mut record = Record::new(1, "a", "A");
        record.set_recovery_wrapper(recovery());
        record.set_secret_wrapper(Wrapper::Passphrase {
            envelope: "b2xk".into(),
        });
        record
            .wrappers
            .push(Entry::Unknown(serde_json::json!({"kind": "fido2"})));
        assert!(record.enrolled());
        assert_eq!(record.factors(), [Factor::Password, Factor::Recovery]);
        let tpm = Wrapper::Tpm {
            public: "cA==".into(),
            private: "cQ==".into(),
        };
        record.set_secret_wrapper(tpm.clone());
        assert_eq!(record.known().next(), Some(&tpm));
        assert!(!record.has(Kind::Passphrase));
        assert!(record.has(Kind::Recovery));
        assert_eq!(record.wrappers.len(), 3);
    }

    #[test]
    fn a_record_filed_under_another_uid_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        store.save(&Record::new(1000, "ada", "Ada")).unwrap();
        std::fs::rename(dir.path().join("1000.json"), dir.path().join("1001.json")).unwrap();
        assert!(store.load(1001).is_err());
    }
}
