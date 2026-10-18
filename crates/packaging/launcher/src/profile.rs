//! The system read profile — the only host paths a sandboxed app can
//! see without a user's consent.
//!
//! The set is fixed and independent of the manifest because these paths are a
//! property of running a GUI app, not of an individual package. Letting a
//! package select host paths would let it define its own containment boundary.
//! An app that needs something outside the profile asks the user through the
//! capability system ([`crate::capabilities`]).
//!
//! Two rules govern what may be added: **no user content** (fonts,
//! driver ICDs, the C library and the CA bundle are the same bytes for
//! every person; `~/Documents` is not and never enters here), and
//! **every GUI app needs it** — otherwise it's a capability, not a
//! profile entry, or the profile drifts back into being the union of
//! what apps happened to ask for.

use std::path::{Path, PathBuf};

/// Read-only host paths every sandboxed app gets.
///
/// Bound with `--ro-bind-try`: an entry is a *permission*, not an
/// assertion the path exists on this host — `/usr/share/nvidia` on an
/// AMD box is normal and not a reason to refuse a launch.
pub const SYSTEM_READ_PATHS: &[&str] = &[
    // Dynamic loader + C library: an app whose ELF can't find its
    // interpreter doesn't start — the floor below which nothing runs.
    "/usr/lib",
    "/usr/lib64",
    "/lib",
    "/lib64",
    "/usr/lib/locale",
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/ld.so.conf.d",
    // Text rendering: the toolkit renders every string through
    // cosmic-text; without fonts an app draws empty boxes.
    "/usr/share/fonts",
    "/usr/share/fontconfig",
    "/etc/fonts",
    // Spelling dictionaries: every toolkit text field checks spelling
    // in-process against the system Hunspell dictionaries (the
    // `spellcheck` crate's `DICTIONARY_DIRS`). Without them a sandboxed app
    // silently checks nothing outside English.
    "/usr/share/hunspell",
    "/usr/share/myspell",
    // Keyboard layout data: `xkb_context_new` returns NULL without its
    // default include path, and winit unwraps that into a panic — every
    // sandboxed Wayland app dies before painting. `X11/locale` carries
    // Compose tables for dead keys, so the two travel together.
    "/usr/share/X11/xkb",
    "/usr/share/X11/locale",
    // GPU loader manifests + driver data (vendor-neutral); which device is
    // narrowed separately by `push_gpu_selection`.
    "/usr/share/vulkan",
    "/usr/share/glvnd",
    "/usr/share/drirc.d",
    "/usr/share/libdrm",
    "/usr/share/nvidia",
    "/etc/vulkan",
    // Mesa and the Vulkan loader enumerate PCI devices and read driver
    // attributes out of sysfs. There is no interface that replaces it.
    "/sys",
    // Locale + machine facts: `/etc/machine-id` is read by graphics/D-Bus
    // client libraries at startup — identifies the machine, not the person.
    "/etc/machine-id",
    "/etc/os-release",
    "/usr/share/zoneinfo",
    "/etc/localtime",
    // TLS trust: whether an app reaches the network is decided by
    // `[sandbox.net]` and the egress gate. The CA bundle only decides
    // whether a *permitted* connection verifies — withholding it produces
    // certificate errors, not security, and it's the same bytes for every user.
    // Fedora's NSS trust module is a public library in /usr/lib64 whose
    // symlink passes through an architecture-specific system alternative.
    // Bind only the public library files, so dlopen resolves the CA provider.
    "/etc/alternatives/libnssckbi.so",
    "/etc/alternatives/libnssckbi.so.x86_64",
    "/etc/alternatives/libnssckbi.so.aarch64",
    "/etc/pki",
    "/etc/ssl",
    "/etc/crypto-policies",
];

/// Read-only paths added **only** when the launch shares the host
/// network namespace.
///
/// These describe the network the machine is on, not the machine
/// itself, so they aren't part of what every app gets: an app under a
/// host-scoped grant has an empty namespace and never resolves
/// anything itself (the gate resolves after the name passes policy),
/// and an app with no network has nothing to resolve. Only the
/// unrestricted case has a resolver to configure — still a system
/// decision, derived from the egress decision already made.
pub const NETWORK_READ_PATHS: &[&str] = &["/etc/resolv.conf", "/etc/hosts"];

