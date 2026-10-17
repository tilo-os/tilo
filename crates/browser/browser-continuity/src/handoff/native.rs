//! Retained-grant acquisition and private retry checkpoints.
use super::StatePath;
use crate::imports::{ImportBundle, MAX_BUNDLE_BYTES, MAX_BUNDLE_ENTRIES};
use crate::sources::{self, GrantedProfile};
use crate::{Plan, Provider};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, Read, Write};
use std::sync::Arc;
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};

/// An exact directory handle from the host's selected-profile capability grant.
/// The host must allow only `Profile::allowed_files`, never cookies or sessions.
pub struct ProfileGrant {
    pub id: String,
    pub directory: File,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedCapture {
    profile_id: String,
    pub bookmarks: Option<ImportBundle>,
    pub history: Option<ImportBundle>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Captured {
    plan: Plan,
    pub profiles: Vec<SelectedCapture>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    identity_public_key: [u8; 32],
    plan_hash: String,
    capture_hash: String,
}
fn failed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Browser continuity could not be verified",
    )
}
fn plan_hash(plan: &Plan) -> io::Result<String> {
    Ok(blake3::hash(&plan.encode().map_err(|_| failed())?)
        .to_hex()
        .to_string())
}
impl Captured {
    /// Capture all explicitly selected domains before any personal record edits.
    /// # Errors
    /// Refuses invalid grants, identities, selected data or capture bounds.
    pub fn capture(plan: Plan, person: &[u8; 32], grants: Vec<ProfileGrant>) -> io::Result<Self> {
        plan.validate().map_err(|_| failed())?;
        if &plan.identity_public_key != person || grants.len() != plan.profiles.len() {
            return Err(failed());
        }
        let mut profiles = Vec::new();
        let mut grants = grants;
        for selection in &plan.profiles {
            let index = grants
                .iter()
                .position(|grant| grant.id == selection.id)
                .ok_or_else(failed)?;
            let grant = grants.remove(index);
            let profile = GrantedProfile::from_directory(grant.directory)?;
            let (bookmarks, history) = match selection.provider {
                Provider::Chromium => (
                    if selection.bookmarks {
                        Some(sources::parse_chromium_bookmarks_bundle(&profile)?)
                    } else {
                        None
                    },
                    if selection.history {
                        Some(sources::parse_chromium_history_bundle(&profile)?)
                    } else {
                        None
                    },
                ),
                Provider::Firefox => {
                    let bundle = sources::parse_firefox_places_selected_bundle(
                        &profile,
                        selection.bookmarks,
                        selection.history,
                    )?;
                    let bookmarks = if selection.bookmarks {
                        Some(bundle.bookmark_bundle())
                    } else {
                        None
                    };
                    let history = if selection.history {
                        let bundle = bundle.history_bundle();
                        Some(bundle)
                    } else {
                        None
                    };
                    (bookmarks, history)
                }
            };
            profiles.push(SelectedCapture {
                profile_id: selection.id.clone(),
                bookmarks,
                history,
            });
        }
        let capture = Self { plan, profiles };
        capture.validate(person)?;
        Ok(capture)
    }
    fn validate(&self, person: &[u8; 32]) -> io::Result<()> {
        self.plan.validate().map_err(|_| failed())?;
        if &self.plan.identity_public_key != person
            || self.profiles.len() != self.plan.profiles.len()
        {
            return Err(failed());
        }
        let mut records = 0usize;
        for (selection, capture) in self.plan.profiles.iter().zip(&self.profiles) {
            if capture.profile_id != selection.id
                || capture.bookmarks.is_some() != selection.bookmarks
                || capture.history.is_some() != selection.history
            {
                return Err(failed());
            }
            for (history, bundle) in [(false, &capture.bookmarks), (true, &capture.history)] {
                let Some(bundle) = bundle else {
                    continue;
                };
                bundle.validate().map_err(|_| failed())?;
                if history && bundle.imported_tree.is_some() {
                    return Err(failed());
                }
                for chunk in &bundle.chunks {
                    if (history && !chunk.bookmarks.is_empty())
                        || (!history && !chunk.history.is_empty())
                    {
                        return Err(failed());
                    }
                    records = records
                        .checked_add(chunk.bookmarks.len())
                        .and_then(|value| value.checked_add(chunk.history.len()))
                        .ok_or_else(failed)?;
                }
                if let Some(tree) = &bundle.imported_tree {
                    records = records.checked_add(tree.nodes.len()).ok_or_else(failed)?;
                }
                if records > MAX_BUNDLE_ENTRIES {
                    return Err(failed());
                }
            }
        }
        self.encode().map(|_| ())
    }
    /// Encode a bounded retry checkpoint.
    /// # Errors
    /// Refuses a capture exceeding the checkpoint limit.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let mut writer = Bounded(Vec::new());
        serde_json::to_writer(&mut writer, self).map_err(|_| failed())?;
        Ok(writer.0)
    }
    fn capture_name(plan: &Plan) -> io::Result<String> {
        Ok(format!("continuity-{}.capture.json", plan_hash(plan)?))
    }
    fn receipt_name(plan: &Plan) -> io::Result<String> {
        Ok(format!("continuity-{}.complete.json", plan_hash(plan)?))
    }
    /// Keep stable source fingerprints so a partial retry cannot recount a changed source.
    /// # Errors
    /// Refuses insecure storage or a failed atomic checkpoint publication.
    pub fn save(&self, state: &StorageDirectory) -> io::Result<()> {
        state.validate_privacy(Privacy::OwnerOnly)?;
        let bytes = self.encode()?;
        state
            .publish(
                Self::capture_name(&self.plan)?.as_ref(),
                &bytes,
                PublishMode::IfAbsent,
                Privacy::OwnerOnly,
            )
            .map_err(io::Error::other)
    }
    /// Load a private checkpoint bound to the exact plan and person.
    /// # Errors
    /// Refuses insecure storage, malformed captures or mismatched selections.
    pub fn load(
        state: &StorageDirectory,
        plan: &Plan,
        person: &[u8; 32],
    ) -> io::Result<Option<Self>> {
        state.validate_privacy(Privacy::OwnerOnly)?;
        let bytes = match read(state, &Self::capture_name(plan)?, MAX_BUNDLE_BYTES) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let capture: Self = serde_json::from_slice(&bytes).map_err(|_| failed())?;
        if capture.plan != *plan {
            return Err(failed());
        }
        capture.validate(person)?;
        Ok(Some(capture))
    }
    /// Publish the receipt after all personal-record edits succeed.
    /// # Errors
    /// Refuses conflicting receipts or failed atomic publication.
    pub fn complete(&self, state: &StorageDirectory) -> io::Result<()> {
        let receipt = self.receipt()?;
        if self.is_complete(state)? {
            return Ok(());
        }
        let bytes = serde_json::to_vec(&receipt).map_err(|_| failed())?;
        state
            .publish(
                Self::receipt_name(&self.plan)?.as_ref(),
                &bytes,
                PublishMode::IfAbsent,
                Privacy::OwnerOnly,
            )
            .map_err(io::Error::other)
    }
    /// Verify an existing completion receipt against the entire capture.
    /// # Errors
    /// Refuses unreadable, malformed or conflicting receipts.
    pub fn is_complete(&self, state: &StorageDirectory) -> io::Result<bool> {
        let bytes = match read(state, &Self::receipt_name(&self.plan)?, 2048) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let actual: Receipt = serde_json::from_slice(&bytes).map_err(|_| failed())?;
        let expected = self.receipt()?;
        if actual.schema != expected.schema
            || actual.identity_public_key != expected.identity_public_key
            || actual.plan_hash != expected.plan_hash
            || actual.capture_hash != expected.capture_hash
        {
            return Err(failed());
        }
        Ok(true)
    }
    fn receipt(&self) -> io::Result<Receipt> {
        Ok(Receipt {
            schema: 1,
            identity_public_key: self.plan.identity_public_key,
            plan_hash: plan_hash(&self.plan)?,
            capture_hash: blake3::hash(&self.encode()?).to_hex().to_string(),
        })
    }
}
fn read(directory: &StorageDirectory, name: &str, limit: usize) -> io::Result<Vec<u8>> {
    let file = directory.open_read(name.as_ref(), Privacy::OwnerOnly)?;
    if file.metadata()?.len() > limit as u64 {
        return Err(failed());
    }
    let mut bytes = Vec::new();
    file.take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(failed());
    }
    Ok(bytes)
}
struct Bounded(Vec<u8>);
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|size| size > MAX_BUNDLE_BYTES)
        {
            return Err(failed());
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Verified capture and its retained private checkpoint authority.
pub struct Prepared {
    pub capture: Captured,
    pub state: Arc<StorageDirectory>,
    pub complete: bool,
}

/// Source authority retained across retries without rediscovering ambient paths.
#[derive(Clone)]
pub enum Source {
    Sandbox(StatePath),
    Granted {
        root: Arc<File>,
        state: Arc<StorageDirectory>,
    },
}
impl Source {
    pub fn sandbox(state_path: StatePath) -> Self {
        Self::Sandbox(state_path)
    }
    pub fn granted(root: File, state: StorageDirectory) -> Self {
        Self::Granted {
            root: Arc::new(root),
            state: Arc::new(state),
        }
    }
    /// # Errors
    /// Refuses invalid, foreign, changed or oversized selected sources.
    pub fn prepare(&self, person: &[u8; 32]) -> io::Result<Option<Prepared>> {
        if !super::supported() {
            return Ok(None);
        }
        match self {
            Self::Sandbox(path) => prepare(person, path),
            Self::Granted { root, state } => {
                prepare_given(root.try_clone()?, Arc::clone(state), person).map(Some)
            }
        }
    }
}
fn read_plan(root: &File, person: &[u8; 32]) -> io::Result<Plan> {
    let directory = StorageDirectory::from_file(root.try_clone()?)?;
    directory.validate_namespace_control()?;
    let bytes = read(&directory, crate::PLAN_FILE, crate::MAX_PLAN_BYTES)?;
    let plan = Plan::decode(&bytes).map_err(|_| failed())?;
    if &plan.identity_public_key != person {
        return Err(failed());
    }
    Ok(plan)
}
/// Prepare an exact host-granted projection, retaining private retry custody.
/// # Errors
/// Refuses invalid source authority, identity, capture or checkpoint storage.
pub fn prepare_given(
    root: File,
    state: Arc<StorageDirectory>,
    person: &[u8; 32],
) -> io::Result<Prepared> {
    let plan = read_plan(&root, person)?;
    let capture = if let Some(capture) = Captured::load(&state, &plan, person)? {
        capture
    } else {
        let mut grants = Vec::new();
        for profile in &plan.profiles {
            grants.push(ProfileGrant {
                id: profile.id.clone(),
                directory: crate::snapshot::profile_directory(&root, profile)?,
            });
        }
        let capture = Captured::capture(plan, person, grants)?;
        capture.save(&state)?;
        capture
    };
    // The checkpoint now owns the stable input; release this acquisition handle.
    drop(root);
    let complete = capture.is_complete(&state)?;
    Ok(Prepared {
        capture,
        state,
        complete,
    })
}
fn prepare(person: &[u8; 32], state_path: &StatePath) -> io::Result<Option<Prepared>> {
    let path = std::path::Path::new(crate::PROJECTION_DIRECTORY);
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() && !meta.is_symlink() => {}
        Ok(_) => return Err(failed()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let root = File::open(path)?;
    read_plan(&root, person)?;
    let statepath = state_path()?;
    tilo_files::native::private::directory(&statepath)?;
    let state = Arc::new(StorageDirectory::open(&statepath)?);
    prepare_given(root, state, person).map(Some)
}
