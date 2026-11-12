//! Ownership of an inherited service connection which can refuse a request.

use std::io;
#[cfg(unix)]
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

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

    /// Exchange lines with the service over the control connection, retaining only the connection:
    /// `exchanged` writes a line ([`Link::send`]) and reads its one-line answer
    /// ([`Link::answer`]) as the service's protocol asks, and no answer may carry descriptors. A
    /// host without inherited descriptors has no connection to exchange on and answers
    /// [`ServiceSocketError::Missing`] without calling `exchanged`, so callers need no target
    /// checks.
    ///
    /// # Safety
    /// As [`with_control`](Self::with_control): this instance alone must own the inherited fd, and
    /// nothing exchanged may return this connection as a lease.
    ///
    /// # Errors
    /// The socket's own errors ([`ServiceSocketError`]), or [`ControlError::Answer`] when a line
    /// could not be written or read.
    pub unsafe fn exchange<T>(
        &self,
        exchanged: impl FnOnce(&mut Link<'_>) -> T,
    ) -> Result<T, ControlError> {
        #[cfg(unix)]
        {
            // SAFETY: the caller guarantees this instance alone owns the fd.
            unsafe { self.with_control(|stream| exchanged(&mut Link { stream })) }
                .map_err(ControlError::Socket)
        }
        #[cfg(not(unix))]
        {
            let _ = exchanged;
            Err(ControlError::Socket(ServiceSocketError::Missing))
        }
    }
}

/// The control connection as an exchange uses it: one line out, its one-line answer back, and
/// never a descriptor.
pub struct Link<'a> {
    #[cfg(unix)]
    stream: &'a UnixStream,
    #[cfg(not(unix))]
    _target: std::marker::PhantomData<&'a ()>,
}

impl Link<'_> {
    /// Write one line to the service.
    ///
    /// # Errors
    /// The line could not be written or flushed.
    pub fn send(&mut self, line: &[u8]) -> Result<(), ControlError> {
        #[cfg(unix)]
        {
            let mut writer = self.stream;
            writer
                .write_all(line)
                .and_then(|()| writer.flush())
                .map_err(ControlError::Answer)
        }
        #[cfg(not(unix))]
        {
            let _ = line;
            Err(ControlError::Socket(ServiceSocketError::Missing))
        }
    }

    /// Read the answer to the line just sent, refusing one that carries descriptors.
    ///
    /// # Errors
    /// The answer could not be read, or came with descriptors this exchange never asked for.
    pub fn answer(&mut self) -> Result<Vec<u8>, ControlError> {
        #[cfg(unix)]
        {
            let (payload, descriptors) =
                crate::fd_transport::recv_with_fds(self.stream).map_err(ControlError::Answer)?;
            if descriptors.is_empty() {
                return Ok(payload);
            }
            Err(ControlError::UnexpectedDescriptors)
        }
        #[cfg(not(unix))]
        {
            Err(ControlError::Socket(ServiceSocketError::Missing))
        }
    }
}

/// Why an exchange over the control connection did not answer.
#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error(transparent)]
    Socket(#[from] ServiceSocketError),
    /// The service answered with descriptors nobody asked for.
    #[error("the service answered with descriptors nobody asked for")]
    UnexpectedDescriptors,
    /// Writing a line or reading its answer failed.
    #[error("the control exchange failed: {0}")]
    Answer(io::Error),
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
