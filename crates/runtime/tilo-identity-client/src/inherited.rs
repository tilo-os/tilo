use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::{BrokerSigningIdentity, IDENTITY_SERVICE, IdentityMetadata, NotAvailable, legacy};

/// Uses only launcher-supplied descriptors, never identity records or a secret store.
pub fn load() -> Result<Arc<BrokerSigningIdentity>, NotAvailable> {
    if let Some(fd) = runtime_portal::service_fd(IDENTITY_SERVICE) {
        // SAFETY: the launcher retains this inherited service descriptor.
        return connect_service(unsafe { BorrowedFd::borrow_raw(fd) });
    }
    if std::env::var_os(legacy::BROKER_ENV_VAR).is_some() {
        return legacy::connect_from_env();
    }
    Err(NotAvailable::ServiceFdMissing)
}

pub fn load_metadata() -> Result<IdentityMetadata, NotAvailable> {
    let identity = load()?;
    let metadata = identity.metadata();
    Ok(IdentityMetadata {
        profile_photo: metadata.profile_photo().cloned(),
        display_name: metadata.display_name().to_owned(),
        avatar_color: metadata.avatar_color(),
        group_public_key: metadata.group_public_key(),
        threshold: metadata.threshold(),
        total_shares: metadata.total_shares(),
        share_index: metadata.share_index(),
        single_device: metadata.is_single_device(),
    })
}

pub use load_metadata as load_bound_metadata;

type ConnectionId = (std::os::fd::RawFd, libc::dev_t, libc::ino_t);
type IdentityConnections =
    Mutex<std::collections::HashMap<ConnectionId, Weak<BrokerSigningIdentity>>>;

/// Shares the framing lock across consumers of one inherited service.
/// The launcher-owned descriptor remains open and is marked close-on-exec.
pub fn connect_service(fd: BorrowedFd<'_>) -> Result<Arc<BrokerSigningIdentity>, NotAvailable> {
    static CONNECTIONS: OnceLock<IdentityConnections> = OnceLock::new();
    let mut connections = CONNECTIONS
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| {
            NotAvailable::ConnectionFailed(io::Error::other(
                "identity connection registry poisoned",
            ))
        })?;
    let raw = fd.as_raw_fd();
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fd is live and fstat initializes stat on success.
    if unsafe { libc::fstat(raw, stat.as_mut_ptr()) } < 0 {
        return Err(NotAvailable::ConnectionFailed(io::Error::last_os_error()));
    }
    // SAFETY: fstat succeeded.
    let stat = unsafe { stat.assume_init() };
    let id = (raw, stat.st_dev, stat.st_ino);
    connections.retain(|_, connection| connection.strong_count() > 0);
    if let Some(connection) = connections.get(&id).and_then(Weak::upgrade) {
        return Ok(connection);
    }
    // SAFETY: fcntl operates only on the borrowed, live descriptor.
    unsafe {
        let flags = libc::fcntl(raw, libc::F_GETFD);
        if flags < 0 || libc::fcntl(raw, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
            return Err(NotAvailable::ConnectionFailed(io::Error::last_os_error()));
        }
    }
    let owned = fd.try_clone_to_owned()?;
    let identity = Arc::new(BrokerSigningIdentity::from_connected_io(UnixStream::from(
        owned,
    ))?);
    connections.insert(id, Arc::downgrade(&identity));
    Ok(identity)
}
