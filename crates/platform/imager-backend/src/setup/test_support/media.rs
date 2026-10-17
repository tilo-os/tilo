//! A real GPT byte fixture for native setup's production layout parser.
use std::{fs::File, os::unix::fs::FileExt};
const IMAGE_BYTES: u64 = 4 * 1024 * 1024;
const MEDIA_BYTES: u64 = 64 * 1024 * 1024;
fn field(raw: &mut [u8], start: usize, value: &[u8]) {
    raw.get_mut(start..start + value.len())
        .expect("fixture field")
        .copy_from_slice(value);
}
fn header(current: u64, alternate: u64, array_lba: u64, entries: &[u8]) -> Vec<u8> {
    let mut raw = vec![0; 512];
    field(&mut raw, 0, b"EFI PART");
    field(&mut raw, 8, &0x10000u32.to_le_bytes());
    field(&mut raw, 12, &92u32.to_le_bytes());
    for (at, value) in [
        (24, current),
        (32, alternate),
        (40, 34),
        (48, IMAGE_BYTES / 512 - 34),
        (72, array_lba),
    ] {
        field(&mut raw, at, &value.to_le_bytes());
    }
    field(&mut raw, 56, &uuid::Uuid::from_u128(5).to_bytes_le());
    field(&mut raw, 80, &128u32.to_le_bytes());
    field(&mut raw, 84, &128u32.to_le_bytes());
    field(&mut raw, 88, &crc32fast::hash(entries).to_le_bytes());
    let crc = crc32fast::hash(raw.get(..92).expect("fixture header"));
    field(&mut raw, 16, &crc.to_le_bytes());
    raw
}
pub(crate) fn disk() -> (tempfile::TempDir, std::path::PathBuf, File) {
    let dir = tempfile::tempdir().expect("fixture directory");
    let path = dir.path().join("medium");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .expect("fixture disk");
    file.set_len(MEDIA_BYTES).expect("fixture geometry");
    let mut entries = vec![0; 16384];
    field(
        &mut entries,
        0,
        &uuid::Uuid::parse_str("c12a7328-f81f-11d2-ba4b-00a0c93ec93b")
            .expect("ESP")
            .to_bytes_le(),
    );
    field(&mut entries, 16, &uuid::Uuid::from_u128(6).to_bytes_le());
    field(&mut entries, 32, &2048u64.to_le_bytes());
    field(&mut entries, 40, &4095u64.to_le_bytes());
    let old = IMAGE_BYTES / 512 - 1;
    let mut mbr = vec![0; 512];
    field(&mut mbr, 450, &[0xee]);
    field(&mut mbr, 454, &1u32.to_le_bytes());
    field(
        &mut mbr,
        458,
        &u32::try_from(old).expect("fixture sectors").to_le_bytes(),
    );
    field(&mut mbr, 510, &[0x55, 0xaa]);
    file.write_all_at(&mbr, 0).expect("MBR");
    file.write_all_at(&entries, 1024).expect("primary entries");
    file.write_all_at(&entries, (old - 32) * 512)
        .expect("backup entries");
    file.write_all_at(&header(1, old, 2, &entries), 512)
        .expect("primary GPT");
    file.write_all_at(&header(old, 1, old - 32, &entries), old * 512)
        .expect("backup GPT");
    file.write_all_at(b"signed boot payload", 2048 * 512)
        .expect("boot payload");
    (dir, path, file)
}
