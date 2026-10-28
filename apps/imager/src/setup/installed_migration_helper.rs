//! Native target-user continuation. The same durable coordinator and promotion
//! witness serve this bounded headless protocol and installer actions.
use super::{
    destination_security,
    home_transfer::promotion::{PromotionEvent, PromotionReview, PromotionWorker},
    migration::PinnedDirectory,
    migration_handoff::{self, Plan},
    user_transfer::{TransferEvent, TransferReview, TransferWorker},
};
use std::{
    ffi::OsString,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};
const LIMIT: usize = 16 * 1024;
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    op: String,
}
fn failure() -> io::Error {
    io::Error::other("Installed migration is unavailable or its grants changed")
}

/// The active first-boot person; no key is supplied through argv or JSON.
fn current_person() -> [u8; 32] {
    runtime_identity::identity_dir_from_env()
        .ok()
        .and_then(|directory| runtime_identity::resolve_active_identity(&directory))
        .and_then(|active| runtime_identity::load_from_path(active).ok())
        .map_or([0; 32], |signer| signer.group_public_key())
}
fn private(path: &Path) -> io::Result<()> {
    let root = PinnedDirectory::open_absolute(path)?;
    root.validate_destination()?;
    destination_security::check_private_file(&root.file)
}
fn child(parent: &Path, name: &str) -> io::Result<PathBuf> {
    let pin = PinnedDirectory::open_absolute(parent)?;
    pin.validate_destination()?;
    destination_security::check_private_file(&pin.file)?;
    match super::nofollow::metadata_nofollow(&pin.file, Path::new(name)) {
        Ok(metadata) => {
            let next = pin.open_child(std::ffi::OsStr::new(name), &metadata)?;
            destination_security::check_private_file(&next.file)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            pin.create_child(std::ffi::OsStr::new(name))?;
        }
        Err(error) => return Err(error),
    }
    Ok(parent.join(name))
}
struct Grants {
    plans: PathBuf,
    operation: String,
    home: PathBuf,
    workspace: PathBuf,
}
fn arguments(args: impl IntoIterator<Item = OsString>) -> io::Result<Grants> {
    let mut values = std::collections::BTreeMap::new();
    let mut args = args.into_iter();
    while let Some(key) = args.next() {
        let key = key.into_string().map_err(|_| failure())?;
        if !["--plans", "--operation", "--home", "--workspace"].contains(&key.as_str())
            || values.contains_key(&key)
        {
            return Err(failure());
        }
        values.insert(key, args.next().ok_or_else(failure)?);
    }
    if values.len() != 4 {
        return Err(failure());
    }
    let get = |name: &str| values.get(name).cloned().ok_or_else(failure);
    let grants = Grants {
        plans: PathBuf::from(get("--plans")?),
        operation: get("--operation")?.into_string().map_err(|_| failure())?,
        home: PathBuf::from(get("--home")?),
        workspace: PathBuf::from(get("--workspace")?),
    };
    for path in [&grants.plans, &grants.home, &grants.workspace] {
        if !path.is_absolute() {
            return Err(failure());
        }
        private(path)?;
    }
    if grants.workspace
        != grants
            .home
            .join(".local/state/tilo/migration")
            .join(&grants.operation)
    {
        return Err(failure());
    }
    Ok(grants)
}
struct Session {
    operation: String,
    home: PathBuf,
    workspace: PathBuf,
    adoption_journal: PathBuf,
    applications_only: bool,
    application_plan_digest: Option<String>,
    transfer: TransferReview,
    copying: Option<TransferWorker>,
    promotion: Option<PromotionReview>,
    adopting: Option<PromotionWorker>,
    copy_attempted: bool,
    adoption_attempted: bool,
    state: &'static str,
    pending: serde_json::Value,
    progress_at: Instant,
}
impl Session {
    fn review(grants: Grants) -> Result<Self, String> {
        let plan = Plan::read_private(&grants.plans, &grants.operation)
            .map_err(|error| error.to_string())?;
        if !plan.categories.home {
            return Err("This continuation requires selected whole-home data; account-only and Flatpak-only plans need their own reviewed adoption.".into());
        }
        let receipts = child(&grants.workspace, "receipts").map_err(|error| error.to_string())?;
        let source =
            migration_handoff::mount_read_only(&plan).map_err(|error| error.to_string())?;
        imager_backend::setup::deferred_handoff::import_selected_from_source(
            &plan,
            source.path(),
            &grants.home,
            &grants.workspace,
            current_person(),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .map_err(|error| error.to_string())?;
        let adoption_journal = grants.workspace.join("adoption.journal");
        match std::fs::symlink_metadata(&adoption_journal) {
            Ok(_) => {
                let (transfer, promotion) = source.recover_promotion(
                    &grants.workspace.join("imported-home"),
                    &receipts,
                    &grants.home,
                    &adoption_journal,
                )?;
                let mut session =
                    Self::from_transfer(grants.operation, grants.home, grants.workspace, transfer)?;
                let preview = promotion.preview()?;
                session.pending = serde_json::json!({"event":"ready-to-adopt","operation":session.operation,"recovered":true,"install_items":preview.installed.len(),"collisions":preview.collisions.len(),"inactive_custody":preview.inactive_custody.len()});
                session.promotion = Some(promotion);
                session.adoption_attempted = true;
                session.copy_attempted = true;
                session.state = "ready-to-adopt";
                Ok(session)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let transfer =
                    source.review_transfer(&grants.workspace.join("imported-home"), &receipts)?;
                Self::from_transfer(grants.operation, grants.home, grants.workspace, transfer)
            }
            Err(error) => Err(error.to_string()),
        }
    }
    fn from_transfer(
        operation: String,
        home: PathBuf,
        workspace: PathBuf,
        transfer: TransferReview,
    ) -> Result<Self, String> {
        let summary = transfer.summary()?;
        let pending = serde_json::json!({"event":"reviewed","operation":operation,"summary":summary,"recovered":transfer.recovered()});
        Ok(Self {
            adoption_journal: workspace.join("adoption.journal"),
            applications_only: false,
            application_plan_digest: None,
            operation,
            home,
            workspace,
            transfer,
            copying: None,
            promotion: None,
            adopting: None,
            copy_attempted: false,
            adoption_attempted: false,
            state: "reviewed",
            pending,
            progress_at: Instant::now(),
        })
    }
    fn command(&mut self, op: &str) -> Result<(), String> {
        match op {
            "start" if self.state == "reviewed" => self.start_copy(),
            "retry" if self.copying.is_none() && self.adopting.is_none() => {
                if self.promotion.is_some() {
                    self.start_adoption(true)
                } else {
                    self.start_copy()
                }
            }
            "adopt" if self.state == "ready-to-adopt" => self.start_adoption(false),
            "cancel" => {
                if let Some(worker) = &self.copying {
                    worker.cancel();
                }
                if let Some(worker) = &self.adopting {
                    worker.cancel();
                }
                self.state = "cancelling";
                self.pending = serde_json::json!({"event":"cancelling","operation":self.operation});
                Ok(())
            }
            _ => Err("This migration action is not available in the current phase".into()),
        }
    }
    fn start_copy(&mut self) -> Result<(), String> {
        self.copying = Some(
            self.transfer
                .start(self.copy_attempted || self.transfer.recovered())?,
        );
        self.copy_attempted = true;
        self.state = "copying";
        self.pending = serde_json::json!({"event":"copying","operation":self.operation});
        Ok(())
    }
    fn start_adoption(&mut self, retry: bool) -> Result<(), String> {
        self.adopting = Some(
            self.promotion
                .as_ref()
                .ok_or("Home adoption is not reviewed")?
                .start(retry || self.adoption_attempted)?,
        );
        self.adoption_attempted = true;
        self.state = "adopting";
        self.pending = serde_json::json!({"event":"adopting","operation":self.operation});
        Ok(())
    }
    fn poll(&mut self) -> Result<bool, String> {
        let mut changed = false;
        while let Some(event) = self.copying.as_mut().and_then(TransferWorker::try_next) {
            match event {
                TransferEvent::CopyProgress(progress) => {
                    if self.progress_at.elapsed() >= Duration::from_millis(250) {
                        self.pending = serde_json::json!({"event":"progress","operation":self.operation,"verified_files":progress.verified_files,"verified_bytes":progress.verified_bytes});
                        self.progress_at = Instant::now();
                        changed = true;
                    }
                }
                TransferEvent::RestoringApplications => {
                    self.state = "restoring-applications";
                    self.pending =
                        serde_json::json!({"event":self.state,"operation":self.operation});
                    changed = true;
                }
                TransferEvent::Finished(result) => {
                    if let Some(mut worker) = self.copying.take() {
                        worker.join_finished()?;
                    }
                    let report = result?;
                    if report.complete() {
                        let promotion = self
                            .transfer
                            .review_promotion(&self.home, &self.adoption_journal)?;
                        let preview = promotion.preview()?;
                        self.pending = serde_json::json!({"event":"ready-to-adopt","operation":self.operation,"install_items":preview.installed.len(),"collisions":preview.collisions.len(),"inactive_custody":preview.inactive_custody.len()});
                        self.promotion = Some(promotion);
                        self.state = "ready-to-adopt";
                    } else {
                        self.state = if report.cancelled {
                            "cancelled"
                        } else {
                            "incomplete"
                        };
                        self.pending = serde_json::json!({"event":self.state,"operation":self.operation,"copy_failures":report.copies.iter().map(|report|report.failed.len()+report.skipped.len()).sum::<usize>(),"application_failures":report.flatpak.failed.len()});
                    }
                    changed = true;
                }
            }
        }
        if let Some(event) = self.adopting.as_mut().and_then(PromotionWorker::try_next) {
            self.adopting = None;
            match event {
                PromotionEvent::Failed(error) => return Err(error),
                PromotionEvent::Finished(report) => {
                    self.state = if report.cancelled {
                        "cancelled"
                    } else if report.complete() {
                        "complete"
                    } else {
                        "incomplete"
                    };
                    if report.complete() && self.applications_only {
                        applications::publish_completion(self)?;
                    }
                    self.pending = serde_json::json!({"event":self.state,"operation":self.operation,"installed":report.installed.len(),"collisions":report.collisions.len(),"inactive_custody":report.inactive_custody.len(),"failures":report.failed.len(),"verified":report.verified});
                }
            }
            changed = true;
        }
        Ok(changed)
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if let Some(worker) = &self.copying {
            worker.cancel();
        }
        if let Some(worker) = &self.adopting {
            worker.cancel();
        }
        // Keep the readonly source lease until every copy/adoption fd and
        // worker has drained. Process exit must not strand its mount.
        while self.copying.is_some() || self.adopting.is_some() {
            if self
                .copying
                .as_mut()
                .and_then(TransferWorker::try_next)
                .is_some_and(|event| matches!(event, TransferEvent::Finished(_)))
            {
                if let Some(mut worker) = self.copying.take() {
                    // Cleanup errors never authorize completion; this path is
                    // cancelling an actor whose terminal result is discarded.
                    let _ = worker.join_finished();
                }
            }
            if self
                .adopting
                .as_mut()
                .and_then(PromotionWorker::try_next)
                .is_some()
            {
                self.adopting = None;
            }
            if self.copying.is_some() || self.adopting.is_some() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
fn emit(output: &mut impl Write, value: &serde_json::Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > LIMIT {
        return Err(failure());
    }
    output.write_all(&bytes)?;
    output.write_all(b"\n")?;
    output.flush()
}
fn lines(input: impl io::Read, sender: mpsc::SyncSender<Result<Command, ()>>) {
    let mut reader = io::BufReader::new(input);
    loop {
        let mut bytes = Vec::new();
        loop {
            let buffer = if let Ok(buffer) = reader.fill_buf() {
                buffer
            } else {
                let _ = sender.send(Err(()));
                return;
            };
            if buffer.is_empty() {
                return;
            }
            let count = buffer
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(buffer.len(), |index| index + 1);
            let Some(chunk) = buffer.get(..count) else {
                return;
            };
            if bytes
                .len()
                .checked_add(count)
                .is_none_or(|length| length > LIMIT)
            {
                let _ = sender.send(Err(()));
                return;
            }
            bytes.extend_from_slice(chunk);
            reader.consume(count);
            if bytes.last() == Some(&b'\n') {
                break;
            }
        }
        if sender
            .send(serde_json::from_slice(&bytes).map_err(|_| ()))
            .is_err()
        {
            return;
        }
    }
}
/// Serve a bounded target-user migration session. No identity is minted or activated.
///
/// # Errors
/// Refuses invalid grants/plans, writable or substituted original sources,
/// unverifiable receipts, and unavailable worker operations.
pub fn run(
    args: impl IntoIterator<Item = OsString>,
    input: impl io::Read + Send + 'static,
    output: &mut impl Write,
) -> io::Result<()> {
    serve(args, input, output, Session::review)
}

mod applications;
/// Restore and explicitly adopt selected application state before original-file
/// access becomes writable. EOF drains workers and releases source mount leases.
///
/// # Errors
/// Refuses missing selection/inventory, changed source/data or adoption collisions.
pub fn run_applications(
    args: impl IntoIterator<Item = OsString>,
    input: impl io::Read + Send + 'static,
    output: &mut impl Write,
) -> io::Result<()> {
    serve(args, input, output, applications::review)
}
fn serve(
    args: impl IntoIterator<Item = OsString>,
    input: impl io::Read + Send + 'static,
    output: &mut impl Write,
    review: fn(Grants) -> Result<Session, String>,
) -> io::Result<()> {
    let grants = arguments(args)?;
    let operation = grants.operation.clone();
    let mut session = match review(grants) {
        Ok(session) => session,
        Err(error) => {
            emit(
                output,
                &serde_json::json!({"event":"failed","operation":operation,"error":error.chars().take(1024).collect::<String>()}),
            )?;
            return Err(failure());
        }
    };
    emit(output, &session.pending)?;
    let (sender, receiver) = mpsc::sync_channel(8);
    std::thread::spawn(move || lines(input, sender));
    loop {
        match receiver.recv_timeout(Duration::from_millis(15)) {
            Ok(Ok(command)) => {
                if let Err(error) = session.command(&command.op) {
                    emit(
                        output,
                        &serde_json::json!({"event":"failed","operation":session.operation,"error":error.chars().take(1024).collect::<String>()}),
                    )?;
                } else {
                    emit(output, &session.pending)?;
                }
            }
            Ok(Err(())) => return Err(failure()),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = session.command("cancel");
                return Ok(());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => (),
        }
        match session.poll() {
            Ok(true) => emit(output, &session.pending)?,
            Ok(false) => (),
            Err(error) => {
                session.state = "failed";
                emit(
                    output,
                    &serde_json::json!({"event":"failed","operation":session.operation,"error":error.chars().take(1024).collect::<String>()}),
                )?;
            }
        }
    }
}
mod access;
/// Hold a read-only original-files grant until EOF; never copy or adopt data.
///
/// # Errors
/// Refuses changed plans, source mounts, target grants or malformed commands.
pub fn run_access(
    args: impl IntoIterator<Item = OsString>,
    input: impl io::Read + Send + 'static,
    output: &mut impl Write,
) -> io::Result<()> {
    access::run(
        arguments(args)?,
        migration_handoff::AccessMode::ReadOnly,
        input,
        output,
    )
}
/// Hold an explicitly requested shared writable original-files grant until EOF.
///
/// # Errors
/// Refuses unknown mount safety, changed source/target identities or UID mismatch.
pub fn run_shared_access(
    args: impl IntoIterator<Item = OsString>,
    input: impl io::Read + Send + 'static,
    output: &mut impl Write,
) -> io::Result<()> {
    access::run(
        arguments(args)?,
        migration_handoff::AccessMode::ReadWrite,
        input,
        output,
    )
}
#[cfg(test)]
mod tests;
