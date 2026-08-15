//! `pkg` — packages, in a layer rather than on the host.
//!
//! # Where packages come from, and why
//!
//! **`dnf`, running inside the guest, installing into `/`.** The guest's `/usr`
//! is an overlay whose upper is the user's layer, so "into `/`" *is* "into the
//! layer" — the host's filesystem is on the read-only side of the mount and
//! cannot be reached however the transaction is spelled. The rpm database the
//! guest sees is the layer's seeded copy of the base's
//! ([`crate::layer::STATE_SEED_DIRS`]), so the solver starts from the image's
//! installed set: `pkg install htop` downloads htop and not glibc.
//!
//! Three alternatives were tried or considered, and rejected:
//!
//! - **`dnf --installroot=<layer>` on the host.** The obvious answer on Fedora,
//!   and it does install the right files. But an installroot that holds only
//!   the delta has no `/bin/sh`, no `ldconfig` and no `/usr/lib/rpm`, so every
//!   scriptlet in the transaction fails. Measured on Fedora 44: `ldconfig`
//!   returns "call to ldconfig failed", `man-db`'s file trigger exits 127, and
//!   `dnf` reports the transaction as failed while leaving the files behind —
//!   the worst of both, a half-applied install that claims not to have
//!   happened. Running the same transaction inside the guest fixes it for free,
//!   because the guest *is* the base plus the layer and every scriptlet finds
//!   what it expects.
//!
//! - **`rpm2cpio` into the layer.** No dependency solving, no scriptlets, no
//!   database. It would work for exactly the packages that need none of those,
//!   which is not a useful set, and there would be no honest answer to
//!   `pkg remove`.
//!
//! - **A container image per layer.** Correct and heavy: a second package
//!   universe, a second update cadence, and gigabytes where the delta is
//!   kilobytes. The point of the layer model is that the base is already there.
//!
//! # The one thing the guest cannot do
//!
//! It cannot *modify* a file that came from the base — the file is owned by
//! host uid 0, which is not mapped into the guest's user namespace, so overlayfs
//! cannot copy it up. In practice that means a layer can install a package and
//! cannot upgrade one that shipped in the image. `pkg` says so rather than
//! letting `dnf` fail somewhere confusing.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where a layer records what `pkg` put in it.
///
/// Inside `/usr`, so it lands in the layer and the guest can read it back
/// through the merged view. `rpm -qa` cannot answer this on its own: it reports
/// the base and the layer as one installed set, which is the point of seeding
/// the database and also the reason it is not the record of what you added.
pub const MANIFEST_PATH: &str = "/usr/lib/ush/packages.json";

/// One package this layer installed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InstalledPackage {
    pub name: String,
    /// `name-version-release.arch`, as rpm reports it after the transaction.
    #[serde(default)]
    pub nevra: String,
    pub at: u64,
    /// Installed with `--one-time`: removed when the session ends unless
    /// `pkg freeze` keeps it.
    #[serde(default)]
    pub one_time: bool,
}

/// The layer's package manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub packages: Vec<InstalledPackage>,
}

impl Manifest {
    pub fn load(path: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)
    }

    pub fn record(&mut self, pkg: InstalledPackage) {
        self.packages.retain(|p| p.name != pkg.name);
        self.packages.push(pkg);
        self.packages.sort_by(|a, b| a.name.cmp(&b.name));
    }

    pub fn forget(&mut self, name: &str) -> bool {
        let before = self.packages.len();
        self.packages.retain(|p| p.name != name);
        self.packages.len() != before
    }

    /// Mark every one-time install as kept. `pkg freeze`.
    pub fn freeze(&mut self) -> Vec<String> {
        let mut promoted = Vec::new();
        for p in &mut self.packages {
            if p.one_time {
                p.one_time = false;
                promoted.push(p.name.clone());
            }
        }
        promoted
    }

    pub fn one_time(&self) -> Vec<String> {
        self.packages
            .iter()
            .filter(|p| p.one_time)
            .map(|p| p.name.clone())
            .collect()
    }
}

