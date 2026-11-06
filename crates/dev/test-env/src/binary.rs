//! The binaries Cargo built for a test, and the ones a run's harness built
//! beside them: the apps and tools of other packages that a test starts as
//! processes of their own.

#![allow(
    clippy::panic,
    clippy::expect_used,
    reason = "test support: failing the calling test is the point"
)]

use std::env;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// A binary of another package that the run's harness built.
///
/// Cargo hands a test only the binaries of its own package
/// ([`package_binary!`](crate::package_binary)), so a test that starts an app
/// of another package — the session's lock screen, the on-screen keyboard — has
/// no path to it, and only the harness can build it. A test that built it
/// itself would run Cargo inside a run that Cargo is already driving: a second
/// workspace resolution, and a compile into the run's target directory with
/// features unified another way, so nearly the whole graph is compiled twice.
/// The harness builds every such binary before the first test starts
/// (`tools/build-sibling-binaries.sh`, started by a nextest setup script,
/// `.config/nextest.toml`) and hands each one's path to the run's tests as
/// `TILO_SIBLING_BIN_<bin>` (`-` as `_`). `package` names a package of the
/// workspace and `bin` a binary target of it.
///
/// A run that moved its binaries somewhere else (a nextest archive) answers
/// with where they were moved to, and a binary of the package under test
/// answers with the path Cargo built it at, as the run's own binaries do.
///
/// ```ignore
/// // The session's lock screen is an app of the session's own package tree, so
/// // no `CARGO_BIN_EXE_*` names it.
/// let lock_screen = tilo_test_env::sibling_binary("tilo-lock-screen", "tilo-lock-screen");
/// ```
///
/// # Panics
///
/// Panics when no run built the binary: the message names the harness command
/// that builds it.
#[must_use]
pub fn sibling_binary(package: &str, bin: &str) -> PathBuf {
    if let Some(built) = built_by_the_run(package, bin) {
        return built;
    }
    let at = build().binary(bin);
    assert!(
        at.is_file(),
        "expected {package}'s `{bin}` at {}, which no run built: a test starts only programs \
         its run built. The harness builds this one before the tests start: `bash \
         tools/build-sibling-binaries.sh {package}:{bin}` (nextest runs it as the \
         session-siblings setup, .config/nextest.toml)",
        at.display()
    );
    at
}

/// Where the run built this binary already: `NEXTEST_BIN_EXE_<bin>` when the
/// run moved its binaries, `TILO_SIBLING_BIN_<bin>` (`-` as `_`) when the run's setup
/// built it before any test started (`tools/build-sibling-binaries.sh`),
/// else `CARGO_BIN_EXE_<bin>` when `package` is the package under test.
fn built_by_the_run(package: &str, bin: &str) -> Option<PathBuf> {
    for name in [
        format!("NEXTEST_BIN_EXE_{bin}"),
        // An environment file's keys are letters, digits and underscores.
        format!("TILO_SIBLING_BIN_{}", bin.replace('-', "_")),
    ] {
        if let Some(built) = env::var_os(name) {
            return Some(PathBuf::from(built));
        }
    }
    if env::var("CARGO_PKG_NAME").as_deref() != Ok(package) {
        return None;
    }
    env::var_os(format!("CARGO_BIN_EXE_{bin}")).map(PathBuf::from)
}

/// The directory this run built its binaries in.
struct Build {
    /// `<target>/<profile>`, where a binary of any package lands.
    directory: PathBuf,
}

/// The build the tests of this process belong to, read from the test binary's
/// own path the first time a test asks.
fn build() -> &'static Build {
    static BUILD: OnceLock<Build> = OnceLock::new();
    BUILD.get_or_init(|| {
        let test_binary = env::current_exe().expect("the test binary's own path");
        Build::of(&test_binary).unwrap_or_else(|| {
            panic!(
                "cannot tell where this run built its binaries from {}: a test binary is \
                 <target>/<profile>/deps/<test>",
                test_binary.display()
            )
        })
    })
}

impl Build {
    /// The build a test binary at `<target>/<profile>/deps/<test>` belongs to.
    fn of(test_binary: &Path) -> Option<Self> {
        Some(Self {
            directory: test_binary.parent()?.parent()?.to_path_buf(),
        })
    }

    /// Where a binary of this build lands: beside the test binary's own
    /// `deps` directory, as Cargo leaves the binaries it builds for a run.
    fn binary(&self, bin: &str) -> PathBuf {
        self.directory
            .join(format!("{bin}{}", env::consts::EXE_SUFFIX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario;

    scenario! {
        fn a_run_is_read_from_its_test_binary;

        phase a_test_binaries_profile_is_the_directory_of_its_binaries;
        {
            let build = Build::of(Path::new("/work/target/debug/deps/session-4f2c")).unwrap();
            assert_eq!(build.directory, Path::new("/work/target/debug"));
            assert_eq!(
                build.binary("osk"),
                Path::new("/work/target/debug").join(format!("osk{}", env::consts::EXE_SUFFIX))
            );
        }

        phase a_named_profile_is_its_own_directory;
        {
            let named = Build::of(Path::new("/work/target/ci/deps/session-4f2c")).unwrap();
            assert_eq!(
                named.binary("tilo-lock-screen"),
                Path::new("/work/target/ci")
                    .join(format!("tilo-lock-screen{}", env::consts::EXE_SUFFIX))
            );
        }

        phase a_test_binaries_own_package_is_the_one_the_run_built;
        {
            // Cargo names the binaries of the package under test, and no other
            // package's, so only that package's answer is taken as built.
            let own = env::var("CARGO_PKG_NAME").expect("the package under test");
            let mut environment = crate::ScopedEnv::new();
            environment
                .set("CARGO_BIN_EXE_own-package-binary", "/built/own-package-binary");
            assert_eq!(
                built_by_the_run(&own, "own-package-binary"),
                Some(PathBuf::from("/built/own-package-binary")),
                "the package under test's own binary is the run's"
            );
            assert_eq!(
                built_by_the_run("another-package", "own-package-binary"),
                None,
                "another package's binary is not the run's"
            );
        }
    }
}
