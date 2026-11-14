//! A folder of the workspace's files as one `.zip` the browser downloads: the
//! files stored as they are, uncompressed, which every file manager opens and
//! which costs the page no compressor in its download.
//!
//! The layout is the ZIP application note's: a local header and the bytes of
//! each file, then the central directory naming them all, then its end
//! record. Names are UTF-8 (general purpose flag bit 11). A folder holding
//! 65,535 files or more, or 4 GiB or more, would need ZIP64, which this does
//! not write: such a folder is refused rather than written broken.

/// Why a folder cannot be written as a zip.
pub const TOO_LARGE: &str = "This folder is too large to download as one zip";

/// The flag saying a member's name is UTF-8.
const UTF8_NAMES: u16 = 1 << 11;
/// Version 2.0 of the format: what reading a stored member needs.
const VERSION: u16 = 20;

/// One member: its path within the zip (`docs/plan.md`, `/`-separated) and
/// its bytes.
pub struct Member<'a> {
    pub path: &'a str,
    pub bytes: &'a [u8],
}

/// The zip holding `members`, each stored as it is.
///
/// # Errors
///
/// [`TOO_LARGE`] for a folder past what a zip without ZIP64 holds.
pub fn zip(members: &[Member<'_>]) -> Result<Vec<u8>, &'static str> {
    let count = u16::try_from(members.len()).map_err(|_| TOO_LARGE)?;
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    for member in members {
        let offset = u32::try_from(out.len()).map_err(|_| TOO_LARGE)?;
        let size = u32::try_from(member.bytes.len()).map_err(|_| TOO_LARGE)?;
        let name = member.path.as_bytes();
        let name_len = u16::try_from(name.len()).map_err(|_| TOO_LARGE)?;
        let crc = crc32fast::hash(member.bytes);
        // Local file header.
        out.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&UTF8_NAMES.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes()); // stored
        out.extend_from_slice(&0_u16.to_le_bytes()); // time
        out.extend_from_slice(&0x0021_u16.to_le_bytes()); // date: 1980-01-01
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(member.bytes);
        // Its central directory entry.
        central.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
        central.extend_from_slice(&VERSION.to_le_bytes());
        central.extend_from_slice(&VERSION.to_le_bytes());
        central.extend_from_slice(&UTF8_NAMES.to_le_bytes());
        central.extend_from_slice(&0_u16.to_le_bytes());
        central.extend_from_slice(&0_u16.to_le_bytes());
        central.extend_from_slice(&0x0021_u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&name_len.to_le_bytes());
        central.extend_from_slice(&[0; 12]); // extra, comment, disk, attributes
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name);
    }
    let start = u32::try_from(out.len()).map_err(|_| TOO_LARGE)?;
    let length = u32::try_from(central.len()).map_err(|_| TOO_LARGE)?;
    out.extend_from_slice(&central);
    // End of central directory.
    out.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]); // this disk, the directory's disk
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&length.to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    u32::try_from(out.len()).map_err(|_| TOO_LARGE)?;
    Ok(out)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::io::Read as _;

    /// What a file manager reads back is every file, under its path, with
    /// its bytes.
    #[test]
    fn a_folder_reads_back_as_its_files() {
        let members = [
            Member {
                path: "Plans/plan.md",
                bytes: b"# Plan\n",
            },
            Member {
                path: "Plans/2026/budget.csv",
                bytes: b"a,b\n1,2\n",
            },
            Member {
                path: "Plans/empty.txt",
                bytes: b"",
            },
            Member {
                path: "Plans/naïve résumé.txt",
                bytes: "über".as_bytes(),
            },
        ];
        let bytes = zip(&members).expect("a small folder zips");
        let mut archive =
            zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("a zip a reader opens");
        assert_eq!(archive.len(), members.len());
        for member in &members {
            let mut file = archive
                .by_name(member.path)
                .expect("each file under its path");
            let mut read = Vec::new();
            file.read_to_end(&mut read).expect("its bytes read back");
            assert_eq!(read, member.bytes, "{}", member.path);
        }
    }

    #[test]
    fn an_empty_folder_is_an_empty_zip() {
        let bytes = zip(&[]).expect("nothing zips");
        let archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("opens");
        assert_eq!(archive.len(), 0);
    }
}
