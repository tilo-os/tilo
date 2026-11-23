//! What merging a prebuilt sysext image would do to `/usr`, checked before it
//! is linked into the search path.
//!
//! systemd-sysext merges an image's `usr/` over `/usr` as an overlay, and for a
//! directory both have, the overlay shows the image's owner, mode and `SELinux`
//! label. An image whose `usr` is owned by a build user, or carries no labels
//! (the kernel then reports `unlabeled_t`), keeps every login out and stops the
//! next boot in systemd-tmpfiles-setup. The signed manifest proves who built the
//! image, not that it is safe to merge, so its contents are read here: every
//! entry owned by root, writable by its owner alone and without set-id bits,
//! labelled when the system is, and every directory the base already has kept
//! with the base's type, mode and label. Anything else is refused and nothing
//! is merged.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::{Error, Result};

/// One entry's owner, mode (with its type bits) and `SELinux` label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub label: Option<String>,
}

impl Meta {
    fn is_dir(&self) -> bool {
        self.mode & 0o170_000 == 0o040_000
    }

    fn is_link(&self) -> bool {
        self.mode & 0o170_000 == 0o120_000
    }
}

/// An image read for merging: each of its entries by absolute path (`/usr`,
/// `/usr/lib`, ...), and what the base system without any extension has at the
/// paths of the image's directories.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inspection {
    pub image: BTreeMap<String, Meta>,
    pub base: BTreeMap<String, Meta>,
}

const REFUSED_LABELS: [&str; 2] = [":unlabeled_t:", ":file_t:"];

fn problem(path: &str, entry: &Meta, base: Option<&Meta>, labelled: bool) -> Option<String> {
    if (entry.uid, entry.gid) != (0, 0) {
        return Some(format!(
            "{path} is owned by {}:{}, not root",
            entry.uid, entry.gid
        ));
    }
    if !entry.is_link() && entry.mode & 0o6022 != 0 {
        return Some(format!(
            "{path} has mode {:04o}: writable by others or set-id",
            entry.mode & 0o7777
        ));
    }
    if labelled {
        match &entry.label {
            None => return Some(format!("{path} has no SELinux label")),
            Some(label) if REFUSED_LABELS.iter().any(|refused| label.contains(refused)) => {
                return Some(format!("{path} is labelled {label}"));
            }
            Some(_) => {}
        }
    }
    let base = base?;
    if base.is_dir() != entry.is_dir() {
        return Some(format!("{path} would replace what the system has there"));
    }
    if entry.is_dir() && base.mode & 0o7777 != entry.mode & 0o7777 {
        return Some(format!(
            "{path} has mode {:04o}, the system's has {:04o}",
            entry.mode & 0o7777,
            base.mode & 0o7777
        ));
    }
    if labelled && entry.is_dir() && base.label != entry.label {
        return Some(format!(
            "{path} is labelled {}, the system's is {}",
            entry.label.as_deref().unwrap_or("nothing"),
            base.label.as_deref().unwrap_or("nothing")
        ));
    }
    None
}

/// Why the image must not be merged, if it must not.
pub fn refusal(inspection: &Inspection) -> Option<String> {
    if !inspection.image.get("/usr").is_some_and(Meta::is_dir) {
        return Some("the image has no /usr".to_owned());
    }
    let labelled = inspection
        .base
        .get("/usr")
        .is_some_and(|usr| usr.label.is_some());
    // The image's own root is never merged: systemd-sysext takes its usr/ alone.
    inspection
        .image
        .iter()
        .filter(|(path, _)| *path != "/")
        .find_map(|(path, entry)| problem(path, entry, inspection.base.get(path), labelled))
}

/// Reads the image mounted read-only in a private mount namespace, where the
/// base `/usr` is seen with every merged extension taken off; the mount ends
/// with the namespace.
const INSPECT: &str = r#"
image=$1
if mountpoint -q /usr; then umount -l /usr; fi
at=$(mktemp -d)
mount -o ro,nosuid,nodev,noexec,loop -- "$image" "$at"
cd "$at"
find . -print0 | xargs -0 stat --printf 'image\0%n\0%f\0%u\0%g\0%C\0'
find . -mindepth 1 -type d -printf '/%P\0' \
  | { xargs -0 -r stat --printf 'base\0%n\0%f\0%u\0%g\0%C\0' 2>/dev/null || true; }
"#;

pub(crate) fn inspect(image: &Path) -> Result<Inspection> {
    let output = Command::new("unshare")
        .args(["--mount", "--propagation", "private", "--"])
        .args(["bash", "-euo", "pipefail", "-c", INSPECT, "inspect"])
        .arg(image)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| Error::os(&error, Some(Path::new("unshare"))))?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Pack(format!(
            "the image could not be read: {}",
            crate::py::truncate(said.trim(), 300)
        )));
    }
    parse(&String::from_utf8_lossy(&output.stdout))
}

