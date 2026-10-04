//! System libraries an app build needs that no crate can provide.
//!
//! An app's own graph builds from Rust sources alone: Wayland, X11 and
//! xkbcommon are opened at run time, fonts are found without fontconfig's C
//! library. A thin Tilo OS build is the exception. It links the toolkit
//! library the OS ships, which must be built with the OS's feature set, and
//! that set links the media stack, `gstreamer-1.0`. Without its development
//! files Cargo fails deep in a `-sys` build script; this names the packages
//! when it does. Only then: `cargo` may be a drop-in that compiles on
//! another machine, so what this one lacks must not stop a build that works.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use anyhow::{Result, bail};

/// A pkg-config module and the package that provides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Library {
    pub module: &'static str,
    pub fedora: &'static str,
    pub debian: &'static str,
}

const PKG_CONFIG: Library = Library {
    module: "pkg-config",
    fedora: "pkgconf-pkg-config",
    debian: "pkg-config",
};

/// What `runtime-dylib`'s media stack asks pkg-config for; `glib-2.0` and
/// `gstreamer-base-1.0` come with these packages.
pub const THIN_RUNTIME: [Library; 3] = [
    Library {
        module: "gstreamer-1.0",
        fedora: "gstreamer1-devel",
        debian: "libgstreamer1.0-dev",
    },
    Library {
        module: "gstreamer-app-1.0",
        fedora: "gstreamer1-plugins-base-devel",
        debian: "libgstreamer-plugins-base1.0-dev",
    },
    Library {
        module: "gstreamer-video-1.0",
        fedora: "gstreamer1-plugins-base-devel",
        debian: "libgstreamer-plugins-base1.0-dev",
    },
];

/// The entries of `libraries` that `pkg_config` does not find, led by
/// pkg-config itself when it cannot run.
#[must_use]
pub fn missing(pkg_config: &OsStr, libraries: &[Library]) -> Vec<Library> {
    let probe = |args: &[&str]| {
        Command::new(pkg_config)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    };
    if probe(&["--version"]).is_err() {
        return std::iter::once(PKG_CONFIG)
            .chain(libraries.iter().copied())
            .collect();
    }
    libraries
        .iter()
        .copied()
        .filter(|library| {
            !probe(&["--exists", library.module]).is_ok_and(|status| status.success())
        })
        .collect()
}

/// How to install `missing`, one command per distribution family.
#[must_use]
pub fn install_hint(missing: &[Library]) -> String {
    let packages = |pick: fn(&Library) -> &'static str| {
        let mut names: Vec<_> = missing.iter().map(pick).collect();
        names.dedup();
        names.join(" ")
    };
    let modules: Vec<_> = missing.iter().map(|library| library.module).collect();
    format!(
        "missing {}\n  Fedora:        sudo dnf install {}\n  Debian/Ubuntu: sudo apt install {}",
        modules.join(", "),
        packages(|library| library.fedora),
        packages(|library| library.debian),
    )
}

/// What to say after a thin build for this machine failed: the packages to
/// install, when pkg-config here does not find the media stack; `None` when
/// it does, so the failure lies elsewhere.
#[must_use]
pub fn thin_build_failure_hint() -> Option<String> {
    let pkg_config = std::env::var_os("PKG_CONFIG").unwrap_or_else(|| OsString::from("pkg-config"));
    thin_build_failure_hint_from(&pkg_config)
}

fn thin_build_failure_hint_from(pkg_config: &OsStr) -> Option<String> {
    let missing = missing(pkg_config, &THIN_RUNTIME);
    (!missing.is_empty()).then(|| {
        format!(
            "if it stopped in a GStreamer build script: a Tilo OS build links the toolkit library \
             the OS ships, which links GStreamer; {}\n\
             a desktop package (`--platform linux`) needs none of them",
            install_hint(&missing)
        )
    })
}

/// Whether a thin build for this machine can find its libraries. Asked once
/// per process; the first `false` says what to install.
pub fn thin_runtime_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let pkg_config =
            std::env::var_os("PKG_CONFIG").unwrap_or_else(|| OsString::from("pkg-config"));
        let missing = missing(&pkg_config, &THIN_RUNTIME);
        if !missing.is_empty() {
            eprintln!(
                "[tilo-app] reloading in place links the toolkit library the OS ships, which links GStreamer; \
                 restarting the app on each change instead. {}",
                install_hint(&missing)
            );
        }
        missing.is_empty()
    })
}

/// The directories fonts are installed in, as the XDG base directories and
/// fontconfig's defaults name them.
fn font_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".local/share")));
    let data_dirs = std::env::var_os("XDG_DATA_DIRS")
        .unwrap_or_else(|| OsString::from("/usr/local/share:/usr/share"));
    data_home
        .into_iter()
        .chain(std::env::split_paths(&data_dirs))
        .map(|dir| dir.join("fonts"))
        .chain(home.map(|home| home.join(".fonts")))
        .collect()
}

fn holds_a_font(dir: &Path, depth: usize) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            return depth
                .checked_sub(1)
                .is_some_and(|depth| holds_a_font(&path, depth));
        }
        path.extension()
            .and_then(OsStr::to_str)
            .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc"))
    })
}

/// Fails when this machine has no font at all, as a bare container has none:
/// every test that lays out text would stop in the text shaper instead.
///
/// # Errors
///
/// Returns an error naming a font package to install.
pub fn require_fonts() -> Result<()> {
    if font_dirs().iter().any(|dir| holds_a_font(dir, 4)) {
        return Ok(());
    }
    bail!(
        "no fonts are installed, and an app's tests lay out text; install one:\n  \
         Fedora:        sudo dnf install google-noto-sans-fonts\n  \
         Debian/Ubuntu: sudo apt install fonts-noto-core"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_pkg_config_is_reported_with_every_library() {
        let found = missing(OsStr::new("/nonexistent/pkg-config"), &THIN_RUNTIME);
        assert_eq!(found[0], PKG_CONFIG);
        assert_eq!(&found[1..], &THIN_RUNTIME);
    }

    #[test]
    fn a_failed_thin_build_names_packages_only_when_they_are_missing() {
        let hint = thin_build_failure_hint_from(OsStr::new("/nonexistent/pkg-config"))
            .expect("nothing found without pkg-config");
        assert!(hint.contains("links GStreamer; missing pkg-config, gstreamer-1.0"));
        assert!(hint.ends_with("a desktop package (`--platform linux`) needs none of them"));
        // `true` answers every `--exists`: the libraries are there.
        assert_eq!(thin_build_failure_hint_from(OsStr::new("true")), None);
    }

    #[test]
    fn the_hint_names_each_package_once_per_distribution() {
        let hint = install_hint(&THIN_RUNTIME);
        assert!(
            hint.starts_with("missing gstreamer-1.0, gstreamer-app-1.0, gstreamer-video-1.0\n")
        );
        assert!(hint.contains("sudo dnf install gstreamer1-devel gstreamer1-plugins-base-devel\n"));
        assert!(
            hint.ends_with("sudo apt install libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev")
        );
    }
}
