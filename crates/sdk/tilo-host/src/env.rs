//! The toolkit's debug, capture and benchmark knobs: `TILO_UI_*` variables.
//!
//! They were called `QUICK_UI_*` before the rename. [`var`] and [`var_os`]
//! take the new name and still read the old one, warning once per variable,
//! so scripts keep working while they move.

use std::collections::BTreeSet;
use std::env::VarError;
use std::ffi::OsString;
use std::sync::{Mutex, PoisonError};

const PREFIX: &str = "TILO_UI_";
const LEGACY_PREFIX: &str = "QUICK_UI_";

/// The pre-rename name of `name`, when it is a `TILO_UI_*` variable.
#[must_use]
pub fn legacy_name(name: &str) -> Option<String> {
    name.strip_prefix(PREFIX)
        .map(|rest| format!("{LEGACY_PREFIX}{rest}"))
}

/// `std::env::var_os(name)`, falling back to the legacy `QUICK_UI_*` name.
#[must_use]
pub fn var_os(name: &str) -> Option<OsString> {
    std::env::var_os(name).or_else(|| {
        let legacy = legacy_name(name)?;
        let value = std::env::var_os(&legacy)?;
        warn_once(&legacy, name);
        Some(value)
    })
}

/// `std::env::var(name)`, falling back to the legacy `QUICK_UI_*` name.
///
/// # Errors
///
/// As `std::env::var`: [`VarError::NotPresent`] when neither name is set,
/// [`VarError::NotUnicode`] when the value is not UTF-8.
pub fn var(name: &str) -> Result<String, VarError> {
    match var_os(name) {
        Some(value) => value.into_string().map_err(VarError::NotUnicode),
        None => Err(VarError::NotPresent),
    }
}

fn warn_once(legacy: &str, name: &str) {
    static WARNED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
    let first = WARNED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(legacy.to_owned());
    if first {
        tracing::warn!("{legacy} is deprecated; set {name} instead");
        eprintln!("warning: {legacy} is deprecated; set {name} instead");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_names_map_one_to_one() {
        assert_eq!(
            legacy_name("TILO_UI_SCREENSHOT_DIR").as_deref(),
            Some("QUICK_UI_SCREENSHOT_DIR")
        );
        assert_eq!(legacy_name("TILO_PROFILE"), None);
    }

    #[test]
    fn an_unset_variable_reads_as_not_present() {
        assert_eq!(
            var("TILO_UI_TEST_NEVER_SET_ANYWHERE"),
            Err(VarError::NotPresent)
        );
    }
}
