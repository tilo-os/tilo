//! Device-local first-use migration. Metadata selects an operation; only the
//! canonical headless helper mounts, copies or adopts its renewed grants.
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    process::{Child, ChildStdin},
    sync::mpsc,
};
fn failure() -> io::Error {
    io::Error::other("Installed migration is unavailable or changed")
}
fn uid() -> u32 {
    rustix::process::geteuid().as_raw()
}
pub(crate) fn child(parent: &File, name: &std::ffi::OsStr, flags: i32) -> io::Result<File> {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| failure())?;
    // SAFETY: parent is live, name terminated; openat returns a newly owned fd.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat returned a newly owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
pub(super) fn directory(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(failure());
    }
    let mut pin = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open("/")?;
    for component in path.components() {
        match component {
            std::path::Component::RootDir => (),
            std::path::Component::Normal(name) => {
                pin = child(&pin, name, libc::O_RDONLY | libc::O_DIRECTORY)?;
            }
            _ => return Err(failure()),
        }
    }
    let meta = pin.metadata()?;
    if meta.uid() != uid() || meta.mode() & 0o022 != 0 {
        return Err(failure());
    }
    Ok(pin)
}
pub(super) fn private(pin: &File) -> io::Result<()> {
    let meta = pin.metadata()?;
    if meta.uid() != uid() || meta.mode() & 0o077 != 0 {
        return Err(failure());
    }
    Ok(())
}
pub(super) fn create(parent: &File, name: &str) -> io::Result<File> {
    match child(
        parent,
        std::ffi::OsStr::new(name),
        libc::O_RDONLY | libc::O_DIRECTORY,
    ) {
        Ok(pin) => {
            private(&pin)?;
            Ok(pin)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let encoded = std::ffi::CString::new(name).map_err(|_| failure())?;
            // SAFETY: live directory and terminated single component.
            if unsafe { libc::mkdirat(parent.as_raw_fd(), encoded.as_ptr(), 0o700) } != 0 {
                return Err(io::Error::last_os_error());
            }
            parent.sync_all()?;
            let pin = child(
                parent,
                std::ffi::OsStr::new(name),
                libc::O_RDONLY | libc::O_DIRECTORY,
            )?;
            private(&pin)?;
            Ok(pin)
        }
        Err(error) => Err(error),
    }
}
fn valid_operation(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Acknowledgement {
    schema: u32,
    operation: String,
    outcome: String,
}
fn acknowledged(pin: &File, operation: &str) -> io::Result<bool> {
    let file = match child(
        pin,
        std::ffi::OsStr::new(&format!("{operation}.acknowledged.json")),
        libc::O_RDONLY,
    ) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    private(&file)?;
    if !file.metadata()?.is_file() {
        return Err(failure());
    }
    let mut bytes = Vec::new();
    file.take(1025).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 {
        return Err(failure());
    }
    let receipt: Acknowledgement = serde_json::from_slice(&bytes).map_err(|_| failure())?;
    if receipt.schema != 1
        || receipt.operation != operation
        || !["complete", "incomplete", "deferred"].contains(&receipt.outcome.as_str())
    {
        return Err(failure());
    }
    Ok(true)
}
fn plans(home: &Path) -> PathBuf {
    home.join(".local/state/tilo/migration-handoff")
}
pub(crate) fn detect(home: &Path) -> io::Result<Vec<String>> {
    let path = plans(home);
    let pin = match directory(&path) {
        Ok(pin) => pin,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    private(&pin)?;
    let mut operations = Vec::new();
    let mut entries = 0usize;
    let mut plan_count = 0usize;
    // Enumeration names are hints only; every record is opened relative to the
    // pinned directory and the helper later parses its authoritative schema.
    for entry in std::fs::read_dir(&path)? {
        entries = entries.checked_add(1).ok_or_else(failure)?;
        if entries > 128 {
            return Err(failure());
        }
        let name = entry?.file_name();
        let Some(name) = name.to_str() else {
            return Err(failure());
        };
        let Some(id) = name.strip_suffix(".migration.json") else {
            continue;
        };
        plan_count = plan_count.checked_add(1).ok_or_else(failure)?;
        if !valid_operation(id) {
            return Err(failure());
        }
        let file = child(&pin, std::ffi::OsStr::new(name), libc::O_RDONLY)?;
        private(&file)?;
        if !file.metadata()?.is_file() || file.metadata()?.len() > 9 * 1024 * 1024 {
            return Err(failure());
        }
        if !acknowledged(&pin, id)? && !crate::retained_files::acknowledged(home, id)? {
            operations.push(id.to_owned());
        }
        if operations.len() > 32 {
            return Err(failure());
        }
    }
    if plan_count == 0 {
        return Err(failure());
    }
    operations.sort();
    Ok(operations)
}
pub(crate) struct Migration {
    home: PathBuf,
    operations: Vec<String>,
    selected: Option<String>,
    worker: Option<Worker>,
    original_copy: Option<OriginalCopyWorker>,
    original_copy_request: Option<OriginalCopyRequest>,
    original_copy_operation: Option<String>,
    access: Option<crate::retained_files::Request>,
    access_request: Option<crate::retained_files::Request>,
    access_release: Option<String>,
    shared_files: bool,
    applications_only: bool,
    transfer_complete: bool,
    original_copy_completion: serde_json::Value,
    cleanup_state: &'static str,
    cleanup_result: serde_json::Value,
    state: &'static str,
    result: serde_json::Value,
}
impl Migration {
    pub(crate) fn open(home: PathBuf) -> Self {
        let (operations, state) = match detect(&home) {
            Ok(operations) => (operations, "pending"),
            Err(_) => (Vec::new(), "unavailable"),
        };
        let intent = operations
            .first()
            .and_then(|operation| crate::retained_files::intent(&home, operation).ok());
        Self {
            home,
            operations,
            selected: None,
            worker: None,
            original_copy: None,
            original_copy_request: None,
            original_copy_operation: None,
            access: None,
            access_request: None,
            access_release: None,
            shared_files: intent == Some(crate::retained_files::Intent::Shared),
            applications_only: intent == Some(crate::retained_files::Intent::Applications),
            transfer_complete: false,
            original_copy_completion: serde_json::Value::Null,
            cleanup_state: "none",
            cleanup_result: serde_json::Value::Null,
            state,
            result: serde_json::Value::Null,
        }
    }
    pub(crate) fn pending(&self) -> bool {
        !self.operations.is_empty() || self.state == "unavailable"
    }
    pub(crate) fn status(&self) -> serde_json::Value {
        serde_json::json!({"state":self.state,"operations":self.operations,"selected":self.selected,"result":self.result,"can_review":self.worker.is_none()&&self.original_copy.is_none()&&!self.busy(),"retained_access":self.shared_files,"applications_only":self.applications_only,"original_copy":self.original_copy.is_some(),"transfer_complete":self.transfer_complete,"original_copy_completion":self.original_copy_completion,"cleanup_state":self.cleanup_state,"cleanup_result":self.cleanup_result})
    }
    pub(crate) fn busy(&self) -> bool {
        [
            "reviewing",
            "copying",
            "verifying",
            "retirement-pending",
            "restoring-applications",
            "adopting",
            "cancelling",
            "releasing-applications",
        ]
        .contains(&self.state)
    }
    pub(crate) fn next_action(&self) -> &'static str {
        if self.transfer_complete && self.cleanup_state == "pending" {
            return "cleanup-resume";
        }
        match self.state {
            "reviewed" => "start",
            "ready-to-adopt" => "adopt",
            "complete" | "existing-files-available" => "continue",
            "failed" | "cancelled" | "incomplete"
                if self.worker.is_some() || self.original_copy.is_some() =>
            {
                "retry"
            }
            _ => "review",
        }
    }
    fn review(&mut self, selected: Option<&str>) -> io::Result<()> {
        if self.state == "unavailable" {
            self.operations = detect(&self.home)?;
            self.state = "pending";
        }
        if self.worker.is_some() || self.original_copy.is_some() {
            return Err(failure());
        }
        let selected = selected
            .or_else(|| self.operations.first().map(String::as_str))
            .ok_or_else(failure)?;
        if !self
            .operations
            .iter()
            .any(|operation| operation == selected)
        {
            return Err(failure());
        }
        let intent = crate::retained_files::intent(&self.home, selected)?;
        self.shared_files = intent == crate::retained_files::Intent::Shared;
        self.applications_only = intent == crate::retained_files::Intent::Applications;
        self.original_copy_operation = None;
        if !self.shared_files {
            let (commands, command_receiver) = mpsc::channel();
            let (events, receiver) = mpsc::channel();
            self.original_copy_operation = Some(selected.into());
            self.original_copy_request = Some(OriginalCopyRequest {
                operation: selected.into(),
                commands: command_receiver,
                events,
            });
            self.original_copy = Some(OriginalCopyWorker { commands, receiver });
        } else if let Some(access) = crate::retained_files::request(&self.home, selected)? {
            if access.applications {
                let operation =
                    crate::retained_files::pending_applications_operation(&self.home, &access)?;
                let (commands, command_receiver) = mpsc::channel();
                let (events, receiver) = mpsc::channel();
                self.original_copy_operation = Some(operation.clone());
                self.original_copy_request = Some(OriginalCopyRequest {
                    operation,
                    commands: command_receiver,
                    events,
                });
                self.original_copy = Some(OriginalCopyWorker { commands, receiver });
            } else {
                self.access_request = Some(access.clone());
            }
            self.access = Some(access);
        } else {
            return Err(failure());
        }
        self.transfer_complete = false;
        self.original_copy_completion = serde_json::Value::Null;
        self.cleanup_state = "none";
        self.cleanup_result = serde_json::Value::Null;
        self.selected = Some(selected.into());
        self.state = "reviewing";
        self.result = serde_json::Value::Null;
        Ok(())
    }
    /// Continue past, or come back later to, the operation in hand: it is
    /// acknowledged and the next one waits.
    fn finish(&mut self, op: &str) -> io::Result<()> {
        if self.transfer_complete && self.cleanup_state != "finished" {
            return Err(failure());
        }
        if op == "continue" && self.access.is_some() && self.state != "existing-files-available" {
            return Err(failure());
        }
        if op == "continue"
            && !["complete", "incomplete", "existing-files-available"].contains(&self.state)
        {
            return Err(failure());
        }
        if self.original_copy.is_some() && self.busy() {
            return Err(failure());
        }
        if self.worker.as_ref().is_some_and(|_| {
            [
                "copying",
                "restoring-applications",
                "adopting",
                "cancelling",
                "reviewing",
            ]
            .contains(&self.state)
        }) {
            return Err(failure());
        }
        let id = self
            .selected
            .as_deref()
            .or_else(|| self.operations.first().map(String::as_str))
            .ok_or_else(failure)?
            .to_owned();
        let outcome = if op == "later" {
            "deferred"
        } else {
            self.state
        };
        if self.state == "existing-files-available"
            && let Some(access) = &self.access
        {
            crate::retained_files::acknowledge(&self.home, access)?;
        } else {
            self.acknowledge(&id, outcome)?;
        }
        self.worker = None;
        self.original_copy = None;
        self.original_copy_request = None;
        self.original_copy_operation = None;
        self.operations.retain(|operation| operation != &id);
        self.selected = None;
        self.original_copy_completion = serde_json::Value::Null;
        self.state = "pending";
        Ok(())
    }
    /// Hand the original copy's owner `op`: resume its cleanup, cancel or
    /// retry it.
    fn command_original_copy(&mut self, op: &str) -> io::Result<()> {
        use launcher::retained_view::installed_copy::CopyOwnerCommand;
        let worker = self.original_copy.as_ref().ok_or_else(failure)?;
        let command = match op {
            "cleanup-resume" if self.transfer_complete && self.cleanup_state == "pending" => {
                CopyOwnerCommand::ResumeCleanup {
                    operation: self.original_copy_operation.clone().ok_or_else(failure)?,
                }
            }
            "cancel" if self.busy() => CopyOwnerCommand::Cancel {},
            "retry" if ["failed", "cancelled", "incomplete"].contains(&self.state) => {
                CopyOwnerCommand::Retry {}
            }
            _ => return Err(failure()),
        };
        worker.commands.send(command).map_err(|_| failure())?;
        if op == "cancel" {
            self.original_copy_completion = serde_json::Value::Null;
        }
        if op == "cleanup-resume" {
            self.cleanup_state = "running";
            return Ok(());
        }
        self.state = if op == "cancel" {
            "cancelling"
        } else {
            "reviewing"
        };
        Ok(())
    }
    pub(crate) fn invoke(&mut self, op: &str, selected: Option<&str>) -> io::Result<()> {
        if op == "review" {
            return self.review(selected);
        }
        if op == "cancel"
            && self.access.is_some()
            && self.worker.is_none()
            && self.original_copy.is_none()
            && self.state == "reviewing"
        {
            self.original_copy_completion = serde_json::Value::Null;
            self.access_release = self.selected.clone();
            self.access_request = None;
            self.state = "cancelled";
            return Ok(());
        }
        if op == "continue" || op == "later" {
            return self.finish(op);
        }
        if self.original_copy.is_some() {
            return self.command_original_copy(op);
        }
        let expected = match op {
            "start" => self.state == "reviewed",
            "adopt" => self.state == "ready-to-adopt",
            "retry" => ["failed", "cancelled", "incomplete"].contains(&self.state),
            "cancel" => {
                ["copying", "restoring-applications", "adopting"].contains(&self.state)
                    || (self.access.is_some() && self.worker.is_some() && self.state == "reviewing")
            }
            _ => false,
        };
        if !expected {
            return Err(failure());
        }
        self.worker.as_mut().ok_or_else(failure)?.send(op)?;
        self.state = if op == "cancel" {
            "cancelling"
        } else if op == "adopt" {
            "adopting"
        } else {
            "copying"
        };
        Ok(())
    }
    pub(crate) fn take_access_request(&mut self) -> Option<crate::retained_files::Request> {
        if self.original_copy.is_some() {
            return None;
        }
        self.access_request.take()
    }
    pub(crate) fn take_access_release(&mut self) -> Option<String> {
        self.access_release.take()
    }
    pub(crate) fn access_outcome(&mut self, operation: &str, available: bool) {
        if self.selected.as_deref() == Some(operation)
            && self.access.is_some()
            && self.original_copy.is_none()
        {
            self.state = if available {
                "existing-files-available"
            } else {
                "failed"
            };
            self.result = serde_json::json!({"existing_files":available,"copied_bytes":0});
        }
    }
    fn acknowledge(&self, id: &str, outcome: &str) -> io::Result<()> {
        let pin = directory(&plans(&self.home))?;
        private(&pin)?;
        let bytes = serde_json::to_vec(&Acknowledgement {
            schema: 1,
            operation: id.into(),
            outcome: outcome.into(),
        })?;
        let name = format!("{id}.acknowledged.json");
        let mut file = child(
            &pin,
            std::ffi::OsStr::new(&name),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        )?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        pin.sync_all()
    }
    pub(crate) fn take_original_copy_request(&mut self) -> Option<OriginalCopyRequest> {
        self.original_copy_request.take()
    }
    /// Take the original copy's next event: its progress, its completion
    /// and, once the transfer is done, its cleanup.
    fn poll_original_copy(&mut self) -> bool {
        use launcher::retained_view::installed_copy::CopyOwnerEvent;
        let Some(worker) = &self.original_copy else {
            return false;
        };
        let event = match worker.receiver.try_recv() {
            Ok(event) => event,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                if !["complete", "cancelled", "failed"].contains(&self.state) {
                    self.state = "failed";
                    self.result = serde_json::Value::Null;
                }
                if self.transfer_complete && self.cleanup_state != "finished" {
                    self.cleanup_state = "unavailable";
                }
                self.original_copy = None;
                return true;
            }
        };
        if Some(event.operation()) != self.original_copy_operation.as_deref() {
            self.original_copy_completion = serde_json::Value::Null;
            if self.transfer_complete {
                self.cleanup_state = "unavailable";
                return true;
            }
            self.state = "failed";
            self.original_copy = None;
            self.result = serde_json::Value::Null;
            return true;
        }
        if self.transfer_complete {
            self.follow_cleanup(&event);
            return true;
        }
        if let CopyOwnerEvent::Complete {
            home_cipher_sha256,
            applications_cipher_sha256,
            ..
        } = &event
        {
            if self.shared_files
                && (home_cipher_sha256.is_some()
                    || applications_cipher_sha256.as_ref().is_none_or(|digest| {
                        digest.len() != 64
                            || !digest
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    }))
            {
                self.state = "failed";
                self.result = serde_json::Value::Null;
                return true;
            }
            // Preserve this real validated owner event when RW access later
            // reports its own outcome. It is observation, never admission.
            self.original_copy_completion =
                serde_json::to_value(&event).unwrap_or(serde_json::Value::Null);
            self.transfer_complete = true;
            self.cleanup_state = "running";
        }
        self.state = match &event {
            CopyOwnerEvent::Copying { .. } => "copying",
            CopyOwnerEvent::Verifying { .. } => "verifying",
            CopyOwnerEvent::Adopting { .. } => "adopting",
            CopyOwnerEvent::RestoringApplications { .. } => "restoring-applications",
            CopyOwnerEvent::Complete { .. } if self.shared_files => "releasing-applications",
            CopyOwnerEvent::Complete { .. } => "complete",
            CopyOwnerEvent::Cancelled { .. } => "cancelled",
            CopyOwnerEvent::Failed { .. }
            | CopyOwnerEvent::CleanupPending { .. }
            | CopyOwnerEvent::CleanupFinished { .. } => "failed",
            CopyOwnerEvent::RetirementPending { .. } => "retirement-pending",
        };
        self.result = serde_json::to_value(event).unwrap_or(serde_json::Value::Null);
        true
    }
    /// The original copy's cleanup after its transfer: pending, finished or
    /// unavailable. Finished, the shared files are reviewed again.
    fn follow_cleanup(&mut self, event: &launcher::retained_view::installed_copy::CopyOwnerEvent) {
        use launcher::retained_view::installed_copy::CopyOwnerEvent;
        self.cleanup_state = match event {
            CopyOwnerEvent::CleanupPending {
                resumable: true, ..
            } => "pending",
            CopyOwnerEvent::CleanupFinished { .. } => "finished",
            _ => "unavailable",
        };
        self.cleanup_result = serde_json::to_value(event).unwrap_or(serde_json::Value::Null);
        if self.cleanup_state == "finished" && self.shared_files {
            let access = self.access.as_ref();
            let unchanged = access.is_some_and(|access| {
                crate::retained_files::request(&self.home, &access.operation)
                    .ok()
                    .flatten()
                    .as_ref()
                    == Some(access)
                    && crate::retained_files::pending_applications_operation(&self.home, access)
                        .ok()
                        .as_deref()
                        == self.original_copy_operation.as_deref()
            });
            if !unchanged {
                self.cleanup_state = "unavailable";
                self.state = "failed";
                return;
            }
            // This event is emitted only after the actual copy root,
            // importer and certified proof responder have retired.
            // Never acquire the RW source while its RO owner is live.
            self.access_request = self.access.clone();
            self.original_copy = None;
            self.original_copy_request = None;
            self.state = "reviewing";
        }
    }
    pub(crate) fn poll(&mut self) -> bool {
        if self.original_copy.is_some() {
            return self.poll_original_copy();
        }
        if self.state == "releasing-applications" {
            let outcome = self.worker.as_mut().and_then(Worker::exited);
            if let Some(success) = outcome {
                self.worker = None;
                if success {
                    self.access_request = self.access.clone();
                    self.state = if self.applications_only {
                        "complete"
                    } else {
                        "reviewing"
                    };
                } else {
                    self.state = "failed";
                }
                return true;
            }
            return false;
        }
        let Some(worker) = self.worker.as_ref() else {
            return false;
        };
        let Some(result) = worker.poll() else {
            return false;
        };
        if let Ok(value) = result {
            let state = match value.get("event").and_then(serde_json::Value::as_str) {
                Some("reviewed") => "reviewed",
                Some("copying" | "progress") => "copying",
                Some("restoring-applications") => "restoring-applications",
                Some("ready-to-adopt") => "ready-to-adopt",
                Some("adopting") => "adopting",
                Some("complete")
                    if value.get("verified").and_then(serde_json::Value::as_bool) == Some(true)
                        && value.get("collisions").and_then(serde_json::Value::as_u64)
                            == Some(0)
                        && value.get("failures").and_then(serde_json::Value::as_u64) == Some(0) =>
                {
                    "complete"
                }
                Some("incomplete") => "incomplete",
                Some("cancelled") => "cancelled",
                Some("cancelling") => "cancelling",
                _ => "failed",
            };
            if state == "failed" && self.state == "reviewing" {
                self.worker = None;
            }
            if state == "complete" && (self.access.is_some() || self.applications_only) {
                if let Some(worker) = self.worker.as_mut() {
                    worker.stdin.take();
                }
                self.state = "releasing-applications";
            } else {
                self.state = state;
            }
            self.result = value;
            // The browser import completes during the helper's review, before
            // the home copy or adoption. Attach its signed projection now so
            // the first Browser launch in this session can take it.
            crate::onboarding_browser::attach_deferred_continuity();
        } else {
            self.state = "failed";
            self.result = serde_json::Value::Null;
            self.worker = None;
        }
        true
    }
}
struct Worker {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    receiver: mpsc::Receiver<Result<serde_json::Value, ()>>,
    operation: String,
}
impl Worker {
    fn exited(&mut self) -> Option<bool> {
        match self.child.as_mut()?.try_wait() {
            Ok(Some(status)) => Some(status.success()),
            Ok(None) => None,
            Err(_) => Some(false),
        }
    }
    fn send(&mut self, op: &str) -> io::Result<()> {
        let stdin = self.stdin.as_mut().ok_or_else(failure)?;
        stdin.write_all(&serde_json::to_vec(&serde_json::json!({"op":op}))?)?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }
    fn poll(&self) -> Option<Result<serde_json::Value, ()>> {
        match self.receiver.try_recv() {
            Ok(Ok(value))
                if value.get("operation").and_then(serde_json::Value::as_str)
                    == Some(&self.operation) =>
            {
                Some(Ok(value))
            }
            Ok(_) | Err(mpsc::TryRecvError::Disconnected) => Some(Err(())),
            Err(mpsc::TryRecvError::Empty) => None,
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        // EOF asks the canonical helper to cancel, drain and release its mount.
        self.stdin.take();
        if let Some(mut child) = self.child.take() {
            std::thread::spawn(move || {
                let deadline =
                    std::time::Instant::now().checked_add(std::time::Duration::from_secs(180));
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) | Err(_) => return,
                        Ok(None) => (),
                    }
                    if deadline.is_none_or(|deadline| std::time::Instant::now() >= deadline) {
                        let _ = child.kill();
                        let _ = child.wait();
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            });
        }
    }
}

