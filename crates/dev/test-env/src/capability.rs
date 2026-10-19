//! Host capabilities a test may need, and lanes that must provide them.
//!
//! Some tests need something only certain machines have: a Vulkan adapter,
//! an audio endpoint, media codecs, `/dev/fuse`, fanotify permission, a
//! filesystem with fs-verity. On a developer machine without it, the test
//! should say so and pass. On a CI
//! lane that is set up to provide it, a missing capability is a broken lane,
//! and a skipped test must not look like a passing one.
//!
//! [`require_capability!`](crate::require_capability) does both: it skips
//! with a message naming the capability, unless `TILO_REQUIRE_<CAP>=1` is set,
//! in which case it fails the test instead.

use std::fmt;

/// A host capability that some tests cannot run without.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Capability {
    /// A usable GPU adapter (CI: Mesa llvmpipe/lavapipe software Vulkan).
    Gpu,
    /// An audio output endpoint (CI: `tools/with-headless-audio.sh`).
    Audio,
    /// Media codecs and `GStreamer` elements beyond the base plugins.
    Media,
    /// A mountable FUSE device (`/dev/fuse` and `fusermount3`).
    Fuse,
    /// The FUSE control filesystem at `/sys/fs/fuse/connections`, through
    /// which a test can abort its own connection.
    FuseControl,
    /// Permission to create a fanotify group.
    Fanotify,
    /// A writable directory on a filesystem with fs-verity (btrfs, or ext4
    /// with the `verity` feature): `TILO_FSVERITY_DIR`, else the test's
    /// temporary directory.
    FsVerity,
    /// A private `PipeWire` with a session manager to serve a headless screen
    /// (`pipewire`, `wireplumber`, `pw-dump` and `GStreamer`'s `PipeWire`
    /// plugin).
    ScreenCapture,
    /// Giving a file in the temporary directory to another user and becoming
    /// it ([`chown_to`], [`switch_identity`]): root with `CAP_CHOWN` and
    /// `CAP_SETUID`/`CAP_SETGID` on a filesystem that can hold that user. Root
    /// in a job sandbox without capabilities is uid 0 and still cannot.
    Chown,
    /// Creating the user or mount namespaces the fixtures use: a real root
    /// process for a mount namespace, or an unprivileged caller for a user
    /// namespace (bubblewrap, `unshare --user`). The job sandbox refuses both.
    UserNamespace,
}

impl Capability {
    /// The variable that turns a missing capability into a test failure.
    #[must_use]
    pub const fn variable(self) -> &'static str {
        match self {
            Self::Gpu => "TILO_REQUIRE_GPU",
            Self::Audio => "TILO_REQUIRE_AUDIO",
            Self::Media => "TILO_REQUIRE_MEDIA",
            Self::Fuse => "TILO_REQUIRE_FUSE",
            Self::FuseControl => "TILO_REQUIRE_FUSECTL",
            Self::Fanotify => "TILO_REQUIRE_FANOTIFY",
            Self::FsVerity => "TILO_REQUIRE_FSVERITY",
            Self::ScreenCapture => "TILO_REQUIRE_SCREEN_CAPTURE",
            Self::Chown => "TILO_REQUIRE_CHOWN",
            Self::UserNamespace => "TILO_REQUIRE_USERNS",
        }
    }

    /// Whether this environment promises the capability
    /// (`TILO_REQUIRE_<CAP>=1`).
    #[must_use]
    pub fn required(self) -> bool {
        std::env::var_os(self.variable()).is_some_and(|value| value == "1")
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Gpu => "GPU",
            Self::Audio => "audio",
            Self::Media => "media codecs",
            Self::Fuse => "FUSE",
            Self::FuseControl => "the FUSE control filesystem",
            Self::Fanotify => "fanotify",
            Self::FsVerity => "fs-verity",
            Self::ScreenCapture => "screen capture",
            Self::Chown => "giving files to another user",
            Self::UserNamespace => "user and mount namespaces",
        })
    }
}

