//! Interrupt immutable sandboxes when accepted file authority is withdrawn.
//! No package-name signals, new grants, or automatic application restarts.
use super::{Binding, Published};
use collab_types::caps::{CapOverride, Effect, Scope, Subject, path_covers};
use std::collections::BTreeMap;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

pub(super) struct Sandbox {
    binding: Binding,
    capabilities: Vec<CapOverride>,
    pid: u32,
    start_time: u64,
    handle: OwnedFd,
}

fn process_handle(pid: u32, start_time: u64) -> io::Result<OwnedFd> {
    // SAFETY: pidfd_open takes an integer PID and flags, and returns a new fd.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful syscall returned an exclusively owned descriptor.
    let handle = unsafe { OwnedFd::from_raw_fd(i32::try_from(fd).map_err(io::Error::other)?) };
    if super::super::proc_stat(pid).is_none_or(|(_, current)| current != start_time) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "launch process identity changed",
        ));
    }
    Ok(handle)
}

impl Sandbox {
    pub(super) fn capture(pid: u32, binding: Binding, caps: &[CapOverride]) -> io::Result<Self> {
        let (_, start_time) = super::super::proc_stat(pid)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "launch process is absent"))?;
        Ok(Self {
            handle: process_handle(pid, start_time)?,
            pid,
            start_time,
            binding,
            capabilities: caps
                .iter()
                .filter(|cap| matches!(cap.scope, Scope::FsPath(_)))
                .cloned()
                .collect(),
        })
    }

    pub(super) fn authorized(&self, published: &Published) -> bool {
        self.authorization_status(published).is_ok()
    }

    pub(super) fn authorization_status(&self, published: &Published) -> Result<(), &'static str> {
        if self.capabilities.is_empty() {
            return Ok(());
        }
        let policy = published
            .rooms
            .get(&self.binding.0)
            .ok_or("room_projection_missing")?;
        policy
            .runtime
            .authority_status(&policy.authority, self.binding.0)
            .map_err(crate::shell::native_workspace_owner::store::AuthorityRejection::reason)?;
        let now = Some(tilo_clock::unix_now_ms() / 1000);
        let app = Subject::app(&self.binding.1);
        let grants: Vec<_> = self
            .capabilities
            .iter()
            .filter(|cap| cap.effect == Effect::Grant && cap.subject == app)
            .collect();
        if grants
            .iter()
            .any(|grant| !grant.is_live_at(now) || !policy.capabilities.contains(grant))
        {
            return Err("captured_grant_withdrawn_or_expired");
        }
        // A new narrowing can cover a descendant of a granted directory.
        // Checking only the grant's root would miss that removed authority.
        if policy.capabilities.iter().any(|restriction| {
            if restriction.subject != app
                || restriction.effect != Effect::Restrict
                || !restriction.is_live_at(now)
                || self.capabilities.contains(restriction)
            {
                return false;
            }
            let Scope::FsPath(path) = &restriction.scope else {
                return false;
            };
            grants.iter().any(|grant| {
                let Scope::FsPath(root) = &grant.scope else {
                    return false;
                };
                (path_covers(root, path) || path_covers(path, root))
                    && !restriction.verb.implies(grant.verb)
            })
        }) {
            return Err("new_narrowing_overlap");
        }
        Ok(())
    }

    fn interrupt(&self) {
        // Freeze the supervised root before discovering descendants. The
        // launcher runs bubblewrap with its existing die-with-parent policy;
        // killing its namespace init also closes descriptors in later forks.
        signal(&self.handle, libc::SIGSTOP);
        let mut processes = BTreeMap::new();
        if super::super::proc_stat(self.pid).is_some_and(|(_, start)| start == self.start_time)
            && let Ok(entries) = std::fs::read_dir("/proc")
        {
            for entry in entries.flatten() {
                if let Some(pid) = entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.parse::<u32>().ok())
                    && let Some(stat) = super::super::proc_stat(pid)
                {
                    processes.insert(pid, stat);
                }
            }
            for (&pid, &(_, start)) in &processes {
                let mut current = pid;
                for _ in 0..32 {
                    if current == self.pid {
                        if let Ok(handle) = process_handle(pid, start) {
                            signal(&handle, libc::SIGKILL);
                        }
                        break;
                    }
                    let Some(&(parent, _)) = processes.get(&current) else {
                        break;
                    };
                    if parent == current {
                        break;
                    }
                    current = parent;
                }
            }
        }
        signal(&self.handle, libc::SIGKILL);
    }
}

fn signal(handle: &OwnedFd, signal: i32) {
    // SAFETY: the signal addresses the immutable pidfd, with no siginfo pointer.
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            handle.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            tracing::error!(%error, signal, "sandbox authority interruption failed");
        }
    }
}

