//! Host capabilities a test may need, and lanes that must provide them. Some tests need something
//! only certain machines have (a Vulkan adapter, an audio endpoint, media codecs, `/dev/fuse`,
//! fanotify permission, a filesystem with fs-verity): on a developer machine without it the test
//! says so and passes, while on a CI lane set up to provide it a missing capability is a broken
//! lane, and a skipped test must not look like a passing one.
//! [`require_capability!`](crate::require_capability) does both: it skips with a message naming the
//! capability, unless `TILO_REQUIRE_<CAP>=1` is set, in which case it fails the test instead.

#[cfg(unix)]
use crate::SpawnUnderEnvLock as _;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Capability {
    Gpu,
    Audio,
    Media,
    Fuse,
    FuseControl,
    Fanotify,
    FsVerity,
    ScreenCapture,
    Chown,
    UserNamespace,
    NamespaceEntry,
    TrustedLoader,
    Swtpm,
    KernelKeyring,
    NetworkTunnel,
}

impl Capability {
    pub const ALL: &'static [Self] = &[
        Self::Gpu,
        Self::Audio,
        Self::Media,
        Self::Fuse,
        Self::FuseControl,
        Self::Fanotify,
        Self::FsVerity,
        Self::ScreenCapture,
        Self::Chown,
        Self::UserNamespace,
        Self::NamespaceEntry,
        Self::TrustedLoader,
        Self::Swtpm,
        Self::KernelKeyring,
        Self::NetworkTunnel,
    ];

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
            Self::NamespaceEntry => "TILO_REQUIRE_SETNS",
            Self::TrustedLoader => "TILO_REQUIRE_LOADER",
            Self::Swtpm => "TILO_REQUIRE_SWTPM",
            Self::KernelKeyring => "TILO_REQUIRE_KEYRING",
            Self::NetworkTunnel => "TILO_REQUIRE_TUN",
        }
    }

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
            Self::NamespaceEntry => "entering namespaces (setns)",
            Self::TrustedLoader => "a trusted native loader",
            Self::Swtpm => "an emulated TPM",
            Self::KernelKeyring => "the kernel keyring",
            Self::NetworkTunnel => "TUN devices with ip and nft",
        })
    }
}

#[cfg(unix)]
pub fn chown_to(uid: u32) -> std::io::Result<()> {
    let file = tempfile::NamedTempFile::new()?;
    std::os::unix::fs::chown(file.path(), Some(uid), None)
}

#[cfg(unix)]
pub fn switch_identity(uid: u32) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt as _;
    chown_to(uid)?;
    let status = std::process::Command::new("true")
        .uid(uid)
        .gid(uid)
        .status_locked()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "becoming uid {uid} exited with {status}"
        )))
    }
}

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
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--",
            "true",
        ],
    )
}

#[cfg(target_os = "linux")]
pub fn bubblewrap_with_proc() -> std::io::Result<()> {
    namespace(
        "bwrap",
        &[
            "--die-with-parent",
            "--unshare-user",
            "--uid",
            "1000",
            "--gid",
            "1000",
            "--unshare-pid",
            "--ro-bind",
            "/",
            "/",
            "--proc",
            "/proc",
            "--",
            "true",
        ],
    )
}

#[cfg(target_os = "linux")]
pub fn bubblewrap_overlay() -> std::io::Result<()> {
    let probe = std::env::temp_dir().join(format!(
        "tilo-test-env-overlay-probe-{}",
        std::process::id()
    ));
    let side = |name: &str| probe.join(name).to_string_lossy().into_owned();
    for name in ["lower", "upper", "work"] {
        std::fs::create_dir_all(probe.join(name))?;
    }
    let (lower, upper, work) = (side("lower"), side("upper"), side("work"));
    let probed = namespace(
        "bwrap",
        &[
            "--unshare-user",
            "--uid",
            "0",
            "--gid",
            "0",
            "--cap-add",
            "ALL",
            "--ro-bind",
            "/",
            "/",
            "--tmpfs",
            "/run",
            "--overlay-src",
            &lower,
            "--overlay",
            &upper,
            &work,
            "/run/probe",
            "--",
            "true",
        ],
    );
    let _ = namespace("chmod", &["-R", "u+rwx", &probe.to_string_lossy()]);
    std::fs::remove_dir_all(&probe)?;
    probed
}

#[cfg(target_os = "linux")]
pub fn network_tunnel() -> std::io::Result<()> {
    if !std::path::Path::new("/dev/net/tun").exists() {
        return Err(std::io::Error::other("no /dev/net/tun"));
    }
    namespace("ip", &["-V"])?;
    namespace(
        "unshare",
        &[
            "--user",
            "--map-root-user",
            "--net",
            "--",
            "nft",
            "list",
            "ruleset",
        ],
    )
}

#[cfg(target_os = "linux")]
pub type NamedProbe = (&'static str, fn() -> std::io::Result<()>);

#[cfg(target_os = "linux")]
pub const PROBES: &[NamedProbe] = &[
    ("user-namespace", user_namespace),
    ("mount-namespace", mount_namespace),
    ("bubblewrap", bubblewrap),
    ("bubblewrap-with-proc", bubblewrap_with_proc),
    ("bubblewrap-overlay", bubblewrap_overlay),
    ("network-tunnel", network_tunnel),
];

#[cfg(target_os = "linux")]
fn namespace(program: &str, args: &[&str]) -> std::io::Result<()> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .spawn_locked()?;
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

#[track_caller]
pub fn capability_unavailable(capability: Capability, reason: fmt::Arguments<'_>) {
    let location = std::panic::Location::caller();
    assert!(
        !capability.required(),
        "{} is set but {capability} is unavailable: {reason}. A test machine \
         without it leaves this test out through its entry in \
         .config/nextest-capabilities.toml",
        capability.variable(),
    );
    eprintln!(
        "skipping test at {location}: {capability} unavailable: {reason} \
         (set {}=1 to make this a failure)",
        capability.variable(),
    );
}

pub trait Probe {
    type Value;

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
             cannot mount the fixture: no /dev/fuse. A test machine without it \
             leaves this test out through its entry in \
             .config/nextest-capabilities.toml"
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