/// Whether this process can give a file in the temporary directory to `uid`,
/// tried on a scratch file there: being uid 0 is not enough without
/// `CAP_CHOWN`, nor is `CAP_CHOWN` where the filesystem cannot represent `uid`
/// (an idmapped mount maps only some users).
///
/// # Errors
///
/// Returns why the scratch file could not be made or given away.
#[cfg(unix)]
pub fn chown_to(uid: u32) -> std::io::Result<()> {
    let file = tempfile::NamedTempFile::new()?;
    std::os::unix::fs::chown(file.path(), Some(uid), None)
}

/// Whether this process can give a file in the temporary directory to `uid`
/// and then become that user, tried on a scratch file and a throwaway child.
///
/// # Errors
///
/// Returns why the chown or the identity switch failed.
#[cfg(unix)]
pub fn switch_identity(uid: u32) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt as _;
    chown_to(uid)?;
    let status = std::process::Command::new("true")
        .uid(uid)
        .gid(uid)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "becoming uid {uid} exited with {status}"
        )))
    }
}

/// Whether this process can create a user namespace with the unprivileged
/// mapping the fixtures use, tried on a throwaway `true`.
///
/// # Errors
///
/// Returns why `unshare` could not be started or exited non-zero.
#[cfg(target_os = "linux")]
pub fn user_namespace() -> std::io::Result<()> {
    namespace(
        "unshare",
        &[
            "--user",
            "--map-user=19991",
            "--map-group=19992",
            "--",
            "true",
        ],
    )
}

/// Whether this process can create a private mount namespace, trying `unshare`
/// as the caller and then `sudo -n` for an unprivileged developer host.
///
/// # Errors
///
/// Returns why neither `unshare` nor `sudo -n unshare` succeeded.
#[cfg(target_os = "linux")]
pub fn mount_namespace() -> std::io::Result<()> {
    match namespace(
        "unshare",
        &[
            "--mount",
            "--fork",
            "--propagation",
            "private",
            "--",
            "true",
        ],
    ) {
        Ok(()) => Ok(()),
        Err(direct) => namespace(
            "sudo",
            &[
                "-n",
                "--",
                "unshare",
                "--mount",
                "--fork",
                "--propagation",
                "private",
                "--",
                "true",
            ],
        )
        .map_err(|sudo| {
            std::io::Error::other(format!("unshare: {direct}; sudo -n unshare: {sudo}"))
        }),
    }
}

/// Whether this process can start bubblewrap with a user namespace of its own,
/// tried on the read-only root the bundle fixtures use.
///
/// # Errors
///
/// Returns why `bwrap` could not be started or exited non-zero.
#[cfg(target_os = "linux")]
pub fn bubblewrap() -> std::io::Result<()> {
    namespace(
        "bwrap",
        &[
            "--die-with-parent",
            "--unshare-all",
            "--ro-bind",
            "/",
            "/",
            "--",
            "true",
        ],
    )
}

#[cfg(target_os = "linux")]
fn namespace(program: &str, args: &[&str]) -> std::io::Result<()> {
    let mut child = std::process::Command::new(program).args(args).spawn()?;
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(std::io::Error::other(format!(
                    "{program} exited with {status}"
                )))
            };
        }
        if started.elapsed() >= std::time::Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("{program} did not finish within 10s"),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Report that `capability` is unavailable: print why the calling test is
/// skipped, or panic if this environment requires the capability.
///
/// [`require_capability!`](crate::require_capability) calls this and then
/// returns from the test. Call it directly where the test cannot simply
/// return, e.g. one returning `Result`.
///
/// # Panics
///
/// When `TILO_REQUIRE_<CAP>=1` is set.
#[track_caller]
pub fn capability_unavailable(capability: Capability, reason: fmt::Arguments<'_>) {
    let location = std::panic::Location::caller();
    assert!(
        !capability.required(),
        "{} is set but {capability} is unavailable: {reason}",
        capability.variable(),
    );
    eprintln!(
        "skipping test at {location}: {capability} unavailable: {reason} \
         (set {}=1 to make this a failure)",
        capability.variable(),
    );
}

/// The outcome of probing for a capability: `true`, `Some(value)` and
/// `Ok(value)` mean available; the error of an `Err` is added to the message.
pub trait Probe {
    /// What the test gets when the capability is available.
    type Value;

    /// Split the probe into its value or an optional detail message.
    ///
    /// # Errors
    ///
    /// Returns the detail (if any) when the capability is unavailable.
    fn into_probe(self) -> Result<Self::Value, Option<String>>;
}

