//! Who may use which capture device: the person's decision per app, per
//! room and per device, kept by the host that brokers capture, and the
//! [`crate::broker::Policy`] that applies it.
//!
//! A decision is recorded the first time an app that declares a device asks
//! for it, and holds until it is revoked; a revoked device is asked about
//! again. The file is the truth: a revocation written by any process stops
//! a capture already running as soon as its host sees the file change.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::DeviceKind;
use crate::broker::Policy;

/// What the person decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

type Key = (String, String, &'static str);

/// One recorded decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub room: String,
    pub app: String,
    pub device: DeviceKind,
    pub decision: Decision,
}

/// Every decision, persisted in one file of the host's profile.
pub struct DeviceGrants {
    path: PathBuf,
    loaded: Mutex<Loaded>,
}

/// The decisions as last read, and the file's modification time then.
struct Loaded {
    modified: Option<std::time::SystemTime>,
    decisions: BTreeMap<Key, Decision>,
}

impl DeviceKind {
    /// The device's name in capability grants (`Scope::Device`) and
    /// manifests.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Camera => "camera",
            Self::Microphone => "microphone",
        }
    }

    /// The device named `name`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "camera" => Some(Self::Camera),
            "microphone" => Some(Self::Microphone),
            _ => None,
        }
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

impl DeviceGrants {
    /// The decisions kept at `path`; none when it does not exist yet.
    ///
    /// # Errors
    ///
    /// The file cannot be read or holds something other than decisions.
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            path: path.to_path_buf(),
            loaded: Mutex::new(read(path)?),
        })
    }

    /// The decisions as the file holds them now: read again when another
    /// process changed it; none when it no longer reads.
    fn current(&self) -> std::sync::MutexGuard<'_, Loaded> {
        let mut loaded = self.loaded.lock().unwrap_or_else(PoisonError::into_inner);
        if modified(&self.path) != loaded.modified {
            *loaded = read(&self.path).unwrap_or(Loaded {
                modified: None,
                decisions: BTreeMap::new(),
            });
        }
        loaded
    }

    /// What was decided for `app` in `room` about `device`, if anything. A
    /// file changed by another process is read again first; one that no
    /// longer reads holds no decisions.
    pub fn decision(&self, room: &str, app: &str, device: DeviceKind) -> Option<Decision> {
        let loaded = self.current();
        let decision = loaded
            .decisions
            .get(&(room.to_owned(), app.to_owned(), device.name()))
            .copied();
        drop(loaded);
        decision
    }

    /// Every decision, in room, app and device order, as the file holds
    /// it now.
    pub fn list(&self) -> Vec<Grant> {
        let loaded = self.current();
        let grants = loaded
            .decisions
            .iter()
            .filter_map(|((room, app, device), decision)| {
                Some(Grant {
                    room: room.clone(),
                    app: app.clone(),
                    device: DeviceKind::from_name(device)?,
                    decision: *decision,
                })
            })
            .collect();
        drop(loaded);
        grants
    }

    /// Record `decision`, durably.
    ///
    /// # Errors
    ///
    /// An id is empty or has control characters, or the file cannot be
    /// written; nothing is recorded then.
    pub fn record(
        &self,
        room: &str,
        app: &str,
        device: DeviceKind,
        decision: Decision,
    ) -> io::Result<()> {
        if !valid_id(room) || !valid_id(app) {
            return Err(invalid("a room or app id is not a valid device-grant id"));
        }
        self.update(|decisions| {
            decisions.insert((room.to_owned(), app.to_owned(), device.name()), decision);
        })
    }

    /// Forget the decision, so the person is asked again, and stop any
    /// capture it allowed. `false` when there was none.
    ///
    /// # Errors
    ///
    /// The file cannot be written; the decision stands then.
    pub fn revoke(&self, room: &str, app: &str, device: DeviceKind) -> io::Result<bool> {
        let mut removed = false;
        self.update(|decisions| {
            removed = decisions
                .remove(&(room.to_owned(), app.to_owned(), device.name()))
                .is_some();
        })?;
        Ok(removed)
    }

    fn update(&self, change: impl FnOnce(&mut BTreeMap<Key, Decision>)) -> io::Result<()> {
        let mut loaded = self.loaded.lock().unwrap_or_else(PoisonError::into_inner);
        let mut next = read(&self.path)?.decisions;
        change(&mut next);
        self.persist(&next)?;
        *loaded = Loaded {
            modified: modified(&self.path),
            decisions: next,
        };
        drop(loaded);
        Ok(())
    }

    fn persist(&self, decisions: &BTreeMap<Key, Decision>) -> io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut text = String::new();
        for ((room, app, device), decision) in decisions {
            let decision = match decision {
                Decision::Allow => "allow",
                Decision::Deny => "deny",
            };
            let _ = writeln!(text, "{room}\t{app}\t{device}\t{decision}");
        }
        let temp = self.path.with_extension("tmp");
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, &self.path)
    }
}

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

