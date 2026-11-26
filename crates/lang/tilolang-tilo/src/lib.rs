//! Tilo platform integration for tilolang apps.
//!
//! The tilolang compiler and runtime know nothing of Tilo. This crate is the boundary to the
//! platform:
//!
//! * [`resources`]: the runtime's store over the device's one resource store, where every
//!   resource of a tilolang app is kept;
//! * [`action`]: `DynAction`, the record of an invoked tilolang action;
//! * [`meta`] and [`surface`]: what the platform derives from package metadata, and the surface
//!   generated from it;
//! * [`publish`]: the resources and typed actions published on the action api;
//! * [`instance`]: one window of an app as an app host runs it, without the toolkit, sending
//!   its window component patches and taking what the person did;

/// The tilolang runtime this build is, as a package's manifest names the runtime its code runs
/// on: [`runtime_of`] the standard library it compiles with.
#[must_use]
pub fn runtime() -> String {
    runtime_of(tilolang::stdlib::digest())
}

/// The tilolang runtime that carries the standard library `std` (its
/// [`tilolang::stdlib::digest`]): `tilolang/<version>+tilolang.<std>`, the build
/// `tilolang-process/tilo-runtime.toml` provides (`tilolang/<version>+tilolang`) keyed by the
/// library it carries. A package compiled against one library runs on that runtime alone, and a
/// device that lacks it fetches it as it fetches any runtime a package requires.
#[must_use]
pub fn runtime_of(std: &str) -> String {
    format!("tilolang/{}+tilolang.{std}", env!("CARGO_PKG_VERSION"))
}

pub mod action;
pub mod authority;
pub mod background;
pub mod bundle;
pub mod call;
pub mod catalogue;
pub mod clock;
pub mod content;
pub mod effects;
#[cfg(any(unix, windows))]
pub mod held;
pub mod host;
pub mod instance;
pub mod members;
pub mod meta;
pub mod names;
pub mod naming;
pub mod native;
pub mod origin;
pub mod page;
pub mod personal;
pub mod platform_apps;
pub mod publish;
pub mod published;
pub mod reading;
pub mod readings;

pub mod keyboard;
pub mod onboarding;
pub mod resources;
pub mod schema;
pub mod search;
pub mod seed;
pub mod share;
pub mod shell;
pub mod sink;
pub mod surface;
pub mod task;
pub mod task_reads;
pub mod triggers;
pub mod upgrade;
pub mod used;
pub mod versions;
pub mod views;

pub use action::{Authorship, DynAction, register_actions};
pub use effects::{HostEffect, OpenEffect, OpenPlan, ReadAsk};
pub use meta::MetaExt;
pub use schema::{FieldSchema, MergeKind, TypeSchema};
/// The compiler, for hosts that build packages from source.
pub use tilolang;
pub use tilolang::HOST_APP;
/// The runtime, for hosts that drive views and read values.
pub use tilolang_runtime as runtime;

#[cfg(test)]
mod tests {
    use tilo_test_env::SpawnUnderEnvLock as _;

    /// The scripts that bake the runtime and the packages that name it key it alike: the shell
    /// helper every packing sources (`tools/apps/tilolang_packages.sh`) names the version this
    /// build's packages name, from the same tree's manifest and standard library.
    #[test]
    fn the_shell_names_the_runtime_this_build_names() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(r#"set -euo pipefail; . "$1/tools/apps/tilolang_packages.sh"; tilolang_runtime_version "$1""#)
            .arg("bash")
            .arg(&repo)
            .output_locked()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let version = String::from_utf8(out.stdout).unwrap();
        assert_eq!(format!("tilolang/{}", version.trim()), super::runtime());
        assert!(super::runtime().ends_with(&format!(".{}", tilolang::stdlib::digest())));
    }
}
