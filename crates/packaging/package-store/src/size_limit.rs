//! The size limit an application package installs under on this machine.
//!
//! Every store holds an application to its distribution limit
//! ([`tilo_app_package::app_size_limit`]): it is the registry's policy, and it
//! bounds what an untrusted package can make the store write. A developer
//! session builds its apps for build speed rather than size and installs them
//! itself, signed with its own development key, and never publishes them. In
//! a build that honours developer overrides ([`crate::dev_env`]), a package
//! whose signature verifies under a key on the session's publisher list
//! ([`DEVELOPER_PUBLISHERS_FILE_ENV`]) installs under the local-build ceiling
//! ([`tilo_app_package::local_build_size_limit`]) instead. A shipped build
//! reads no such list, so every package it installs keeps the distribution
//! limit.

use std::ffi::OsString;

use tilo_app_package::{PackageMeta, Verification};

use crate::dev_env::dev_override;

/// Names a developer session's publisher list: the keys it signs its own
/// runtime bundles and app packages with, one hex key a line. The launcher
/// trusts runtime bundles of these keys instead of the image's list; the
/// store installs app packages signed by them under the local-build ceiling.
/// A developer override: a shipped build ignores it.
pub const DEVELOPER_PUBLISHERS_FILE_ENV: &str = "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE";

/// The wire and materialized size limit `meta`'s package installs under on
/// this machine (see the module documentation); `None` for a non-app bundle.
pub fn app_size_limit(meta: &PackageMeta) -> Option<u64> {
    app_size_limit_among(
        meta,
        &developer_publishers_from(dev_override(DEVELOPER_PUBLISHERS_FILE_ENV)),
    )
}

/// [`app_size_limit`] given the developer session's publisher keys.
pub fn app_size_limit_among(meta: &PackageMeta, developer_publishers: &[[u8; 32]]) -> Option<u64> {
    let local_build = !developer_publishers.is_empty()
        && matches!(
            tilo_app_package::verify(meta, developer_publishers),
            Verification::Trusted { .. }
        );
    if local_build {
        tilo_app_package::local_build_size_limit(&meta.manifest)
    } else {
        tilo_app_package::app_size_limit(&meta.manifest)
    }
}

/// The keys of the publisher list at `list`, the path the build's gate let
/// through ([`dev_override`]): none without one, or when it cannot be read.
pub fn developer_publishers_from(list: Option<OsString>) -> Vec<[u8; 32]> {
    let Some(text) = list.and_then(|path| std::fs::read_to_string(path).ok()) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| hex::decode(line).ok()?.try_into().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dev_env::dev_override_from;

    #[test]
    fn only_an_open_gate_reads_the_developer_publisher_list() {
        let dir = tempfile::tempdir().unwrap();
        let list = dir.path().join("publishers.txt");
        let key = [7u8; 32];
        std::fs::write(
            &list,
            format!("# the session\n{}\n\nnot-a-key\n", hex::encode(key)),
        )
        .unwrap();
        assert_eq!(
            developer_publishers_from(dev_override_from(
                true,
                DEVELOPER_PUBLISHERS_FILE_ENV,
                Some(list.clone().into())
            )),
            vec![key]
        );
        // A shipped build: the variable is set and ignored, so no package
        // is a local build.
        assert!(
            developer_publishers_from(dev_override_from(
                false,
                DEVELOPER_PUBLISHERS_FILE_ENV,
                Some(list.into())
            ))
            .is_empty()
        );
        assert!(developer_publishers_from(None).is_empty());
    }
}
