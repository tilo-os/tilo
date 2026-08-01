//! The system read profile — the only host paths a sandboxed app can
//! see without a user's consent.
//!
//! Every manifest in the tree used to hand-list fonts, mesa, `/usr/lib`,
//! the Wayland socket and `/dev/dri`. That list was identical in 45 of
//! 46 packages, which is the tell: it is not a property of the app, it
//! is a property of *running a GUI program on this system*. Worse, it
//! was a place where a package named host paths, and the launcher bound
//! whatever it named. A package asking for `/` got `/`.
//!
//! So the set moved here. It is fixed, it is the same for every package,
//! and no manifest field feeds it. An app that needs something outside
//! it asks the user through the capability system
//! ([`crate::capabilities`]) — the one route, already built, already
//! consented to, already enforced.
//!
//! Two rules govern what may be added to this list:
//!
//! 1. **It carries no user content.** Fonts, driver ICDs, the C library
//!    and the CA bundle are the same bytes for every person on the
//!    machine. `~/Documents` is not, and never enters here.
//! 2. **Every GUI app needs it.** If only some apps need it, it is a
//!    capability, not a profile entry — otherwise the profile drifts
//!    back into being the union of what apps happened to ask for, which
//!    is the thing being removed.

use std::path::{Path, PathBuf};

/// Read-only host paths every sandboxed app gets.
///
/// Bound with `--ro-bind-try`: an entry is a *permission*, not an
/// assertion that the path exists on this host. `/usr/share/nvidia` on
/// an AMD box and `/etc/crypto-policies` on a non-Fedora base are both
/// normal, and neither is a reason to refuse a launch.
pub const SYSTEM_READ_PATHS: &[&str] = &[
    // ---- Dynamic loader + C library ------------------------------
    // An app whose ELF cannot find its interpreter does not start, so
    // this is the floor below which nothing runs at all.
    "/usr/lib",
    "/usr/lib64",
    "/lib",
    "/lib64",
    "/usr/lib/locale",
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/ld.so.conf.d",
    // ---- Text rendering ------------------------------------------
    // The toolkit renders every string through cosmic-text; without
    // fonts an app draws empty boxes, which is indistinguishable from
    // a broken app.
    "/usr/share/fonts",
    "/usr/share/fontconfig",
    "/etc/fonts",
    // ---- GPU: loader manifests + driver data ---------------------
    // Which *device* the app may open is narrowed separately by
    // `push_gpu_selection`; these are the vendor-neutral loader
    // manifests and driver config that say how to talk to it.
    "/usr/share/vulkan",
    "/usr/share/glvnd",
    "/usr/share/drirc.d",
    "/usr/share/libdrm",
    "/usr/share/nvidia",
    "/etc/vulkan",
    // Mesa and the Vulkan loader enumerate PCI devices and read driver
    // attributes out of sysfs. There is no interface that replaces it.
    "/sys",
    // ---- Locale + machine facts ----------------------------------
    // `/etc/machine-id` is read by graphics and D-Bus client libraries
    // on startup. It identifies the machine, not the person.
    "/etc/machine-id",
    "/etc/os-release",
    "/usr/share/zoneinfo",
    "/etc/localtime",
    // ---- TLS + name resolution -----------------------------------
    // Whether an app reaches the network at all is decided by
    // `[sandbox.net]` and the egress gate. These files only decide
    // whether a *permitted* connection can be verified and resolved —
    // withholding them from an app that already has egress buys
    // nothing and produces certificate errors instead of security.
    "/etc/pki",
    "/etc/ssl",
    "/etc/crypto-policies",
    "/etc/resolv.conf",
    "/etc/hosts",
];

