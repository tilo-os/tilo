//! The old names.
//!
//! The programs that were `/usr/bin/tilo-<name>` live in [`LIBEXEC`] now,
//! off `PATH`; each old name runs the program it stands for, adding nothing.
//! Scripts, units and habits keep working while people learn
//! `tilo <noun> <verb>`.
//!
//! How an old name reaches its program depends on who installs it. A VM-wide
//! component ships with the image, so its names link to `tilo`, which runs the
//! program under the name it was called by; `tilo` and the link are one
//! image's build. A session component is swapped onto an image that may be far
//! older, whose `tilo` does not know the name yet — a device swapped ahead of
//! its image would otherwise keep working through links that resolve nowhere.
//! So its names link straight to their programs, and which side a name is on
//! follows from its component's scope in `os/components.toml`.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};

/// Where Tilo's own programs live.
pub const LIBEXEC: &str = "/usr/libexec/tilo";

/// Every old `/usr/bin` name of a VM-wide component, whose program moved to
/// [`LIBEXEC`]. The image manifest (`os/components.toml`) links each of these
/// to `tilo`, and a test keeps the two lists the same. A session component's
/// names are not here: they link straight to their programs (see the module
/// docs), so `tilo` never sees them as `argv[0]`.
pub const MOVED: &[&str] = &[
    "tilo-crypto",
    "tilo-device-admin",
    "tilo-device-key-init",
    "tilo-enroll",
    "tilo-greeter",
    "tilo-image-trust",
    "tilo-installer",
    "tilo-relay",
    "tilo-relay-admin",
    "tilo-rollback",
    "tilo-sibling-unlock",
    "tilo-vm",
];

/// The old name `argv[0]` is, if it is one.
pub fn moved(args: &[OsString]) -> Option<&'static str> {
    let name = args
        .first()
        .and_then(|arg| Path::new(arg).file_name())?
        .to_str()?;
    MOVED.iter().copied().find(|moved| *moved == name)
}

/// Where Tilo's own programs are: [`LIBEXEC`], or in a debug build the
/// directory `TILO_LIBEXEC_DIR` names, so a test can stand in for them. A
/// shipped `tilo` never lets the environment choose what an old name runs.
pub fn libexec() -> PathBuf {
    if cfg!(debug_assertions)
        && let Some(dir) = std::env::var_os("TILO_LIBEXEC_DIR").filter(|dir| !dir.is_empty())
    {
        return dir.into();
    }
    PathBuf::from(LIBEXEC)
}

/// Where the program an old name stands for is.
pub fn target(name: &str) -> PathBuf {
    libexec().join(name)
}

/// Become the program `name` stands for, with `args` after `argv[0]`. Returns
/// only when that can't happen.
pub fn exec(name: &str, args: &[OsString]) -> Result<std::convert::Infallible> {
    let program = target(name);
    if !crate::executable_file(&program) {
        bail!("{name} isn't on this computer");
    }
    // A link back to this program would run it again, forever.
    let own = std::env::current_exe().and_then(std::fs::canonicalize).ok();
    if own.is_some() && std::fs::canonicalize(&program).ok() == own {
        bail!("{} is tilo itself, not {name}", program.display());
    }
    let error = Command::new(&program)
        .arg0(name)
        .args(args.get(1..).unwrap_or_default())
        .exec();
    bail!("running {}: {error}", program.display())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use std::collections::BTreeSet;
    use std::ffi::OsString;

    use super::{LIBEXEC, MOVED, moved};

    /// The image's files: (component, scope, destination, link target or
    /// build output).
    fn manifest() -> Vec<(String, String, String, String)> {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap()
            .to_path_buf();
        let text = std::fs::read_to_string(repo.join("os/components.toml")).unwrap();
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        let mut files = Vec::new();
        for component in parsed["component"].as_array().unwrap() {
            let name = component["name"].as_str().unwrap_or_default().to_owned();
            let scope = component["scope"].as_str().unwrap_or_default().to_owned();
            for file in component
                .get("files")
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
            {
                let to = file["to"].as_str().unwrap().to_owned();
                let from = file
                    .get("link")
                    .or_else(|| file.get("from"))
                    .and_then(toml::Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                files.push((name.clone(), scope.clone(), to, from));
            }
        }
        files
    }

    /// An old name reaches its program from any image: the image's own names
    /// resolve through `tilo`, a session component's link straight to their
    /// programs, and either way the program is in /usr/libexec/tilo under the
    /// name that runs it.
    #[test]
    fn an_old_name_reaches_its_program_from_any_image() {
        let files = manifest();
        // Nouns (`tilo app ...`) and the surfaces a login starts are not old
        // names.
        let nouns = [
            "tilo-app",
            "tilo-action",
            "tilo-device",
            "tilo-package",
            "tilo-agent",
        ];
        let surfaces = [
            "tilo-session",
            "tilo-lock-screen",
            "tilo-osk",
            "tilo-quick-settings",
            "tilo-window-menu",
        ];
        // A device gets its session from a swap, and its daemon through `swap
        // vmd --slot vm`; both land on whatever image the device already runs.
        let swapped: BTreeSet<&str> = ["vmd"].into_iter().collect();
        let mut through_tilo: BTreeSet<&str> = BTreeSet::new();
        let mut direct: BTreeSet<&str> = BTreeSet::new();
        for (component, scope, to, from) in &files {
            let Some(name) = to.strip_prefix("/usr/bin/") else {
                continue;
            };
            if !name.starts_with("tilo-") || surfaces.contains(&name) {
                continue;
            }
            if nouns.contains(&name) {
                assert_eq!(from, "tilo", "{to} is a noun of tilo itself");
                continue;
            }
            if scope == "session" || swapped.contains(component.as_str()) {
                assert_eq!(
                    *from,
                    format!("../libexec/tilo/{name}"),
                    "{to} arrives with a swap onto an image whose `tilo` may not know it yet, \
                     so it must link to its program"
                );
                direct.insert(name);
            } else {
                assert_eq!(
                    from, "tilo",
                    "{to} ships with the image, so it resolves through that image's tilo"
                );
                through_tilo.insert(name);
            }
        }
        assert!(
            !direct.is_empty() && !through_tilo.is_empty(),
            "both kinds of old name must exist for this test to prove anything"
        );
        let moved_names: BTreeSet<&str> = MOVED.iter().copied().collect();
        assert_eq!(
            through_tilo, moved_names,
            "tilo runs exactly the old names its own image links to it"
        );
        for name in through_tilo.union(&direct) {
            let place = std::path::Path::new(LIBEXEC)
                .join(name)
                .display()
                .to_string();
            assert!(
                files
                    .iter()
                    .any(|(_, _, to, from)| *to == place && from.as_str() == *name),
                "{name} must be installed at {place}"
            );
        }
        for name in MOVED {
            assert_eq!(
                moved(&[OsString::from(format!("/usr/bin/{name}"))]),
                Some(*name)
            );
        }
        for name in &direct {
            assert_eq!(
                moved(&[OsString::from(format!("/usr/bin/{name}"))]),
                None,
                "{name} links to its program, so tilo never runs it by name"
            );
        }
        assert_eq!(moved(&[OsString::from("/usr/bin/tilo")]), None);
        assert_eq!(moved(&[OsString::from("tilo-device")]), None);
    }
}
