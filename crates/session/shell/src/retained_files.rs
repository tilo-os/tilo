//! Device-local original-file custody. The actor outlives onboarding and every
//! resumed lease must match the exact previously acknowledged plan
//! ([`tilo_onboarding_host::retained`]).
use crate::shell::chrome_state::Chrome;
use std::{
    io::{self, BufRead, Read, Write},
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};
pub(crate) use tilo_onboarding_host::retained::*;
#[derive(serde::Deserialize)]
#[serde(tag = "platform", content = "units", deny_unknown_fields)]
enum NativePath {
    Unix(Vec<u8>),
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Available {
    event: String,
    operation: String,
    plan_sha256: String,
    read_only: bool,
    access_mode: String,
    source: NativePath,
    source_display: String,
    copied_bytes: u64,
}
impl Available {
    fn path(self, expected: &Request) -> io::Result<PathBuf> {
        if self.event != "existing-files-available"
            || self.operation != expected.operation
            || self.plan_sha256 != expected.digest
            || self.read_only
            || self.access_mode != "read-write"
            || self.copied_bytes != 0
            || self.source_display.len() > 16384
        {
            return Err(failure());
        }
        let NativePath::Unix(bytes) = self.source;
        if bytes.is_empty() || bytes.len() > 4096 || bytes.contains(&0) {
            return Err(failure());
        }
        let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
        if !path.is_absolute()
            || path == Path::new("/")
            || path.to_str().is_none()
            || path.components().any(|part| {
                matches!(
                    part,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
        {
            return Err(failure());
        }
        Ok(path)
    }
}
struct Actor {
    request: Request,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    receiver: mpsc::Receiver<io::Result<Available>>,
    path: Option<PathBuf>,
    last_check: Instant,
}
impl Actor {
    fn start(
        home: &Path,
        expected: Request,
        wake: crate::session_runtime::Wake,
    ) -> io::Result<Self> {
        if request(home, &expected.operation)?.as_ref() != Some(&expected) {
            return Err(failure());
        }
        let mut pin = crate::onboarding_migration::directory(home)?;
        for segment in [
            ".local",
            "state",
            "tilo",
            "migration",
            expected.operation.as_str(),
        ] {
            pin = crate::onboarding_migration::create(&pin, segment)?;
            crate::onboarding_migration::private(&pin)?;
        }
        let executable = crate::onboarding_portable_home::helper_executable()?;
        let mut child = Command::new(executable)
            .args(["--access-shared-files", "--plans"])
            .arg(plans(home))
            .arg("--operation")
            .arg(&expected.operation)
            .arg("--home")
            .arg(home)
            .arg("--workspace")
            .arg(
                home.join(".local/state/tilo/migration")
                    .join(&expected.operation),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().ok_or_else(failure)?;
        let stdout = child.stdout.take().ok_or_else(failure)?;
        let (sender, receiver) = mpsc::sync_channel(8);
        std::thread::spawn(move || {
            let mut reader = io::BufReader::new(stdout);
            loop {
                let mut bytes = Vec::new();
                let read = (&mut reader).take(BOUND + 1).read_until(b'\n', &mut bytes);
                if !matches!(read,Ok(count)if count>0&&count as u64<=BOUND)
                    || bytes.last() != Some(&b'\n')
                {
                    let _ = sender.send(Err(failure()));
                    wake();
                    return;
                }
                if sender
                    .send(serde_json::from_slice(&bytes).map_err(io::Error::from))
                    .is_err()
                {
                    return;
                }
                wake();
            }
        });
        Ok(Self {
            request: expected,
            child: Some(child),
            stdin: Some(stdin),
            receiver,
            path: None,
            last_check: Instant::now(),
        })
    }
    fn poll(&mut self) -> io::Result<bool> {
        if self
            .child
            .as_mut()
            .ok_or_else(failure)?
            .try_wait()?
            .is_some()
        {
            return Err(failure());
        }
        if self.last_check.elapsed() >= Duration::from_secs(1) {
            let pipe = self.stdin.as_mut().ok_or_else(failure)?;
            pipe.write_all(b"{\"op\":\"revalidate\"}\n")?;
            pipe.flush()?;
            self.last_check = Instant::now();
        }
        let mut changed = false;
        loop {
            match self.receiver.try_recv() {
                Ok(event) => {
                    let path = event?.path(&self.request)?;
                    if self.path.as_ref().is_some_and(|current| current != &path) {
                        return Err(failure());
                    }
                    changed |= self.path.is_none();
                    self.path = Some(path);
                }
                Err(mpsc::TryRecvError::Empty) => return Ok(changed),
                Err(mpsc::TryRecvError::Disconnected) => return Err(failure()),
            }
        }
    }
}
impl Drop for Actor {
    fn drop(&mut self) {
        self.stdin.take();
        if let Some(mut child) = self.child.take() {
            std::thread::spawn(move || {
                let deadline = Instant::now().checked_add(Duration::from_secs(180));
                loop {
                    if !matches!(child.try_wait(), Ok(None)) {
                        return;
                    }
                    if deadline.is_none_or(|end| Instant::now() >= end) {
                        let _ = child.kill();
                        let _ = child.wait();
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            });
        }
    }
}
pub(crate) struct Session {
    home: PathBuf,
    actor: Option<Actor>,
    claims: Vec<Claim>,
    wake: crate::session_runtime::Wake,
}
struct Claim {
    workspace: crate::shell::state::WorkspaceId,
    ids: Vec<String>,
}
impl Session {
    pub(crate) fn open(home: PathBuf, wake: crate::session_runtime::Wake) -> Self {
        let mut session = Self {
            home,
            actor: None,
            claims: Vec::new(),
            wake,
        };
        if let Ok(entries) = std::fs::read_dir(plans(&session.home)) {
            let mut selected = None;
            let mut count = 0usize;
            for entry in entries.take(129) {
                count = count.saturating_add(1);
                let Ok(entry) = entry else { return session };
                let name = entry.file_name();
                let Some(id) = name
                    .to_str()
                    .and_then(|name| name.strip_suffix(".access.json"))
                else {
                    continue;
                };
                if !valid_id(id) {
                    return session;
                }
                if acknowledged(&session.home, id).unwrap_or(false)
                    && let Ok(Some(expected)) = request(&session.home, id)
                {
                    if selected.is_some() {
                        return session;
                    }
                    selected = Some(expected);
                }
            }
            if count <= 128
                && let Some(expected) = selected
            {
                session.actor = Actor::start(
                    &session.home,
                    expected,
                    std::sync::Arc::clone(&session.wake),
                )
                .ok();
            }
        }
        session
    }
    pub(crate) fn start(&mut self, expected: Request) -> io::Result<bool> {
        if let Some(actor) = &self.actor {
            return if actor.request == expected {
                Ok(actor.path.is_some())
            } else {
                Err(failure())
            };
        }
        self.actor = Some(Actor::start(
            &self.home,
            expected,
            std::sync::Arc::clone(&self.wake),
        )?);
        Ok(false)
    }
    pub(crate) fn release(&mut self, operation: &str, chrome: &Chrome) {
        if self
            .actor
            .as_ref()
            .is_some_and(|actor| actor.request.operation == operation)
        {
            self.withdraw(chrome);
            self.actor = None;
        }
    }
    fn withdraw(&mut self, chrome: &Chrome) {
        let mut state = chrome.borrow_mut();
        let mut tree = state.tree_mut();
        let person = self.actor.as_ref().map(|actor| {
            collab_types::caps::Principal::person_from_key(
                collab_types::crypto::Ed25519PublicKey::from_bytes(
                    actor.request.identity_public_key,
                ),
            )
            .id()
            .to_owned()
        });
        for claim in self.claims.drain(..) {
            if let Some(person) = &person {
                for id in claim.ids {
                    tree.narrow_sandbox(claim.workspace, person, &id);
                }
            }
        }
        // The accepted state writer exports this narrowing through the existing
        // pidfd sandbox supervision, which interrupts old mounts/descriptors.
        tree.queue_save();
        drop(tree);
        state.bump();
        drop(state);
        crate::shell::launcher::clear_retained_files();
    }
    pub(crate) fn pump(&mut self, chrome: &Chrome) -> Option<(String, bool)> {
        let actor = self.actor.as_mut()?;
        let operation = actor.request.operation.clone();
        match actor.poll() {
            Err(_) => {
                self.withdraw(chrome);
                self.actor = None;
                return Some((operation, false));
            }
            Ok(changed) => {
                let path = actor.path.clone();
                if let Some(path) = path {
                    self.publish(chrome, &operation, &path);
                }
                if changed {
                    return Some((operation, true));
                }
            }
        }
        None
    }
    fn publish(&mut self, chrome: &Chrome, operation: &str, path: &Path) {
        use crate::shell::state::{Capability, FsMode, GRANT_SESSION};
        let mut state = chrome.borrow_mut();
        let mut tree = state.tree_mut();
        let mut changed = false;
        let workspace = tree.desktop.active;
        let runtime = tree.native_runtime.clone();
        let expected_key = self
            .actor
            .as_ref()
            .map(|actor| actor.request.identity_public_key);
        if !self.claims.iter().any(|claim| claim.workspace == workspace)
            && let Some(ws) = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|ws| ws.id == workspace)
            && tilo_identity_host::broker::trusted_current_device_public(&ws.room().to_string())
                .is_ok_and(|identity| Some(identity.user_root_public_key) == expected_key)
        {
            let ids = ws.grant_app(
                crate::shell::search::providers::applications::SYSTEM_FILE_MANAGER,
                &Capability::Filesystem {
                    path: path.into(),
                    mode: FsMode::ReadWrite,
                    scope: GRANT_SESSION,
                },
            );
            if runtime.remember_files_home_consent(ws, &ids) {
                self.claims.push(Claim { workspace, ids });
                changed = true;
            } else {
                ws.revoke_sandbox_matching(|row| ids.contains(&row.id));
            }
        }
        let mut projections = Vec::new();
        for claim in &self.claims {
            if let Some(ws) = tree
                .desktop
                .workspaces
                .iter()
                .find(|ws| ws.id == claim.workspace)
            {
                let identity = tilo_identity_host::broker::trusted_current_device_public(
                    &ws.room().to_string(),
                )
                .ok();
                let live = identity.is_some_and(|identity| {
                    Some(identity.user_root_public_key) == expected_key
                        && !claim.ids.is_empty()
                        && claim.ids.iter().all(|id| {
                            ws.app_capabilities_at(
                                crate::shell::search::providers::applications::SYSTEM_FILE_MANAGER,
                                tilo_clock::unix_now_ns() / 1_000_000_000,
                            )
                            .iter()
                            .any(|row| {
                                row.id == *id
                                    && tree.native_runtime.files_home_consent_current(
                                        ws,
                                        row,
                                        &identity.principal_id,
                                    )
                            })
                        })
                });
                projections.push((ws.room(), live));
            }
        }
        if changed {
            tree.queue_save();
        }
        drop(tree);
        for (room, live) in projections {
            changed |=
                crate::shell::launcher::set_retained_files(room, live.then_some((operation, path)));
        }
        if changed {
            state.bump();
        }
        drop(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_receipt_requires_exact_operation_digest_and_writable_originals() {
        let expected = Request {
            operation: "operation".into(),
            digest: "a".repeat(64),
            applications: false,
            identity_public_key: [4; 32],
        };
        let event = serde_json::json!({"event":"existing-files-available","operation":"operation","plan_sha256":expected.digest,"read_only":false,"access_mode":"read-write","source":{"platform":"Unix","units":b"/run/retained/home"},"source_display":"/run/retained/home","copied_bytes":0});
        let receipt: Available = serde_json::from_value(event.clone()).unwrap();
        assert_eq!(
            receipt.path(&expected).unwrap(),
            Path::new("/run/retained/home")
        );
        for (field, value) in [
            ("read_only", serde_json::json!(true)),
            ("copied_bytes", serde_json::json!(1)),
            ("operation", serde_json::json!("other")),
            ("plan_sha256", serde_json::json!("b".repeat(64))),
        ] {
            let mut altered = event.clone();
            altered[field] = value;
            let receipt: Available = serde_json::from_value(altered).unwrap();
            assert!(receipt.path(&expected).is_err());
        }
    }
}