/// Operation is routing only; the root independently opens its protected declaration.
pub(crate) struct OriginalCopyRequest {
    pub operation: String,
    pub commands: mpsc::Receiver<launcher::retained_view::installed_copy::CopyOwnerCommand>,
    pub events: mpsc::Sender<launcher::retained_view::installed_copy::CopyOwnerEvent>,
}
struct OriginalCopyWorker {
    commands: mpsc::Sender<launcher::retained_view::installed_copy::CopyOwnerCommand>,
    receiver: mpsc::Receiver<launcher::retained_view::installed_copy::CopyOwnerEvent>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::{
        ffi::OsStrExt as _,
        fs::{PermissionsExt, symlink},
    };
    fn fixture() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(plans(home.path())).unwrap();
        std::fs::set_permissions(plans(home.path()), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        home
    }
    #[test]
    fn metadata_selection_and_explicit_defer_do_not_claim_a_copy() {
        let home = fixture();
        let plan = plans(home.path()).join("setup.migration.json");
        std::fs::write(&plan, b"{}").unwrap();
        std::fs::set_permissions(&plan, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut migration = Migration::open(home.path().into());
        assert_eq!(
            migration.status()["operations"],
            serde_json::json!(["setup"])
        );
        assert!(migration.invoke("start", None).is_err());
        assert!(migration.invoke("continue", None).is_err());
        migration.invoke("later", None).unwrap();
        assert!(!migration.pending());
        assert!(detect(home.path()).unwrap().is_empty());
        let receipt: Acknowledgement = serde_json::from_slice(
            &std::fs::read(plans(home.path()).join("setup.acknowledged.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(receipt.outcome, "deferred");
        assert_eq!(std::fs::read(plan).unwrap(), b"{}");
        assert!(!home.path().join(".local/state/tilo/migration").exists());
    }
    #[test]
    fn original_copy_review_queues_the_native_actor_and_semantic_cancel() {
        let home = fixture();
        let operation = uuid::Uuid::new_v4().to_string();
        let plan = plans(home.path()).join(format!("{operation}.migration.json"));
        std::fs::write(&plan, b"{}").unwrap();
        std::fs::set_permissions(&plan, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut migration = Migration::open(home.path().into());
        migration.invoke("review", Some(&operation)).unwrap();
        let request = migration.take_original_copy_request().unwrap();
        assert_eq!(request.operation, operation);
        assert!(migration.worker.is_none(), "no rootless source-copy helper");
        assert!(!home.path().join(".local/state/tilo/migration").exists());
        assert!(migration.invoke("continue", None).is_err());
        migration.invoke("cancel", None).unwrap();
        assert!(matches!(
            request.commands.try_recv().unwrap(),
            launcher::retained_view::installed_copy::CopyOwnerCommand::Cancel {}
        ));
        request
            .events
            .send(
                launcher::retained_view::installed_copy::CopyOwnerEvent::RetirementPending {
                    operation: operation.clone(),
                    message: "Draining owned importer".into(),
                },
            )
            .unwrap();
        assert!(migration.poll());
        assert_eq!(migration.status()["state"], "retirement-pending");
        assert!(migration.invoke("continue", None).is_err());
    }

    #[test]
    fn completed_copy_cleanup_resume_uses_live_owner_and_preserves_completion() {
        use launcher::retained_view::installed_copy::{CopyOwnerCommand, CopyOwnerEvent};
        let home = fixture();
        let operation = uuid::Uuid::new_v4().to_string();
        let plan = plans(home.path()).join(format!("{operation}.migration.json"));
        std::fs::write(&plan, b"{}").unwrap();
        std::fs::set_permissions(&plan, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut migration = Migration::open(home.path().into());
        migration.invoke("review", Some(&operation)).unwrap();
        let request = migration.take_original_copy_request().unwrap();
        request
            .events
            .send(CopyOwnerEvent::Complete {
                operation: operation.clone(),
                home_cipher_sha256: Some("held receipt".into()),
                applications_cipher_sha256: None,
            })
            .unwrap();
        assert!(migration.poll());
        assert_eq!(migration.status()["state"], "complete");
        assert_eq!(migration.status()["transfer_complete"], true);
        assert!(migration.invoke("continue", None).is_err());
        request
            .events
            .send(CopyOwnerEvent::CleanupPending {
                operation: operation.clone(),
                message: "held temporary files".into(),
                resumable: true,
            })
            .unwrap();
        assert!(migration.poll());
        assert_eq!(migration.status()["state"], "complete");
        assert_eq!(migration.next_action(), "cleanup-resume");
        assert!(migration.invoke("retry", None).is_err());
        migration.invoke("cleanup-resume", None).unwrap();
        assert!(
            matches!(request.commands.try_recv().unwrap(), CopyOwnerCommand::ResumeCleanup { operation: actual } if actual == operation)
        );
        request
            .events
            .send(CopyOwnerEvent::CleanupFinished {
                operation: operation.clone(),
                metadata_quarantined: true,
            })
            .unwrap();
        assert!(migration.poll());
        assert_eq!(migration.status()["state"], "complete");
        assert_eq!(migration.status()["cleanup_state"], "finished");
        assert_eq!(migration.status()["result"]["event"], "complete");
        assert_eq!(migration.next_action(), "continue");
        migration.invoke("continue", None).unwrap();
        assert!(!migration.pending());
    }
    #[test]
    fn composite_owner_never_releases_rw_on_completion_or_wrong_cleanup() {
        let home = fixture();
        let stage = uuid::Uuid::new_v4().to_string();
        let source = uuid::Uuid::new_v4().to_string();
        let plan = plans(home.path()).join(format!("{stage}.migration.json"));
        std::fs::write(&plan, b"{}").unwrap();
        std::fs::set_permissions(&plan, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut migration = Migration::open(home.path().into());
        migration.invoke("review", Some(&stage)).unwrap();
        let request = migration.take_original_copy_request().unwrap();
        migration.shared_files = true;
        migration.selected = Some(source.clone());
        migration.access = Some(crate::retained_files::Request {
            operation: source.clone(),
            digest: "a".repeat(64),
            applications: true,
            identity_public_key: [7; 32],
        });
        // Even an accidentally queued old RW request remains fenced by the
        // genuinely pending original-copy owner, rather than a status label.
        migration.access_request = migration.access.clone();
        assert!(migration.take_access_request().is_none());
        migration.access_outcome(&source, true);
        assert_eq!(migration.status()["state"], "reviewing");
        request
            .events
            .send(
                launcher::retained_view::installed_copy::CopyOwnerEvent::Complete {
                    operation: stage.clone(),
                    home_cipher_sha256: None,
                    applications_cipher_sha256: Some("b".repeat(64)),
                },
            )
            .unwrap();
        assert!(migration.poll());
        assert_eq!(migration.status()["state"], "releasing-applications");
        let completed = migration.status()["original_copy_completion"].clone();
        assert_eq!(completed["operation"], stage);
        assert_eq!(completed["event"], "complete");
        assert_eq!(completed["home_cipher_sha256"], serde_json::Value::Null);
        assert_eq!(completed["applications_cipher_sha256"], "b".repeat(64));
        // Access cannot change while the owned RO copy still exists.
        migration.access_outcome(&source, true);
        assert_eq!(migration.status()["original_copy_completion"], completed);
        assert!(migration.take_access_request().is_none());
        assert!(migration.invoke("continue", None).is_err());
        request
            .events
            .send(
                launcher::retained_view::installed_copy::CopyOwnerEvent::CleanupFinished {
                    operation: source,
                    metadata_quarantined: false,
                },
            )
            .unwrap();
        assert!(migration.poll());
        assert_eq!(migration.status()["cleanup_state"], "unavailable");
        assert_eq!(
            migration.status()["original_copy_completion"],
            serde_json::Value::Null
        );
        assert!(migration.take_access_request().is_none());
    }

    #[test]
    fn validated_copy_completion_survives_later_rw_observation() {
        use launcher::retained_view::installed_copy::CopyOwnerEvent;
        let home = fixture();
        let stage = uuid::Uuid::new_v4().to_string();
        let path = plans(home.path()).join(format!("{stage}.migration.json"));
        std::fs::write(&path, b"{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut migration = Migration::open(home.path().into());
        migration.invoke("review", Some(&stage)).unwrap();
        let owner = migration.take_original_copy_request().unwrap();
        owner
            .events
            .send(CopyOwnerEvent::Complete {
                operation: stage.clone(),
                home_cipher_sha256: None,
                applications_cipher_sha256: Some("c".repeat(64)),
            })
            .unwrap();
        assert!(migration.poll());
        let completed = migration.status()["original_copy_completion"].clone();
        owner
            .events
            .send(CopyOwnerEvent::CleanupFinished {
                operation: stage.clone(),
                metadata_quarantined: true,
            })
            .unwrap();
        assert!(migration.poll());
        let cleanup = migration.status()["cleanup_result"].clone();
        // Simulate only the existing actor's next access callback after its
        // already-tested RO gate. This does not construct a source grant.
        migration.original_copy = None;
        migration.access = Some(crate::retained_files::Request {
            operation: stage.clone(),
            digest: "d".repeat(64),
            applications: true,
            identity_public_key: [8; 32],
        });
        migration.access_outcome(&stage, true);
        let observed = migration.status();
        assert_eq!(observed["state"], "existing-files-available");
        assert_eq!(observed["result"]["existing_files"], true);
        assert_eq!(observed["original_copy_completion"], completed);
        assert_eq!(observed["cleanup_result"], cleanup);
        assert_eq!(completed["applications_cipher_sha256"], "c".repeat(64));
    }

    #[test]
    fn empty_and_redirected_metadata_and_fifo_plan_remain_pending_errors() {
        let home = fixture();
        assert!(detect(home.path()).is_err());
        let path = plans(home.path()).join("setup.migration.json");
        let encoded = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: terminated fixture path creates only a temporary FIFO.
        assert_eq!(unsafe { libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);
        assert!(detect(home.path()).is_err());
        std::fs::remove_file(&path).unwrap();
        symlink("/etc/passwd", &path).unwrap();
        assert!(detect(home.path()).is_err());
        let migration = Migration::open(home.path().into());
        assert!(migration.pending());
        assert_eq!(migration.status()["state"], "unavailable");
    }
}