/// Device nodes every GUI app gets, bound with `--dev-bind-try`.
///
/// `--dev /dev` mounts a fresh, near-empty devtmpfs, so a GPU node has
/// to be bound back explicitly. `push_gpu_selection` then lays a tmpfs
/// over `/dev/dri` and re-binds only the compositor's render node, so
/// this entry is what that narrowing lands on top of — the app never
/// sees more than one GPU.
pub const SYSTEM_DEVICE_PATHS: &[&str] = &[
    "/dev/dri",
    "/dev/nvidia0",
    "/dev/nvidiactl",
    "/dev/nvidia-modeset",
    "/dev/nvidia-uvm",
    "/dev/nvidia-uvm-tools",
    "/dev/nvidia-caps",
];

/// Push the profile's binds onto a bwrap argv.
///
/// Callers must splice this in *before* the trailing entrypoint token —
/// bwrap treats everything after it as the child's argv.
pub fn push_system_profile(argv: &mut Vec<String>) {
    for path in SYSTEM_READ_PATHS {
        push(argv, "--ro-bind-try", path, path);
    }
    for path in SYSTEM_DEVICE_PATHS {
        push(argv, "--dev-bind-try", path, path);
    }
}

/// The profile as Landlock read-only rules.
///
/// A bind the ruleset does not name is visible and unreadable — every
/// `open()` on it returns `EACCES`, which reads from inside the app as
/// a corrupted system rather than a policy. So the two layers are
/// derived from one list.
pub fn landlock_ro_paths() -> Vec<PathBuf> {
    SYSTEM_READ_PATHS.iter().map(PathBuf::from).collect()
}

/// The device nodes as Landlock read-write rules. A GPU context is
/// opened `O_RDWR`; a read-only rule surfaces at device-open time and
/// looks exactly like "no GPU present".
pub fn landlock_rw_device_paths() -> Vec<PathBuf> {
    SYSTEM_DEVICE_PATHS.iter().map(PathBuf::from).collect()
}

/// Whether `path` is covered by the read profile — used to explain a
/// refusal ("this is already yours") rather than to grant anything.
pub fn covers(path: &Path) -> bool {
    SYSTEM_READ_PATHS
        .iter()
        .chain(SYSTEM_DEVICE_PATHS.iter())
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

    /// The profile is a *system* list. If a user path ever appears in
    /// it, every app on the machine silently gained access to it — so
    /// this is checked mechanically rather than by review.
    #[test]
    fn the_profile_names_no_user_path() {
        for p in SYSTEM_READ_PATHS.iter().chain(SYSTEM_DEVICE_PATHS.iter()) {
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

    /// `/` (or any prefix of every path) in the profile would hand the
    /// whole host to every app, which is the bug this file exists to
    /// remove. Cheap to assert, catastrophic to miss.
    #[test]
    fn the_profile_does_not_contain_the_root() {
        for p in SYSTEM_READ_PATHS.iter().chain(SYSTEM_DEVICE_PATHS.iter()) {
            assert_ne!(*p, "/", "the root filesystem is not a profile entry");
            assert!(
                Path::new(p).components().count() >= 2,
                "{p} is a top-level directory; the profile names specific \
                 subtrees, not whole filesystems"
            );
        }
    }

    #[test]
    fn the_profile_binds_read_only_and_devices_as_devices() {
        let mut argv = Vec::new();
        push_system_profile(&mut argv);
        assert!(
            argv.windows(3).any(|w| w[0] == "--ro-bind-try"
                && w[1] == "/usr/share/fonts"
                && w[2] == "/usr/share/fonts"),
            "fonts must be read-only: {argv:?}"
        );
        assert!(
            argv.windows(3)
                .any(|w| w[0] == "--dev-bind-try" && w[1] == "/dev/dri"),
            "the GPU node needs a device bind: {argv:?}"
        );
        // Nothing in the profile may be writable. A `--bind` here would
        // be a system directory an app can rewrite.
        assert!(
            !argv.iter().any(|a| a == "--bind" || a == "--bind-try"),
            "no profile entry may be read-write: {argv:?}"
        );
    }
}
