//! The system volume a person's keyslot goes on, and what can authorise
//! adding it on a running system without asking anyone for the old disk
//! passphrase.

use crate::keyring;
use crate::luks::Authority;
use anyhow::Result;
use std::path::{Path, PathBuf};
use tilo_rollback::argv;
use tilo_rollback::exec::Runner;
use tilo_rollback::keyslots::verify_passphrase;
use tilo_rollback::policy::Workspace;
use zeroize::Zeroizing;

#[derive(Clone, Debug)]
pub struct SystemVolume {
    /// The LUKS2 device.
    pub device: PathBuf,
    /// Its open device-mapper name, when it is open.
    pub mapping: Option<String>,
}

/// How the daemon finds the system volume.
#[derive(Clone, Debug)]
pub enum Locate {
    /// The device under the mounted root.
    Root,
    Fixed(SystemVolume),
    /// No encrypted system volume.
    Absent,
}

impl Locate {
    pub fn find(&self, runner: &dyn Runner) -> Option<SystemVolume> {
        match self {
            Self::Root => root_volume(runner),
            Self::Fixed(volume) => Some(volume.clone()),
            Self::Absent => None,
        }
    }
}

/// By content, not configuration: the block device under the physical root
/// (`/sysroot`; `/` is composefs, which none backs), and the kernel's record in
/// sysfs of the LUKS2 partition underneath that mapping. No device-mapper
/// ioctl and no reading of the mount table's source name.
fn root_volume(runner: &dyn Runner) -> Option<SystemVolume> {
    ["/sysroot", "/"].into_iter().find_map(|mount| {
        let out = runner
            .run("findmnt", &argv!["-nro", "MAJ:MIN", mount], None)
            .ok()?;
        let text = out.stdout_utf8();
        let majmin = text.lines().next()?.trim().to_owned();
        let (mapping, partition) = crypt_backing(Path::new("/sys"), &majmin)?;
        let device = Path::new("/dev").join(partition);
        let luks = runner
            .run("cryptsetup", &argv!["isLuks", device.as_os_str()], None)
            .ok()?;
        luks.ok().then_some(SystemVolume {
            device,
            mapping: Some(mapping),
        })
    })
}

/// The mapping name and the one underlying device of the block device
/// `majmin`, when it is an open LUKS mapping.
fn crypt_backing(sys: &Path, majmin: &str) -> Option<(String, String)> {
    let (major, minor) = majmin.split_once(':')?;
    if major.parse::<u32>().is_err() || minor.parse::<u32>().is_err() {
        return None;
    }
    let block = sys.join("dev/block").join(majmin);
    let uuid = std::fs::read_to_string(block.join("dm/uuid")).ok()?;
    if !uuid.starts_with("CRYPT-LUKS") {
        return None;
    }
    let name = std::fs::read_to_string(block.join("dm/name")).ok()?;
    let mut slaves = std::fs::read_dir(block.join("slaves")).ok()?;
    let slave = slaves.next()?.ok()?.file_name().into_string().ok()?;
    if slaves.next().is_some() {
        return None;
    }
    Some((name.trim().to_owned(), slave))
}

/// A credential that already opens the volume.
pub enum Held {
    Passphrase(Zeroizing<Vec<u8>>),
    VolumeKey(Zeroizing<Vec<u8>>),
}

impl Held {
    pub fn authority(&self) -> Authority<'_> {
        match self {
            Self::Passphrase(p) => Authority::Passphrase(p),
            Self::VolumeKey(k) => Authority::VolumeKey(k),
        }
    }
}

/// The volume key the initrd links into uid 0's user keyring while the
/// system volume has no sign-in yet (`link-volume-key=` in tilo-root-discover).
pub const LINKED_VOLUME_KEY: &str = "tilo-auth:volume-key";
/// LUKS2 volume keys are at most 512 bits for every cipher the installer uses;
/// the slack only lets a longer one fail at the key check instead of here.
const MAX_VOLUME_KEY: usize = 128;

