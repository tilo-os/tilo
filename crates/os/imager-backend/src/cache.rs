//! Host support for the retained installer download cache.
//!
//! There is deliberately no generic filesystem fallback: it could not supply
//! the native providers' pinned namespace and ownership guards.

/// Refuse hosts without a native private-cache provider, before starting I/O.
pub fn ensure_supported() -> Result<(), String> {
    if cfg!(any(target_os = "linux", windows)) {
        Ok(())
    } else {
        unsupported()
    }
}

/// The explicit failure for a cache operation without a native provider.
pub fn unsupported<T>() -> Result<T, String> {
    Err("Retained installer downloads are unsupported on this platform".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn support_matches_native_provider_targets() {
        assert_eq!(
            ensure_supported().is_ok(),
            cfg!(any(target_os = "linux", windows))
        );
        if !cfg!(any(target_os = "linux", windows)) {
            assert_eq!(ensure_supported(), unsupported());
        }
        assert!(unsupported::<()>().is_err());
    }
}
