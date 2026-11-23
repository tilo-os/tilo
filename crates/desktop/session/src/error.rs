//! What stops a session: the host it runs on failed under it.

/// The session's host failed: its event loop, a client's socket or its displays.
#[derive(Debug)]
pub struct Error(Box<dyn std::error::Error + Send + Sync>);

impl Error {
    /// The host failed with `error`.
    pub fn host(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self(error.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.0)
    }
}
