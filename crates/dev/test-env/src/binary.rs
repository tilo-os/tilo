//! The binaries Cargo built for a test, and the ones it did not: the apps and
//! tools of other packages that a test starts as processes of their own.

#![allow(
    clippy::panic,
    clippy::expect_used,
    reason = "test support: failing the calling test is the point"
)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::{env, fmt};
use tilo_lock::MutexRecover as _;

/// A binary of another package, built the first time a test of this process
/// asks for it.
///
/// Cargo hands a test only the binaries of its own package
/// ([`package_binary!`](crate::package_binary)), so a test that starts an app
/// of another package — the session's lock screen, the on-screen keyboard — has
/// no path to it, and the run it belongs to may not have built it at all. This
/// asks Cargo for it instead: `package` names a package of the workspace, `bin`
/// a binary target of it, and the answer is built with the profile and in the
/// target directory the tests themselves were built with, once per test
/// process. Cargo compiles nothing when the binary is up to date.
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
/// Panics when the build fails, and when Cargo built the binary somewhere other
/// than the directory this run's binaries are in: the message names the command
/// it ran.
#[must_use]
pub fn sibling_binary(package: &str, bin: &str) -> PathBuf {
    if let Some(built) = built_by_the_run(package, bin) {
        return built;
    }
    let at = build().binary(bin);
    build_once(package, bin);
    assert!(
        at.is_file(),
        "expected {package}'s `{bin}` at {} after building it: this run's binaries are there, so \
         one built elsewhere (a `--target` or `--target-dir` of its own) is one the tests cannot \
         start",
        at.display()
    );
    at
}

/// Where the run built this binary already: `NEXTEST_BIN_EXE_<bin>` when the
/// run moved its binaries, else `CARGO_BIN_EXE_<bin>` when `package` is the
/// package under test.
fn built_by_the_run(package: &str, bin: &str) -> Option<PathBuf> {
    if let Some(relocated) = env::var_os(format!("NEXTEST_BIN_EXE_{bin}")) {
        return Some(PathBuf::from(relocated));
    }
    if env::var("CARGO_PKG_NAME").as_deref() != Ok(package) {
        return None;
    }
    env::var_os(format!("CARGO_BIN_EXE_{bin}")).map(PathBuf::from)
}

/// The directory and profile this run built its binaries with.
struct Build {
    /// `<target>/<profile>`, where a binary of any package lands.
    directory: PathBuf,
    profile: String,
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
        let directory = test_binary.parent()?.parent()?.to_path_buf();
        let profile = directory.file_name()?.to_str()?.to_owned();
        Some(Self { directory, profile })
    }

    /// Where a binary of this build lands: beside the test binary's own
    /// `deps` directory, as Cargo leaves the binaries it builds for a run.
    fn binary(&self, bin: &str) -> PathBuf {
        self.directory
            .join(format!("{bin}{}", env::consts::EXE_SUFFIX))
    }

    /// Cargo's arguments naming this build's profile: the default profile's
    /// own for `debug`, which `--profile dev` and `--profile test` build in
    /// too.
    fn profile_arguments(&self) -> Vec<OsString> {
        match self.profile.as_str() {
            "debug" => Vec::new(),
            "release" => vec![OsString::from("--release")],
            named => vec![OsString::from("--profile"), OsString::from(named)],
        }
    }
}

/// Build `bin` unless this process built it already.
fn build_once(package: &str, bin: &str) {
    static BUILT: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
    // Held across the build: two tests of this process never compile the same
    // workspace at once.
    let mut built = BUILT.lock_recover();
    let key = (package.to_owned(), bin.to_owned());
    if built.contains(&key) {
        return;
    }
    build_binary(package, bin);
    built.push(key);
}

/// Run Cargo for one binary of `package`.
fn build_binary(package: &str, bin: &str) {
    let mut arguments: Vec<OsString> = ["build", "--locked", "--package", package, "--bin", bin]
        .into_iter()
        .map(OsString::from)
        .collect();
    arguments.extend(build().profile_arguments());
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut command = Command::new(&cargo);
    command.args(&arguments);
    // This test's own package, from which Cargo finds the workspace whatever
    // directory a test of it has moved to.
    if let Some(directory) = env::var_os("CARGO_MANIFEST_DIR") {
        command.current_dir(directory);
    }
    let output = command.output().unwrap_or_else(|error| {
        panic!(
            "cannot run {} to build {package}'s `{bin}`: {error}",
            cargo.to_string_lossy()
        )
    });
    assert!(
        output.status.success(),
        "building {package}'s `{bin}` for the tests failed: {}\n{}",
        Invocation::of(&command),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The command a build ran, rendered the way it would be typed.
struct Invocation {
    program: OsString,
    arguments: Vec<OsString>,
}

impl Invocation {
    fn of(command: &Command) -> Self {
        Self {
            program: command.get_program().to_owned(),
            arguments: command
                .get_args()
                .map(std::borrow::ToOwned::to_owned)
                .collect(),
        }
    }
}

impl fmt::Display for Invocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.program.to_string_lossy())?;
        for argument in &self.arguments {
            write!(formatter, " {}", argument.to_string_lossy())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario;

    scenario! {
        fn a_run_is_read_from_its_test_binary_and_named_back_to_cargo;

        phase the_default_profile_is_built_where_the_tests_were;
        {
            let build = Build::of(Path::new("/work/target/debug/deps/session-4f2c")).unwrap();
            assert_eq!(build.directory, Path::new("/work/target/debug"));
            assert!(
                build.profile_arguments().is_empty(),
                "the default profile needs no argument: {:?}",
                build.profile_arguments()
            );
            assert_eq!(
                build.binary("osk"),
                Path::new("/work/target/debug").join(format!("osk{}", env::consts::EXE_SUFFIX))
            );
        }

        phase the_release_and_named_profiles_are_named;
        {
            let release = Build::of(Path::new("/work/target/release/deps/session-4f2c")).unwrap();
            assert_eq!(release.profile_arguments(), [OsString::from("--release")]);
            let named = Build::of(Path::new("/work/target/ci/deps/session-4f2c")).unwrap();
            assert_eq!(
                named.profile_arguments(),
                [OsString::from("--profile"), OsString::from("ci")]
            );
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
