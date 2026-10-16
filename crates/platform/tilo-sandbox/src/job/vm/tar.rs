//! Unpacking a step's outputs: the tar archive its guest wrote, which the host reads as
//! untrusted. Only regular files and directories come out, below the destination: links,
//! devices and paths with `..` or a root are skipped, so nothing written can point elsewhere.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

const BLOCK: usize = 512;

/// Unpack the archive `from` reads into `dest`, a directory nothing else writes. Returns the
/// number of files written.
///
/// # Errors
/// A malformed archive, or a file that could not be written.
pub fn unpack(from: &mut impl Read, dest: &Path) -> io::Result<u64> {
    let mut files = 0_u64;
    let mut long_name: Option<Vec<u8>> = None;
    let mut header = [0_u8; BLOCK];
    loop {
        if !read_block(from, &mut header)? || header.iter().all(|&b| b == 0) {
            return Ok(files);
        }
        let size = field_size(&header)?;
        let kind = header.get(156).copied().unwrap_or(0);
        let name = match long_name.take() {
            Some(n) => n,
            None => header_name(&header),
        };
        match kind {
            // GNU's long name, and pax's extended header: the name of the next entry.
            b'L' => long_name = Some(trim_nul(&read_data(from, size)?)),
            b'x' => long_name = pax_path(&read_data(from, size)?),
            b'0' | 0 | b'7' => {
                let target = place(dest, &name);
                match target {
                    Some(path) => {
                        if let Some(parent) = path.parent() {
                            fs::create_dir_all(parent)?;
                        }
                        let mode = field_octal(header.get(100..108).unwrap_or(&[]));
                        let perm = if mode & 0o111 != 0 { 0o755 } else { 0o644 };
                        let mut file = OpenOptions::new()
                            .write(true)
                            .create(true)
                            .truncate(true)
                            .mode(perm)
                            .custom_flags(libc::O_NOFOLLOW)
                            .open(&path)?;
                        copy_exact(from, &mut file, size)?;
                        files = files.saturating_add(1);
                    }
                    None => skip(from, size)?,
                }
                skip(from, padding(size))?;
            }
            b'5' => {
                if let Some(path) = place(dest, &name) {
                    fs::create_dir_all(path)?;
                }
                skip(from, size.saturating_add(padding(size)))?;
            }
            _ => skip(from, size.saturating_add(padding(size)))?,
        }
    }
}

/// Where `name` goes below `dest`, if it stays there.
fn place(dest: &Path, name: &[u8]) -> Option<PathBuf> {
    let name = std::str::from_utf8(name).ok()?;
    let rel = Path::new(name.trim_start_matches("./"));
    let mut out = dest.to_path_buf();
    let mut any = false;
    for c in rel.components() {
        match c {
            Component::Normal(part) => {
                out.push(part);
                any = true;
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    any.then_some(out)
}

fn read_block(from: &mut impl Read, block: &mut [u8; BLOCK]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < BLOCK {
        let rest = block.get_mut(filled..).unwrap_or(&mut []);
        match from.read(rest)? {
            0 if filled == 0 => return Ok(false),
            0 => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "a truncated tar header",
                ));
            }
            n => filled = filled.saturating_add(n),
        }
    }
    Ok(true)
}

/// An entry's data that is metadata (a long name): small, so read whole.
fn read_data(from: &mut impl Read, size: u64) -> io::Result<Vec<u8>> {
    if size > 1 << 20 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a tar name record over 1 MiB",
        ));
    }
    let mut data = vec![0; usize::try_from(size).unwrap_or(0)];
    from.read_exact(&mut data)?;
    skip(from, padding(size))?;
    Ok(data)
}

fn copy_exact(from: &mut impl Read, to: &mut File, size: u64) -> io::Result<()> {
    let copied = io::copy(&mut from.take(size), to)?;
    if copied == size {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "a truncated file in the outputs",
        ))
    }
}

fn skip(from: &mut impl Read, n: u64) -> io::Result<()> {
    let skipped = io::copy(&mut from.take(n), &mut io::sink())?;
    if skipped == n {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "a truncated tar archive",
        ))
    }
}

fn padding(size: u64) -> u64 {
    size.wrapping_neg() % 512
}

