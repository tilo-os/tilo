//! A person's look-round, proposals and daily limit in one workspace.

use std::collections::{BTreeMap, btree_map::Entry};
use std::io::ErrorKind;
use std::path::Path;

use super::{FILE, Initiative, Kept};
use tilo_lock::MutexRecover as _;
use uuid::Uuid;

/// Both fields come from the workspace's authorization decision. Files for a person
/// use their full root key, in the same canonical spelling on every device.
#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Owner {
    pub(super) workspace: String,
    pub(super) person: [u8; 32],
}

impl Kept {
    /// A scheduled look and a manual look charge the same day, including when a
    /// person takes their first look before they turn on a schedule. An event made
    /// by an earlier version keeps its original ID and accounting history.
    pub(super) fn spending_event(&self, owner: &Owner) -> String {
        self.spending_event
            .clone()
            .or_else(|| self.event.clone())
            .unwrap_or_else(|| {
                let name = format!(
                    "tilo.initiative/{}/{}",
                    owner.workspace,
                    hex::encode(owner.person)
                );
                Uuid::new_v5(&Uuid::NAMESPACE_URL, name.as_bytes()).to_string()
            })
    }

    /// The first installation uses the same stable name as a manual look. A
    /// removed installation gets a new id, even if two devices restart it at once.
    pub(super) fn next_event(&self, owner: &Owner) -> String {
        self.event.as_ref().map_or_else(
            || self.spending_event(owner),
            |previous| replacement(previous),
        )
    }
    /// Match the event author Chat pinned to the instruction. Ordinary event IDs
    /// are v4. Previous personal installations form a deterministic v5 sequence,
    /// so only its length is stored when a schedule is removed and recreated.
    pub(super) fn authored_by(&self, account: &str, author: &str) -> bool {
        if account == author || self.event.as_deref() == Some(author) {
            return true;
        }
        if !Uuid::parse_str(author).is_ok_and(|id| id.get_version_num() == 5) {
            return false;
        }
        let mut previous = account.to_owned();
        (1..self.replacements).any(|_| {
            previous = replacement(&previous);
            previous == author
        })
    }
}

fn replacement(previous: &str) -> String {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("tilo.initiative/replaces/{previous}").as_bytes(),
    )
    .to_string()
}

fn read(path: &Path) -> Result<Option<Kept>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| error.to_string()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

impl Initiative {
    fn file(&self, owner: &Owner) -> std::path::PathBuf {
        self.inner
            .workspaces
            .workspace_root(&owner.workspace)
            .join("initiative")
            .join(hex::encode(owner.person))
            .join(FILE)
    }

    /// An older build kept one file per workspace. Its recorded person could
    /// change when a second member resumed the schedule. Only the event's
    /// original author can keep it, and only matching records carry its proposals.
    fn read(&self, owner: &Owner) -> Result<Kept, String> {
        if let Some(kept) = read(&self.file(owner))? {
            return Ok(kept);
        }
        let root = self.inner.workspaces.workspace_root(&owner.workspace);
        let Some(mut kept) = read(&root.join(FILE))? else {
            return Ok(Kept::default());
        };
        let recorded = kept
            .person
            .take()
            .map(|person| person.eq_ignore_ascii_case(&hex::encode(owner.person)));
        let creator = kept
            .event
            .as_deref()
            .and_then(|event| super::super::events::stored_person(&root, event));
        let Some(creator) = creator else {
            // Without a surviving installation, only the host's account record
            // can attribute a workspace that was provisioned for this person.
            return Ok(
                if kept.event.is_none()
                    && recorded != Some(false)
                    && self.inner.workspaces.owns(&owner.person, &owner.workspace)
                {
                    kept
                } else {
                    Kept::default()
                },
            );
        };
        if creator != owner.person {
            return Ok(Kept::default());
        }
        if recorded == Some(true) {
            return Ok(kept);
        }
        // Keep the creator's schedule accounted for, even if its book can no
        // longer be attributed to one person. Leave the old file intact.
        Ok(Kept {
            event: kept.event,
            day_limit_micros: kept.day_limit_micros,
            ..Default::default()
        })
    }

    fn load<'a>(
        &self,
        owner: &Owner,
        all: &'a mut BTreeMap<Owner, Kept>,
    ) -> Result<&'a mut Kept, String> {
        Ok(match all.entry(owner.clone()) {
            Entry::Occupied(found) => found.into_mut(),
            Entry::Vacant(slot) => slot.insert(self.read(owner)?),
        })
    }

    pub(super) fn kept(&self, owner: &Owner) -> Result<Kept, String> {
        self.load(owner, &mut self.inner.kept.lock_recover())
            .cloned()
    }

    /// Publish an edit only after its private file was written. A damaged file
    /// or one we cannot read is preserved so the operation can be retried.
    pub(super) fn change<T>(
        &self,
        owner: &Owner,
        work: impl FnOnce(&mut Kept) -> T,
    ) -> Result<T, String> {
        let mut all = self.inner.kept.lock_recover();
        let kept = self.load(owner, &mut all)?;
        let mut changed = kept.clone();
        let answer = work(&mut changed);
        let bytes = serde_json::to_vec_pretty(&changed).map_err(|error| error.to_string())?;
        tilo_files::native::atomic::write_private(&self.file(owner), &bytes)
            .map_err(|error| error.to_string())?;
        *kept = changed;
        drop(all);
        Ok(answer)
    }
}