impl Probe for bool {
    type Value = ();

    fn into_probe(self) -> Result<(), Option<String>> {
        if self { Ok(()) } else { Err(None) }
    }
}

impl<T> Probe for Option<T> {
    type Value = T;

    fn into_probe(self) -> Result<T, Option<String>> {
        self.ok_or(None)
    }
}

impl<T, E: fmt::Display> Probe for Result<T, E> {
    type Value = T;

    fn into_probe(self) -> Result<T, Option<String>> {
        self.map_err(|error| Some(error.to_string()))
    }
}

/// Evaluate a capability probe; on success yield its value, otherwise skip
/// the test (return) or, under `TILO_REQUIRE_<CAP>=1`, fail it.
///
/// This example uses an available fixture adapter to demonstrate yielding
/// the probed value.
///
/// ```
/// use tilo_test_env::require_capability;
///
/// fn adapter() -> Option<&'static str> { Some("fixture adapter") }
///
/// fn renders_a_frame() {
///     let adapter = require_capability!(Gpu, adapter(), "no wgpu adapter for offscreen render");
///     assert_eq!(adapter, "fixture adapter");
/// }
/// # renders_a_frame();
/// ```
///
/// The probe may be a `bool`, an `Option` or a `Result` whose error is
/// appended to the message. The expansion contains `return;`, so use it in
/// tests returning `()`; elsewhere call [`capability_unavailable`].
#[macro_export]
macro_rules! require_capability {
    ($capability:ident, $probe:expr, $($reason:tt)+) => {
        match $crate::Probe::into_probe($probe) {
            ::core::result::Result::Ok(value) => value,
            ::core::result::Result::Err(detail) => {
                let reason = ::std::format!($($reason)+);
                match detail {
                    ::core::option::Option::Some(detail) => $crate::capability_unavailable(
                        $crate::Capability::$capability,
                        ::core::format_args!("{reason}: {detail}"),
                    ),
                    ::core::option::Option::None => $crate::capability_unavailable(
                        $crate::Capability::$capability,
                        ::core::format_args!("{reason}"),
                    ),
                }
                return;
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::Capability;
    use crate::ScopedEnv;
    use std::cell::Cell;

    fn probe_gpu(available: Option<u32>) -> Option<u32> {
        let seen = Cell::new(None);
        let run = || {
            let value = require_capability!(Gpu, available, "fixture adapter {}", 1);
            seen.set(Some(value));
        };
        run();
        seen.get()
    }

    #[test]
    fn an_available_capability_yields_the_probed_value() {
        let mut env = ScopedEnv::new();
        env.set("TILO_REQUIRE_GPU", "1");
        assert_eq!(probe_gpu(Some(7)), Some(7));
    }

    #[test]
    fn an_unavailable_capability_skips_unless_required() {
        let mut env = ScopedEnv::new();
        env.remove("TILO_REQUIRE_GPU");
        assert!(!Capability::Gpu.required());
        assert_eq!(probe_gpu(None), None);
        env.set("TILO_REQUIRE_GPU", "0");
        assert_eq!(probe_gpu(None), None);
    }

    #[test]
    fn a_required_capability_fails_the_test_with_its_reason() {
        let mut env = ScopedEnv::new();
        env.set("TILO_REQUIRE_FUSE", "1");
        let failure = std::panic::catch_unwind(|| {
            fn mount() {
                require_capability!(
                    Fuse,
                    Err::<(), _>("no /dev/fuse"),
                    "cannot mount the fixture"
                );
            }
            mount();
        })
        .expect_err("a required capability must fail the test");
        let message = failure
            .downcast_ref::<String>()
            .expect("formatted panic message");
        assert_eq!(
            message,
            "TILO_REQUIRE_FUSE is set but FUSE is unavailable: \
             cannot mount the fixture: no /dev/fuse"
        );
    }

    #[test]
    fn a_boolean_probe_gates_without_a_value() {
        let mut env = ScopedEnv::new();
        env.remove("TILO_REQUIRE_AUDIO");
        let reached = Cell::new(false);
        let run = |available: bool| {
            require_capability!(Audio, available, "no sink");
            reached.set(true);
        };
        run(false);
        assert!(!reached.get());
        run(true);
        assert!(reached.get());
    }
}