/// The name of a ustar header: `prefix/name`.
fn header_name(h: &[u8; BLOCK]) -> Vec<u8> {
    let name = trim_nul(h.get(0..100).unwrap_or(&[]));
    let ustar = h.get(257..262) == Some(b"ustar".as_slice());
    let prefix = if ustar {
        trim_nul(h.get(345..500).unwrap_or(&[]))
    } else {
        Vec::new()
    };
    if prefix.is_empty() {
        name
    } else {
        let mut full = prefix;
        full.push(b'/');
        full.extend_from_slice(&name);
        full
    }
}

fn trim_nul(b: &[u8]) -> Vec<u8> {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    b.get(..end).unwrap_or(&[]).to_vec()
}

/// The size field: octal, or base-256 for large files (GNU).
fn field_size(h: &[u8; BLOCK]) -> io::Result<u64> {
    let field = h.get(124..136).unwrap_or(&[]);
    if field.first().is_some_and(|&b| b & 0x80 != 0) {
        let mut v = 0_u64;
        for &b in field.get(1..).unwrap_or(&[]) {
            v = v
                .checked_mul(256)
                .and_then(|v| v.checked_add(u64::from(b)))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "a tar size out of range")
                })?;
        }
        return Ok(v);
    }
    Ok(field_octal(field))
}

fn field_octal(field: &[u8]) -> u64 {
    field
        .iter()
        .skip_while(|&&b| b == b' ')
        .take_while(|&&b| (b'0'..=b'7').contains(&b))
        .fold(0_u64, |v, &b| {
            v.saturating_mul(8)
                .saturating_add(u64::from(b.wrapping_sub(b'0')))
        })
}

/// The `path` record of a pax extended header (`<len> path=<value>\n` records).
fn pax_path(data: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(data).ok()?;
    text.split_terminator('\n').find_map(|record| {
        let (_, kv) = record.split_once(' ')?;
        kv.strip_prefix("path=").map(|p| p.as_bytes().to_vec())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(name: &str, kind: u8, size: usize, mode: u32) -> Vec<u8> {
        let mut h = vec![0_u8; BLOCK];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(format!("{mode:07o}").as_bytes());
        h[124..135].copy_from_slice(format!("{size:011o}").as_bytes());
        h[156] = kind;
        h[257..263].copy_from_slice(b"ustar\0");
        h
    }

    fn entry(out: &mut Vec<u8>, name: &str, kind: u8, data: &[u8], mode: u32) {
        out.extend(header(name, kind, data.len(), mode));
        out.extend_from_slice(data);
        out.resize(out.len().next_multiple_of(BLOCK), 0);
    }

    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn files_and_long_names_come_out_and_nothing_leaves_the_destination() {
        let mut tar = Vec::new();
        entry(&mut tar, "target/out/", b'5', b"", 0o755);
        entry(&mut tar, "target/out/app", b'0', b"binary", 0o755);
        entry(&mut tar, "src/notes.txt", b'0', b"notes", 0o644);
        entry(&mut tar, "../escape", b'0', b"no", 0o644);
        entry(&mut tar, "/etc/passwd", b'0', b"no", 0o644);
        entry(&mut tar, "target/link", b'2', b"", 0o777);
        let long = format!("target/{}/deep", "d".repeat(150));
        entry(
            &mut tar,
            "././@LongLink",
            b'L',
            format!("{long}\0").as_bytes(),
            0o644,
        );
        entry(&mut tar, "short-name", b'0', b"long", 0o644);
        tar.extend([0; 1024]);

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out");
        fs::create_dir(&dest).unwrap();
        let n = unpack(&mut tar.as_slice(), &dest).unwrap();
        assert_eq!(n, 3);
        assert_eq!(fs::read(dest.join("target/out/app")).unwrap(), b"binary");
        assert_eq!(fs::read(dest.join("src/notes.txt")).unwrap(), b"notes");
        assert_eq!(fs::read(dest.join(&long)).unwrap(), b"long");
        assert!(!dest.join("target/link").exists());
        assert!(!dir.path().join("escape").exists());
        assert!(!dest.join("etc/passwd").exists());
        let mode = fs::metadata(dest.join("target/out/app"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
    }

    #[test]
    fn an_empty_or_zeroed_disk_holds_no_outputs() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(unpack(&mut [0_u8; 4096].as_slice(), dir.path()).unwrap(), 0);
        assert_eq!(unpack(&mut [].as_slice(), dir.path()).unwrap(), 0);
    }
}
