//! The preparing computer's serving side of another-device handoff.
//!
//! The medium carries only the ordinary encrypted package and a public
//! descriptor. Serving it needs an authenticated peer endpoint bound to this
//! person's identity, which only a Unix-hosted preparation can own. That
//! boundary lives here: the journey holds one optional value and renders its
//! state without selecting code by target.

use std::io;

use zeroize::Zeroizing;

pub use tilo_handoff::{HandoffIdentity, SourceState};

/// A bound source for one prepared package. Dropping it stops serving.
pub struct Handoff {
    inner: inner::Handoff,
}

impl Handoff {
    /// Bind the endpoint this medium's descriptor will name. The returned
    /// identity is public; the generated package password never leaves this
    /// value until the destination proves its identity and a person approves
    /// the exact request.
    ///
    /// # Errors
    ///
    /// The host has no identity custody or peer endpoint, or the operation id
    /// is invalid.
    pub fn bind(
        identity: &crate::setup::prepared_identity::PreparedIdentity,
        operation_id: String,
    ) -> io::Result<(Self, Zeroizing<String>)> {
        let (inner, key) = inner::Handoff::bind(identity, operation_id)?;
        Ok((Self { inner }, key))
    }

    pub fn identity(&self) -> &HandoffIdentity {
        self.inner.identity()
    }

    /// The generated package password. Only the caller that seals the package
    /// keeps it.
    pub fn key(&self) -> &Zeroizing<String> {
        self.inner.key()
    }

    pub fn cancel(&self) {
        self.inner.cancel();
    }

    /// # Errors
    ///
    /// The approval token is malformed or the endpoint stopped.
    pub fn approve(&self, approval: &str) -> io::Result<()> {
        self.inner.approve(approval)
    }

    /// The next state to show, if anything changed since the last poll.
    pub fn poll(&self) -> Option<SourceState> {
        self.inner.poll()
    }

    /// Make the exact ciphertext available to the approved destination.
    ///
    /// # Errors
    ///
    /// The package is empty or oversized, the digest differs from the bytes, or
    /// the endpoint stopped.
    pub fn serve(&self, ciphertext: Vec<u8>, digest: [u8; 32]) -> io::Result<()> {
        self.inner.serve(ciphertext, digest)
    }
}

/// Test-only: serve a source the fixture already bound, without identity
/// custody the test process does not have.
#[cfg(all(unix, any(test, feature = "test-support")))]
impl Handoff {
    pub fn from_source(source: tilo_handoff::Source, key: Zeroizing<String>) -> Self {
        Self {
            inner: inner::Handoff { source, key },
        }
    }
}

#[cfg(unix)]
mod inner {
    use super::{HandoffIdentity, SourceState, Zeroizing, io};

    pub(super) struct Handoff {
        pub(super) source: tilo_handoff::Source,
        pub(super) key: Zeroizing<String>,
    }

    impl Handoff {
        pub(super) fn bind(
            identity: &crate::setup::prepared_identity::PreparedIdentity,
            operation_id: String,
        ) -> io::Result<(Self, Zeroizing<String>)> {
            let authority = identity.bootstrap_source_identity()?;
            let source = tilo_handoff::Source::bind_with_identity(operation_id, authority)?;
            let key = Zeroizing::new(format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ));
            Ok((
                Self {
                    source,
                    key: key.clone(),
                },
                key,
            ))
        }

        pub(super) fn identity(&self) -> &HandoffIdentity {
            self.source.identity()
        }

        pub(super) fn key(&self) -> &Zeroizing<String> {
            &self.key
        }

        pub(super) fn cancel(&self) {
            self.source.cancel();
        }

        pub(super) fn approve(&self, approval: &str) -> io::Result<()> {
            self.source.approve(approval)
        }

        pub(super) fn poll(&self) -> Option<SourceState> {
            self.source.poll()
        }

        pub(super) fn serve(&self, ciphertext: Vec<u8>, digest: [u8; 32]) -> io::Result<()> {
            self.source.serve(ciphertext, self.key.clone(), digest)
        }
    }
}

#[cfg(not(unix))]
mod inner {
    use super::{HandoffIdentity, SourceState, Zeroizing, io};

    /// No endpoint can be bound on this host, so this value is never created;
    /// the methods exist so the journey compiles without a target gate.
    pub(super) struct Handoff {
        identity: HandoffIdentity,
        key: Zeroizing<String>,
    }

    impl Handoff {
        pub(super) fn bind(
            _identity: &crate::setup::prepared_identity::PreparedIdentity,
            _operation_id: String,
        ) -> io::Result<(Self, Zeroizing<String>)> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Serving a prepared package from this computer requires a Unix host",
            ))
        }

        pub(super) fn identity(&self) -> &HandoffIdentity {
            &self.identity
        }

        pub(super) fn key(&self) -> &Zeroizing<String> {
            &self.key
        }

        pub(super) fn cancel(&self) {}

        pub(super) fn approve(&self, _approval: &str) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Serving a prepared package from this computer requires a Unix host",
            ))
        }

        pub(super) fn poll(&self) -> Option<SourceState> {
            None
        }

        pub(super) fn serve(&self, _ciphertext: Vec<u8>, _digest: [u8; 32]) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Serving a prepared package from this computer requires a Unix host",
            ))
        }
    }
}
