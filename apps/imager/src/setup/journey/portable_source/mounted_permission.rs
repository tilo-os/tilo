//! Device-local permission continuation for actual discovered Home mount entries.
//! No source epoch, inventory or complete archive is invented by this controller.
use super::*;
use crate::setup::migration_review::{HomeRootSet, MountedHomeLocations, MountedRootChooser};
use std::io;

enum Reply {
    Requested(io::Result<MountedRootChooser>),
    Approved(io::Result<()>),
    Prepared(Result<Reviewed, ReviewFailure>),
}
struct Worker {
    receiver: Receiver<(Option<HomeRootSet>, Reply)>,
    handle: Option<std::thread::JoinHandle<()>>,
    received: Option<(Option<HomeRootSet>, Reply)>,
}
impl Worker {
    fn poll(&mut self) -> io::Result<Option<(Option<HomeRootSet>, Reply)>> {
        if self.received.is_none() {
            match self.receiver.try_recv() {
                Ok(reply) => self.received = Some(reply),
                Err(mpsc::TryRecvError::Empty) => return Ok(None),
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }
        if self
            .handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return Ok(None);
        }
        if let Some(handle) = self.handle.take() {
            handle
                .join()
                .map_err(|_| io::Error::other("Mounted permission worker failed"))?;
        }
        self.received
            .take()
            .map(Some)
            .ok_or_else(|| io::Error::other("Mounted permission worker reply missing"))
    }
}
pub(super) struct Controller {
    roots: Option<HomeRootSet>,
    context: MountedContext,
    chooser: Option<MountedRootChooser>,
    worker: Option<Worker>,
    cancel: Arc<AtomicBool>,
    phase: &'static str,
    retired: bool,
    labels: Vec<(String, String, bool)>,
}
impl Controller {
    pub fn new(locations: Arc<MountedHomeLocations>, context: MountedContext) -> Self {
        let roots = HomeRootSet::new(locations);
        let labels = roots.locations();
        Self {
            roots: Some(roots),
            context,
            chooser: None,
            worker: None,
            cancel: Arc::new(AtomicBool::new(false)),
            phase: "permission",
            retired: false,
            labels,
        }
    }
    pub fn busy(&self) -> bool {
        self.worker.is_some() || self.chooser.is_some()
    }
    pub fn choosing(&self) -> bool {
        self.chooser.is_some()
    }
    pub fn locations(&self) -> Vec<(String, String, bool)> {
        if self.retired {
            return Vec::new();
        }
        self.labels.clone()
    }
    pub fn needs_restart(&self) -> bool {
        self.retired && !self.busy()
    }
    pub fn retained_context(&self) -> MountedContext {
        self.context.clone()
    }
    pub fn projection(&self) -> serde_json::Value {
        serde_json::json!({"phase":self.phase,"locations":self.locations().into_iter()
            .map(|(id,name,approved)| serde_json::json!({"id":id,"name":name,"approved":approved}))
            .collect::<Vec<_>>()})
    }
    pub fn request(
        &mut self,
        location: String,
        title: String,
        accept: String,
    ) -> Result<(), &'static str> {
        if self.busy() || self.cancel.load(Ordering::SeqCst) {
            return Err("installer-portable-source-busy");
        }
        let mut roots = self
            .roots
            .take()
            .ok_or("installer-portable-source-failed")?;
        let cancel = self.cancel.clone();
        let host = ui::host::current();
        let (tx, rx) = mpsc::channel();
        self.phase = "checking";
        let handle = std::thread::spawn(move || {
            let _scope = ui::host::enter(&host);
            let result = roots.request(&location, &title, &accept, &cancel);
            let _ = tx.send((Some(roots), Reply::Requested(result)));
        });
        self.worker = Some(Worker {
            receiver: rx,
            handle: Some(handle),
            received: None,
        });
        Ok(())
    }
    pub fn finish(&mut self) -> Result<(), &'static str> {
        if self.busy() || self.cancel.load(Ordering::SeqCst) {
            return Err("installer-portable-source-busy");
        }
        let locations = self.locations();
        if locations.is_empty() || locations.iter().any(|(_, _, approved)| !approved) {
            return Err("installer-mounted-home-grants-required");
        }
        let roots = self
            .roots
            .take()
            .ok_or("installer-portable-source-failed")?;
        let context = self.context.clone();
        let cancel = self.cancel.clone();
        let (tx, rx) = mpsc::channel();
        self.phase = "reviewing";
        let handle = std::thread::spawn(move || {
            let result = prepare(roots, context, &cancel);
            let _ = tx.send((None, Reply::Prepared(result)));
        });
        self.worker = Some(Worker {
            receiver: rx,
            handle: Some(handle),
            received: None,
        });
        Ok(())
    }
    /// Retires the exact chooser generation; no late answer can acquire authority.
    pub fn cancel_request(&mut self) {
        self.chooser = None;
        if let Some(roots) = &mut self.roots {
            roots.cancel_request();
        }
        // A request/approval may still own its set off-frame. Keep it until exit.
        if self.worker.is_some() {
            self.cancel.store(true, Ordering::SeqCst);
            self.phase = "cancelling";
        } else {
            self.phase = "permission";
        }
    }
    pub fn cancel(&mut self) {
        self.retired = true;
        self.cancel.store(true, Ordering::SeqCst);
        self.cancel_request();
        self.phase = if self.worker.is_some() {
            "cancelling"
        } else {
            "cancelled"
        };
    }
    pub fn poll(&mut self) -> Option<Result<Option<Reviewed>, ReviewFailure>> {
        if let Some(worker) = &mut self.worker {
            let (roots, reply) = match worker.poll() {
                Ok(Some(reply)) => reply,
                Ok(None) => return None,
                Err(_) => {
                    self.worker = None;
                    self.retired = true;
                    self.phase = "failed";
                    return Some(Err(ReviewFailure::Other));
                }
            };
            self.worker = None;
            self.roots = roots;
            if let Some(roots) = &self.roots {
                self.labels = roots.locations();
            }
            if self.cancel.load(Ordering::SeqCst) {
                if let Some(roots) = &mut self.roots {
                    roots.cancel_request();
                }
                self.retired |= self.roots.is_none();
                self.phase = if self.retired {
                    "cancelled"
                } else {
                    "permission"
                };
                if !self.retired {
                    self.cancel = Arc::new(AtomicBool::new(false));
                }
                return Some(Ok(None));
            }
            return Some(match reply {
                Reply::Requested(Ok(chooser)) => {
                    self.chooser = Some(chooser);
                    self.phase = "permission";
                    Ok(None)
                }
                Reply::Approved(Ok(())) => {
                    self.phase = "permission";
                    Ok(None)
                }
                Reply::Requested(Err(error)) | Reply::Approved(Err(error)) => {
                    if let Some(roots) = &mut self.roots {
                        roots.cancel_request();
                    }
                    self.phase = "failed";
                    Err(ReviewFailure::home(error))
                }
                Reply::Prepared(result) => {
                    if result.is_err() {
                        self.retired = true;
                    }
                    self.phase = if result.is_ok() { "reviewed" } else { "failed" };
                    result.map(Some)
                }
            });
        }
        let answer = self.chooser.as_mut()?.poll();
        let selection = match answer {
            Ok(None) => return None,
            Ok(Some(selection)) => selection,
            Err(error) => {
                self.chooser = None;
                if let Some(roots) = &mut self.roots {
                    roots.cancel_request();
                }
                self.phase = "permission";
                return Some(if error.kind() == io::ErrorKind::Interrupted {
                    Ok(None)
                } else {
                    Err(ReviewFailure::Permission)
                });
            }
        };
        self.chooser = None;
        let Some(mut roots) = self.roots.take() else {
            return Some(Err(ReviewFailure::Other));
        };
        let cancel = self.cancel.clone();
        let (tx, rx) = mpsc::channel();
        self.phase = "checking";
        let handle = std::thread::spawn(move || {
            let result = roots.approve(selection, &cancel);
            let _ = tx.send((Some(roots), Reply::Approved(result)));
        });
        self.worker = Some(Worker {
            receiver: rx,
            handle: Some(handle),
            received: None,
        });
        Some(Ok(None))
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.cancel();
    }
}
fn prepare(
    roots: HomeRootSet,
    context: MountedContext,
    cancel: &AtomicBool,
) -> Result<Reviewed, ReviewFailure> {
    context
        .grant
        .validate()
        .map_err(|_| ReviewFailure::SourceChanged)?;
    let roots = Arc::new(
        roots
            .into_granted_roots(cancel)
            .map_err(ReviewFailure::home)?,
    );
    let home = HomeSource::review_granted_original_roots(context.grant.selected(), roots, cancel)
        .map_err(ReviewFailure::home)?;
    let applications = if context.choices.applications && !context.choices.system_refs.is_empty() {
        let source = PortableFlatpakSource::review(
            &NativeRunner,
            context.grant.selected(),
            context.choices.system_refs.clone(),
        )
        .map_err(|_| ReviewFailure::Applications)?;
        source.validate().map_err(|_| ReviewFailure::Applications)?;
        Some(source)
    } else {
        None
    };
    home.validate_with_cancel(cancel)
        .map_err(ReviewFailure::home)?;
    context
        .grant
        .validate()
        .map_err(|_| ReviewFailure::SourceChanged)?;
    crate::setup::migration::check_cancel(cancel).map_err(ReviewFailure::home)?;
    let original = Arc::new(OriginalSource::from_native(super::NativeOriginalSource {
        grant: context.grant.clone(),
        choices: context.choices.clone(),
        intent: Mutex::new(None),
    }));
    Ok(Reviewed {
        original: Some(original),
        grant: context.grant,
        choices: context.choices,
        home: Some(home),
        applications,
    })
}

#[cfg(all(test, feature = "native-capture-qualification"))]
#[path = "mounted_permission/native_tests.rs"]
mod native_tests;
