//! The capture's demo clip plays on every VP9 decoder: Profile 0, 8-bit,
//! 4:2:0, read from the pinned fixture's own `vpcC` record.

#![allow(
    clippy::expect_used,
    reason = "a malformed fixture must fail the test explicitly"
)]

use std::path::Path;

/// The payload of the first box of `kind` among the boxes in `data`, and the
/// rest of `data` after that box.
fn next_box(data: &[u8], kind: [u8; 4]) -> Option<(&[u8], &[u8])> {
    let mut rest = data;
    while let (Some(size32), Some(found)) = (rest.get(0..4), rest.get(4..8)) {
        let size32 = u32::from_be_bytes(size32.try_into().ok()?);
        let (header, size) = match size32 {
            0 => (8, rest.len()),
            1 => {
                let large = u64::from_be_bytes(rest.get(8..16)?.try_into().ok()?);
                (16, usize::try_from(large).ok()?)
            }
            n => (8, usize::try_from(n).ok()?),
        };
        let (whole, after) = (rest.get(..size)?, rest.get(size..)?);
        if found == kind {
            return Some((whole.get(header..)?, after));
        }
        rest = after;
    }
    None
}

/// The payload of the first box of `kind` among the boxes in `data`.
fn child(data: &[u8], kind: [u8; 4]) -> Option<&[u8]> {
    next_box(data, kind).map(|(payload, _)| payload)
}

/// The VP9 codec configuration (`vpcC`) of the file's video track:
/// `(profile, bit depth, chroma subsampling)`.
fn vp9_config(file: &[u8]) -> Option<(u8, u8, u8)> {
    let mut rest = child(file, *b"moov")?;
    while let Some((trak, after)) = next_box(rest, *b"trak") {
        rest = after;
        let stsd = [*b"mdia", *b"minf", *b"stbl", *b"stsd"]
            .into_iter()
            .try_fold(trak, child)?;
        // A full box (version, flags) and the entry count lead the entries.
        let Some(entry) = child(stsd.get(8..)?, *b"vp09") else {
            continue;
        };
        // The visual sample entry's fixed fields come before its child boxes.
        let vpcc = child(entry.get(78..)?, *b"vpcC")?;
        // A full box: version and flags, then profile, level, and one byte of
        // bit depth (4 bits), chroma subsampling (3) and full range (1).
        let profile = *vpcc.get(4)?;
        let packed = *vpcc.get(6)?;
        return Some((
            profile,
            packed.checked_shr(4)?,
            packed.checked_shr(1)? & 0b111,
        ));
    }
    None
}

#[test]
fn the_demo_clip_is_vp9_profile_0_8_bit_4_2_0() {
    let fixture = Path::new(env!("OUT_DIR")).join("vp9-aac.mp4");
    let file = std::fs::read(&fixture).expect("the pinned demo clip is staged");
    let (profile, bit_depth, chroma) =
        vp9_config(&file).expect("the clip has a VP9 track with a vpcC record");
    assert_eq!(profile, 0, "VP9 Profile 0, which every VP9 decoder plays");
    assert_eq!(bit_depth, 8, "8-bit samples");
    // 0 and 1 are 4:2:0 (vertical or colocated chroma); 2 is 4:2:2, 3 is 4:4:4.
    assert!(chroma <= 1, "4:2:0 chroma, found subsampling code {chroma}");
}