/// GPU device nodes, bound with `--dev-bind-try` for an app granted the
/// `gpu` capability ([`crate::gpu::granted`]) and for no other.
///
/// A toolkit app does not open the GPU: its room's render service draws its
/// frames (`tilo-render-service`). The GPU kernel driver is among the most
/// exploited kernel surfaces, so an app reaches it only when it declares it
/// needs to (a web engine, video, games, 3D, ML). `--dev /dev` mounts a
/// fresh, near-empty devtmpfs, so these nodes exist in a sandbox only when
/// [`push_gpu_devices`] binds them; `push_gpu_selection`'s narrowing lands
/// on top, so such an app never sees more than one GPU.
pub const GPU_DEVICE_PATHS: &[&str] = &[
    "/dev/dri",
    "/dev/nvidia0",
    "/dev/nvidiactl",
    "/dev/nvidia-modeset",
    "/dev/nvidia-uvm",
    "/dev/nvidia-uvm-tools",
    "/dev/nvidia-caps",
];

/// Paths below the profile's binds that an app must not see, each covered by
/// an empty read-only tmpfs.
///
/// `/sys/fs/cgroup` is the cgroup hierarchy: the session's app tree and
/// every app's cgroup in it (`crate::app_cgroup`), named by app id with its
/// memory and CPU counters. Read-only already (the `/sys` bind is), it would
/// still tell an app which other apps run and how much they use, and a
/// writable cgroupfs is the one place a process could move or reconfigure
/// cgroups from. Nothing an app runs needs it: `available_parallelism` and
/// the engines' memory probes fall back to the affinity mask and
/// `/proc/meminfo`. Its own place stays visible as `/proc/self/cgroup`,
/// which reads `0::/` in the cgroup namespace bwrap gives it.
pub const HIDDEN_PATHS: &[&str] = &["/sys/fs/cgroup"];

/// Push the profile's binds onto a bwrap argv.
///
/// Callers must splice this in *before* the trailing entrypoint token —
/// bwrap treats everything after it as the child's argv.
pub fn push_system_profile(argv: &mut Vec<String>, shares_host_network: bool) {
    for path in SYSTEM_READ_PATHS {
        push(argv, "--ro-bind-try", path, path);
    }
    for path in HIDDEN_PATHS {
        argv.extend(["--tmpfs", path, "--remount-ro", path].map(str::to_string));
    }
    if shares_host_network {
        for path in NETWORK_READ_PATHS {
            push(argv, "--ro-bind-try", path, path);
        }
    }
}

/// Bind the GPU device nodes, for an app granted `gpu` only.
pub fn push_gpu_devices(argv: &mut Vec<String>) {
    for path in GPU_DEVICE_PATHS {
        push(argv, "--dev-bind-try", path, path);
    }
}

/// The profile as Landlock read-only rules.
///
/// A bind the ruleset does not name is visible and unreadable — every
/// `open()` on it returns `EACCES`, which reads from inside the app as
/// a corrupted system rather than a policy. So the two layers are
/// derived from one list.
pub fn landlock_ro_paths(shares_host_network: bool) -> Vec<PathBuf> {
    let extra = if shares_host_network {
        NETWORK_READ_PATHS
    } else {
        &[]
    };
    SYSTEM_READ_PATHS
        .iter()
        .chain(extra.iter())
        .map(PathBuf::from)
        .collect()
}

/// The GPU device nodes as Landlock read-write rules, for an app granted
/// `gpu`. A GPU context is opened `O_RDWR`; a read-only rule surfaces at
/// device-open time and looks exactly like "no GPU present".
pub fn landlock_rw_device_paths() -> Vec<PathBuf> {
    GPU_DEVICE_PATHS.iter().map(PathBuf::from).collect()
}

