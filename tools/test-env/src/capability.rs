//! Host capabilities a test may need, and jobs that must provide them. Some tests need something
//! only certain machines have (a Vulkan adapter, an audio endpoint, media codecs, `/dev/fuse`,
//! fanotify permission, a filesystem with fs-verity): on a developer machine without it the test
//! says so and passes, while on a CI job set up to provide it a missing capability is a broken
//! job, and a skipped test must not look like a passing one.
//! [`require_capability!`](crate::require_capability) does both: it skips with a message naming the
//! capability, unless `TILO_REQUIRE_<CAP>=1` is set, in which case it fails the test instead.
//!
//! Two capabilities are about the account a test executes as, not about hardware: [`Capability::NonRoot`]
//! and [`Capability::Root`]. A test that observes what file permissions refuse, or what only root may
//! do, has an outcome that depends on who executes it, so it states that premise and the micro-VM tier
//! executes it as that account (`setpriv --reuid=1000` for the first, the guest's own root for the
//! second) instead of letting a root CI job pass it vacuously.

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
    XServer,
    NonRoot,
    Root,
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
        Self::XServer,
        Self::NonRoot,
        Self::Root,
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
            Self::XServer => "TILO_REQUIRE_XSERVER",
            Self::NonRoot => "TILO_REQUIRE_NONROOT",
            Self::Root => "TILO_REQUIRE_ROOT",
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
            Self::XServer => "an X server for Wayland (xwayland-satellite and Xwayland)",
            Self::NonRoot => "an ordinary account that file permissions confine",
            Self::Root => "real root, which file permissions do not confine",
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

/// Whether this process's file permissions confine it: an ordinary account cannot read a directory
/// whose mode is 0000, while root holding `CAP_DAC_OVERRIDE` reads it anyway. A capability-dropped
/// job sandbox's root is confined too, which is why this alone cannot tell the two accounts apart.
#[cfg(target_os = "linux")]
fn file_permissions_confine() -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt as _;
    let directory = tempfile::tempdir()?;
    let refused = directory.path().join("no-access");
    std::fs::create_dir(&refused)?;
    std::fs::set_permissions(&refused, std::fs::Permissions::from_mode(0o000))?;
    let read = std::fs::read_dir(&refused);
    std::fs::set_permissions(&refused, std::fs::Permissions::from_mode(0o700))?;
    match read {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Ok(true),
        Err(error) => Err(error),
    }
}

/// The effective user id, read back from a file this process creates.
#[cfg(target_os = "linux")]
fn effective_uid() -> std::io::Result<u32> {
    use std::os::unix::fs::MetadataExt as _;
    let file = tempfile::NamedTempFile::new()?;
    Ok(file.as_file().metadata()?.uid())
}

/// Which account a test's premise names.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Account {
    Ordinary,
    Root,
}

/// Whether a process with this effective user id and this confinement is the named account.
#[cfg(target_os = "linux")]
fn premise(account: Account, uid: u32, confined: bool) -> Result<(), String> {
    match account {
        Account::Ordinary if uid == 0 => Err(
            "this process executes as uid 0, so it is root and not the ordinary account the test \
             needs"
                .into(),
        ),
        Account::Ordinary if !confined => Err(format!(
            "uid {uid} reads a mode-0000 directory, so it holds CAP_DAC_OVERRIDE"
        )),
        Account::Root if uid != 0 => Err(format!("this process executes as uid {uid}, not root")),
        Account::Root if confined => Err(
            "file permissions confine this uid 0 process, so its capabilities were dropped".into(),
        ),
        Account::Ordinary | Account::Root => Ok(()),
    }
}

/// The premise of a test that observes what an ordinary account may not read. It fails for real
/// root, and also for a job sandbox's root, which executes as uid 0 but holds no capability.
#[cfg(target_os = "linux")]
pub fn non_root_identity() -> std::io::Result<()> {
    premise(
        Account::Ordinary,
        effective_uid()?,
        file_permissions_confine()?,
    )
    .map_err(std::io::Error::other)
}

/// The premise of a test that observes what only root may do. It fails for an ordinary account and
/// for a job sandbox's root, whose dropped capabilities leave it confined like one.
#[cfg(target_os = "linux")]
pub fn root_identity() -> std::io::Result<()> {
    premise(Account::Root, effective_uid()?, file_permissions_confine()?)
        .map_err(std::io::Error::other)
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
    ("non-root-identity", non_root_identity),
    ("root-identity", root_identity),
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
    use crate::scenario;
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

    scenario! {
        fn a_capability_checks_a_test_on_what_the_machine_has;

        phase an_available_capability_yields_the_probed_value;
        {
            let mut env = ScopedEnv::new();
            env.set("TILO_REQUIRE_GPU", "1");
            assert_eq!(probe_gpu(Some(7)), Some(7));
        }

        phase an_unavailable_capability_skips_unless_required;
        {
            let mut env = ScopedEnv::new();
            env.remove("TILO_REQUIRE_GPU");
            assert!(!Capability::Gpu.required());
            assert_eq!(probe_gpu(None), None);
            env.set("TILO_REQUIRE_GPU", "0");
            assert_eq!(probe_gpu(None), None);
        }

        phase a_boolean_probe_checks_without_a_value;
        {
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

    #[cfg(target_os = "linux")]
    #[test]
    fn an_identity_premise_states_the_account_this_process_executes_as() {
        use super::{
            Account, effective_uid, file_permissions_confine, non_root_identity, premise,
            root_identity,
        };
        for (account, uid, confined) in [
            (Account::Ordinary, 1000, true),
            (Account::Ordinary, 0, true),
            (Account::Ordinary, 1000, false),
            (Account::Root, 0, false),
            (Account::Root, 1000, true),
            (Account::Root, 0, true),
        ] {
            // Only the ordinary account of a real machine, and real root, satisfy a premise.
            assert_eq!(
                premise(account, uid, confined).is_ok(),
                matches!(
                    (account, uid, confined),
                    (Account::Ordinary, 1000, true) | (Account::Root, 0, false)
                ),
                "{account:?} as uid {uid}, confined {confined}"
            );
        }
        // Here: the ordinary account, real root, or a job sandbox's root, which executes as
        // uid 0 with its capabilities dropped and so is neither.
        let ordinary = non_root_identity();
        let root = root_identity();
        let sandbox_root = effective_uid().unwrap() == 0 && file_permissions_confine().unwrap();
        assert_eq!(
            usize::from(ordinary.is_ok()) + usize::from(root.is_ok()),
            usize::from(!sandbox_root),
            "one account premise holds here unless this is a job sandbox's root: non-root \
             {ordinary:?}, root {root:?}, sandbox root {sandbox_root}"
        );
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
}
