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

/// By content, not configuration: the mapping the root sits on, and what
/// cryptsetup says is underneath it.
fn root_volume(runner: &dyn Runner) -> Option<SystemVolume> {
    let source = ["/sysroot", "/"].into_iter().find_map(|mount| {
        let out = runner
            .run("findmnt", &argv!["-nro", "SOURCE", mount], None)
            .ok()?;
        let text = out.stdout_utf8();
        let first = text.lines().next()?.split('[').next()?.trim().to_owned();
        (out.ok() && !first.is_empty()).then_some(first)
    })?;
    let mapping = source.strip_prefix("/dev/mapper/")?.to_owned();
    let status = runner
        .run("cryptsetup", &argv!["status", &mapping], None)
        .ok()?;
    let device = status.stdout_utf8().lines().find_map(|line| {
        line.trim()
            .strip_prefix("device:")
            .map(|d| d.trim().to_owned())
    })?;
    let luks = runner
        .run("cryptsetup", &argv!["isLuks", &device], None)
        .ok()?;
    luks.ok().then(|| SystemVolume {
        device: PathBuf::from(device),
        mapping: Some(mapping),
    })
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