fn read(path: &Path) -> io::Result<Loaded> {
    let mut decisions = BTreeMap::new();
    let modified = modified(path);
    match std::fs::read_to_string(path) {
        Ok(text) => {
            for line in text.lines().filter(|line| !line.is_empty()) {
                let fields: Vec<&str> = line.split('\t').collect();
                let [room, app, device, decision] = fields.as_slice() else {
                    return Err(invalid("a device grant line has the wrong shape"));
                };
                let device = DeviceKind::from_name(device)
                    .ok_or_else(|| invalid("a device grant names an unknown device"))?;
                let decision = match *decision {
                    "allow" => Decision::Allow,
                    "deny" => Decision::Deny,
                    _ => return Err(invalid("a device grant has an unknown decision")),
                };
                decisions.insert(
                    ((*room).to_owned(), (*app).to_owned(), device.name()),
                    decision,
                );
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(Loaded {
        modified,
        decisions,
    })
}

/// The [`Policy`] of one app in one room: a device it did not declare is
/// refused without asking anyone; a declared one follows the person's
/// recorded decision, and `consent` asks them when there is none.
pub struct GrantPolicy<C> {
    pub grants: std::sync::Arc<DeviceGrants>,
    pub room: String,
    pub app: String,
    pub declared: Vec<DeviceKind>,
    pub consent: C,
}

impl<C> Policy for GrantPolicy<C>
where
    C: Fn(DeviceKind) -> Result<bool, String> + Send + Sync,
{
    fn admit(&self, device: DeviceKind) -> Result<(), String> {
        if !self.declared.contains(&device) {
            return Err(format!(
                "{} does not declare the {}",
                self.app,
                device.name()
            ));
        }
        let decision = if let Some(decision) = self.grants.decision(&self.room, &self.app, device) {
            decision
        } else {
            let decision = if (self.consent)(device)? {
                Decision::Allow
            } else {
                Decision::Deny
            };
            self.grants
                .record(&self.room, &self.app, device, decision)
                .map_err(|error| format!("the decision could not be kept: {error}"))?;
            decision
        };
        match decision {
            Decision::Allow => Ok(()),
            Decision::Deny => Err(format!(
                "the {} is not allowed for {}",
                device.name(),
                self.app
            )),
        }
    }

    fn allows(&self, device: DeviceKind) -> bool {
        self.declared.contains(&device)
            && self.grants.decision(&self.room, &self.app, device) == Some(Decision::Allow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn decisions_are_per_app_room_and_device_persisted_and_revocable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/device-grants");
        let grants = DeviceGrants::open(&path).unwrap();
        assert_eq!(grants.decision("r", "a", DeviceKind::Camera), None);
        grants
            .record("r", "a", DeviceKind::Camera, Decision::Allow)
            .unwrap();
        grants
            .record("r", "a", DeviceKind::Microphone, Decision::Deny)
            .unwrap();
        assert_eq!(grants.decision("other-room", "a", DeviceKind::Camera), None);
        assert_eq!(grants.decision("r", "other-app", DeviceKind::Camera), None);

        let reopened = DeviceGrants::open(&path).unwrap();
        assert_eq!(
            reopened.decision("r", "a", DeviceKind::Camera),
            Some(Decision::Allow)
        );
        assert_eq!(
            reopened.decision("r", "a", DeviceKind::Microphone),
            Some(Decision::Deny)
        );
        assert_eq!(
            reopened.list(),
            [
                Grant {
                    room: "r".into(),
                    app: "a".into(),
                    device: DeviceKind::Camera,
                    decision: Decision::Allow,
                },
                Grant {
                    room: "r".into(),
                    app: "a".into(),
                    device: DeviceKind::Microphone,
                    decision: Decision::Deny,
                },
            ]
        );
        assert!(reopened.revoke("r", "a", DeviceKind::Camera).unwrap());
        assert!(!reopened.revoke("r", "a", DeviceKind::Camera).unwrap());
        assert_eq!(
            DeviceGrants::open(&path)
                .unwrap()
                .decision("r", "a", DeviceKind::Camera),
            None
        );
        assert!(
            grants
                .record("r\tx", "a", DeviceKind::Camera, Decision::Allow)
                .is_err()
        );
        std::fs::write(&path, "r\ta\tcamera\tmaybe\n").unwrap();
        assert!(DeviceGrants::open(&path).is_err(), "corruption is an error");
    }

    #[test]
    fn the_policy_asks_once_refuses_undeclared_devices_and_follows_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let grants = Arc::new(DeviceGrants::open(&dir.path().join("grants")).unwrap());
        let asked = Arc::new(AtomicUsize::new(0));
        let answer = Arc::new(Mutex::new(true));
        let policy = GrantPolicy {
            grants: Arc::clone(&grants),
            room: "r".into(),
            app: "so.tilo.video".into(),
            declared: vec![DeviceKind::Camera],
            consent: {
                let (asked, answer) = (Arc::clone(&asked), Arc::clone(&answer));
                move |_| {
                    asked.fetch_add(1, Ordering::SeqCst);
                    Ok(*answer.lock().unwrap())
                }
            },
        };
        let refused = policy.admit(DeviceKind::Microphone).unwrap_err();
        assert!(refused.contains("does not declare"), "{refused}");
        assert_eq!(
            asked.load(Ordering::SeqCst),
            0,
            "an undeclared device asked"
        );

        policy.admit(DeviceKind::Camera).unwrap();
        policy.admit(DeviceKind::Camera).unwrap();
        assert_eq!(asked.load(Ordering::SeqCst), 1, "asked twice");
        assert!(policy.allows(DeviceKind::Camera));

        // Revoked by another process: another handle on the same file.
        let elsewhere = DeviceGrants::open(&dir.path().join("grants")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        elsewhere
            .revoke("r", "so.tilo.video", DeviceKind::Camera)
            .unwrap();
        assert!(!policy.allows(DeviceKind::Camera));
        *answer.lock().unwrap() = false;
        assert!(policy.admit(DeviceKind::Camera).is_err());
        assert!(policy.admit(DeviceKind::Camera).is_err());
        assert_eq!(asked.load(Ordering::SeqCst), 2, "a denial asked again");
    }
}
