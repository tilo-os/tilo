//! A transfer a prepared setup declared and the person still has to finish
//! after first login: browser data, portable applications or a portable home.
//!
//! Every kind runs the same course. Setup records the person's choice
//! (`complete` or `deferred`) as a private receipt bound to the exact prepared
//! package; the canonical helper reviews the sealed package, then carries it
//! out; this controller only retains intent, bounded public progress and
//! those receipts, and never substitutes a receipt for the helper's proofs.
//! What differs per kind (the declaration, the helper's events, the package
//! custody checks and the helper's arguments) is the kind's [`Kind`] policy.
use crate::prepared::Pending as Prepared;
use std::{
    fs::OpenOptions,
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use zeroize::{Zeroize, Zeroizing};

/// What a helper's last event settles.
pub enum Settled {
    /// Reviewed; `proceed` when an automatic continuation may go on.
    Ready {
        result: serde_json::Value,
        proceed: bool,
    },
    /// Carried out and verified; the kind's verified projection, if any.
    Complete {
        result: serde_json::Value,
        projection: Option<PathBuf>,
    },
    /// Carried out only in part.
    Incomplete {
        result: serde_json::Value,
    },
    Failed,
}

/// One kind of continuation's policy.
pub trait Kind: Sized + 'static {
    type Declaration: Clone + PartialEq + serde::Serialize + serde::de::DeserializeOwned;
    type Event: serde::de::DeserializeOwned;
    /// The one error every refusal reports.
    const FAILURE: &'static str;
    /// The kind's directory under `.local/state/tilo`, beside the package.
    const ROOT: &'static str;
    /// Largest choice receipt.
    const RECEIPT_BOUND: u64;
    /// Largest helper event line.
    const LINE_BOUND: usize;
    /// The helper's mode flag.
    const HELPER_FLAG: &'static str;
    /// The helper's workspace directory under `.local/state/tilo`.
    const WORKSPACE: &'static str;
    /// Whether the first workspace segments (`.local/state/tilo`) are checked
    /// as the shared namespace rather than as private directories.
    const WORKSPACE_NAMESPACE: bool;
    const OUTPUT_BOUND: usize;
    const TIMEOUT: Duration;
    /// State after a successful review.
    const READY: &'static str;
    /// The operation that carries a reviewed transfer out, and its helper action.
    const PROCEED: &'static str;
    const PROCEED_ACTION: &'static str;
    /// State while it is carried out.
    const PROCEEDING: &'static str;
    /// Whether `continue` on a complete transfer is accepted.
    const CONTINUES_WHEN_COMPLETE: bool;
    /// Whether a failure forgets the password, so a retry asks again.
    const FORGETS_PASSWORD_ON_FAILURE: bool;
    /// Whether a failure clears the last public result.
    const FAILURE_CLEARS_RESULT: bool;
    /// Receipts only move from `deferred` to `complete`, and rewriting the
    /// same receipt is a no-op.
    const MONOTONIC_RECEIPTS: bool;
    /// A `complete` receipt alone is not completion: only a verified
    /// completion proof ([`Kind::verified_completion`]) is.
    const COMPLETION_NEEDS_PROOF: bool;

    fn declared(prepared: &Prepared, identity: &Path) -> io::Result<Option<Self::Declaration>>;
    fn valid(declaration: &Self::Declaration) -> bool;
    fn operation(declaration: &Self::Declaration) -> &str;
    /// The kind's marks in its public status.
    fn mark(
        declaration: &Self::Declaration,
        status: &mut serde_json::Map<String, serde_json::Value>,
    );
    /// Check the saved expectation and the package's custody; the package's
    /// path and the operation naming the helper's workspace.
    fn package(cx: &Context<Self>, directory: &Path) -> io::Result<(PathBuf, String)>;
    fn settle(cx: &Context<Self>, event: Self::Event) -> Settled;
    /// Create the receipt directory when the kind owns it.
    fn prepare_root(_home: &Path) -> io::Result<()> {
        Ok(())
    }
    /// A completion proof already on disk.
    fn verified_completion(_cx: &Context<Self>) -> io::Result<Option<PathBuf>> {
        Ok(None)
    }
}

pub fn failure<K: Kind>() -> io::Error {
    io::Error::other(K::FAILURE)
}

pub fn root<K: Kind>(home: &Path) -> PathBuf {
    home.join(".local/state/tilo").join(K::ROOT)
}

/// The exact prepared transfer a continuation belongs to.
pub struct Context<K: Kind> {
    pub home: PathBuf,
    pub identity: PathBuf,
    pub declaration: K::Declaration,
    pub digest: [u8; 32],
}

