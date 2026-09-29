//! Process-environment reads for [`crate::env`].

use std::ffi::OsString;

pub(crate) fn var_os(key: &str) -> Option<OsString> {
    std::env::var_os(key)
}
