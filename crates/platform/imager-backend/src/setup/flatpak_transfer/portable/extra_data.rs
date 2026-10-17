//! Validate original raw extra-data against signed commit metadata. Installed
//! files/extra may have been transformed by apply_extra and is never authority.
use crate::setup::migration::{PinnedDirectory, changed};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use glib::{Bytes, Variant, VariantTy};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::{self, Read},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

const COMMIT_LIMIT: u64 = 16 * 1024 * 1024;
// Finite native-parser bound; common proprietary installation archives fit.
const PAYLOAD_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
const OVERHEAD: u64 = 4 * 1024 * 1024;
#[derive(Default)]
pub(super) struct Storage {
    pub downloaded: u64,
    pub installed: u64,
}
impl Storage {
    pub(super) fn staging_bytes(&self) -> io::Result<u64> {
        self.downloaded
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(self.installed))
            .ok_or_else(|| changed("Extra-data storage overflows"))
    }
    pub(super) fn target_bytes(&self) -> io::Result<u64> {
        self.downloaded
            .checked_add(self.installed)
            .ok_or_else(|| changed("Extra-data storage overflows"))
    }
}
struct Source {
    name: Vec<u8>,
    size: u64,
    digest: [u8; 32],
    installed: u64,
}

fn read(
    pin: &PinnedDirectory,
    path: &Path,
    limit: u64,
    cancel: Option<&AtomicBool>,
) -> io::Result<Vec<u8>> {
    pin.validate()?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let mut file = cap_primitives::fs::open(&pin.file, path, &options)?;
    let before = file.metadata()?;
    if !before.is_file() || before.len() > limit {
        return Err(changed(
            "Flatpak extra-data metadata exceeds its reviewed bound",
        ));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(usize::try_from(before.len()).map_err(io::Error::other)?)
        .map_err(io::Error::other)?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check_cancel(cancel)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if (bytes.len() as u64).saturating_add(count as u64) > limit {
            return Err(changed("Flatpak extra-data grew beyond its bound"));
        }
        bytes.extend_from_slice(
            buffer
                .get(..count)
                .ok_or_else(|| changed("Invalid native read length"))?,
        );
    }
    let after = file.metadata()?;
    if before.len() != after.len()
        || before.modified()? != after.modified()?
        || bytes.len() as u64 != before.len()
    {
        return Err(changed("Original Flatpak extra-data changed during review"));
    }
    pin.validate()?;
    Ok(bytes)
}
fn check_cancel(cancel: Option<&AtomicBool>) -> io::Result<()> {
    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        Err(changed("Extra-data verification cancelled"))
    } else {
        Ok(())
    }
}
fn variant(bytes: Vec<u8>, kind: &str) -> io::Result<Variant> {
    let kind = VariantTy::new(kind).map_err(io::Error::other)?;
    let value = Variant::from_bytes_with_type(&Bytes::from_owned(bytes), kind);
    if !value.is_normal_form() {
        return Err(changed("Noncanonical Flatpak binary metadata"));
    }
    Ok(value)
}
fn lookup(dict: &Variant, key: &str, kind: &str) -> io::Result<Option<Variant>> {
    if dict.type_().as_str() != "a{sv}" || dict.n_children() > 4096 {
        return Err(changed("Invalid Flatpak metadata dictionary"));
    }
    let mut found = None;
    for entry in dict.iter() {
        if entry.child_value(0).str() == Some(key) {
            let value = entry
                .child_value(1)
                .as_variant()
                .ok_or_else(|| changed("Invalid metadata variant"))?;
            if found.is_some() || value.type_().as_str() != kind {
                return Err(changed("Duplicate or mistyped Flatpak extra-data metadata"));
            }
            found = Some(value);
        }
    }
    Ok(found)
}
fn bytes(value: &Variant) -> io::Result<&[u8]> {
    value.fixed_array::<u8>().map_err(io::Error::other)
}
fn sources(value: &Variant) -> io::Result<Vec<Source>> {
    if value.n_children() > 256 {
        return Err(changed("Too many Flatpak extra-data sources"));
    }
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    let mut result = Vec::new();
    for row in value.iter() {
        let name = bytes(&row.child_value(0))?.to_vec();
        let bare = name
            .strip_suffix(&[0])
            .ok_or_else(|| changed("Invalid extra-data name"))?;
        if bare.is_empty()
            || bare.len() > 255
            || bare == b"."
            || bare == b".."
            || bare.contains(&b'/')
            || bare.contains(&0)
            || !names.insert(name.clone())
        {
            return Err(changed("Invalid or duplicate extra-data name"));
        }
        let size = u64::from_be(
            row.child_value(1)
                .get::<u64>()
                .ok_or_else(|| changed("Invalid extra-data size"))?,
        );
        let installed = u64::from_be(
            row.child_value(2)
                .get::<u64>()
                .ok_or_else(|| changed("Invalid extra-data installed size"))?,
        );
        let digest: [u8; 32] = bytes(&row.child_value(3))?
            .try_into()
            .map_err(|_| changed("Invalid extra-data SHA256"))?;
        let uri = row.child_value(4);
        if !uri
            .str()
            .is_some_and(|uri| uri.starts_with("https:") || uri.starts_with("http:"))
        {
            return Err(changed("Unsupported signed extra-data URI"));
        }
        total = total
            .checked_add(size)
            .ok_or_else(|| changed("Extra-data size overflows"))?;
        if total > PAYLOAD_LIMIT || installed > PAYLOAD_LIMIT {
            return Err(changed(
                "Extra-data exceeds the bounded native transfer limit",
            ));
        }
        result.push(Source {
            name,
            size,
            digest,
            installed,
        });
    }
    Ok(result)
}
/// Returns conservative additional raw/download and deployed storage. All raw
/// blobs must exist before any target repository transaction is authorized.
pub(super) fn verify(
    repository: &Path,
    hash: &str,
    cancel: Option<&AtomicBool>,
) -> io::Result<Storage> {
    if !super::commit(hash) {
        return Err(changed("Invalid extra-data commit"));
    }
    let pin = PinnedDirectory::open_absolute(repository)?;
    let (prefix, suffix) = hash.split_at(2);
    let object = Path::new("objects")
        .join(prefix)
        .join(format!("{suffix}.commit"));
    let raw = read(&pin, &object, COMMIT_LIMIT, cancel)?;
    if hex::encode(Sha256::digest(&raw)) != hash {
        return Err(changed("Extra-data source commit content changed"));
    }
    let commit = variant(raw, "(a{sv}aya(say)sstayay)")?;
    let Some(value) = lookup(
        &commit.child_value(0),
        "xa.extra-data-sources",
        "a(ayttays)",
    )?
    else {
        return Ok(Storage::default());
    };
    let expected = sources(&value)?;
    if expected.is_empty() {
        return Ok(Storage::default());
    }
    let total = expected.iter().try_fold(0u64, |sum, item| {
        sum.checked_add(item.size)
            .ok_or_else(|| changed("Extra-data size overflows"))
    })?;
    let detached = Path::new("objects")
        .join(prefix)
        .join(format!("{suffix}.commitmeta"));
    let raw = read(&pin, &detached, total.checked_add(OVERHEAD).ok_or_else(|| changed("Extra-data bounds overflow"))?, cancel)
        .map_err(|error| io::Error::new(error.kind(), format!("Original extra-data download is unavailable or changed; reconnect this app before preparing offline transfer: {error}")))?;
    let dict = variant(raw, "a{sv}")?;
    let payload = lookup(&dict,"xa.extra-data","a(ayay)")?.ok_or_else(|| changed("Original raw extra-data download is missing; processed application files cannot replace it"))?;
    if payload.n_children() != expected.len() {
        return Err(changed(
            "Detached extra-data selection differs from signed sources",
        ));
    }
    let mut seen = BTreeSet::new();
    for row in payload.iter() {
        check_cancel(cancel)?;
        let name_value = row.child_value(0);
        let name = bytes(&name_value)?;
        let item = expected
            .iter()
            .find(|item| item.name == name)
            .ok_or_else(|| changed("Unreviewed detached extra-data name"))?;
        if !seen.insert(name.to_vec()) {
            return Err(changed("Duplicate detached extra-data"));
        }
        let data_value = row.child_value(1);
        let data = bytes(&data_value)?;
        if data.len() as u64 != item.size {
            return Err(changed(
                "Raw extra-data length differs from signed metadata",
            ));
        }
        let mut digest = Sha256::new();
        for chunk in data.chunks(64 * 1024) {
            check_cancel(cancel)?;
            digest.update(chunk);
        }
        if <[u8; 32]>::from(digest.finalize()) != item.digest {
            return Err(changed(
                "Raw extra-data SHA256 differs from signed metadata",
            ));
        }
    }
    pin.validate()?;
    let installed = expected.iter().try_fold(0u64, |sum, item| {
        sum.checked_add(item.installed)
            .ok_or_else(|| changed("Extra-data installed size overflows"))
    })?;
    Ok(Storage {
        downloaded: total,
        installed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use glib::variant::ToVariant;
    use std::{collections::HashMap, fs, os::unix::fs::PermissionsExt};
    fn fixture() -> (tempfile::TempDir, String, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let data = b"retained original proprietary download";
        let sources = vec![(
            b"download.bin\0".to_vec(),
            (data.len() as u64).to_be(),
            8192u64.to_be(),
            Sha256::digest(data).to_vec(),
            "https://127.0.0.1:1/unavailable".to_owned(),
        )]
        .to_variant();
        let metadata = HashMap::from([("xa.extra-data-sources".to_owned(), sources)]);
        let commit = (
            metadata,
            Vec::<u8>::new(),
            Vec::<(String, Vec<u8>)>::new(),
            "fixture".to_owned(),
            String::new(),
            0u64,
            vec![0u8; 32],
            vec![0u8; 32],
        )
            .to_variant();
        let hash = hex::encode(Sha256::digest(commit.data()));
        let (prefix, suffix) = hash.split_at(2);
        let objects = root.path().join("objects").join(prefix);
        fs::create_dir_all(&objects).unwrap();
        fs::write(objects.join(format!("{suffix}.commit")), commit.data()).unwrap();
        let detached = objects.join(format!("{suffix}.commitmeta"));
        let payload = HashMap::from([(
            "xa.extra-data".to_owned(),
            vec![(b"download.bin\0".to_vec(), data.to_vec())].to_variant(),
        )])
        .to_variant();
        fs::write(&detached, payload.data()).unwrap();
        (root, hash, detached)
    }
    #[test]
    fn retained_raw_extra_data_checks_signed_big_endian_sizes_and_hashes() {
        let (root, hash, detached) = fixture();
        let before = fs::read(&detached).unwrap();
        assert_eq!(
            verify(root.path(), &hash, None)
                .unwrap()
                .staging_bytes()
                .unwrap(),
            8192 + 2 * b"retained original proprietary download".len() as u64
        );
        assert_eq!(fs::read(&detached).unwrap(), before);
        let mut data = b"retained original proprietary download".to_vec();
        *data.first_mut().unwrap() ^= 1;
        let bad = HashMap::from([(
            "xa.extra-data".to_owned(),
            vec![(b"download.bin\0".to_vec(), data.clone())].to_variant(),
        )])
        .to_variant();
        fs::write(&detached, bad.data()).unwrap();
        assert!(verify(root.path(), &hash, None).is_err());
        fs::remove_file(&detached).unwrap();
        assert!(verify(root.path(), &hash, None).is_err());
    }
    #[test]
    fn untrusted_binary_extra_data_refuses_type_confusion_and_changed_commit() {
        let (root, hash, detached) = fixture();
        let wrong =
            HashMap::from([("xa.extra-data".to_owned(), "wrong type".to_variant())]).to_variant();
        fs::write(&detached, wrong.data()).unwrap();
        assert!(verify(root.path(), &hash, None).is_err());
        let (prefix, suffix) = hash.split_at(2);
        fs::write(
            root.path()
                .join("objects")
                .join(prefix)
                .join(format!("{suffix}.commit")),
            b"changed commit",
        )
        .unwrap();
        assert!(verify(root.path(), &hash, None).is_err());
    }
}