/// The `dnf` argv for an install inside a guest.
///
/// Split out so the flags can be asserted on without a network. Weak
/// dependencies are off: a layer should hold what you asked for, and pulling a
/// recommends-chain into it is how a delta stops being one.
pub fn install_argv(packages: &[String], releasever: &str) -> Vec<String> {
    let mut argv: Vec<String> = vec![
        "dnf5".into(),
        "install".into(),
        "-y".into(),
        format!("--releasever={releasever}"),
        "--setopt=install_weak_deps=False".into(),
    ];
    argv.extend(packages.iter().cloned());
    argv
}

/// The `dnf` argv for a removal inside a guest.
pub fn remove_argv(packages: &[String], releasever: &str) -> Vec<String> {
    let mut argv: Vec<String> = vec![
        "dnf5".into(),
        "remove".into(),
        "-y".into(),
        format!("--releasever={releasever}"),
    ];
    argv.extend(packages.iter().cloned());
    argv
}

/// `%fedora`, or whatever the base says its release is.
pub fn releasever() -> String {
    Command::new("rpm")
        .args(["-E", "%fedora"])
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            (!s.is_empty() && s != "%fedora").then_some(s)
        })
        .unwrap_or_else(|| "rawhide".into())
}

/// `name-version-release.arch` for an installed package, or the bare name.
pub fn nevra(name: &str) -> String {
    Command::new("rpm")
        .args(["-q", name])
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            (o.status.success() && !s.is_empty()).then_some(s)
        })
        .unwrap_or_else(|| name.to_string())
}

/// What `pkg inspect` reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inspection {
    pub nevra: String,
    pub summary: String,
    pub files: Vec<PathBuf>,
    /// rpm scriptlets present in the package.
    pub scripts: Vec<String>,
    /// systemd units the package ships.
    pub services: Vec<String>,
    /// Why this package might deserve a second look.
    pub risks: Vec<String>,
}

/// Inspect an installed package with rpm.
///
/// The "risk" read is deliberately mechanical and says what it looked at: files
/// that are setuid or setgid, scriptlets that run as root at install time,
/// services that would start on boot, and anything landing outside the paths a
/// layer normally owns. It is not a malware verdict and does not pretend to be
/// one — see [`crate::guard`] for the same honesty applied to `scan`.
pub fn inspect(name: &str) -> Inspection {
    let mut out = Inspection {
        nevra: nevra(name),
        ..Default::default()
    };

    if let Some(text) = run_capture("rpm", &["-q", "--qf", "%{SUMMARY}", name]) {
        out.summary = text.trim().to_string();
    }
    if let Some(text) = run_capture("rpm", &["-ql", name]) {
        out.files = text.lines().map(PathBuf::from).collect();
    }
    if let Some(text) = run_capture("rpm", &["-q", "--scripts", name]) {
        for line in text.lines() {
            if let Some(head) = line.strip_suffix(" scriptlet (using /bin/sh):") {
                out.scripts.push(head.to_string());
            } else if line.ends_with("scriptlet:") {
                out.scripts.push(line.trim_end_matches(':').to_string());
            }
        }
    }

    for file in &out.files {
        let name = file.to_string_lossy();
        if name.contains("/systemd/system/") && name.ends_with(".service") {
            out.services.push(name.into_owned());
        }
    }

    for file in &out.files {
        if let Ok(meta) = file.symlink_metadata() {
            use std::os::unix::fs::PermissionsExt;
            let mode = meta.permissions().mode();
            if mode & 0o4000 != 0 {
                out.risks.push(format!("setuid: {}", file.display()));
            }
            if mode & 0o2000 != 0 {
                out.risks.push(format!("setgid: {}", file.display()));
            }
        }
    }
    if !out.scripts.is_empty() {
        out.risks.push(format!(
            "runs {} install-time scriptlet(s) as root inside your layer: {}",
            out.scripts.len(),
            out.scripts.join(", ")
        ));
    }
    if !out.services.is_empty() {
        out.risks.push(format!(
            "ships {} systemd unit(s); nothing in a guest starts them, but they \
             are in the layer: {}",
            out.services.len(),
            out.services.join(", ")
        ));
    }
    out
}

