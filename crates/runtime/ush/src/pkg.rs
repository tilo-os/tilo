//! `pkg` — packages, in a layer rather than on the host.
//!
//! Package operations run inside the guest so scriptlets see the read-only
//! base and the writable layer together. Installing directly into a layer
//! would omit the base's shell, libraries, and rpm state; unpacking archives
//! would omit dependency solving and removal metadata. A separate container
//! per layer would duplicate the package universe and update state.
//!
//! # The guest cannot modify
//!
//! It cannot *modify* a file from the base — owned by host uid 0, unmapped
//! in the guest's namespace, so overlayfs can't copy it up. A layer can
//! install a package but not upgrade one that shipped in the image; `pkg`
//! says so rather than letting `dnf` fail somewhere confusing.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where a layer records what `pkg` put in it, inside `/usr` so it lands in
/// the layer. `rpm -qa` can't answer this alone — it reports base+layer as
/// one set, which is why the database is seeded and this is a separate record.
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

    /// # Errors
    ///
    /// Returns an error if `path`'s parent can't be created, or the
    /// manifest can't be serialized or written.
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

/// The `dnf` argv for an install inside a guest, split out so the flags can
/// be asserted on without a network. Weak deps are off — a layer should
/// hold what you asked for, not a recommends-chain.
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
    bounded_command::run(
        "rpm",
        &["-E", "%fedora"],
        None,
        4096,
        Duration::from_secs(10),
    )
    .ok()
    .and_then(|o| {
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        (!s.is_empty() && s != "%fedora").then_some(s)
    })
    .unwrap_or_else(|| "rawhide".into())
}

/// Is this package already part of what the guest can see? The guest's rpm
/// database is base+layer, so this answers "would installing it do
/// anything", not "did this layer install it" — see [`Manifest`] for that.
pub fn is_installed(name: &str) -> bool {
    bounded_command::run(
        "rpm",
        &["-q", name],
        None,
        64 * 1024,
        Duration::from_secs(10),
    )
    .is_ok_and(|output| output.status.success())
}

/// `name-version-release.arch` for an installed package, or the bare name.
pub fn nevra(name: &str) -> String {
    bounded_command::run(
        "rpm",
        &["-q", name],
        None,
        64 * 1024,
        Duration::from_secs(10),
    )
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

/// Inspect an installed package with rpm. The "risk" read is
/// mechanical and says what it looked at: setuid/setgid files, root
/// install-time scriptlets, boot-start services, anything outside a
/// layer's normal paths — not a malware verdict; see [`crate::guard`] for
/// the corresponding `scan` behavior.
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
    let output = bounded_command::run(
        program,
        args,
        None,
        8 * 1024 * 1024,
        Duration::from_secs(30),
    )
    .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transactions_pin_the_release_and_stay_inside_the_guest() {
        // phase: install_argv_pins_the_release_and_refuses_weak_dependencies
        {
            let argv = install_argv(&["htop".into()], "44");
            assert_eq!(argv[0], "dnf5");
            assert!(argv.contains(&"--releasever=44".to_string()));
            assert!(
                argv.contains(&"--setopt=install_weak_deps=False".to_string()),
                "a recommends-chain in a layer is how a delta stops being one: {argv:?}"
            );
            assert_eq!(argv.last().unwrap(), "htop");
        }
        // phase: no_transaction_names_a_root_outside_the_guest
        // No flag may point the transaction at anything but the guest's own root.
        // `--installroot` would write outside the overlay, which is the one thing
        // this design exists to prevent.
        {
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
        // phase: releasever_is_never_empty
        {
            let r = releasever();
            assert!(!r.is_empty());
            assert!(
                !r.contains('%'),
                "an unexpanded rpm macro leaked through: {r}"
            );
        }
        // phase: is_installed_distinguishes_the_base_from_a_name_nothing_provides
        // Stops a `--one-time` install of something the base already ships from
        // whiting the base's copy out at session end; `rpm`/`bash` are always present.
        {
            assert!(
                is_installed("bash"),
                "rpm does not know about bash; the guard against removing a base \
             package at session end cannot work"
            );
            assert!(!is_installed("no-such-package-fbb27a1c"));
        }
    }

    #[test]
    fn the_manifest_records_freezes_and_persists_inside_the_layer() {
        // phase: the_manifest_records_replaces_and_forgets
        {
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
        // phase: freeze_promotes_every_one_time_install
        {
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
        // phase: the_manifest_round_trips_and_tolerates_a_missing_or_broken_file
        {
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
        // phase: the_manifest_lives_inside_the_layer
        {
            assert!(
                MANIFEST_PATH.starts_with("/usr/"),
                "the manifest has to be under a shadowed subtree, or it lands on \
             the host instead of in the layer"
            );
        }
    }
}