pub(super) fn reconcile(published: &mut Published) {
    let revoked: Vec<_> = published
        .sandboxes
        .iter()
        .filter_map(|(&pid, sandbox)| {
            sandbox
                .authorization_status(published)
                .err()
                .map(|reason| (pid, reason))
        })
        .collect();
    for (pid, authority_rejection) in revoked {
        if let Some(sandbox) = published.sandboxes.remove(&pid) {
            sandbox.interrupt();
            tracing::info!(room = %sandbox.binding.0, package_id = %sandbox.binding.1, authority_rejection,
                "application interrupted after accepted filesystem authority withdrawal");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::caps::Verb;
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    struct Process(Child);
    impl Process {
        fn new() -> Self {
            Self(Command::new("sleep").arg("60").spawn().unwrap())
        }
        fn stopped(&mut self) -> bool {
            let deadline = Instant::now().checked_add(Duration::from_secs(3)).unwrap();
            while Instant::now() < deadline {
                if self.0.try_wait().unwrap().is_some() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            false
        }
        fn register(
            &self,
            published: &mut Published,
            room: crate::shell::state::RoomId,
            app: &str,
            capabilities: &[CapOverride],
        ) {
            published.sandboxes.insert(
                self.0.id(),
                Sandbox::capture(
                    self.0.id(),
                    (room, app.into(), "person".into()),
                    capabilities,
                )
                .unwrap(),
            );
        }
    }
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn accepted_revocation_interrupts_only_the_launched_app_and_room() {
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        let room = tree.desktop.workspaces[0].room;
        let grant = CapOverride::grant(
            Subject::app("so.tilo.android"),
            Scope::FsPath("/disposable/document".into()),
            Verb::Write,
        )
        .with_id("document");
        tree.desktop.workspaces[0].capabilities.push(grant.clone());
        tree.append_empty_workspace("Other isolated room");
        let other_room = tree.desktop.workspaces.last_mut().unwrap();
        other_room.capabilities.push(grant.clone());
        let other_room = other_room.room;
        let mut published = Published {
            rooms: super::super::Export::of(&tree).0,
            ..Published::default()
        };
        let mut revoked = Process::new();
        let mut unrelated = Process::new();
        let mut other_instance = Process::new();
        let snapshot = Sandbox::capture(
            revoked.0.id(),
            (room, "so.tilo.android".into(), "person".into()),
            std::slice::from_ref(&grant),
        )
        .unwrap();
        assert!(snapshot.authorized(&published));
        published.sandboxes.insert(revoked.0.id(), snapshot);
        unrelated.register(&mut published, room, "so.tilo.other", &[]);
        other_instance.register(
            &mut published,
            other_room,
            "so.tilo.android",
            std::slice::from_ref(&grant),
        );
        // Adding an unrelated grant must preserve both supervised processes.
        published
            .rooms
            .get_mut(&room)
            .unwrap()
            .capabilities
            .push(CapOverride::grant(
                Subject::app("so.tilo.other"),
                Scope::FsPath("/elsewhere".into()),
                Verb::Read,
            ));
        reconcile(&mut published);
        assert!(revoked.0.try_wait().unwrap().is_none());
        assert!(unrelated.0.try_wait().unwrap().is_none());
        published
            .rooms
            .get_mut(&room)
            .unwrap()
            .capabilities
            .retain(|cap| cap.id != "document");
        reconcile(&mut published);
        assert!(
            revoked.stopped(),
            "old sandbox must lose its open descriptors"
        );
        assert!(
            unrelated.0.try_wait().unwrap().is_none(),
            "another app is not interrupted"
        );
        assert!(
            other_instance.0.try_wait().unwrap().is_none(),
            "same app in another room is not interrupted"
        );
        assert!(
            !Sandbox::capture(
                unrelated.0.id(),
                (room, "so.tilo.android".into(), "person".into()),
                &[grant]
            )
            .unwrap()
            .authorized(&published),
            "a pending stale launch must be refused before its gate opens"
        );
    }

    #[test]
    fn new_descendant_restriction_and_expired_grant_interrupt_running_sandboxes() {
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        let room = tree.desktop.workspaces[0].room;
        let grant = CapOverride::grant(
            Subject::app("so.tilo.android"),
            Scope::FsPath("/disposable".into()),
            Verb::Write,
        );
        tree.desktop.workspaces[0].capabilities.push(grant.clone());
        let mut published = Published {
            rooms: super::super::Export::of(&tree).0,
            ..Published::default()
        };
        let mut restricted = Process::new();
        published.sandboxes.insert(
            restricted.0.id(),
            Sandbox::capture(
                restricted.0.id(),
                (room, "so.tilo.android".into(), "person".into()),
                &[grant],
            )
            .unwrap(),
        );
        published
            .rooms
            .get_mut(&room)
            .unwrap()
            .capabilities
            .push(CapOverride::new(
                Subject::app("so.tilo.android"),
                Scope::FsPath("/disposable/child".into()),
                Effect::Restrict,
                Verb::Read,
            ));
        reconcile(&mut published);
        assert!(restricted.stopped());
        let mut expired = Process::new();
        let expired_grant = CapOverride::grant(
            Subject::app("so.tilo.android"),
            Scope::FsPath("/expired".into()),
            Verb::Read,
        )
        .with_expiry(1);
        published
            .rooms
            .get_mut(&room)
            .unwrap()
            .capabilities
            .push(expired_grant.clone());
        published.sandboxes.insert(
            expired.0.id(),
            Sandbox::capture(
                expired.0.id(),
                (room, "so.tilo.android".into(), "person".into()),
                &[expired_grant],
            )
            .unwrap(),
        );
        reconcile(&mut published);
        assert!(expired.stopped());
    }

    #[test]
    fn process_handle_refuses_a_reused_numeric_pid() {
        let mut process = Process::new();
        let (_, start) = super::super::super::proc_stat(process.0.id()).unwrap();
        assert!(process_handle(process.0.id(), start.saturating_add(1)).is_err());
        assert!(process.0.try_wait().unwrap().is_none());
    }
}
