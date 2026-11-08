//! The file access list the running desk hands the Files daemon.
//! The Files mount decides every open and read against a signed capability list, and the session is
//! what keeps that list current: while it runs it publishes the desk's file capabilities on every
//! change, so taking a folder away stops the app that holds it at its next open — without closing
//! the app, restarting it, or asking it to notice anything.
//! The list is the workspace's own permissions in the mount's terms: the grants apps hold through
//! portals, and what each member's role lets them reach in the workspace's files (a viewer reads,
//! an editor writes). Both follow the desk's replica revision, so a role change or a removed member
//! reaches the daemon in the same pass, never on a timer.
//! The daemon signs the list it reads into the ACL every mount enforces
//! (`WorkspaceAuthority::apply`), so writing the file is the session's whole part wherever the
//! daemon holds the governor key. A desktop that keeps that key out of reach — a host install,
//! where only the protected signing session may use it — names a signer in
//! `TILO_FS_AUTHORITY_PUBLISHER` and the session runs it after each write. Publishing is coalesced:
//! a burst of desk changes publishes the newest list, and an unchanged list is neither rewritten
//! nor published again.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use collab_types::Role;
use collab_types::caps::{
    Access, Actor, CapOverride, FsAuthority, Scope, Subject, WorkspaceMember,
};
use collab_types::crypto::Ed25519PublicKey;
use tilo_desktop_model::DesktopTree;
use tilo_lock::MutexRecover as _;

/// The program that signs a list this desktop cannot sign itself. Absent where the daemon holds the
/// governor key and signs the list it reads.
const PUBLISHER_ENV: &str = "TILO_FS_AUTHORITY_PUBLISHER";
/// How long one list is retried while the daemon is not serving yet.
const RETRY_BUDGET: Duration = Duration::from_secs(60);
const FIRST_RETRY: Duration = Duration::from_millis(100);
const LAST_RETRY: Duration = Duration::from_secs(5);

/// The desk's file capabilities and each workspace's readers, as the Files daemon enforces them for
/// this device.
/// Every workspace exports together (one mount serves them all), so two workspaces granting the
/// same path to different people union. A grant to this person is exported twice: once by the
/// person's key and once by this device's, which is the identity the mount itself authenticates as,
/// so translation can never widen a grant and a revoked one takes both away.
#[must_use]
pub(crate) fn list(
    tree: &DesktopTree,
    person: &str,
    device: Option<[u8; 32]>,
    now: u64,
) -> FsAuthority {
    let actor = person_key(person).map(Actor::signed);
    let mut capabilities = Vec::new();
    let mut workspace_members = std::collections::BTreeMap::new();
    for workspace in &tree.workspaces {
        for capability in workspace.sandbox.iter().filter(|capability| {
            capability.persists_across_restart() && matches!(capability.scope, Scope::FsPath(_))
        }) {
            push_with_device(&mut capabilities, capability, actor.as_ref(), device);
        }
        // What the workspace's roles let each member reach in its files. The roles are the
        // workspace's one statement of who may do what, so a role change rewrites this list and the
        // daemon enforces the new answer at the next open; a member removed from the workspace
        // holds nothing here, and their device stops at once without being told.
        for (member, role) in workspace.roster() {
            let capability = CapOverride::grant(
                Subject::person(member.clone()),
                Scope::FsPath(WORKSPACE_FILES.into()),
                role_files_access(role),
            )
            .with_id(role_capability_id(&workspace.id().to_string(), &member));
            push_with_device(&mut capabilities, &capability, actor.as_ref(), device);
        }
        // Signed membership is a separate projection from Files access: who the workspace's policy
        // lets read it, per relay shard.
        workspace_members.insert(
            tilo_network_actor::workspace_topic_id(workspace.id()).to_string(),
            workspace
                .workspace_readers_at(now)
                .iter()
                .filter_map(|(reader, until)| member(reader, *until))
                .collect::<Vec<_>>(),
        );
    }
    FsAuthority {
        capabilities,
        workspace_members,
    }
}

fn person_key(person: &str) -> Option<Ed25519PublicKey> {
    let bytes: [u8; 32] = hex::decode(person).ok()?.try_into().ok()?;
    Some(Ed25519PublicKey::from_bytes(bytes))
}

/// Where a workspace's files appear in the mount (see `fs::WORKSPACE_FILES_PATH` ): the path a
/// role's access ranges over.
const WORKSPACE_FILES: &str = "/var/workspace";

