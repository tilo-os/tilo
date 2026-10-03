//! `tilo-vmd install-packages`: third-party packages for verification,
//! installed with the one package manager line the VM's image supports.
//!
//! On a bootc image (the dev image, ostree- or composefs-backed) `dnf install --transient` puts them in
//! a transient overlay on `/usr` that the next reboot discards, so nothing
//! outlives the test and no image ever carries them. Sessions freeze their
//! `/usr` when they start, so a slot sees the packages after its session
//! restarts (`tilo-vm slot apps install` does that).

use std::path::Path;

use tilo_vm_protocol::valid_package_name;

/// The argv that installs `packages`, given which of the paths it asks
/// about exist.
///
/// # Errors
/// A package name that is not one, or no package manager this knows.
pub fn install_argv(
    packages: &[String],
    exists: impl Fn(&Path) -> bool,
) -> Result<Vec<String>, String> {
    if packages.is_empty() {
        return Err("no packages given".into());
    }
    if let Some(bad) = packages.iter().find(|p| !valid_package_name(p)) {
        return Err(format!("'{bad}' is not a package name"));
    }
    // A composefs-backed bootc system has no `/run/ostree-booted`; its deployments live
    // under `/sysroot/composefs`.
    let bootc = exists(Path::new("/run/ostree-booted")) || exists(Path::new("/sysroot/composefs"));
    let mut argv: Vec<String> = if exists(Path::new("/usr/bin/dnf")) {
        let mut argv = vec!["dnf", "install", "--assumeyes"];
        if bootc {
            argv.push("--transient");
        }
        argv.push("--setopt=install_weak_deps=False");
        argv
    } else if exists(Path::new("/usr/bin/rpm-ostree")) {
        vec![
            "rpm-ostree",
            "install",
            "--apply-live",
            "--idempotent",
            "--assumeyes",
        ]
    } else {
        return Err("the VM's image has neither dnf nor rpm-ostree".into());
    }
    .into_iter()
    .map(str::to_owned)
    .collect();
    argv.push("--".into());
    argv.extend(packages.iter().cloned());
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(paths: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |path| paths.iter().any(|p| Path::new(p) == path)
    }

    fn packages() -> Vec<String> {
        vec!["xterm".into(), "gtk4-devel-tools".into()]
    }

    #[test]
    fn a_bootc_image_installs_transiently_with_dnf() {
        let argv = install_argv(
            &packages(),
            with(&["/usr/bin/dnf", "/usr/bin/rpm-ostree", "/run/ostree-booted"]),
        )
        .unwrap();
        assert_eq!(
            argv.join(" "),
            "dnf install --assumeyes --transient --setopt=install_weak_deps=False -- xterm gtk4-devel-tools"
        );
    }

    #[test]
    fn a_composefs_bootc_image_installs_transiently_too() {
        let argv =
            install_argv(&packages(), with(&["/usr/bin/dnf", "/sysroot/composefs"])).unwrap();
        assert!(argv.contains(&"--transient".to_owned()), "{argv:?}");
    }

    #[test]
    fn a_plain_system_installs_with_dnf_and_rpm_ostree_is_the_fallback() {
        let plain = install_argv(&packages(), with(&["/usr/bin/dnf"])).unwrap();
        assert!(!plain.contains(&"--transient".to_owned()), "{plain:?}");
        let ostree = install_argv(&packages(), with(&["/usr/bin/rpm-ostree"])).unwrap();
        assert_eq!(ostree[..3], ["rpm-ostree", "install", "--apply-live"]);
        assert!(install_argv(&packages(), with(&[])).is_err());
    }

    #[test]
    fn names_that_are_not_packages_are_refused() {
        let error = install_argv(&["--installroot=/".into()], with(&["/usr/bin/dnf"])).unwrap_err();
        assert!(error.contains("not a package name"), "{error}");
        assert!(install_argv(&[], with(&["/usr/bin/dnf"])).is_err());
    }
}
