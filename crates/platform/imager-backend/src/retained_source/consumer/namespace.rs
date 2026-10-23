//! Portable, opaque owner of the canonical consumer helper's mount namespace.
//! Neither descriptors nor numeric credentials can construct this authority.
use std::io;

pub struct ManagedConsumerNamespace {
    #[cfg(target_os = "linux")]
    native: super::native_namespace::ManagedConsumerNamespace,
    #[cfg(not(target_os = "linux"))]
    unavailable: std::convert::Infallible,
}

impl ManagedConsumerNamespace {
    /// Verify the actual signed helper role before isolating its namespace.
    pub fn isolate() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            super::native_namespace::ManagedConsumerNamespace::isolate()
                .map(|native| Self { native })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(crate::retained_source::Unsupported.into())
        }
    }

    /// Revalidate the executing helper and exact held kernel namespace.
    /// # Errors
    /// Returns an error if helper authentication fails, namespace identity has
    /// changed, procfs cannot be read, or the mount table is invalid or shared.
    pub fn check(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            self.native.check()
        }
        #[cfg(not(target_os = "linux"))]
        {
            match self.unavailable {}
        }
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn unsupported_host_cannot_mint_namespace_custody() {
        let error = ManagedConsumerNamespace::isolate().err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(
            error
                .get_ref()
                .unwrap()
                .is::<crate::retained_source::Unsupported>()
        );
    }
}