#[derive(PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Choice<D> {
    schema: u32,
    setup_digest: [u8; 32],
    declaration: D,
    outcome: String,
}

struct Worker {
    command: bounded_command::AsyncCommand,
    _scratch: tempfile::TempDir,
}

pub struct Continuation<K: Kind> {
    cx: Context<K>,
    state: &'static str,
    result: serde_json::Value,
    worker: Option<Worker>,
    password: Zeroizing<Vec<u8>>,
    automatic: bool,
    projection: Option<PathBuf>,
}

/// A private, owner-only regular file of at most `bound` bytes in a private
/// directory, read without following links.
pub fn private_file<K: Kind>(path: &Path, bound: u64) -> io::Result<Vec<u8>> {
    let parent = crate::migration::directory(path.parent().ok_or_else(failure::<K>)?)?;
    crate::migration::private(&parent)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || metadata.len() > bound
    {
        return Err(failure::<K>());
    }
    let mut bytes = Vec::new();
    file.take(bound.checked_add(1).ok_or_else(failure::<K>)?)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(|_| failure::<K>())? > bound {
        return Err(failure::<K>());
    }
    Ok(bytes)
}

/// Open `path` without following links and check it is a private regular
/// file the session owns, of exactly `length` bytes when given. Large hashing
/// and authentication belong to the helper, not to the compositor's thread.
pub fn private_package<K: Kind>(path: &Path, length: Option<u64>) -> io::Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || length.is_some_and(|length| metadata.len() != length)
    {
        return Err(failure::<K>());
    }
    Ok(())
}

impl<K: Kind> Continuation<K> {
    pub fn open(home: PathBuf, identity: PathBuf, declaration: K::Declaration) -> io::Result<Self> {
        if !K::valid(&declaration) {
            return Err(failure::<K>());
        }
        let prepared = Prepared::open(&home)?;
        if K::declared(&prepared, &identity)?.as_ref() != Some(&declaration) {
            return Err(failure::<K>());
        }
        let digest = prepared.digest();
        let state = match private_file::<K>(&root::<K>(&home).join("choice.json"), K::RECEIPT_BOUND)
        {
            Ok(bytes) => {
                let receipt: Choice<K::Declaration> =
                    serde_json::from_slice(&bytes).map_err(|_| failure::<K>())?;
                if receipt.schema != 1
                    || receipt.setup_digest != digest
                    || receipt.declaration != declaration
                    || !["complete", "deferred"].contains(&receipt.outcome.as_str())
                {
                    return Err(failure::<K>());
                }
                match receipt.outcome.as_str() {
                    "complete" if K::COMPLETION_NEEDS_PROOF => "pending",
                    "complete" => "complete",
                    _ => "deferred",
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => "pending",
            Err(error) => return Err(error),
        };
        let cx = Context {
            home,
            identity,
            declaration,
            digest,
        };
        let projection = if state == "deferred" {
            None
        } else {
            K::verified_completion(&cx)?
        };
        let state = if projection.is_some() {
            "complete"
        } else {
            state
        };
        Ok(Self {
            cx,
            state,
            result: serde_json::Value::Null,
            worker: None,
            password: Zeroizing::new(Vec::new()),
            automatic: false,
            projection,
        })
    }

    /// Retain only the credential just authenticated for this exact prepared
    /// package. Onboarding may continue the already-consented transfer with
    /// it after activation; retaining the password alone starts no process.
    pub fn receive_prepared_password(
        &mut self,
        digest: [u8; 32],
        password: Zeroizing<Vec<u8>>,
    ) -> io::Result<()> {
        self.validate()?;
        if digest != self.cx.digest
            || password.is_empty()
            || password.len() > 4096
            || std::str::from_utf8(&password).is_err()
            || self.busy()
            || !self.pending()
        {
            return Err(failure::<K>());
        }
        self.password = password;
        self.automatic = true;
        Ok(())
    }

    fn validate(&self) -> io::Result<()> {
        let prepared = Prepared::open(&self.cx.home)?;
        if prepared.digest() != self.cx.digest
            || K::declared(&prepared, &self.cx.identity)?.as_ref() != Some(&self.cx.declaration)
        {
            return Err(failure::<K>());
        }
        Ok(())
    }

    pub fn continue_automatic(&mut self) -> bool {
        if self.automatic && self.state == "pending" && !self.password.is_empty() && !self.busy() {
            if self.invoke("review", b"").is_err() {
                self.automatic = false;
            }
            return true;
        }
        false
    }

    pub fn deferred(&self) -> bool {
        self.state == "deferred"
    }

    pub fn pending(&self) -> bool {
        !["complete", "deferred"].contains(&self.state)
    }

    pub fn needs_password(&self) -> bool {
        self.password.is_empty() && !self.busy() && self.pending()
    }

    pub fn busy(&self) -> bool {
        self.worker.is_some()
    }

    pub fn operation(&self) -> &str {
        K::operation(&self.cx.declaration)
    }

    pub fn context(&self) -> &Context<K> {
        &self.cx
    }

    pub fn state(&self) -> &'static str {
        self.state
    }

    /// The projection the helper's verified completion produced.
    pub fn projection(&self) -> Option<&Path> {
        self.projection.as_deref()
    }

    pub fn status(&self) -> serde_json::Value {
        let mut status = serde_json::Map::new();
        for (key, value) in [
            ("state", serde_json::json!(self.state)),
            ("operations", serde_json::json!([self.operation()])),
            ("selected", serde_json::json!(self.operation())),
            ("result", self.result.clone()),
            ("can_review", serde_json::json!(!self.busy())),
            (
                "requires_password",
                serde_json::json!(self.needs_password()),
            ),
            ("complete", serde_json::json!(self.state == "complete")),
            ("deferred", serde_json::json!(self.state == "deferred")),
        ] {
            status.insert(key.to_owned(), value);
        }
        K::mark(&self.cx.declaration, &mut status);
        serde_json::Value::Object(status)
    }

    pub fn next_action(&self) -> &'static str {
        if self.state == K::READY {
            K::PROCEED
        } else {
            "review"
        }
    }