fn parse(out: &str) -> Result<Inspection> {
    let fields: Vec<&str> = out.split('\0').collect();
    let mut inspection = Inspection::default();
    let mut entries = fields.chunks_exact(6);
    for entry in &mut entries {
        let &[kind, path, mode, uid, gid, label] = entry else {
            return Err(Error::Value("the image listing ended early".to_owned()));
        };
        let number = |text: &str, radix| {
            u32::from_str_radix(text, radix)
                .map_err(|_| Error::Value(format!("unreadable image entry {path}: {text}")))
        };
        let meta = Meta {
            mode: number(mode, 16)?,
            uid: number(uid, 10)?,
            gid: number(gid, 10)?,
            label: (!label.is_empty() && label != "?").then(|| label.to_owned()),
        };
        let path = match path.strip_prefix('.') {
            Some("") => "/".to_owned(),
            Some(rest) => rest.to_owned(),
            None => path.to_owned(),
        };
        match kind {
            "image" => inspection.image.insert(path, meta),
            "base" => inspection.base.insert(path, meta),
            other => return Err(Error::Value(format!("unreadable image listing: {other}"))),
        };
    }
    if entries.remainder() != [""] {
        return Err(Error::Value("the image listing ended early".to_owned()));
    }
    Ok(inspection)
}

#[cfg(test)]
mod tests {
    use super::*;

    const USR: &str = "system_u:object_r:usr_t:s0";
    const LIB: &str = "system_u:object_r:lib_t:s0";

    fn meta(mode: u32, label: &str) -> Meta {
        Meta {
            mode,
            uid: 0,
            gid: 0,
            label: Some(label.to_owned()),
        }
    }

    fn good() -> Inspection {
        Inspection {
            image: BTreeMap::from([
                ("/".to_owned(), meta(0o40755, "system_u:object_r:root_t:s0")),
                ("/usr".to_owned(), meta(0o40755, USR)),
                ("/usr/lib".to_owned(), meta(0o40555, LIB)),
                ("/usr/lib/tilo".to_owned(), meta(0o40755, LIB)),
                ("/usr/lib/tilo/proton".to_owned(), meta(0o100_755, LIB)),
                ("/usr/lib/tilo/current".to_owned(), meta(0o120_777, LIB)),
            ]),
            base: BTreeMap::from([
                ("/usr".to_owned(), meta(0o40755, USR)),
                ("/usr/lib".to_owned(), meta(0o40555, LIB)),
            ]),
        }
    }

    fn change(path: &str, edit: impl FnOnce(&mut Meta)) -> Option<String> {
        let mut inspection = good();
        edit(inspection.image.get_mut(path).unwrap());
        refusal(&inspection)
    }

    #[test]
    fn a_root_owned_labelled_image_keeping_the_base_is_merged() {
        assert_eq!(refusal(&good()), None);
    }

    #[test]
    fn an_entry_of_a_build_user_is_refused() {
        assert_eq!(
            change("/usr/lib/tilo/proton", |m| (m.uid, m.gid) = (99, 65534)).as_deref(),
            Some("/usr/lib/tilo/proton is owned by 99:65534, not root")
        );
    }

    #[test]
    fn an_unlabelled_image_is_refused_where_the_system_is_labelled() {
        assert_eq!(
            change("/usr", |m| m.label =
                Some("system_u:object_r:unlabeled_t:s0".into()))
            .as_deref(),
            Some("/usr is labelled system_u:object_r:unlabeled_t:s0")
        );
        assert_eq!(
            change("/usr/lib/tilo/proton", |m| m.label = None).as_deref(),
            Some("/usr/lib/tilo/proton has no SELinux label")
        );
        let mut unlabelled = good();
        for entry in unlabelled
            .image
            .values_mut()
            .chain(unlabelled.base.values_mut())
        {
            entry.label = None;
        }
        assert_eq!(
            refusal(&unlabelled),
            None,
            "a system without SELinux has no labels"
        );
    }

    #[test]
    fn a_directory_of_the_base_keeps_its_mode_label_and_type() {
        assert_eq!(
            change("/usr/lib", |m| m.mode = 0o40755).as_deref(),
            Some("/usr/lib has mode 0755, the system's has 0555")
        );
        assert_eq!(
            change("/usr/lib", |m| m.label = Some(USR.into())).as_deref(),
            Some(
                "/usr/lib is labelled system_u:object_r:usr_t:s0, the system's is system_u:object_r:lib_t:s0"
            )
        );
        let mut link = good();
        link.base
            .insert("/usr/lib/tilo".to_owned(), meta(0o120_777, LIB));
        assert_eq!(
            refusal(&link).as_deref(),
            Some("/usr/lib/tilo would replace what the system has there")
        );
    }

    #[test]
    fn writable_or_set_id_entries_are_refused() {
        assert_eq!(
            change("/usr/lib/tilo/proton", |m| m.mode = 0o104_755).as_deref(),
            Some("/usr/lib/tilo/proton has mode 4755: writable by others or set-id")
        );
        assert_eq!(
            change("/usr/lib/tilo/proton", |m| m.mode = 0o100_777).as_deref(),
            Some("/usr/lib/tilo/proton has mode 0777: writable by others or set-id")
        );
    }

    #[test]
    fn the_listing_is_read_whole() {
        let out = "image\0.\x0041ed\x000\x000\0system_u:object_r:root_t:s0\0\
                   image\0./usr\x0041ed\x000\x000\0?\0\
                   base\0/usr\x0041ed\x000\x000\0system_u:object_r:usr_t:s0\0";
        let inspection = parse(out).unwrap();
        assert_eq!(inspection.image["/"].mode, 0o40755);
        assert_eq!(inspection.image["/usr"].label, None);
        assert_eq!(inspection.base["/usr"].label.as_deref(), Some(USR));
        assert!(parse("image\0./usr\x0041ed\0").is_err());
    }
}
