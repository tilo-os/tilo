//! Ownership of an inherited service connection which can refuse a request.

#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// An owned session lease. Reader clones cannot prolong the guest after its
/// owner replaces or drops this lease.
#[cfg(unix)]
pub struct SessionLease(UnixStream);

#[cfg(unix)]
impl From<UnixStream> for SessionLease {
    fn from(stream: UnixStream) -> Self {
        Self(stream)
    }
}

#[cfg(unix)]
impl std::ops::Deref for SessionLease {
    type Target = UnixStream;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(unix)]
impl Drop for SessionLease {
    fn drop(&mut self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

/// One launcher-owned connection. A successful session consumes it; a refused
/// request can return it for another request without reclaiming its raw fd.
pub struct InheritedServiceSocket {
    #[cfg(unix)]
    name: &'static str,
    claimed: AtomicBool,
    #[cfg(unix)]
    retry: Mutex<Option<UnixStream>>,
}

/// A serialized inherited control connection whose sessions use separate leases.
/// The raw inherited descriptor is claimed once and never reacquired.
pub struct InheritedControlSocket {
    socket: InheritedServiceSocket,
    #[cfg(unix)]
    exchange: Mutex<()>,
}

impl InheritedControlSocket {
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            socket: InheritedServiceSocket::new(name),
            #[cfg(unix)]
            exchange: Mutex::new(()),
        }
    }

    #[must_use]
    pub fn was_taken(&self) -> bool {
        self.socket.was_taken()
    }

    /// Exchange one serialized request, retaining only the control connection.
    ///
    /// # Errors
    /// Returns inherited ownership or poisoned connection errors.
    ///
    /// # Safety
    /// This instance alone must own the inherited fd. The request must negotiate
    /// a protocol using independent session lifetime descriptors; this connection
    /// must never itself be returned as a session lease.
    #[cfg(unix)]
    pub unsafe fn with_control<T>(
        &self,
        request: impl FnOnce(&UnixStream) -> T,
    ) -> Result<T, ServiceSocketError> {
        let exchange = self
            .exchange
            .lock()
            .map_err(|_| ServiceSocketError::Poisoned)?;
        // SAFETY: the caller guarantees this instance exclusively owns the fd.
        let stream = unsafe { self.socket.take() }?;
        let result = request(&stream);
        self.socket.retain_connection(stream)?;
        drop(exchange);
        Ok(result)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceSocketError {
    #[error("the service descriptor is missing")]
    Missing,
    #[error("this window's service connection is already in use")]
    InUse,
    #[error("the service connection ownership lock is poisoned")]
    Poisoned,
}

impl InheritedServiceSocket {
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        #[cfg(not(unix))]
        let _ = name;
        Self {
            #[cfg(unix)]
            name,
            claimed: AtomicBool::new(false),
            #[cfg(unix)]
            retry: Mutex::new(None),
        }
    }

    #[must_use]
    pub fn was_taken(&self) -> bool {
        self.claimed.load(Ordering::Acquire)
    }

    /// Take the refused connection, or take ownership of the inherited fd once.
    ///
    /// # Errors
    /// Returns `Missing` for an absent descriptor, `InUse` once consumed, or
    /// `Poisoned` when connection ownership cannot be safely recovered.
    ///
    /// # Safety
    /// This instance must be the sole owner of the named inherited service fd;
    /// no other code may take or close that fd after this call.
    #[cfg(unix)]
    pub unsafe fn take(&self) -> Result<UnixStream, ServiceSocketError> {
        let mut retry = self
            .retry
            .lock()
            .map_err(|_| ServiceSocketError::Poisoned)?;
        let stream = retry.take();
        drop(retry);
        if let Some(stream) = stream {
            return Ok(stream);
        }
        if self.claimed.swap(true, Ordering::AcqRel) {
            return Err(ServiceSocketError::InUse);
        }
        // SAFETY: the caller guarantees exclusive ownership of this service fd.
        let fd = unsafe { crate::take_service_fd(self.name) }.ok_or(ServiceSocketError::Missing)?;
        Ok(UnixStream::from(fd))
    }

    /// Retain the same connection after a refusal that handed out no session.
    /// Callers must never return a connection which now carries a live session.
    ///
    /// # Errors
    /// Returns `InUse` if a retry is already retained, or `Poisoned` if the
    /// ownership lock cannot be acquired safely.
    #[cfg(unix)]
    pub fn retry_after_refusal(&self, stream: UnixStream) -> Result<(), ServiceSocketError> {
        self.retain_connection(stream)
    }

    #[cfg(unix)]
    fn retain_connection(&self, stream: UnixStream) -> Result<(), ServiceSocketError> {
        let mut retry = self
            .retry
            .lock()
            .map_err(|_| ServiceSocketError::Poisoned)?;
        if retry.is_some() {
            return Err(ServiceSocketError::InUse);
        }
        *retry = Some(stream);
        drop(retry);
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn replacing_the_owned_lease_ends_it_even_while_a_reader_clone_exists() {
        let (mut supervisor, terminal) = UnixStream::pair().unwrap();
        let lease = SessionLease::from(terminal);
        let clone = lease.try_clone().unwrap();
        supervisor
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        drop(lease);
        let mut byte = [0u8; 1];
        assert_eq!(supervisor.read(&mut byte).unwrap(), 0);
        assert!(clone.take_error().unwrap().is_none());
        drop(clone);
    }

    #[test]
    fn refusal_reuses_the_connection_without_reclaiming_a_consumed_descriptor() {
        let connection = InheritedServiceSocket::new("test-refused-service");
        let (stream, mut peer) = UnixStream::pair().expect("service pair");
        connection.claimed.store(true, Ordering::Release);
        connection
            .retry_after_refusal(stream)
            .expect("retain refusal");
        // SAFETY: this fixture owns the connection; the already-claimed path
        // must never try to acquire an inherited descriptor.
        let mut stream = unsafe { connection.take() }.expect("retry");
        stream.write_all(b"open").expect("request");
        let mut request = [0; 4];
        peer.read_exact(&mut request).expect("same peer");
        assert_eq!(&request, b"open");
        assert!(connection.was_taken());
        // SAFETY: same exclusive fixture ownership as above.
        assert!(matches!(
            unsafe { connection.take() },
            Err(ServiceSocketError::InUse)
        ));
        connection
            .retry_after_refusal(stream)
            .expect("another refusal");
        // SAFETY: same exclusive fixture ownership as above.
        let mut stream = unsafe { connection.take() }.expect("second retry");
        peer.write_all(b"live").expect("reply");
        stream
            .read_exact(&mut request)
            .expect("same connection reply");
        assert_eq!(&request, b"live");
    }
}