fn run_capture(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_argv_pins_the_release_and_refuses_weak_dependencies() {
        let argv = install_argv(&["htop".into()], "44");
        assert_eq!(argv[0], "dnf5");
        assert!(argv.contains(&"--releasever=44".to_string()));
        assert!(
            argv.contains(&"--setopt=install_weak_deps=False".to_string()),
            "a recommends-chain in a layer is how a delta stops being one: {argv:?}"
        );
        assert_eq!(argv.last().unwrap(), "htop");
    }

    /// No flag may point the transaction at anything but the guest's own root.
    /// `--installroot` would write outside the overlay, which is the one thing
    /// this design exists to prevent.
    #[test]
    fn no_transaction_names_a_root_outside_the_guest() {
        for argv in [
            install_argv(&["htop".into()], "44"),
            remove_argv(&["htop".into()], "44"),
        ] {
            for arg in &argv {
                assert!(
                    !arg.starts_with("--installroot"),
                    "the transaction must land in the guest's own /, which is \
                     the layer: {argv:?}"
                );
            }
        }
    }

    #[test]
    fn the_manifest_records_replaces_and_forgets() {
        let mut m = Manifest::default();
        m.record(InstalledPackage {
            name: "htop".into(),
            nevra: "htop-3.4.1-1.fc44.x86_64".into(),
            at: 1,
            one_time: false,
        });
        m.record(InstalledPackage {
            name: "sl".into(),
            nevra: "sl-5.02-25.fc44.x86_64".into(),
            at: 2,
            one_time: true,
        });
        assert_eq!(m.packages.len(), 2);
        // Re-installing replaces rather than duplicates.
        m.record(InstalledPackage {
            name: "htop".into(),
            nevra: "htop-3.4.2-1.fc44.x86_64".into(),
            at: 3,
            one_time: false,
        });
        assert_eq!(m.packages.len(), 2);
        assert_eq!(m.packages[0].nevra, "htop-3.4.2-1.fc44.x86_64");

        assert_eq!(m.one_time(), vec!["sl".to_string()]);
        assert!(m.forget("sl"));
        assert!(!m.forget("sl"));
        assert!(m.one_time().is_empty());
    }

    #[test]
    fn freeze_promotes_every_one_time_install() {
        let mut m = Manifest::default();
        for (name, one_time) in [("sl", true), ("htop", false), ("nethack", true)] {
            m.record(InstalledPackage {
                name: name.into(),
                nevra: name.into(),
                at: 1,
                one_time,
            });
        }
        let promoted = m.freeze();
        assert_eq!(promoted, vec!["nethack".to_string(), "sl".to_string()]);
        assert!(
            m.one_time().is_empty(),
            "after a freeze nothing is scheduled for removal"
        );
        assert_eq!(m.packages.len(), 3, "freeze must not drop a package");
    }

    #[test]
    fn the_manifest_round_trips_and_tolerates_a_missing_or_broken_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usr/lib/ush/packages.json");
        assert!(Manifest::load(&path).packages.is_empty());

        let mut m = Manifest::default();
        m.record(InstalledPackage {
            name: "htop".into(),
            nevra: "htop-3.4.1-1.fc44.x86_64".into(),
            at: 7,
            one_time: false,
        });
        m.save(&path).unwrap();
        assert_eq!(Manifest::load(&path), m);

        std::fs::write(&path, b"{ truncated").unwrap();
        assert!(
            Manifest::load(&path).packages.is_empty(),
            "a broken manifest must read as empty, not abort the session"
        );
    }

    #[test]
    fn the_manifest_lives_inside_the_layer() {
        assert!(
            MANIFEST_PATH.starts_with("/usr/"),
            "the manifest has to be under a shadowed subtree, or it lands on \
             the host instead of in the layer"
        );
    }

    #[test]
    fn releasever_is_never_empty() {
        let r = releasever();
        assert!(!r.is_empty());
        assert!(!r.contains('%'), "an unexpanded rpm macro leaked through: {r}");
    }
}