/// What a member's role lets them do to the workspace's files: a viewer and a commenter read them,
/// an editor and everything above changes them.
const fn role_files_access(role: Role) -> Access {
    match role {
        Role::Viewer | Role::Commenter => Access::Read,
        Role::Editor | Role::Moderator | Role::Owner => Access::Write,
    }
}

/// The id of the capability a workspace's role gives a member, so the same member's grant is
/// recognisable across revisions and in the daemon's log.
fn role_capability_id(workspace: &str, member: &str) -> String {
    format!("role:{workspace}:{member}")
}

/// Publish `capability` as it stands, and again bound to this device's key when it is the key of
/// the person sitting here: the mount acts as the device, so the person's grants must reach it
/// without widening anything.
fn push_with_device(
    capabilities: &mut Vec<CapOverride>,
    capability: &CapOverride,
    actor: Option<&Actor>,
    device: Option<[u8; 32]>,
) {
    capabilities.push(capability.clone());
    let (Some(device), Some(actor)) = (device, actor) else {
        return;
    };
    if !capability.subject.matches_actor(actor) {
        return;
    }
    let mut bound = capability.clone();
    bound.subject = Subject::User(Ed25519PublicKey::from_bytes(device));
    capabilities.push(bound);
}

fn member(person: &str, until: Option<u64>) -> Option<WorkspaceMember> {
    Some(WorkspaceMember {
        user: person_key(person)?,
        until,
    })
}

/// This device's key, which is the identity the Files mount acts as.
pub(crate) fn device_key() -> Option<[u8; 32]> {
    static KEY: OnceLock<Option<[u8; 32]>> = OnceLock::new();
    *KEY.get_or_init(|| {
        let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
        tilo_user_identity::persist::read_device_key(&paths.device_privkey)
            .ok()
            .map(|key| key.verifying_key().to_bytes())
    })
}

/// Writes the access list where the daemon's `--acl` reads it, then has it signed and published,
/// newest list first.
/// One publisher serves a session: the daemon reads one file and holds one authority, so two
/// publishers would race revisions against each other.
pub(crate) struct Publisher {
    inner: Arc<Inner>,
}

struct Inner {
    /// The signer of a list this desktop cannot sign itself, when it names one.
    signer: Option<Signer>,
    acl: PathBuf,
    state: Mutex<State>,
    /// Signalled when the worker goes idle, so a test (or a session ending) can wait for what is in
    /// flight.
    idle: Condvar,
}

/// A program that signs a written list and hands it to the daemon's authority, and the socket
/// directory it reaches that daemon on.
struct Signer {
    program: PathBuf,
    socket_dir: PathBuf,
}

#[derive(Default)]
struct State {
    /// The bytes in force: written and accepted by the daemon.
    in_force: Option<Vec<u8>>,
    /// The newest list not yet in force.
    pending: Option<Vec<u8>>,
    running: bool,
    last_error: Option<String>,
}

