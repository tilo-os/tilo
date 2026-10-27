//! The old names.
//!
//! The programs that were `/usr/bin/tilo-<name>` live in [`LIBEXEC`] now,
//! off `PATH`; each old name is a link to `tilo`, which runs the program
//! under the name it was called by, adding nothing. Scripts, units and
//! habits keep working while people learn `tilo <noun> <verb>`. The few in
//! `DIRECT` are links straight to their program instead.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};

/// Where Tilo's own programs live.
pub const LIBEXEC: &str = "/usr/libexec/tilo";

/// Every old `/usr/bin` name whose program moved to [`LIBEXEC`]. The image
/// manifest (`os/components.toml`) links each of these to `tilo`, and a
/// test keeps the two lists the same.
pub const MOVED: &[&str] = &[
    "tilo-app-package",
    "tilo-control",
    "tilo-crypto",
    "tilo-device-admin",
    "tilo-device-key-init",
    "tilo-enroll",
    "tilo-greeter",
    "tilo-identity",
    "tilo-image-trust",
    "tilo-install",
    "tilo-installer",
    "tilo-launcher",
    "tilo-legacy",
    "tilo-model-gateway",
    "tilo-model-http",
    "tilo-model-manager",
    "tilo-relay",
    "tilo-relay-admin",
    "tilo-rollback",
    "tilo-runtime-bundle",
    "tilo-runtime-mcp",
    "tilo-secret-service",
    "tilo-sibling-unlock",
    "tilo-speech-worker",
    "tilo-test-registry",
    "tilo-vm",
];

/// The old `/usr/bin` names whose program moved to [`LIBEXEC`] but whose
/// link names that program itself, not `tilo`. `tilo-vmd` is swapped onto
/// older images to give them the daemon they need, and such an image's
/// `tilo` runs no programs by name (`os/components.toml`, component `vmd`).
/// `tilo` never reads this list; the image-layout test holds it to the image.
#[cfg(test)]
pub const DIRECT: &[&str] = &["tilo-vmd"];

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

    use super::{DIRECT, LIBEXEC, MOVED, moved};

    /// The image's files: (destination, link target or build output).
    fn manifest() -> Vec<(String, String)> {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap()
            .to_path_buf();
        let text = std::fs::read_to_string(repo.join("os/components.toml")).unwrap();
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        let mut files = Vec::new();
        for component in parsed["component"].as_array().unwrap() {
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
                files.push((to, from));
            }
        }
        files
    }

    /// Every program that left /usr/bin is in /usr/libexec/tilo, its old
    /// name is a link to `tilo` (or, for [`DIRECT`], to the program itself),
    /// and `tilo` knows where each `tilo` link went; nothing Tilo builds is
    /// on PATH under a `tilo-` name except those links.
    #[test]
    fn every_old_name_is_a_link_to_tilo_and_its_program_is_off_path() {
        let files = manifest();
        let on_path: BTreeSet<&str> = files
            .iter()
            .filter_map(|(to, _)| to.strip_prefix("/usr/bin/"))
            .filter(|name| name.starts_with("tilo-"))
            .collect();
        let nouns = [
            "tilo-app",
            "tilo-action",
            "tilo-device",
            "tilo-package",
            "tilo-agent",
        ];
        let old: BTreeSet<&str> = on_path
            .iter()
            .copied()
            .filter(|name| !nouns.contains(name))
            .collect();
        let moved_names: BTreeSet<&str> = MOVED.iter().copied().collect();
        let direct: BTreeSet<&str> = DIRECT.iter().copied().collect();
        assert!(
            moved_names.is_disjoint(&direct),
            "a name links to tilo or to its program, not both"
        );
        assert_eq!(old, &moved_names | &direct);
        for (to, from) in &files {
            if let Some(name) = to.strip_prefix("/usr/bin/")
                && name.starts_with("tilo-")
            {
                if direct.contains(name) {
                    assert_eq!(
                        *from,
                        format!("../libexec/tilo/{name}"),
                        "{to} must be a link to its program"
                    );
                } else {
                    assert_eq!(from, "tilo", "{to} must be a link to tilo");
                }
            }
        }
        for name in DIRECT {
            let place = std::path::Path::new(LIBEXEC)
                .join(name)
                .display()
                .to_string();
            assert!(
                files.iter().any(|(to, from)| *to == place && from == name),
                "{name} must be installed at {place}"
            );
            assert_eq!(moved(&[OsString::from(format!("/usr/bin/{name}"))]), None);
        }
        for name in MOVED {
            let place = std::path::Path::new(LIBEXEC)
                .join(name)
                .display()
                .to_string();
            assert!(
                files.iter().any(|(to, from)| *to == place && from == name),
                "{name} must be installed at {place}"
            );
            assert_eq!(
                moved(&[OsString::from(format!("/usr/bin/{name}"))]),
                Some(*name)
            );
        }
        assert_eq!(moved(&[OsString::from("/usr/bin/tilo")]), None);
        assert_eq!(moved(&[OsString::from("tilo-device")]), None);
    }
}
