//! Device-local permission custody, not a coherent epoch or export declaration.
use super::*;
use std::{
    collections::BTreeMap,
    sync::{atomic::Ordering, mpsc},
};

mod roots;
pub(in crate::setup) use roots::TransferredHomeRoots;
pub use roots::{GrantedHomeRoots, GrantedRoot, RootKind};

/// One approved Home and separately chosen mounted entries. Never serializable;
/// no caller path or JSON can create an approved mounted root.
pub struct HomeRootSet {
    locations: Arc<MountedHomeLocations>,
    id: uuid::Uuid,
    pending: Option<(uuid::Uuid, Arc<AtomicBool>)>,
    approved: BTreeMap<usize, Approved>,
}
enum Approved {
    Directory(Arc<PinnedDirectory>),
    File(File),
}
struct Request {
    set: uuid::Uuid,
    nonce: uuid::Uuid,
    index: usize,
    alive: Arc<AtomicBool>,
}
enum Channel {
    Directory(ui::PortalChannel),
    File(ui::portal::OpenFileChannel),
}
/// Owns one actual platform request. Dropping it retires late answers.
pub struct MountedRootChooser {
    request: Option<Request>,
    channel: Channel,
}
enum Selected {
    Directory(PathBuf),
    File(File),
}
/// A consumed platform answer, not yet an admitted source capability.
pub struct MountedRootSelection {
    request: Request,
    selected: Selected,
}
impl HomeRootSet {
    pub(in crate::setup) fn new(locations: Arc<MountedHomeLocations>) -> Self {
        Self {
            locations,
            id: uuid::Uuid::new_v4(),
            pending: None,
            approved: BTreeMap::new(),
        }
    }
    /// Display-only IDs bound to this exact review generation.
    pub fn locations(&self) -> Vec<(String, String, bool)> {
        self.locations
            .candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                (
                    format!("{}:{index}", self.id),
                    candidate.relative.display().to_string(),
                    self.approved.contains_key(&index),
                )
            })
            .collect()
    }
    /// Off-frame: validate the held discovery before asking the shared picker.
    /// `title` and `accept` are localized by the calling controller.
    pub fn request(
        &mut self,
        location: &str,
        title: &str,
        accept: &str,
        cancel: &AtomicBool,
    ) -> io::Result<MountedRootChooser> {
        super::super::super::migration::check_cancel(cancel)?;
        self.locations.validate(cancel)?;
        let index = self
            .locations()
            .iter()
            .position(|(id, _, _)| id == location)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Mounted location is not in this review",
                )
            })?;
        let candidate = &self.locations.candidates[index];
        let metadata = Metadata::from_file(&candidate.entry)?;
        if !metadata.is_dir() && !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Mounted entry cannot be granted by this picker",
            ));
        }
        self.cancel_request();
        let nonce = uuid::Uuid::new_v4();
        let alive = Arc::new(AtomicBool::new(true));
        let path = self
            .locations
            .home
            .join(&self.locations.candidates[index].relative);
        let channel = if metadata.is_dir() {
            Channel::Directory(
                ui::PickRequest::directory(title, accept)
                    .initial_directory(path)
                    .send(),
            )
        } else {
            let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
            Channel::File(
                ui::PickRequest::open(title, accept)
                    .initial_directory(parent)
                    .send_open_file(),
            )
        };
        self.pending = Some((nonce, alive.clone()));
        Ok(MountedRootChooser {
            request: Some(Request {
                set: self.id,
                nonce,
                index,
                alive,
            }),
            channel,
        })
    }
    /// Off-frame: consumes the exact pending platform answer, then reattests
    /// original placement, ancestry and both held descriptors before adoption.
    /// This grants permission only; it does not enable whole-home export.
    pub fn approve(
        &mut self,
        selection: MountedRootSelection,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.check_request(&selection.request, cancel)?;
        self.locations.validate(cancel)?;
        let candidate = &self.locations.candidates[selection.request.index];
        let approved = match selection.selected {
            Selected::Directory(path) => {
                Approved::Directory(PinnedDirectory::open_absolute(&path)?)
            }
            Selected::File(file) => Approved::File(file),
        };
        let file = match &approved {
            Approved::Directory(pin) => {
                pin.validate()?;
                &pin.file
            }
            Approved::File(file) => file,
        };
        if Fact::capture(&Metadata::from_file(file)?)? != candidate.fact
            || fs_native::snapshot_lease::mount_id(file)? != candidate.mount
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Chosen mounted root differs from reviewed entry",
            ));
        }
        self.locations.validate(cancel)?;
        self.check_request(&selection.request, cancel)?;
        self.approved.insert(selection.request.index, approved);
        self.pending = None;
        Ok(())
    }
    fn check_request(&self, request: &Request, cancel: &AtomicBool) -> io::Result<()> {
        super::super::super::migration::check_cancel(cancel)?;
        if request.set != self.id
            || !request.alive.load(Ordering::Acquire)
            || self.pending.as_ref().is_none_or(|(nonce, alive)| {
                *nonce != request.nonce || !Arc::ptr_eq(alive, &request.alive)
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Mounted root request is cancelled or stale",
            ));
        }
        Ok(())
    }
    /// Validate permission custody without claiming a content epoch.
    pub fn validate(&self, cancel: &AtomicBool) -> io::Result<()> {
        self.locations.validate(cancel)?;
        for (index, approved) in &self.approved {
            super::super::super::migration::check_cancel(cancel)?;
            let candidate = &self.locations.candidates[*index];
            let file = match approved {
                Approved::Directory(pin) => {
                    pin.validate()?;
                    &pin.file
                }
                Approved::File(file) => file,
            };
            if Fact::capture(&Metadata::from_file(file)?)? != candidate.fact
                || fs_native::snapshot_lease::mount_id(file)? != candidate.mount
            {
                return Err(changed_source("Approved mounted root changed"));
            }
        }
        Ok(())
    }
    /// Retires the one-use request; its later platform answer cannot be approved.
    pub fn cancel_request(&mut self) {
        if let Some((_, alive)) = self.pending.take() {
            alive.store(false, Ordering::Release);
        }
    }
}
impl Drop for HomeRootSet {
    fn drop(&mut self) {
        self.cancel_request();
    }
}
impl MountedRootChooser {
    fn refuse(&mut self, kind: io::ErrorKind) -> io::Result<Option<MountedRootSelection>> {
        if let Some(request) = self.request.take() {
            request.alive.store(false, Ordering::Release);
        }
        Err(kind.into())
    }
    /// Drains only the chooser answer; source validation runs separately off-frame.
    pub fn poll(&mut self) -> io::Result<Option<MountedRootSelection>> {
        if self
            .request
            .as_ref()
            .is_none_or(|request| !request.alive.load(Ordering::Acquire))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Mounted root chooser was retired",
            ));
        }
        let selected = match &self.channel {
            Channel::Directory(channel) => match channel.try_recv() {
                Ok(ui::PortalOutcome::Opened(path)) => Selected::Directory(path),
                Ok(ui::PortalOutcome::Cancelled) => return self.refuse(io::ErrorKind::Interrupted),
                Ok(ui::PortalOutcome::Failed(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                    return self.refuse(io::ErrorKind::PermissionDenied);
                }
                Err(mpsc::TryRecvError::Empty) => return Ok(None),
            },
            Channel::File(channel) => match channel.try_recv() {
                Ok(ui::portal::OpenFileOutcome::Opened(opened)) => {
                    Selected::File(opened.into_file())
                }
                Ok(ui::portal::OpenFileOutcome::Cancelled) => {
                    return self.refuse(io::ErrorKind::Interrupted);
                }
                Ok(ui::portal::OpenFileOutcome::Failed(_))
                | Err(mpsc::TryRecvError::Disconnected) => {
                    return self.refuse(io::ErrorKind::PermissionDenied);
                }
                Err(mpsc::TryRecvError::Empty) => return Ok(None),
            },
        };
        let request = self.request.take().ok_or(io::ErrorKind::PermissionDenied)?;
        Ok(Some(MountedRootSelection { request, selected }))
    }
}
impl Drop for MountedRootChooser {
    fn drop(&mut self) {
        if let Some(request) = &self.request {
            request.alive.store(false, Ordering::Release);
        }
    }
}
