//! Fixed internal consuming helper process. The source actor stays in its
//! original namespace and retains its UDisks/source witnesses independently.
use launcher::ordinary_child::{
    ChildExposureOwner, ConsumingBootstrapCommand, ConsumingBootstrapEntry,
    OrdinaryChildSupervisor, OwnedOrdinaryChild,
};
use launcher::owned_service::ServiceCancellation;
use std::{
    ffi::OsString,
    io,
    os::fd::{AsRawFd, FromRawFd},
    os::unix::net::UnixStream,
};

use super::namespace;

/// Run the fixed root consuming role. The canonical launcher verifies this
/// executable before any inherited descriptor or namespace is accepted.
/// Arguments route transport only and cannot construct namespace authority.
pub fn run(arguments: impl Iterator<Item = OsString>) -> io::Result<()> {
    launcher::retained_view::RootConsumerChannel::verify_current_helper()?;
    let mut arguments = arguments;
    let value = arguments.next().ok_or(io::ErrorKind::InvalidInput)?;
    if arguments.next().is_some() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let value = value.to_str().ok_or(io::ErrorKind::InvalidInput)?;
    if value.is_empty() || value.len() > 10 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let descriptor: i32 = value.parse().map_err(|_| io::ErrorKind::InvalidInput)?;
    if descriptor < 3 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    // This fixed role accepts only an inherited private Unix stream. Its
    // descriptor number conveys no source, caller or namespace authority.
    let control = unsafe { UnixStream::from_raw_fd(descriptor) };
    control.peer_addr()?;
    let flags = unsafe { libc::fcntl(control.as_raw_fd(), libc::F_GETFD) };
    if flags < 0
        || unsafe { libc::fcntl(control.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    let namespace = namespace::ManagedConsumerNamespace::isolate()?;
    let mut entry = ConsumingBootstrapEntry::authenticate(control)?;
    let cancellation = ServiceCancellation::default();
    // Declaration order matters on errors: first stop/reap the actual outer
    // launcher, then let unresolved staging custody retain this helper alive.
    let mut exposure = ChildExposureOwner::default();
    let mut consumer: Option<OrdinaryChildSupervisor> = None;
    loop {
        namespace.check()?;
        let command = match entry.next_command() {
            Ok(command) => command,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                // A quiet finite controller turn is neither an EOF nor an
                // observed retirement. Revalidate the namespace and parent on
                // the next turn without ending the ordinary consumer lifetime.
                continue;
            }
            Err(error) => return Err(error),
        };
        match command {
            ConsumingBootstrapCommand::Launch(observed) => {
                if consumer.is_some() {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                // The authenticated original parent must first supply the
                // exact observed target routing. The child then independently
                // proves that same Shell over its own private QPE endpoint.
                let target = entry.authenticate_shell()?;
                observed.check_target(&target)?;
                let mut child =
                    OwnedOrdinaryChild::spawn(observed.command(), &target, cancellation.clone())?;
                entry.handoff_ordinary(&mut child)?;
                consumer = Some(child.start_supervision()?);
            }
            ConsumingBootstrapCommand::Exposure(request) => {
                // This closed request comes only from the mutually proven
                // original root parent. The launcher controller independently
                // binds the real init witness and outer child; R cannot detach
                // until those actual kernel subjects are observed stopped.
                request.apply(&mut exposure, &mut entry, consumer.as_mut())?;
                namespace.check()?;
            }
            ConsumingBootstrapCommand::Retire => {
                // Root must already have completed R and independently checked
                // clone absence. X alone cannot authorize mount teardown or
                // replace an init/outer-child stop observation.
                if !exposure.cleanup_observed() {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                if let Some(owner) = &mut consumer {
                    owner.retire_before_exit();
                }
                namespace.check()?;
                return Ok(());
            }
        }
    }
}
