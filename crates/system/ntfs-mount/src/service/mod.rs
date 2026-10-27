//! The mount service: answers the shell and Settings, keeps the grants and
//! the open drives, and leaves every touch of real hardware to a [`Backend`].

pub mod peer;
pub mod server;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub mod sandbox;

use std::path::PathBuf;

use crate::grants::GrantStore;
use crate::policy::{self, DriveFacts, Grant};
use crate::protocol::{
    Consent, Drive, Mode, Mounted, ReadOnlyReason, RememberOffer, Remembered, RememberedDrive,
    Request, Response,
};

/// One NTFS volume the backend found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// `<kernel name>:<disk sequence number>`: unique per insertion.
    pub id: String,
    /// The kernel's name for the block device (`sdb1`).
    pub name: String,
    pub label: String,
    pub size_bytes: u64,
    pub facts: DriveFacts,
}

/// Who is asking, as the kernel reports the connecting process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Caller {
    /// The compositor of the person at the seat: the only process that
    /// draws the consent prompt, so the only one that may open drives.
    Shell { uid: u32 },
    /// Any other process of the person at the seat.
    Person { uid: u32 },
}

impl Caller {
    pub fn uid(self) -> u32 {
        match self {
            Self::Shell { uid } | Self::Person { uid } => uid,
        }
    }
}

/// What the preflight found, before any driver touched the volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Health {
    pub read_only: Option<ReadOnlyReason>,
}

/// One mount the backend is asked to make.
#[derive(Clone, Copy, Debug)]
pub struct MountRequest<'a> {
    pub candidate: &'a Candidate,
    pub mode: Mode,
    pub read_only: bool,
    pub uid: u32,
}

/// Everything that touches devices, drivers, mounts and sessions.
pub trait Backend {
    /// What the backend keeps for an open drive (a helper process, say).
    type Mount;

    /// The NTFS volumes attached now.
    ///
    /// # Errors
    ///
    /// A message for the person when the devices cannot be listed.
    fn drives(&mut self) -> Result<Vec<Candidate>, String>;

    /// Check the volume in userspace, without loading or using a driver.
    ///
    /// # Errors
    ///
    /// A message for the person when the volume must not be opened.
    fn preflight(&mut self, candidate: &Candidate) -> Result<Health, String>;

