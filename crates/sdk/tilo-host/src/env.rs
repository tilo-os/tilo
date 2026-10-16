//! The toolkit's debug, capture and benchmark knobs: `TILO_UI_*` variables.

use std::env::VarError;
use std::ffi::OsString;

/// `std::env::var_os(name)` through the host's environment access.
#[must_use]
pub fn var_os(name: &str) -> Option<OsString> {
    #[cfg(feature = "native")]
    {
        crate::native::env_vars::var_os(name)
    }
    #[cfg(not(feature = "native"))]
    {
        let _ = name;
        None
    }
}

/// `std::env::var(name)` through the host's environment access.
///
/// # Errors
///
/// As `std::env::var`: [`VarError::NotPresent`] when the variable is unset,
/// [`VarError::NotUnicode`] when the value is not UTF-8.
pub fn var(name: &str) -> Result<String, VarError> {
    match var_os(name) {
        Some(value) => value.into_string().map_err(VarError::NotUnicode),
        None => Err(VarError::NotPresent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_variable_reads_as_not_present() {
        assert_eq!(
            var("TILO_UI_TEST_NEVER_SET_ANYWHERE"),
            Err(VarError::NotPresent)
        );
    }
}
