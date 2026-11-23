//! The portable admission boundary fails before examining any native source.
//! Launcher-specific installation and run types remain Linux-only.
use std::{fs::File, io, path::Path};

pub struct CaptureHostOwner {
    never: std::convert::Infallible,
}

impl CaptureHostOwner {
    pub fn verify(_runtime: &Path, _publisher: &str, _source: File, _uid: u32) -> io::Result<Self> {
        Err(crate::capture::Error::Unsupported.into())
    }

    pub fn with_inventory_scratch(self, _scratch: File, _quota: u64) -> io::Result<Self> {
        Err(crate::capture::Error::Unsupported.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_admission_is_typed_unsupported_before_runtime_access() {
        let source = tempfile::tempfile().unwrap();
        let result =
            CaptureHostOwner::verify(Path::new("missing-runtime"), "publisher", source, 1000);
        let error = result.err().expect("native capture must not be admitted");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(matches!(
            error
                .get_ref()
                .unwrap()
                .downcast_ref::<crate::capture::Error>(),
            Some(crate::capture::Error::Unsupported)
        ));
    }
}