    /// Mount the volume and return where.
    fn mount(&mut self, request: &MountRequest<'_>) -> Result<(PathBuf, Self::Mount), String>;

    /// Unmount it; `gone` when the device has already disappeared.
    ///
    /// # Errors
    ///
    /// A message for the person when it stays mounted, typically because
    /// files on it are still open.
    fn unmount(
        &mut self,
        path: &std::path::Path,
        mount: &mut Self::Mount,
        gone: bool,
    ) -> Result<(), String>;

    /// Whether what serves the mount is still running.
    fn alive(&mut self, mount: &mut Self::Mount) -> bool;

    /// Whether `uid` still has a session on this machine.
    fn owner_present(&self, uid: u32) -> bool;

    /// A fresh identifier for a new grant.
    fn new_grant_id(&mut self) -> String;
}

struct Active<M> {
    drive: String,
    facts: DriveFacts,
    uid: u32,
    mounted: Mounted,
    handle: M,
}

/// The service's state.
pub struct Service<B: Backend> {
    backend: B,
    grants: GrantStore,
    active: Vec<Active<B::Mount>>,
}

fn failed(message: impl Into<String>) -> Response {
    Response::Failed {
        message: message.into(),
    }
}

impl<B: Backend> Service<B> {
    pub fn new(backend: B, grants: GrantStore) -> Self {
        Self {
            backend,
            grants,
            active: Vec::new(),
        }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Whether no drive is open, so the service holds nothing it could lose
    /// by exiting.
    pub fn is_idle(&self) -> bool {
        self.active.is_empty()
    }

    /// Answer one request.
    pub fn handle(&mut self, caller: Caller, request: Request) -> Response {
        let uid = caller.uid();
        match request {
            Request::Drives => self.list(uid),
            Request::Open { drive, consent } => {
                if !matches!(caller, Caller::Shell { .. }) {
                    return Response::Refused {
                        reason: "only the shell opens drives, after asking the person".into(),
                    };
                }
                self.open(uid, &drive, consent)
            }
            Request::Close { drive } => self.close(uid, &drive),
            Request::Remembered => self.remembered(uid),
            Request::Forget { grant } => self.forget(uid, &grant),
            Request::SetMode { grant, mode } => self.set_mode(uid, &grant, mode),
        }
    }

    /// Close what should no longer be open: drives that were unplugged,
    /// drives whose driver stopped, and drives whose owner has no session
    /// left.
    pub fn housekeeping(&mut self) {
        let present: Vec<String> = match self.backend.drives() {
            Ok(drives) => drives.into_iter().map(|drive| drive.id).collect(),
            Err(_) => return,
        };
        let mut index = 0;
        while let Some(active) = self.active.get_mut(index) {
            let gone = !present.contains(&active.drive) || !self.backend.alive(&mut active.handle);
            if gone || !self.backend.owner_present(active.uid) {
                let path = PathBuf::from(&active.mounted.path);
                if self
                    .backend
                    .unmount(&path, &mut active.handle, gone)
                    .is_ok()
                {
                    self.active.remove(index);
                    continue;
                }
            }
            index = index.saturating_add(1);
        }
    }

    fn load_grants(&self, uid: u32) -> Result<Vec<Grant>, String> {
        self.grants
            .load(uid)
            .map_err(|error| format!("remembered drives are unreadable: {error}"))
    }

    fn describe(&self, uid: u32, candidate: &Candidate, grants: &[Grant]) -> Drive {
        let assessment = policy::assess(&candidate.facts, grants);
        Drive {
            id: candidate.id.clone(),
            label: candidate.label.clone(),
            size_bytes: candidate.size_bytes,
            placement: candidate.facts.placement(),
            hot_added: !candidate.facts.present_at_boot,
            remembered: assessment.remembered.map(|grant| Remembered {
                grant: grant.id.clone(),
                mode: grant.mode,
            }),
            needs_consent: assessment.needs_consent,
            preselect: assessment.preselect,
            can_remember: if assessment.can_remember_open {
                RememberOffer::AnyMode
            } else if assessment.can_remember {
                RememberOffer::OpenSafely
            } else {
                RememberOffer::Nothing
            },
            mounted: self
                .active
                .iter()
                .find(|active| active.drive == candidate.id && active.uid == uid)
                .map(|active| active.mounted.clone()),
        }
    }

    fn list(&mut self, uid: u32) -> Response {
        let grants = match self.load_grants(uid) {
            Ok(grants) => grants,
            Err(message) => return failed(message),
        };
        match self.backend.drives() {
            Ok(candidates) => Response::Drives {
                drives: candidates
                    .iter()
                    .map(|candidate| self.describe(uid, candidate, &grants))
                    .collect(),
            },
            Err(message) => failed(message),
        }
    }

    fn open(&mut self, uid: u32, drive: &str, consent: Option<Consent>) -> Response {
        let candidates = match self.backend.drives() {
            Ok(candidates) => candidates,
            Err(message) => return failed(message),
        };
        let Some(candidate) = candidates
            .into_iter()
            .find(|candidate| candidate.id == drive)
        else {
            return failed("this drive is no longer connected");
        };
        if let Some(active) = self.active.iter().find(|active| active.drive == drive) {
            return if active.uid == uid {
                Response::Opened {
                    mounted: active.mounted.clone(),
                }
            } else {
                failed("someone else on this computer has this drive open")
            };
        }
        let mut grants = match self.load_grants(uid) {
            Ok(grants) => grants,
            Err(message) => return failed(message),
        };
        let assessment = policy::assess(&candidate.facts, &grants);
        let mode = match (consent, assessment.remembered) {
            (Some(consent), _) => consent.mode,
            (None, Some(grant)) if !assessment.needs_consent => grant.mode,
            (None, _) => {
                return Response::NeedsConsent {
                    drive: self.describe(uid, &candidate, &grants),
                };
            }
        };
        if let Some(Consent { remember: true, .. }) = consent {
            let id = self.backend.new_grant_id();
            if let Some(grant) =
                policy::remember(&candidate.facts, mode, id, candidate.label.clone())
            {
                grants.retain(|existing| !existing.applies_to(&candidate.facts));
                grants.push(grant);
                if let Err(error) = self.grants.save(uid, &grants) {
                    return failed(format!("could not remember this drive: {error}"));
                }
            }
        }
        let health = match self.backend.preflight(&candidate) {
            Ok(health) => health,
            Err(message) => return failed(message),
        };
        let request = MountRequest {
            candidate: &candidate,
            mode,
            read_only: health.read_only.is_some(),
            uid,
        };
        match self.backend.mount(&request) {
            Ok((path, handle)) => {
                let mounted = Mounted {
                    path: path.to_string_lossy().into_owned(),
                    mode,
                    read_only: health.read_only,
                };
                self.active.push(Active {
                    drive: candidate.id,
                    facts: candidate.facts,
                    uid,
                    mounted: mounted.clone(),
                    handle,
                });
                Response::Opened { mounted }
            }
            Err(message) => failed(message),
        }
    }

    fn unmount_where(&mut self, keep: impl Fn(&Active<B::Mount>) -> bool) -> Result<(), String> {
        let mut result = Ok(());
        let mut index = 0;
        while let Some(active) = self.active.get_mut(index) {
            if keep(active) {
                index = index.saturating_add(1);
                continue;
            }
            let path = PathBuf::from(&active.mounted.path);
            match self.backend.unmount(&path, &mut active.handle, false) {
                Ok(()) => {
                    self.active.remove(index);
                }
                Err(message) => {
                    result = Err(message);
                    index = index.saturating_add(1);
                }
            }
        }
        result
    }

    fn close(&mut self, uid: u32, drive: &str) -> Response {
        if !self
            .active
            .iter()
            .any(|active| active.drive == drive && active.uid == uid)
        {
            return failed("this drive is not open");
        }
        match self.unmount_where(|active| active.drive != drive || active.uid != uid) {
            Ok(()) => Response::Done,
            Err(message) => failed(message),
        }
    }

    fn remembered(&self, uid: u32) -> Response {
        match self.load_grants(uid) {
            Ok(grants) => Response::Remembered {
                drives: grants
                    .iter()
                    .map(|grant| RememberedDrive {
                        grant: grant.id.clone(),
                        label: grant.label.clone(),
                        placement: grant.placement(),
                        mode: grant.mode,
                        mounted: self
                            .active
                            .iter()
                            .find(|active| active.uid == uid && grant.applies_to(&active.facts))
                            .map(|active| active.mounted.path.clone()),
                    })
                    .collect(),
            },
            Err(message) => failed(message),
        }
    }

    fn forget(&mut self, uid: u32, id: &str) -> Response {
        let mut grants = match self.load_grants(uid) {
            Ok(grants) => grants,
            Err(message) => return failed(message),
        };
        let Some(position) = grants.iter().position(|grant| grant.id == id) else {
            return failed("this drive is not remembered");
        };
        let grant = grants.remove(position);
        if let Err(error) = self.grants.save(uid, &grants) {
            return failed(format!("could not forget this drive: {error}"));
        }
        match self.unmount_where(|active| active.uid != uid || !grant.applies_to(&active.facts)) {
            Ok(()) => Response::Done,
            Err(message) => failed(format!("Forgotten, but it is still open: {message}")),
        }
    }

    fn set_mode(&self, uid: u32, id: &str, mode: Mode) -> Response {
        let mut grants = match self.load_grants(uid) {
            Ok(grants) => grants,
            Err(message) => return failed(message),
        };
        let Some(grant) = grants.iter_mut().find(|grant| grant.id == id) else {
            return failed("this drive is not remembered");
        };
        if !policy::mode_allowed(grant, mode) {
            return Response::Refused {
                reason: "a removable drive is only remembered for opening safely".into(),
            };
        }
        grant.mode = mode;
        match self.grants.save(uid, &grants) {
            Ok(()) => Response::Done,
            Err(error) => failed(format!("could not change this drive: {error}")),
        }
    }
}