    pub fn invoke(&mut self, op: &str, password: &[u8]) -> io::Result<()> {
        if op == "cancel" && self.busy() {
            self.worker = None;
            self.password.zeroize();
            self.state = "cancelled";
            return Ok(());
        }
        if self.busy() {
            return Err(failure::<K>());
        }
        if op == "resume" && self.deferred() {
            self.validate()?;
            self.state = "pending";
            return Ok(());
        }
        if op == "later" {
            self.acknowledge("deferred")?;
            self.state = "deferred";
            self.password.zeroize();
            return Ok(());
        }
        if op == "continue" && K::CONTINUES_WHEN_COMPLETE && self.state == "complete" {
            self.validate()?;
            return Ok(());
        }
        let action = match op {
            "review" | "retry" => "review",
            proceed if proceed == K::PROCEED && self.state == K::READY => K::PROCEED_ACTION,
            _ => return Err(failure::<K>()),
        };
        if !password.is_empty() {
            if password.len() > 4096 || std::str::from_utf8(password).is_err() {
                return Err(failure::<K>());
            }
            self.password = Zeroizing::new(password.to_vec());
        }
        if self.password.is_empty() {
            return Err(failure::<K>());
        }
        match crate::portable_home::helper_executable()
            .and_then(|helper| self.start_with(action, helper))
        {
            Ok(worker) => {
                self.worker = Some(worker);
                self.state = if action == "review" {
                    "reviewing"
                } else {
                    K::PROCEEDING
                };
                Ok(())
            }
            Err(error) => {
                self.state = "unavailable";
                if K::FORGETS_PASSWORD_ON_FAILURE {
                    self.password.zeroize();
                }
                Err(error)
            }
        }
    }

    fn start_with(&self, action: &str, executable: &Path) -> io::Result<Worker> {
        self.validate()?;
        let directory = root::<K>(&self.cx.home);
        let (package, operation) = K::package(&self.cx, &directory)?;
        let mut pin = crate::migration::directory(&self.cx.home)?;
        for segment in [".local", "state", "tilo", K::WORKSPACE, operation.as_str()] {
            pin = crate::migration::create(&pin, segment)?;
            if K::WORKSPACE_NAMESPACE && [".local", "state", "tilo"].contains(&segment) {
                tilo_files::native::atomic::StorageDirectory::from_file(pin.try_clone()?)?
                    .validate_namespace_control()?;
            } else {
                crate::migration::private(&pin)?;
            }
        }
        let scratch = tempfile::Builder::new()
            .prefix(".setup-review-")
            .tempdir_in(&directory)?;
        let mut command = Command::new(executable);
        command
            .arg(K::HELPER_FLAG)
            .arg("--package")
            .arg(package)
            .arg("--receipt")
            .arg(directory.join("expectation.json"))
            .arg("--setup-package")
            .arg(
                self.cx
                    .home
                    .join(".local/state/tilo/prepared-setup/prepared-setup.sealed"),
            )
            .arg("--setup-quarantine")
            .arg(scratch.path().join("contents"))
            .arg("--home")
            .arg(&self.cx.home)
            .arg("--workspace")
            .arg(
                self.cx
                    .home
                    .join(".local/state/tilo")
                    .join(K::WORKSPACE)
                    .join(&operation),
            )
            .arg("--action")
            .arg(action);
        let command = bounded_command::spawn_prepared(
            command,
            Some(&self.password),
            K::OUTPUT_BOUND,
            K::TIMEOUT,
        )
        .map_err(|_| failure::<K>())?;
        Ok(Worker {
            command,
            _scratch: scratch,
        })
    }