/// Landlock read-write rules for an engine that builds its own sandbox
/// inside ours, keyed on the seccomp profile the launch was granted.
///
/// Chromium's zygote writes `setgroups`, `uid_map` and `gid_map` through
/// `/proc/self` from each child it has just forked into a new user
/// namespace. Those children have fresh PIDs, so no rule narrower than the
/// sandbox's own `/proc` — mounted for its private PID namespace, listing
/// only its own processes — reaches them. Without it the engine refuses to
/// start rather than run its renderers unsandboxed.
pub fn landlock_rw_nested_sandbox_paths(profile: crate::seccomp::SeccompProfile) -> Vec<PathBuf> {
    if profile.allows_nested_sandbox() {
        vec![PathBuf::from("/proc")]
    } else {
        Vec::new()
    }
}

/// Whether `path` is covered by the read profile — used to explain a
/// refusal ("this is already yours") rather than to grant anything.
pub fn covers(path: &Path) -> bool {
    SYSTEM_READ_PATHS
        .iter()
        .chain(NETWORK_READ_PATHS.iter())
        .any(|p| path == Path::new(p) || path.starts_with(p))
}

fn push(argv: &mut Vec<String>, kind: &str, src: &str, dst: &str) {
    argv.push(kind.to_string());
    argv.push(src.to_string());
    argv.push(dst.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_profile_is_read_only_and_names_no_user_path() {
        // Phase `the_profile_names_no_user_path`.
        // The profile is a *system* list. If a user path ever appears in it,
        // every app on the machine silently gained access — checked
        // mechanically rather than by review.
        {
            for p in SYSTEM_READ_PATHS
                .iter()
                .chain(NETWORK_READ_PATHS.iter())
                .chain(GPU_DEVICE_PATHS.iter())
            {
                assert!(
                    p.starts_with('/'),
                    "{p} is not absolute; a relative entry resolves against \
                     whatever cwd the launcher inherited"
                );
                assert!(
                    !p.contains('~') && !p.contains('$'),
                    "{p} contains a placeholder; the profile is expanded by \
                     nobody and must be literal"
                );
                assert!(
                    !p.starts_with("/home") && !p.starts_with("/root"),
                    "{p} is a home directory: the profile must carry no user content"
                );
                assert!(
                    !p.contains(".."),
                    "{p} contains `..`; profile entries are literal paths"
                );
            }
        }
        // Phase `the_profile_does_not_contain_the_root`.
        // `/` (or any prefix of every path) in the profile would hand the
        // whole host to every app — the bug this file exists to remove. Cheap
        // to assert, catastrophic to miss.
        {
            for p in SYSTEM_READ_PATHS
                .iter()
                .chain(NETWORK_READ_PATHS.iter())
                .chain(GPU_DEVICE_PATHS.iter())
            {
                assert_ne!(*p, "/", "the root filesystem is not a profile entry");
                assert!(
                    Path::new(p).components().count() >= 2,
                    "{p} is a top-level directory; the profile names specific \
                     subtrees, not whole filesystems"
                );
            }
        }
        // Phase `the_profile_binds_read_only_and_devices_as_devices`.
        {
            let mut argv = Vec::new();
            push_system_profile(&mut argv, false);
            assert!(
                argv.windows(3).any(|w| w[0] == "--ro-bind-try"
                    && w[1] == "/usr/share/fonts"
                    && w[2] == "/usr/share/fonts"),
                "fonts must be read-only: {argv:?}"
            );
            // Without it every sandboxed Wayland app panics on `XKBNotFound`
            // before it paints.
            assert!(
                argv.windows(3).any(|w| w[0] == "--ro-bind-try"
                    && w[1] == "/usr/share/X11/xkb"
                    && w[2] == "/usr/share/X11/xkb"),
                "the xkb keymap data must be read-only in the sandbox: {argv:?}"
            );
            // Every text field checks spelling in-process; without the
            // dictionaries (bound and Landlock-readable) non-English
            // checking silently finds nothing.
            for dictionaries in ["/usr/share/hunspell", "/usr/share/myspell"] {
                assert!(
                    argv.windows(3).any(|w| w[0] == "--ro-bind-try"
                        && w[1] == dictionaries
                        && w[2] == dictionaries),
                    "{dictionaries} must be read-only in the sandbox: {argv:?}"
                );
                assert!(
                    landlock_ro_paths(false).contains(&PathBuf::from(dictionaries)),
                    "{dictionaries} must be readable under Landlock"
                );
            }
            // The GPU is not part of what every app gets: an app draws
            // through its room's render service unless it declares `gpu`.
            assert!(
                !argv
                    .iter()
                    .any(|a| a.starts_with("/dev/dri") || a.starts_with("/dev/nvidia")),
                "no GPU node in the default profile: {argv:?}"
            );
            let mut gpu = Vec::new();
            push_gpu_devices(&mut gpu);
            assert!(
                gpu.windows(3)
                    .any(|w| w[0] == "--dev-bind-try" && w[1] == "/dev/dri"),
                "a `gpu` app's GPU node needs a device bind: {gpu:?}"
            );
            // The cgroup hierarchy is covered, after `/sys` is bound, by an
            // empty tree no app can write either.
            let sys = argv
                .windows(3)
                .position(|w| w[0] == "--ro-bind-try" && w[1] == "/sys")
                .expect("/sys is bound");
            let hidden = argv
                .windows(4)
                .position(|w| {
                    w == [
                        "--tmpfs",
                        "/sys/fs/cgroup",
                        "--remount-ro",
                        "/sys/fs/cgroup",
                    ]
                })
                .expect("/sys/fs/cgroup is covered and read-only");
            assert!(sys < hidden, "the cover must come after the bind: {argv:?}");
            // Nothing in the profile may be writable. A `--bind` here would
            // be a system directory an app can rewrite.
            assert!(
                !argv.iter().any(|a| a == "--bind" || a == "--bind-try"),
                "no profile entry may be read-write: {argv:?}"
            );
        }
    }
}

#[cfg(test)]
mod nested_sandbox_tests {
    use super::*;
    use crate::seccomp::SeccompProfile;

    #[test]
    fn only_the_web_engine_profile_may_write_proc() {
        for profile in [SeccompProfile::TiloDefault, SeccompProfile::TiloJit] {
            assert!(
                landlock_rw_nested_sandbox_paths(profile).is_empty(),
                "{} must not reach /proc",
                profile.as_str()
            );
        }
        assert_eq!(
            landlock_rw_nested_sandbox_paths(SeccompProfile::TiloWebEngine),
            vec![PathBuf::from("/proc")]
        );
    }
}

#[cfg(test)]
mod network_profile_tests {
    use super::*;

    #[test]
    fn network_profile_resolver_files_and_landlock_rules_match_the_binds() {
        // Phase `the_resolver_files_appear_only_for_an_unrestricted_network`.
        // The machine's resolver configuration is not part of what every app
        // gets: an app with no network has nothing to resolve, and one under
        // a host-scoped grant resolves through the gate — either way, handing
        // out the resolver list gives no functional gain.
        {
            let mut without = Vec::new();
            push_system_profile(&mut without, false);
            for path in NETWORK_READ_PATHS {
                assert!(
                    !without.iter().any(|a| a == path),
                    "{path} must not be bound for a launch with no host network: {without:?}"
                );
            }

            let mut with = Vec::new();
            push_system_profile(&mut with, true);
            for path in NETWORK_READ_PATHS {
                assert!(
                    with.windows(3)
                        .any(|w| w[0] == "--ro-bind-try" && w[1] == *path && w[2] == *path),
                    "{path} must be bound for a launch that shares the host network: {with:?}"
                );
            }
        }
        // Phase `the_landlock_rules_match_the_binds`.
        // The bind list and the Landlock ruleset are derived from the same
        // two constants, so they cannot disagree about what is readable.
        {
            for shares in [false, true] {
                let mut argv = Vec::new();
                push_system_profile(&mut argv, shares);
                for rule in landlock_ro_paths(shares) {
                    let s = rule.display().to_string();
                    assert!(
                        argv.contains(&s),
                        "landlock names {s} but nothing binds it (shares={shares})"
                    );
                }
            }
        }
    }
}
