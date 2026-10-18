//! The door an app asks for independent jobs through (`runtime_portal::jobs`).
//!
//! A sandboxed app sees no cgroup and cannot reach the session's app spawner,
//! which owns the tree. Its launcher can: it runs outside the sandbox, in the
//! app's own main leaf, for as long as the app runs. So every app launched in
//! a delegated tree gets a socket (`TILO_SVC_JOBS_FD`) whose other end is a
//! thread of its launcher. Each request there (`job`, a label) is passed on
//! to the spawner naming the launcher itself as the app, and the spawner's
//! answer, a job's open `cgroup.procs`, is handed back. The app can thus put
//! its own processes in jobs of its own app, and nothing else: it names no
//! app, and a `cgroup.procs` descriptor moves only the processes the writer
//! can see, which in the sandbox are its own.

use std::os::fd::{AsFd as _, OwnedFd};

use runtime_portal::jobs;

use crate::services::ResolvedService;

/// In-sandbox descriptor slot of the door.
pub const CHILD_FD: i32 = 35;

/// Open the door for an app this launcher is about to start, when it runs in
/// a delegated app tree; `None` otherwise (then the app's jobs run in its own
/// cgroup, as before).
#[must_use]
pub fn open(no_services: bool) -> Option<ResolvedService> {
    use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};
    if no_services || !crate::app_cgroup::current_is_app_cgroup() {
        return None;
    }
    let (ours, theirs) = match socketpair(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    ) {
        Ok(pair) => pair,
        Err(error) => {
            tracing::warn!(%error, "no job door for the app");
            return None;
        }
    };
    let started = std::thread::Builder::new()
        .name("tilo-job-door".into())
        .spawn(move || serve(&ours));
    if let Err(error) = started {
        tracing::warn!(%error, "no job door for the app");
        return None;
    }
    Some(ResolvedService {
        name: jobs::SERVICE.to_owned(),
        host: theirs,
        child_fd: CHILD_FD,
    })
}

/// Answer the app's requests until it closes its end.
fn serve(door: &OwnedFd) {
    let member = match jobs::own_pidfd() {
        Ok(member) => member,
        Err(error) => {
            tracing::warn!(%error, "the job door cannot name its app");
            return;
        }
    };
    let mut buffer = [0_u8; jobs::MAX_MESSAGE];
    loop {
        let Ok(Some((length, _))) = jobs::receive(door.as_fd(), &mut buffer) else {
            return;
        };
        let fields = jobs::decode(buffer.get(..length).unwrap_or_default());
        let answer = match jobs::parse_request(&fields) {
            Some(label) => jobs::slot_for_member(member.as_fd(), &label)
                .and_then(|slot| slot.ok_or_else(|| std::io::Error::other("no app tree"))),
            None => Err(std::io::Error::other("malformed job request")),
        };
        let sent = match answer {
            Ok(slot) => {
                let fields = jobs::granted_fields(slot.oom_score_adj());
                let fields: Vec<&[u8]> = fields.iter().map(Vec::as_slice).collect();
                jobs::send(door.as_fd(), &fields, &[slot.procs()])
            }
            Err(error) => {
                let fields = jobs::refused_fields(&error.to_string());
                let fields: Vec<&[u8]> = fields.iter().map(Vec::as_slice).collect();
                jobs::send(door.as_fd(), &fields, &[])
            }
        };
        if sent.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_door_has_a_reserved_slot_and_the_name_the_app_reads() {
        assert!(crate::services::RESERVED_CHILD_FDS.contains(&super::CHILD_FD));
        assert_eq!(
            crate::sandbox::svc_env_var(runtime_portal::jobs::SERVICE),
            "TILO_SVC_JOBS_FD"
        );
    }
}
