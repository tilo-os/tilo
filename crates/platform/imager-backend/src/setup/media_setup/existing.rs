//! A retry may acknowledge identical completed ciphertext; never replace it.
use super::*;
use cap_fs_ext::OpenOptionsSyncExt;
use sha2::{Digest, Sha256};
use std::io::Read;

fn read(
    pin: &PinnedDirectory,
    name: &str,
    limit: u64,
    cancel: &AtomicBool,
) -> io::Result<(std::fs::File, cap_primitives::fs::Metadata)> {
    check_cancel(cancel)?;
    let expected = crate::setup::nofollow::metadata_nofollow(&pin.file, Path::new(name))?;
    if !expected.is_file()
        || expected.is_symlink()
        || expected.len() > limit
        || expected.uid() != pin.identity.uid()
        || expected.mode() & 0o022 != 0
    {
        return Err(changed("Existing setup delivery has invalid type or size"));
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(&pin.file, Path::new(name), &options)?;
    if !crate::setup::migration::same_snapshot(
        &expected,
        &cap_primitives::fs::Metadata::from_file(&file)?,
    ) {
        return Err(changed("Existing setup delivery changed while opening"));
    }
    Ok((file, expected))
}
fn unchanged(
    pin: &PinnedDirectory,
    name: &str,
    file: &std::fs::File,
    expected: &cap_primitives::fs::Metadata,
) -> io::Result<()> {
    pin.validate_destination()?;
    if !crate::setup::migration::same_snapshot(
        expected,
        &cap_primitives::fs::Metadata::from_file(file)?,
    ) || !crate::setup::migration::same_snapshot(
        expected,
        &crate::setup::nofollow::metadata_nofollow(&pin.file, Path::new(name))?,
    ) {
        return Err(changed("Existing setup delivery changed"));
    }
    Ok(())
}
pub(super) fn verify(
    package: &mut PreparedMediaHandle,
    root: &Arc<PinnedDirectory>,
    cancel: &AtomicBool,
) -> io::Result<Option<DeliveryReceipt>> {
    check_cancel(cancel)?;
    let metadata = match crate::setup::nofollow::metadata_nofollow(&root.file, Path::new(DIRECTORY))
    {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let pin = root.open_child(std::ffi::OsStr::new(DIRECTORY), &metadata)?;
    pin.validate_destination()?;
    let (mut receipt_file, receipt_meta) = match read(&pin, RECEIPT_FILE, 1024, cancel) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    receipt_file.by_ref().take(1025).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != receipt_meta.len() {
        return Err(changed("Existing setup receipt changed"));
    }
    let expected: crate::imager_model::PreparedSetupExpectation = serde_json::from_slice(&bytes)?;
    if !expected.valid()
        || expected.sealed_bytes != package.info().sealed_bytes
        || expected.ciphertext_sha256 != hex::encode(package.ciphertext_digest())
    {
        return Err(changed(
            "Existing setup delivery differs from the prepared identity/accounts",
        ));
    }
    package.verify_held_ciphertext(cancel)?;
    let (mut file, file_meta) = read(&pin, FILE, expected.sealed_bytes, cancel)?;
    if file_meta.len() != expected.sealed_bytes {
        return Err(changed("Existing setup ciphertext size differs"));
    }
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    let mut total = 0u64;
    loop {
        check_cancel(cancel)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .filter(|size| *size <= expected.sealed_bytes)
            .ok_or_else(|| changed("Existing ciphertext exceeded its bound"))?;
        digest.update(
            buffer
                .get(..count)
                .ok_or_else(|| changed("Invalid ciphertext read"))?,
        );
    }
    if total != expected.sealed_bytes
        || hex::encode(digest.finalize()) != expected.ciphertext_sha256
    {
        return Err(changed("Existing setup ciphertext differs"));
    }
    unchanged(&pin, FILE, &file, &file_meta)?;
    unchanged(&pin, RECEIPT_FILE, &receipt_file, &receipt_meta)?;
    package.verify_held_ciphertext(cancel)?;
    durability::sync_file(&file)?;
    durability::sync_file(&receipt_file)?;
    durability::sync_directory(&pin.file)?;
    durability::sync_directory(&root.file)?;
    Ok(Some(DeliveryReceipt {
        relative_path: Path::new(DIRECTORY).join(FILE),
        sealed_bytes: expected.sealed_bytes,
        ciphertext_sha256: expected.ciphertext_sha256,
    }))
}