/// The first of, in order: the secret this boot unlocked the volume with
/// (tilo-rollback leaves it in `/run` for its own confirmation); the volume
/// key the initrd linked as `linked`; what the person just typed, when it is
/// also a disk passphrase (the installer makes the account password the disk
/// passphrase on machines without a TPM); the open mapping's volume key, when
/// the kernel shows it.
pub fn held_credential(
    runner: &dyn Runner,
    ws: &Workspace,
    volume: &SystemVolume,
    proof_file: &Path,
    linked: Option<&str>,
    typed: Option<&str>,
) -> Result<Option<Held>> {
    if let Ok(proof) = std::fs::read(proof_file) {
        let proof = Zeroizing::new(proof);
        if verify_passphrase(runner, ws, &volume.device, &proof, None)? {
            return Ok(Some(Held::Passphrase(proof)));
        }
    }
    if let Some(description) = linked {
        match keyring::read_one(description, MAX_VOLUME_KEY) {
            Ok(Some(key)) => return Ok(Some(Held::VolumeKey(key))),
            Ok(None) => {}
            Err(e) => eprintln!("tilo-authd: could not read the linked volume key: {e}"),
        }
    }
    if let Some(typed) = typed.filter(|t| !t.is_empty())
        && verify_passphrase(runner, ws, &volume.device, typed.as_bytes(), None)?
    {
        return Ok(Some(Held::Passphrase(Zeroizing::new(
            typed.as_bytes().to_vec(),
        ))));
    }
    if let Some(mapping) = &volume.mapping
        && let Some(key) = mapping_volume_key(runner, mapping)
    {
        return Ok(Some(Held::VolumeKey(key)));
    }
    Ok(None)
}

/// The volume key from the device-mapper table. A mapping set up with the
/// key in the kernel keyring shows only a reference (`:64:logon:…`), which
/// no process can read back; that is the usual case for LUKS2.
fn mapping_volume_key(runner: &dyn Runner, mapping: &str) -> Option<Zeroizing<Vec<u8>>> {
    let out = runner
        .run("dmsetup", &argv!["table", "--showkeys", mapping], None)
        .ok()?;
    if !out.ok() {
        return None;
    }
    let text = Zeroizing::new(out.stdout_utf8());
    parse_crypt_table_key(&text)
}

fn parse_crypt_table_key(table: &str) -> Option<Zeroizing<Vec<u8>>> {
    let mut fields = table.lines().next()?.split_whitespace();
    let (_start, _len, target, _cipher, key) = (
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
    );
    if target != "crypt" || key.starts_with(':') {
        return None;
    }
    hex::decode(key).ok().map(Zeroizing::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_mapping_is_followed_through_sysfs() {
        let sys = tempfile::tempdir().unwrap();
        let root = sys.path().join("dev/block/253:0");
        std::fs::create_dir_all(root.join("dm")).unwrap();
        std::fs::create_dir_all(root.join("slaves/vda3")).unwrap();
        std::fs::write(root.join("dm/name"), "root\n").unwrap();
        std::fs::write(root.join("dm/uuid"), "CRYPT-LUKS2-0123-root\n").unwrap();
        assert_eq!(
            crypt_backing(sys.path(), "253:0"),
            Some(("root".into(), "vda3".into()))
        );
        // composefs, or a plain partition: no mapping at all.
        assert_eq!(crypt_backing(sys.path(), "0:31"), None);
        assert_eq!(crypt_backing(sys.path(), "../253:0"), None);
        std::fs::write(root.join("dm/uuid"), "CRYPT-PLAIN-root\n").unwrap();
        assert_eq!(crypt_backing(sys.path(), "253:0"), None);
        std::fs::write(root.join("dm/uuid"), "CRYPT-LUKS2-0123-root\n").unwrap();
        std::fs::create_dir_all(root.join("slaves/vdb1")).unwrap();
        assert_eq!(
            crypt_backing(sys.path(), "253:0"),
            None,
            "two devices under it"
        );
    }

    #[test]
    fn only_a_key_the_table_spells_out_is_used() {
        assert_eq!(
            parse_crypt_table_key("0 2048 crypt aes-xts-plain64 00ff10 0 7:0 0 1 allow_discards\n")
                .unwrap()
                .as_slice(),
            &[0x00, 0xff, 0x10]
        );
        assert!(
            parse_crypt_table_key(
                "0 2048 crypt aes-xts-plain64 :64:logon:cryptsetup:1234-d0 0 7:0 0\n"
            )
            .is_none()
        );
        assert!(parse_crypt_table_key("0 2048 linear 7:0 0\n").is_none());
    }
}
