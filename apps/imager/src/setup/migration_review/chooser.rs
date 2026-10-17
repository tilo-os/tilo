//! UI-only chooser adapter; native request custody and grants stay in the backend.
use imager_backend::migration_review::{
    self as native, GrantedHomeRoots, MountedHomeLocations, MountedRootRequest,
    MountedRootSelection, RootKind,
};
use std::{
    io,
    sync::{Arc, atomic::AtomicBool, mpsc},
};

/// App adapter retaining the native, non-serializable permission generation.
pub struct HomeRootSet(native::HomeRootSet);

impl HomeRootSet {
    pub fn from_native(roots: native::HomeRootSet) -> Self {
        Self(roots)
    }
    pub fn new(locations: Arc<MountedHomeLocations>) -> Self {
        Self(native::HomeRootSet::new(locations))
    }
    pub fn locations(&self) -> Vec<(String, String, bool)> {
        self.0.locations()
    }
    /// Off-frame: native discovery is validated before opening the shared picker.
    pub fn request(
        &mut self,
        location: &str,
        title: &str,
        accept: &str,
        cancel: &AtomicBool,
    ) -> io::Result<MountedRootChooser> {
        let request = self.0.begin_request(location, cancel)?;
        let channel = match request.kind() {
            RootKind::Directory => Channel::Directory(
                ui::PickRequest::directory(title, accept)
                    .initial_directory(request.initial_directory())
                    .send(),
            ),
            RootKind::File => Channel::File(
                ui::PickRequest::open(title, accept)
                    .initial_directory(request.initial_directory())
                    .send_open_file(),
            ),
        };
        Ok(MountedRootChooser {
            request: Some(request),
            channel,
        })
    }
    pub fn approve(
        &mut self,
        selection: MountedRootSelection,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.0.approve(selection, cancel)
    }
    pub fn validate(&self, cancel: &AtomicBool) -> io::Result<()> {
        self.0.validate(cancel)
    }
    pub fn cancel_request(&mut self) {
        self.0.cancel_request();
    }
    pub fn into_granted_roots(self, cancel: &AtomicBool) -> io::Result<GrantedHomeRoots> {
        self.0.into_granted_roots(cancel)
    }
}

enum Channel {
    Directory(ui::PortalChannel),
    File(ui::portal::OpenFileChannel),
}

/// Owns one actual platform request. Dropping it retires late answers.
pub struct MountedRootChooser {
    request: Option<MountedRootRequest>,
    channel: Channel,
}

impl MountedRootChooser {
    fn refuse(&mut self, kind: io::ErrorKind) -> io::Result<Option<MountedRootSelection>> {
        self.request.take();
        Err(kind.into())
    }
    /// Drain only the picker answer; source validation runs separately off-frame.
    pub fn poll(&mut self) -> io::Result<Option<MountedRootSelection>> {
        if self
            .request
            .as_ref()
            .is_none_or(|request| !request.is_active())
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Mounted root chooser was retired",
            ));
        }
        match &self.channel {
            Channel::Directory(channel) => match channel.try_recv() {
                Ok(ui::PortalOutcome::Opened(path)) => {
                    let request = self.request.take().ok_or(io::ErrorKind::PermissionDenied)?;
                    request.select_directory(path).map(Some)
                }
                Ok(ui::PortalOutcome::Cancelled) => self.refuse(io::ErrorKind::Interrupted),
                Ok(ui::PortalOutcome::Failed(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                    self.refuse(io::ErrorKind::PermissionDenied)
                }
                Err(mpsc::TryRecvError::Empty) => Ok(None),
            },
            Channel::File(channel) => match channel.try_recv() {
                Ok(ui::portal::OpenFileOutcome::Opened(opened)) => {
                    let request = self.request.take().ok_or(io::ErrorKind::PermissionDenied)?;
                    request.select_file(opened.into_file()).map(Some)
                }
                Ok(ui::portal::OpenFileOutcome::Cancelled) => {
                    self.refuse(io::ErrorKind::Interrupted)
                }
                Ok(ui::portal::OpenFileOutcome::Failed(_))
                | Err(mpsc::TryRecvError::Disconnected) => {
                    self.refuse(io::ErrorKind::PermissionDenied)
                }
                Err(mpsc::TryRecvError::Empty) => Ok(None),
            },
        }
    }
}
