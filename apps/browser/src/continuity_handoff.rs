//! Browser-owned consumer of an authenticated, identity-bound host selection.
//! Host capabilities authorize exact legacy files; parsing never grants access.
use std::fs::File;
use std::io::{self, Read, Write};
use std::sync::Arc;

use browser_continuity::{Plan, Provider};
use serde::{Deserialize, Serialize};
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};

use crate::continuity::{ImportBundle, MAX_BUNDLE_BYTES, MAX_BUNDLE_ENTRIES};
use crate::continuity_sources::{self as sources, GrantedProfile};
use crate::history::BrowsingHistory;
use crate::personal_bookmarks::{PersonalBookmarkAction, PersonalBookmarks};

/// An exact directory handle from the host's selected-profile capability grant.
/// The host must allow only `Profile::allowed_files`, never cookies or sessions.
pub(crate) struct ProfileGrant {
    pub id: String,
    pub directory: File,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectedCapture {
    profile_id: String,
    bookmarks: Option<ImportBundle>,
    history: Option<ImportBundle>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Captured {
    plan: Plan,
    profiles: Vec<SelectedCapture>,
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
    pub(crate) fn capture(
        plan: Plan,
        person: &[u8; 32],
        grants: Vec<ProfileGrant>,
    ) -> io::Result<Self> {
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
                        let crate::continuity_bookmarks::PersonalLibraryEdit::ImportBookmarkBundle { bundle } =
                            bundle.bookmark_action()
                        else {
                            return Err(failed());
                        };
                        Some(bundle)
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
    fn encode(&self) -> io::Result<Vec<u8>> {
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
    /// Keep the stable source fingerprints privately so a partial retry cannot recount a changed source.
    pub(crate) fn save(&self, state: &StorageDirectory) -> io::Result<()> {
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
    pub(crate) fn load(
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
    pub(crate) fn apply(
        &self,
        bookmarks: &ui::framework::PersonRecord<PersonalBookmarks>,
        history: &ui::framework::PersonRecord<BrowsingHistory>,
    ) -> io::Result<()> {
        for capture in &self.profiles {
            if let Some(bundle) = &capture.bookmarks {
                bookmarks
                    .apply(&PersonalBookmarkAction::Import {
                        bundle: bundle.clone(),
                    })
                    .map_err(|_| failed())?;
            }
            if let Some(bundle) = &capture.history {
                history
                    .invoke(
                        &BrowsingHistory::IMPORT_HISTORY_BUNDLE
                            .with(serde_json::to_string(bundle).map_err(|_| failed())?),
                    )
                    .map_err(|_| failed())?;
            }
        }
        Ok(())
    }
    pub(crate) fn complete(&self, state: &StorageDirectory) -> io::Result<()> {
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
    pub(crate) fn is_complete(&self, state: &StorageDirectory) -> io::Result<bool> {
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
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Status {
    Absent,
    Loading,
    Ready,
    Publishing,
    Complete,
    Failed,
}
struct Prepared {
    capture: Captured,
    state: Arc<StorageDirectory>,
    complete: bool,
}
enum Completion {
    Prepared(io::Result<Option<Prepared>>),
    Published(io::Result<()>),
}
/// Source acquisition is retained across initial loading and retries. Production
/// always resolves the fixed sandbox projection; tests retain exact granted
/// directory handles without installing files into the host namespace.
#[derive(Clone)]
enum Source {
    Sandbox(ui::storage::AppFiles),
    #[cfg(test)]
    Granted {
        root: Arc<File>,
        state: Arc<StorageDirectory>,
    },
}
impl Source {
    fn prepare(&self, person: &[u8; 32]) -> io::Result<Option<Prepared>> {
        match self {
            Self::Sandbox(files) => prepare(person, files),
            #[cfg(test)]
            Self::Granted { root, state } => {
                prepare_given(root.try_clone()?, Arc::clone(state), person).map(Some)
            }
        }
    }
}

/// Native work remains on its worker; only typed personal-record edits run on the view.
pub(crate) struct Controller {
    person: [u8; 32],
    source: Source,
    waker: ui::WakeHandle,
    receiver: Option<std::sync::mpsc::Receiver<Completion>>,
    ready: Option<Prepared>,
    status: Status,
}
impl Controller {
    pub(crate) fn start(
        person: [u8; 32],
        files: ui::storage::AppFiles,
        waker: ui::WakeHandle,
    ) -> Self {
        let mut controller = Self {
            person,
            source: Source::Sandbox(files),
            waker,
            receiver: None,
            ready: None,
            status: Status::Absent,
        };
        controller.retry();
        controller
    }
    pub(crate) fn status(&self) -> Status {
        self.status
    }
    pub(crate) fn retry(&mut self) -> bool {
        if self.receiver.is_some() {
            return false;
        }
        let person = self.person;
        let source = self.source.clone();
        self.run_prepare(move || source.prepare(&person));
        true
    }
    fn run_prepare(
        &mut self,
        action: impl FnOnce() -> io::Result<Option<Prepared>> + Send + 'static,
    ) {
        let waker = self.waker.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        self.status = Status::Loading;
        self.receiver = Some(receiver);
        self.ready = None;
        std::thread::spawn(move || {
            let result = action();
            let _ = sender.send(Completion::Prepared(result));
            waker.wake();
        });
    }
    #[cfg(test)]
    pub(crate) fn load_granted(&mut self, root: File, state: StorageDirectory) {
        assert!(!matches!(self.status, Status::Ready | Status::Publishing));
        self.source = Source::Granted {
            root: Arc::new(root),
            state: Arc::new(state),
        };
        // Retire only the previous acquisition result. Persona mutations still
        // run exclusively in poll; this hook cannot detach an applying job.
        self.receiver = None;
        assert!(self.retry());
    }
    pub(crate) fn poll(
        &mut self,
        bookmarks: &ui::framework::PersonRecord<PersonalBookmarks>,
        history: &ui::framework::PersonRecord<BrowsingHistory>,
    ) -> bool {
        let Some(receiver) = &self.receiver else {
            return false;
        };
        let result = match receiver.try_recv() {
            Ok(value) => value,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(_) => Completion::Published(Err(failed())),
        };
        self.receiver = None;
        match result {
            Completion::Prepared(Ok(None)) => self.status = Status::Absent,
            Completion::Prepared(Ok(Some(prepared))) if prepared.complete => {
                self.status = Status::Complete;
            }
            Completion::Prepared(Ok(Some(prepared))) => {
                self.status = Status::Ready;
                if prepared.capture.apply(bookmarks, history).is_err() {
                    self.ready = Some(prepared);
                    self.status = Status::Failed;
                    return true;
                }
                let waker = self.waker.clone();
                let (sender, receiver) = std::sync::mpsc::channel();
                self.status = Status::Publishing;
                self.receiver = Some(receiver);
                std::thread::spawn(move || {
                    let result = prepared.capture.complete(&prepared.state);
                    let _ = sender.send(Completion::Published(result));
                    waker.wake();
                });
            }
            Completion::Published(Ok(())) => self.status = Status::Complete,
            Completion::Prepared(Err(_)) | Completion::Published(Err(_)) => {
                self.status = Status::Failed;
            }
        }
        true
    }
}
fn read_plan(root: &File, person: &[u8; 32]) -> io::Result<Plan> {
    let directory = StorageDirectory::from_file(root.try_clone()?)?;
    directory.validate_namespace_control()?;
    let bytes = read(
        &directory,
        browser_continuity::PLAN_FILE,
        browser_continuity::MAX_PLAN_BYTES,
    )?;
    let plan = Plan::decode(&bytes).map_err(|_| failed())?;
    if &plan.identity_public_key != person {
        return Err(failed());
    }
    Ok(plan)
}
fn prepare_given(
    root: File,
    state: Arc<StorageDirectory>,
    person: &[u8; 32],
) -> io::Result<Prepared> {
    let plan = read_plan(&root, person)?;
    let capture = match Captured::load(&state, &plan, person)? {
        Some(capture) => capture,
        None => {
            let mut grants = Vec::new();
            for profile in &plan.profiles {
                grants.push(ProfileGrant {
                    id: profile.id.clone(),
                    directory: browser_continuity::snapshot::profile_directory(&root, profile)?,
                });
            }
            let capture = Captured::capture(plan, person, grants)?;
            capture.save(&state)?;
            capture
        }
    };
    let complete = capture.is_complete(&state)?;
    Ok(Prepared {
        capture,
        state,
        complete,
    })
}
fn prepare(person: &[u8; 32], files: &ui::storage::AppFiles) -> io::Result<Option<Prepared>> {
    let path = std::path::Path::new(browser_continuity::PROJECTION_DIRECTORY);
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() && !meta.is_symlink() => {}
        Ok(_) => return Err(failed()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let root = File::open(path)?;
    read_plan(&root, person)?;
    let location = files.paths().state();
    let statepath = location.native_path().map_err(|_| failed())?;
    tilo_files::native::private::directory(statepath)?;
    let state = Arc::new(StorageDirectory::open(statepath)?);
    prepare_given(root, state, person).map(Some)
}

#[cfg(test)]
mod tests;