impl Publisher {
    /// The publisher of this person's session, where the Files mount of this device serves; `None`
    /// when this user session has no Files workspace.
    pub(crate) fn for_session() -> Option<Self> {
        let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        let home = PathBuf::from(var("HOME")?);
        let config = var("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from);
        let signer = var(PUBLISHER_ENV).and_then(|program| {
            let runtime = PathBuf::from(var("XDG_RUNTIME_DIR")?);
            let socket_dir = var("TILO_AUTHORITY_SOCKET_DIR")
                .map_or_else(|| runtime.join("tilo/files-workspace"), PathBuf::from);
            Some(Signer {
                program: PathBuf::from(program),
                socket_dir,
            })
        });
        Some(Self::at(signer, config.join("tilo/fs-capabilities.json")))
    }

    fn at(signer: Option<Signer>, acl: PathBuf) -> Self {
        Self {
            inner: Arc::new(Inner {
                signer,
                acl,
                state: Mutex::new(State::default()),
                idle: Condvar::new(),
            }),
        }
    }

    /// Hand a list to the daemon. `false` when it is already in force or already on its way: most
    /// desk changes touch nothing Files enforces.
    pub(crate) fn publish(&self, authority: &FsAuthority) -> bool {
        let Ok(bytes) = serde_json::to_vec_pretty(authority) else {
            return false;
        };
        {
            let mut state = self.inner.state.lock_recover();
            if state.in_force.as_ref() == Some(&bytes) || state.pending.as_ref() == Some(&bytes) {
                return false;
            }
            state.pending = Some(bytes);
            if state.running {
                return true;
            }
            state.running = true;
        }
        let inner = Arc::clone(&self.inner);
        let spawn = std::thread::Builder::new()
            .name("tilo-fs-authority".into())
            .spawn(move || work(&inner));
        if spawn.is_err() {
            self.inner.state.lock_recover().running = false;
            return false;
        }
        true
    }

    /// Whether the newest list handed over did not reach the daemon, and no worker is still trying
    /// it: nothing of what the desk decided then is enforced until one does.
    pub(crate) fn unpublished(&self) -> bool {
        let state = self.inner.state.lock_recover();
        state.last_error.is_some() && !state.running && state.pending.is_none()
    }

    /// Wait until every list handed over so far is in force or has failed,
    /// returning the last error when one did.
    ///
    /// # Errors
    /// The publisher could not reach the daemon for the newest list.
    #[cfg(test)]
    pub(crate) fn wait_until_idle(&self, timeout: Duration) -> Result<(), String> {
        let started = Instant::now();
        let mut state = self.inner.state.lock_recover();
        while state.running {
            let left = timeout.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Err("the access list is still being published".into());
            }
            let (next, _) = self
                .inner
                .idle
                .wait_timeout(state, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
        }
        match &state.last_error {
            Some(error) if state.in_force.is_none() => Err(error.clone()),
            _ => Ok(()),
        }
    }
}

/// Publish one list after another until nothing is pending, then go idle.
fn work(inner: &Inner) {
    loop {
        let Some(bytes) = take_pending(inner) else {
            return;
        };
        let started = Instant::now();
        let mut backoff = FIRST_RETRY;
        loop {
            match write_and_publish(inner, &bytes) {
                Ok(()) => {
                    let mut state = inner.state.lock_recover();
                    state.in_force = Some(bytes);
                    state.last_error = None;
                    drop(state);
                    break;
                }
                Err(error) => {
                    note_failure(inner, &error);
                    // A newer list makes this one pointless; wait for it here instead of publishing
                    // a list already replaced.
                    if started.elapsed() >= RETRY_BUDGET || newer_list(inner, backoff) {
                        break;
                    }
                    backoff = backoff.saturating_mul(2).min(LAST_RETRY);
                }
            }
        }
    }
}

/// Remember why the newest list is not in force, saying it once.
fn note_failure(inner: &Inner, error: &str) {
    let mut state = inner.state.lock_recover();
    if state.last_error.as_deref() != Some(error) {
        tracing::warn!(%error, "the file access list is not published yet");
    }
    state.last_error = Some(error.to_owned());
}

/// Wait `backoff` for a newer list, or for the worker to be asked to stop: `true` when this list
/// should not be retried any more.
fn newer_list(inner: &Inner, backoff: Duration) -> bool {
    let waited = inner.idle.wait_timeout(inner.state.lock_recover(), backoff);
    let state = match waited {
        Ok((state, _)) => state,
        Err(poisoned) => poisoned.into_inner().0,
    };
    let replaced = state.pending.is_some() || !state.running;
    drop(state);
    replaced
}

/// The newest list handed over, or `None` after going idle. Taking the list and going idle happen
/// under one lock, so a list handed over as this worker stops is either taken here or starts
/// another worker: never lost.
fn take_pending(inner: &Inner) -> Option<Vec<u8>> {
    let mut state = inner.state.lock_recover();
    if let Some(bytes) = state.pending.take() {
        return Some(bytes);
    }
    state.running = false;
    drop(state);
    inner.idle.notify_all();
    None
}

/// Write the list the daemon's `--acl` reads, then have it signed when this desktop names a signer.
/// The write is atomic: the daemon mounts this file and must never read a half-written list and
/// conclude the person has no permissions.
fn write_and_publish(inner: &Inner, bytes: &[u8]) -> Result<(), String> {
    write_atomically(&inner.acl, bytes)
        .map_err(|error| format!("write the access list {}: {error}", inner.acl.display()))?;
    let Some(signer) = &inner.signer else {
        return Ok(());
    };
    let output = std::process::Command::new(&signer.program)
        .arg("publish-authority")
        .arg("--acl")
        .arg(&inner.acl)
        .env("TILO_AUTHORITY_SOCKET_DIR", &signer.socket_dir)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("run {}: {error}", signer.program.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    let said = said.trim();
    Err(format!(
        "{} publish-authority failed ({}): {}",
        signer.program.display(),
        output.status,
        if said.is_empty() {
            "no reason given"
        } else {
            said
        }
    ))
}

fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
    if let Some(parent) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        if let Ok(metadata) = std::fs::metadata(parent)
            && metadata.permissions().mode() & 0o077 != 0
        {
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let temporary = path.with_extension(format!("new-{}", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion"
)]
mod tests {
    use super::*;
    use collab_types::caps::{Access, CapOverride, Effect, Expiry, Principal};
    use std::os::unix::fs::PermissionsExt as _;
    use tilo_desktop_model::Workspace;

    const PERSON: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const FOLDER: &str = "/var/workspace/brief";

    fn tree_with(grant: CapOverride) -> DesktopTree {
        let mut workspace = Workspace::create("Home");
        workspace.sandbox.push(grant);
        DesktopTree {
            workspaces: vec![workspace],
            ..DesktopTree::default()
        }
    }

    fn app_folder_grant() -> CapOverride {
        CapOverride::grant(
            Subject::app("so.tilo.notes"),
            Scope::FsPath(FOLDER.into()),
            Access::Read,
        )
    }

    /// Does the list the session published let `actor` read `path` ? This is the decision every
    /// Files open asks ([`collab_types::decide_capability`]), so an answer here is the daemon's
    /// answer.
    fn may_read(list: &FsAuthority, actor: &Actor, path: &str) -> bool {
        collab_types::decide_capability(
            actor,
            &collab_types::ResourceRef::FsPath(path.into()),
            Access::Read,
            &list.capabilities,
            Some(0),
        ) == collab_types::CapDecision::Allow
    }

    /// [`may_read`] for a write.
    fn may_write(list: &FsAuthority, actor: &Actor, path: &str) -> bool {
        collab_types::decide_capability(
            actor,
            &collab_types::ResourceRef::FsPath(path.into()),
            Access::Write,
            &list.capabilities,
            Some(0),
        ) == collab_types::CapDecision::Allow
    }

    #[test]
    fn a_members_role_decides_whether_they_write_the_workspace_files() {
        const MEMBER: &str = "2222222222222222222222222222222222222222222222222222222222222222";
        const NOTE: &str = "/var/workspace/brief/plan.md";
        let member = Actor::signed(person_key(MEMBER).unwrap());
        let mut tree = tree_with(app_folder_grant());
        tree.workspaces[0].admit(MEMBER, Role::Viewer);
        let read_only = list(&tree, PERSON, None, 0);
        assert!(
            may_read(&read_only, &member, NOTE),
            "a member reads the workspace's files"
        );
        assert!(
            !may_write(&read_only, &member, NOTE),
            "a member with a read-only role cannot write them"
        );

        tree.workspaces[0].admit(MEMBER, Role::Editor);
        let editing = list(&tree, PERSON, None, 0);
        assert!(may_write(&editing, &member, NOTE), "an editor writes them");
        assert_ne!(
            editing.capabilities, read_only.capabilities,
            "a role change is a new list, so the daemon is handed it"
        );

        tree.workspaces[0].policy.members.remove(MEMBER);
        let removed = list(&tree, PERSON, None, 0);
        assert!(
            !may_read(&removed, &member, NOTE),
            "a removed member reads nothing"
        );
        assert!(!may_write(&removed, &member, NOTE));
    }

    #[test]
    fn a_revoked_folder_is_refused_at_the_next_open_and_the_app_keeps_running() {
        let device = Some([9u8; 32]);
        let person = person_key(PERSON).unwrap();
        let grant = CapOverride::grant(
            Subject::Principal(Principal::person_from_key(person)),
            Scope::FsPath(FOLDER.into()),
            Access::Read,
        )
        .with_id("brief");
        let keeps = CapOverride::grant(
            Subject::Principal(Principal::person_from_key(person)),
            Scope::FsPath("/var/workspace/notes".into()),
            Access::Read,
        )
        .with_id("notes");
        let mut tree = tree_with(grant);
        tree.workspaces[0].sandbox.push(keeps);
        let actor = Actor::signed(person);
        let published = list(&tree, PERSON, device, 0);
        assert!(may_read(&published, &actor, &format!("{FOLDER}/plan.md")));
        // The device key the mount authenticates as holds the same grant.
        assert!(may_read(
            &published,
            &Actor::signed(Ed25519PublicKey::from_bytes([9u8; 32])),
            &format!("{FOLDER}/plan.md")
        ));

        // The person takes the folder back while an app has it open.
        assert!(tree.workspaces[0].revoke_sandbox("brief"));
        let now = list(&tree, PERSON, device, 0);
        assert!(
            !may_read(&now, &actor, &format!("{FOLDER}/plan.md")),
            "the next open is refused"
        );
        assert!(
            may_read(&now, &actor, "/var/workspace/notes/today.md"),
            "everything else the app reaches still works: it is not restarted"
        );
    }

    #[test]
    fn only_durable_file_capabilities_are_published() {
        let transient = CapOverride::grant(
            Subject::app("so.tilo.notes"),
            Scope::FsPath("/var/workspace/scratch".into()),
            Access::Read,
        )
        .expiring(Expiry::EndOfSession);
        let device_only = CapOverride::grant(
            Subject::app("so.tilo.notes"),
            Scope::Device("camera".into()),
            Access::Read,
        );
        let mut tree = tree_with(app_folder_grant());
        tree.workspaces[0].sandbox.push(transient);
        tree.workspaces[0].sandbox.push(device_only);
        let published = list(&tree, PERSON, None, 0);
        assert_eq!(published.capabilities.len(), 1);
        assert!(matches!(published.capabilities[0].scope, Scope::FsPath(_)));
        assert_eq!(published.capabilities[0].effect, Effect::Grant);
    }

    #[test]
    fn each_workspace_publishes_who_may_read_it() {
        let mut tree = tree_with(app_folder_grant());
        tree.workspaces[0].admit(PERSON, collab_types::Role::Editor);
        let published = list(&tree, PERSON, None, 0);
        let readers = published
            .workspace_members
            .get(&tilo_network_actor::workspace_topic_id(tree.workspaces[0].id()).to_string())
            .expect("the workspace's topic");
        assert!(
            readers
                .iter()
                .any(|reader| reader.user == person_key(PERSON).unwrap()),
            "the reader the workspace admits is published"
        );
    }

    /// A stand-in for the Files daemon: records each publication and copies the list it was handed,
    /// so a test reads exactly what would be signed.
    fn stand_in(dir: &Path) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;
        let seen = dir.join("seen");
        let script = dir.join("fs");
        std::fs::create_dir_all(&seen).unwrap();
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 set -e\n\
                 test \"$1\" = publish-authority\n\
                 test \"$2\" = --acl\n\
                 test -n \"$TILO_AUTHORITY_SOCKET_DIR\"\n\
                 n=$(cat {seen}/count 2>/dev/null || echo 0)\n\
                 n=$((n+1))\n\
                 echo \"$n\" > {seen}/count\n\
                 cp \"$3\" {seen}/$n.json\n",
                seen = seen.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (script, seen)
    }

    fn published_count(seen: &Path) -> u64 {
        std::fs::read_to_string(seen.join("count"))
            .ok()
            .and_then(|count| count.trim().parse().ok())
            .unwrap_or(0)
    }

    #[test]
    fn a_changed_list_is_published_once_and_an_unchanged_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let (script, seen) = stand_in(dir.path());
        let publisher = Publisher::at(
            Some(Signer {
                program: script,
                socket_dir: dir.path().join("sockets"),
            }),
            dir.path().join("config/fs-capabilities.json"),
        );
        let tree = tree_with(app_folder_grant());
        let first = list(&tree, PERSON, None, 0);
        assert!(publisher.publish(&first));
        publisher
            .wait_until_idle(tilo_test_env::hang_bound())
            .expect("published");
        assert_eq!(published_count(&seen), 1);
        let written: FsAuthority =
            serde_json::from_slice(&std::fs::read(seen.join("1.json")).unwrap()).unwrap();
        assert_eq!(written.capabilities, first.capabilities);
        assert!(
            !publisher.publish(&first),
            "an unchanged list is not published again"
        );
        publisher
            .wait_until_idle(tilo_test_env::hang_bound())
            .expect("idle");
        assert_eq!(published_count(&seen), 1);
    }

    #[test]
    fn a_burst_of_changes_publishes_the_newest_list() {
        let dir = tempfile::tempdir().unwrap();
        let (script, seen) = stand_in(dir.path());
        let publisher = Publisher::at(
            Some(Signer {
                program: script,
                socket_dir: dir.path().join("sockets"),
            }),
            dir.path().join("config/fs-capabilities.json"),
        );
        let mut tree = tree_with(app_folder_grant());
        publisher.publish(&list(&tree, PERSON, None, 0));
        let id = tree.workspaces[0].sandbox[0].id.clone();
        tree.workspaces[0].revoke_sandbox(&id);
        publisher.publish(&list(&tree, PERSON, None, 0));
        publisher
            .wait_until_idle(tilo_test_env::hang_bound())
            .expect("published");
        let last = published_count(&seen);
        let newest: FsAuthority =
            serde_json::from_slice(&std::fs::read(seen.join(format!("{last}.json"))).unwrap())
                .unwrap();
        assert!(newest.capabilities.is_empty(), "the newest list won");
    }

    #[test]
    fn a_list_given_up_on_is_handed_over_again() {
        let dir = tempfile::tempdir().unwrap();
        let (script, _seen) = stand_in(dir.path());
        let publisher = Publisher::at(
            Some(Signer {
                program: script,
                socket_dir: dir.path().join("sockets"),
            }),
            dir.path().join("config/fs-capabilities.json"),
        );
        // A worker that gave up: the list is not in force and nothing is trying it,
        // so the next change of the desk hands it over again.
        publisher.inner.state.lock_recover().last_error = Some("the daemon refused it".into());
        assert!(
            publisher.unpublished(),
            "nothing is trying the list the daemon refused"
        );
        // A list being tried is the worker's, not the desk's to hand over again.
        publisher.inner.state.lock_recover().running = true;
        assert!(
            !publisher.unpublished(),
            "a worker is still trying the list"
        );
    }

    #[test]
    fn a_daemon_that_is_not_serving_yet_is_tried_until_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let (script, seen) = stand_in(dir.path());
        let gate = dir.path().join("gate");
        // The daemon refuses every list until the gate exists, and says it was asked.
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
             if [ ! -e {gate} ]; then echo refused >> {seen}/refused; exit 1; fi\n\
             n=$(cat {seen}/count 2>/dev/null || echo 0)\n\
             n=$((n+1))\n\
             echo \"$n\" > {seen}/count\n",
                gate = gate.display(),
                seen = seen.display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let publisher = Publisher::at(
            Some(Signer {
                program: script,
                socket_dir: dir.path().join("sockets"),
            }),
            dir.path().join("config/fs-capabilities.json"),
        );
        publisher.publish(&list(&tree_with(app_folder_grant()), PERSON, None, 0));
        // The first attempt is refused before the gate exists: the daemon's own word for that is
        // what this waits on, so nothing here races the clock.
        tilo_test_env::wait_until("the daemon refusing the first attempt", || {
            seen.join("refused").is_file()
        });
        // Its refusal reaches the publisher a moment after the daemon said it, and the worker goes
        // on trying the list: the desk leaves it to the worker.
        assert!(
            !publisher.unpublished(),
            "a worker is still trying the list"
        );
        std::fs::write(&gate, b"").unwrap();
        publisher
            .wait_until_idle(tilo_test_env::hang_bound())
            .expect("published once the daemon served");
        assert_eq!(published_count(&seen), 1);
        assert!(
            !publisher.unpublished(),
            "the list the daemon took is in force"
        );
        assert_eq!(
            std::fs::metadata(dir.path().join("config/fs-capabilities.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "only this person reads the list"
        );
    }
    #[test]
    fn a_daemon_that_holds_the_governor_key_needs_no_publisher() {
        let dir = tempfile::tempdir().unwrap();
        let acl = dir.path().join("config/fs-capabilities.json");
        let publisher = Publisher::at(None, acl.clone());
        let newest = list(&tree_with(app_folder_grant()), PERSON, None, 0);
        assert!(publisher.publish(&newest));
        publisher
            .wait_until_idle(tilo_test_env::hang_bound())
            .expect("written");
        let written: FsAuthority = serde_json::from_slice(&std::fs::read(&acl).unwrap()).unwrap();
        assert_eq!(written.capabilities, newest.capabilities);
        assert!(
            !publisher.unpublished(),
            "the written list is the one the daemon signs itself"
        );
    }
}
