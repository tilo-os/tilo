//! The workspace a capture's instance joins: a capture of a development build the person on this
//! device started runs its instance in its own process, as a window without a session does, but
//! over the workspace's one store, so what it calls (a session's playback, another app's actions)
//! goes through the device's dispatch as a launched window's calls do. The device's daemon makes
//! the instance's collaboration, identity and content connections, attested as that instance, and
//! the launch hands them to the process with its release ([`crate::launch_hold`]).

/// The service the instance's collaboration connection is handed as.
pub const COLLABORATION: &str = "tilolang-joined-collaboration";
/// The service the instance's identity connection is handed as.
pub const IDENTITY: &str = "tilolang-joined-identity";
pub const CONTENT: &str = "tilolang-joined-content";
pub const COLLABORATION_CHILD_FD: i32 = 44;
pub const IDENTITY_CHILD_FD: i32 = 45;
pub const CONTENT_CHILD_FD: i32 = 46;
/// The variable the launcher names the instance, its workspace and the person at it with:
/// `<instance> <workspace> <person>`.
pub const ENV: &str = "TILO_TILOLANG_JOINED";

/// The instance a capture's process joins its workspace as, and the connections it joins over.
#[derive(Debug)]
pub struct Joined {
    pub instance: u64,
    pub workspace: String,
    pub person: String,
    pub collaboration: crate::packet::Descriptor,
    pub identity: crate::packet::Descriptor,
    pub content: crate::packet::Descriptor,
}

/// What [`ENV`] says: the instance, its workspace and the person at it.
#[must_use]
pub fn named(value: &str) -> Option<(u64, String, String)> {
    let mut parts = value.splitn(3, ' ');
    let instance = parts.next()?.parse().ok()?;
    let workspace = parts.next().filter(|workspace| !workspace.is_empty())?;
    let person = parts
        .next()
        .map(str::trim)
        .filter(|person| !person.is_empty())?;
    Some((instance, workspace.to_owned(), person.to_owned()))
}

/// The joined instance this launch handed the process, taken once: `None` when it handed none, as
/// a capture of a release, or a program run by hand, has.
#[must_use]
pub fn take() -> Option<Joined> {
    #[cfg(target_os = "linux")]
    {
        static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let (instance, workspace, person) = named(&std::env::var(ENV).ok()?)?;
        crate::service_fd(COLLABORATION)?;
        crate::service_fd(IDENTITY)?;
        if TAKEN.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return None;
        }
        // SAFETY: the launcher handed these descriptors to the process for the instance alone,
        // and the flag above lets them be owned exactly once.
        let collaboration = unsafe { crate::take_service_fd(COLLABORATION) }?;
        // SAFETY: as above.
        let identity = unsafe { crate::take_service_fd(IDENTITY) }?;
        // SAFETY: as above.
        let content = unsafe { crate::take_service_fd(CONTENT) }?;
        Some(Joined {
            instance,
            workspace,
            person,
            collaboration,
            identity,
            content,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_variable_names_the_instance_and_its_person() {
        assert_eq!(
            super::named("7 ws-1 m-ada"),
            Some((7, "ws-1".to_owned(), "m-ada".to_owned()))
        );
        assert_eq!(super::named("7 ws-1 "), None);
        assert_eq!(super::named("7 m-ada"), None);
        assert_eq!(super::named("seven ws-1 m-ada"), None);
        assert_eq!(super::named("7"), None);
    }
}