    fn acknowledge(&self, outcome: &str) -> io::Result<()> {
        K::prepare_root(&self.cx.home)?;
        self.validate()?;
        let directory = root::<K>(&self.cx.home);
        let pin = crate::migration::directory(&directory)?;
        crate::migration::private(&pin)?;
        let choice = Choice {
            schema: 1,
            setup_digest: self.cx.digest,
            declaration: self.cx.declaration.clone(),
            outcome: outcome.into(),
        };
        let bytes = serde_json::to_vec(&choice).map_err(|_| failure::<K>())?;
        if u64::try_from(bytes.len()).map_err(|_| failure::<K>())? > K::RECEIPT_BOUND {
            return Err(failure::<K>());
        }
        let destination = directory.join("choice.json");
        let replace = match private_file::<K>(&destination, K::RECEIPT_BOUND) {
            Ok(previous) => {
                let previous: Choice<K::Declaration> =
                    serde_json::from_slice(&previous).map_err(|_| failure::<K>())?;
                if K::MONOTONIC_RECEIPTS && previous == choice {
                    return pin.sync_all();
                }
                if previous.schema != 1
                    || previous.setup_digest != self.cx.digest
                    || previous.declaration != self.cx.declaration
                    || (K::MONOTONIC_RECEIPTS
                        && (previous.outcome != "deferred" || outcome != "complete"))
                {
                    return Err(failure::<K>());
                }
                true
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error),
        };
        let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
        staged.write_all(&bytes)?;
        staged.as_file().sync_all()?;
        self.validate()?;
        if replace {
            staged.persist(&destination).map_err(|_| failure::<K>())?;
        } else {
            staged
                .persist_noclobber(&destination)
                .map_err(|_| failure::<K>())?;
        }
        pin.sync_all()
    }

    pub fn poll(&mut self) -> bool {
        let Some(worker) = &mut self.worker else {
            return false;
        };
        let Some(result) = worker.command.try_wait() else {
            return false;
        };
        self.worker = None;
        let event = result.map_err(|_| failure::<K>()).and_then(|output| {
            if !output.status.success() {
                return Err(failure::<K>());
            }
            let last = output
                .stdout
                .split(|byte| *byte == b'\n')
                .rfind(|line| !line.is_empty())
                .ok_or_else(failure::<K>)?;
            if last.len() > K::LINE_BOUND {
                return Err(failure::<K>());
            }
            serde_json::from_slice::<K::Event>(last).map_err(|_| failure::<K>())
        });
        match event.map_or(Settled::Failed, |event| K::settle(&self.cx, event)) {
            Settled::Ready { result, proceed } => {
                self.state = K::READY;
                self.result = result;
                if self.automatic && proceed && self.invoke(K::PROCEED, b"").is_err() {
                    self.automatic = false;
                }
            }
            Settled::Complete { result, projection } => {
                self.result = result;
                self.state = if self.acknowledge("complete").is_ok() {
                    self.projection = projection;
                    "complete"
                } else {
                    "incomplete"
                };
                self.password.zeroize();
            }
            Settled::Incomplete { result } => {
                self.state = "incomplete";
                self.result = result;
                self.password.zeroize();
            }
            Settled::Failed => {
                self.state = "failed";
                if K::FAILURE_CLEARS_RESULT {
                    self.result = serde_json::Value::Null;
                }
                if K::FORGETS_PASSWORD_ON_FAILURE {
                    self.password.zeroize();
                }
            }
        }
        true
    }
}

#[cfg(test)]
impl<K: Kind> Continuation<K> {
    /// Stop continuing on its own after a review.
    pub fn manual(&mut self) {
        self.automatic = false;
    }

    pub fn holds_password(&self) -> bool {
        !self.password.is_empty()
    }

    /// Run the helper `executable` for `action` as the controller would.
    pub fn run_with(&mut self, action: &str, executable: &Path) -> io::Result<()> {
        self.worker = Some(self.start_with(action, executable)?);
        Ok(())
    }
}
