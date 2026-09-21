use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    InvalidInput,
    PermissionDenied,
    Unsupported,
    Busy,
    Stale,
    Closed,
    Failed,
}

#[derive(Clone, Debug)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    uncertain: bool,
}
impl Error {
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn uncertain(&self) -> bool {
        self.uncertain
    }
    pub(crate) fn invalid_input(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidInput, message)
    }
    pub(crate) fn failed(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Failed, message)
    }
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn busy(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Busy, message)
    }
    pub(crate) fn stale(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Stale, message)
    }
    pub(crate) fn closed(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Closed, message)
    }
    pub(crate) fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unsupported, message)
    }
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn permission_denied(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::PermissionDenied, message)
    }
    pub(crate) fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            uncertain: false,
        }
    }
    pub(crate) fn io(error: std::io::Error) -> Self {
        let kind = match error.kind() {
            std::io::ErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
            std::io::ErrorKind::Unsupported => ErrorKind::Unsupported,
            std::io::ErrorKind::WouldBlock => ErrorKind::Busy,
            _ => ErrorKind::Failed,
        };
        Self::new(kind, error.to_string())
    }
    pub(crate) fn with_uncertainty(mut self) -> Self {
        self.uncertain = true;
        self
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}
impl std::error::Error for Error {}

// Private adapter retains the native protocol's diagnostic conversions without
// adding a string factory to the public Error contract.
#[derive(Clone, Debug)]
pub(crate) struct Failure(Error);
impl Failure {
    pub(crate) fn with_message(mut self, message: String) -> Self {
        self.0.message = message;
        self
    }
    pub(crate) fn into_error(self) -> Error {
        self.0
    }
    pub(crate) fn kind(&self) -> ErrorKind {
        self.0.kind()
    }
    pub(crate) fn message(&self) -> &str {
        self.0.message()
    }
    pub(crate) fn uncertain(&self) -> bool {
        self.0.uncertain()
    }
    pub(crate) fn with_uncertainty(self) -> Self {
        Self(self.0.with_uncertainty())
    }
    pub(crate) fn io_context(error: std::io::Error, context: &str) -> Self {
        let message = format!("{context}{error}");
        let mut error = Error::io(error);
        error.message = message;
        Self(error)
    }
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn busy(message: impl Into<String>) -> Self {
        Self(Error::busy(message))
    }
    pub(crate) fn stale(message: impl Into<String>) -> Self {
        Self(Error::stale(message))
    }
    pub(crate) fn closed(message: impl Into<String>) -> Self {
        Self(Error::closed(message))
    }
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn unsupported(message: impl Into<String>) -> Self {
        Self(Error::unsupported(message))
    }
    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn permission_denied(message: impl Into<String>) -> Self {
        Self(Error::permission_denied(message))
    }
    pub(crate) fn invalid_input(message: impl Into<String>) -> Self {
        Self(Error::invalid_input(message))
    }
    #[cfg(test)]
    pub(crate) fn contains(&self, needle: &str) -> bool {
        self.message().contains(needle)
    }
}
impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self(error)
    }
}
impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self(Error::failed(message))
    }
}
impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Self(Error::failed(message))
    }
}
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for Failure {}
impl PartialEq<str> for Failure {
    fn eq(&self, other: &str) -> bool {
        self.message() == other
    }
}
impl PartialEq<&str> for Failure {
    fn eq(&self, other: &&str) -> bool {
        self.message() == *other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contextual_failure_preserves_kind_diagnostic_and_uncertainty() {
        for (native, expected) in [
            (
                std::io::ErrorKind::PermissionDenied,
                ErrorKind::PermissionDenied,
            ),
            (std::io::ErrorKind::Unsupported, ErrorKind::Unsupported),
            (std::io::ErrorKind::WouldBlock, ErrorKind::Busy),
            (std::io::ErrorKind::Other, ErrorKind::Failed),
        ] {
            for already_uncertain in [false, true] {
                for context in [
                    "Journal publication uncertain; retained residue: ",
                    "Final publication uncertain; retained residue: ",
                ] {
                    let mut original = Failure::io_context(
                        std::io::Error::new(native, "opaque diagnostic"),
                        "Native: ",
                    );
                    if already_uncertain {
                        original = original.with_uncertainty();
                    }
                    let message = format!("{context}{original}");
                    let wrapped = original.with_message(message);
                    assert_eq!(wrapped.kind(), expected);
                    assert_eq!(
                        wrapped.message(),
                        format!("{context}Native: opaque diagnostic")
                    );
                    assert_eq!(wrapped.uncertain(), already_uncertain);
                    let uncertain = wrapped.with_uncertainty().into_error();
                    assert_eq!(uncertain.kind(), expected);
                    assert_eq!(
                        uncertain.message(),
                        format!("{context}Native: opaque diagnostic")
                    );
                    assert!(uncertain.uncertain());
                }
            }
        }
    }

    #[test]
    fn native_io_classification_uses_kind_and_preserves_diagnostic() {
        for (native, expected) in [
            (
                std::io::ErrorKind::PermissionDenied,
                ErrorKind::PermissionDenied,
            ),
            (std::io::ErrorKind::Unsupported, ErrorKind::Unsupported),
            (std::io::ErrorKind::WouldBlock, ErrorKind::Busy),
            (std::io::ErrorKind::Other, ErrorKind::Failed),
        ] {
            let error =
                Failure::io_context(std::io::Error::new(native, "opaque diagnostic"), "Native: ")
                    .into_error();
            assert_eq!(error.kind(), expected);
            assert_eq!(error.message(), "Native: opaque diagnostic");
            assert!(!error.uncertain());
        }
        assert_eq!(
            Failure::from("PermissionDenied Busy Unsupported").kind(),
            ErrorKind::Failed
        );
    }
}
