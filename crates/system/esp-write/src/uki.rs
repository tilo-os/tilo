//! Read, or rewrite a key in, a UKI's sealed command line or its `.osrel`:
//! `/usr/libexec/tilo-uki-cmdline`, a link to `tilo-esp-write`.
//!
//! ```text
//! tilo-uki-cmdline show [UKI]
//! tilo-uki-cmdline blank KEY [UKI]
//! tilo-uki-cmdline remove KEY [UKI]
//! tilo-uki-cmdline set KEY=VALUE [UKI]
//! tilo-uki-cmdline append KEY[=VALUE] [UKI]
//! tilo-uki-cmdline replace OLD=VALUE NEW=VALUE [UKI]
//! tilo-uki-cmdline osrel [UKI]
//! tilo-uki-cmdline set-osrel KEY=VALUE [KEY=VALUE ...] UKI
//! tilo-uki-cmdline boot-digest UKI
//! ```
//!
//! Run on a booted machine: the ESP is mounted at /boot on the sealed layout,
//! so the UKI is under /boot/EFI/Linux and root can write it. With no path it
//! finds the single UKI there, in either the image build's layout or the one
//! `bootc install` leaves.
//!
//! `blank` exists to make a negative result available. A check that reports
//! the generation as measured is only evidence if it can be shown to fail when
//! the generation is absent — otherwise "passed" might mean "asserted
//! nothing". So this overwrites `tilo.generation=<N>` in the `.cmdline`
//! section **with spaces of exactly the same length**: the section keeps its
//! size, every following section keeps its file offset, and the PE stays
//! loadable. Rewriting the section shorter would move the rest of the image
//! and change what fails, which would prove nothing about the generation.
//!
//! `set` is the other side of the same coin, and it is what closes the
//! composefs seal. `bootc container ukify` and `bootc install` compute
//! different digests for the same image (bootc#2334), so install refuses the
//! UKI and names the value it wants. *Rebuilding* the UKI with that value does
//! not converge: a rebuild produces a new rootfs layer, so install then
//! computes a third digest, and a fourth. Rewriting the digest inside the UKI
//! that already exists cannot move the target — `/boot` is the one tree
//! excluded from the composefs digest — so it converges in exactly two
//! passes. Both digests are 128 hex characters, so the replacement is
//! length-preserving like `blank`.
//!
//! `set-osrel` is the third of the family, and it is what makes a derived UKI
//! *identifiable*. systemd-boot discovers any `*.efi` at the top level of
//! `\EFI\Linux` as a Type #2 entry and takes its menu title from the `.osrel`
//! section's `PRETTY_NAME`, its ordering from `SORT_KEY`. A repair image
//! derived from a deployment therefore appears in the boot menu under the
//! deployment's own name — indistinguishable from the thing it exists to
//! repair — unless those two values are changed.
//!
//! It cannot be length-preserving (the replacement is shorter, the added key
//! is new), so it works differently: `.osrel` is a small text section with raw
//! slack after it (629 bytes of content in a 1024-byte raw section, on the
//! image this was written against). The new content is written into that
//! slack and the section's **`VirtualSize`** header field is updated. No file
//! offset moves, so every following section is untouched — the same property
//! the length-preserving edits have, bought a different way. It refuses when
//! the new content does not fit the raw section, or when the raw section is
//! larger than the gap to the next section's virtual address. `append` and
//! `remove` edit `.cmdline` the same way.
//!
//! Why not a Type #1 loader entry instead, which would carry a title in a
//! plain text file: bootc's composefs backend parses every `.conf` in
//! `/boot/loader/entries` as one of its own, and a foreign one makes `bootc
//! status` fail outright ("efi does not start with custom prefix"), taking
//! `bootc upgrade` with it. Type #2 needs no `.conf` at all, so bootc never
//! sees it. Established by putting one on a booted machine, not by reading.
//!
//! Two things it deliberately does not do:
//!
//! * It does not fix up the PE checksum. Nothing on this path verifies one,
//!   and a UKI whose signature had to be re-made would no longer be the
//!   artefact under test.
//! * It does not touch anything but those bytes, so the boot it produces
//!   measures normally into PCR 11 — with a different value, because the
//!   command line changed, which is the whole point. The result is a UKI that
//!   measures perfectly and carries no generation.
//!
//! Exit status: 0 done, 1 refused or failed (the reason on stderr), including
//! a usage error, which prints the usage above.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sha2::{Digest, Sha256};

/// The name the link is installed under; `tilo-esp-write` run as this name
/// is this tool.
pub const NAME: &str = "tilo-uki-cmdline";

const USAGE: &str = "Read, or rewrite a key in, a UKI's sealed command line or its .osrel.

    tilo-uki-cmdline show [UKI]
    tilo-uki-cmdline blank KEY [UKI]
    tilo-uki-cmdline remove KEY [UKI]
    tilo-uki-cmdline set KEY=VALUE [UKI]
    tilo-uki-cmdline append KEY[=VALUE] [UKI]
    tilo-uki-cmdline replace OLD=VALUE NEW=VALUE [UKI]
    tilo-uki-cmdline osrel [UKI]
    tilo-uki-cmdline set-osrel KEY=VALUE [KEY=VALUE ...] UKI
    tilo-uki-cmdline boot-digest UKI

With no UKI it acts on the single UKI under /boot/EFI/Linux.";

const ACTIONS: [&str; 9] = [
    "show",
    "blank",
    "remove",
    "set",
    "append",
    "replace",
    "osrel",
    "set-osrel",
    "boot-digest",
];

/// Why the tool refused: the message it prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl Error {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    fn io(path: &Path, error: &std::io::Error) -> Self {
        Self(format!("{}: {error}", path.display()))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// One row of a PE section table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// File offset of the section's 40-byte header.
    pub header: usize,
    pub name: String,
    pub virtual_address: u32,
    pub virtual_size: u32,
    pub raw_size: u32,
    pub raw_offset: u32,
}

/// `data[start:start + len]` as Python slices: cut to the end of `data`.
fn clamp(data: &[u8], start: usize, len: usize) -> &[u8] {
    let end = start.saturating_add(len).min(data.len());
    data.get(start.min(end)..end).unwrap_or_default()
}

fn u16_at(data: &[u8], at: usize) -> Result<u16> {
    at.checked_add(2)
        .and_then(|end| data.get(at..end))
        .and_then(|b| b.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or_else(|| Error::new("truncated PE headers"))
}

fn u32_at(data: &[u8], at: usize) -> Result<u32> {
    at.checked_add(4)
        .and_then(|end| data.get(at..end))
        .and_then(|b| b.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| Error::new("truncated PE headers"))
}

/// The PE machine field (`0xaa64` for ARM64, `0x8664` for x86-64).
///
/// # Errors
/// No PE signature where `e_lfanew` points.
pub fn machine(data: &[u8]) -> Result<u16> {
    let pe = signature(data)?;
    u16_at(data, pe.saturating_add(4))
}

fn signature(data: &[u8]) -> Result<usize> {
    let pe = u32_at(data, 0x3C)? as usize;
    if clamp(data, pe, 4) != b"PE\0\0" {
        return Err(Error::new("not a PE image: no PE signature at e_lfanew"));
    }
    Ok(pe)
}

/// Every section of the PE in `data`, in table order.
///
/// # Errors
/// No PE signature, or headers cut short.
pub fn section_table(data: &[u8]) -> Result<Vec<Section>> {
    let pe = signature(data)?;
    let count = u16_at(data, pe.saturating_add(6))?;
    let optional = u16_at(data, pe.saturating_add(20))?;
    let base = pe.saturating_add(24).saturating_add(optional.into());
    (0..usize::from(count))
        .map(|index| {
            let header = base.saturating_add(index.saturating_mul(40));
            let raw_name = clamp(data, header, 8);
            let end = raw_name
                .iter()
                .rposition(|&b| b != 0)
                .map_or(0, |at| at.saturating_add(1));
            let name = raw_name
                .get(..end)
                .unwrap_or_default()
                .iter()
                .map(|&b| {
                    if b.is_ascii() {
                        char::from(b)
                    } else {
                        '\u{fffd}'
                    }
                })
                .collect();
            Ok(Section {
                header,
                name,
                virtual_size: u32_at(data, header.saturating_add(8))?,
                virtual_address: u32_at(data, header.saturating_add(12))?,
                raw_size: u32_at(data, header.saturating_add(16))?,
                raw_offset: u32_at(data, header.saturating_add(20))?,
            })
        })
        .collect()
}

/// bootc 1.16.4's boot identity: SHA-256 over the `.linux` then the
/// `.initrd` section's virtual bytes, as lowercase hex.
///
/// # Errors
/// Not exactly one of either section, or one outside the file.
pub fn boot_digest(data: &[u8]) -> Result<String> {
    let table = section_table(data)?;
    let mut digest = Sha256::new();
    for required in [".linux", ".initrd"] {
        let matching: Vec<&Section> = table.iter().filter(|s| s.name == required).collect();
        let [section] = matching.as_slice() else {
            return Err(Error::new(format!("UKI needs exactly one {required}")));
        };
        let offset = section.raw_offset as usize;
        let size = section.virtual_size as usize;
        let content = offset
            .checked_add(size)
            .filter(|_| size != 0 && section.virtual_size <= section.raw_size)
            .and_then(|end| data.get(offset..end))
            .ok_or_else(|| Error::new("invalid UKI section bounds"))?;
        digest.update(content);
    }
    Ok(hex::encode(digest.finalize()))
}

/// The `.cmdline` section's file offset and size (the smaller of its raw and
/// virtual sizes).
///
/// # Errors
/// Not a PE, or no `.cmdline` section.
pub fn find_cmdline(data: &[u8]) -> Result<(usize, usize)> {
    section_table(data)?
        .into_iter()
        .find(|s| s.name == ".cmdline")
        .map(|s| {
            (
                s.raw_offset as usize,
                s.raw_size.min(s.virtual_size) as usize,
            )
        })
        .ok_or_else(|| Error::new("this PE has no .cmdline section, so it is not a UKI"))
}

/// A section's geometry, and how far its content may grow in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Room {
    /// File offset of the section's header.
    pub header: usize,
    pub raw_offset: usize,
    pub raw_size: usize,
    pub virtual_size: usize,
    /// The raw size, capped by the distance to the next section's virtual
    /// address: growing past either would make a section overlap something,
    /// which is a UKI that does not load.
    pub room: usize,
}

fn find_room(data: &[u8], name: &str, missing: &str) -> Result<Room> {
    let table = section_table(data)?;
    let section = table
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| Error::new(missing))?;
    let next = table
        .iter()
        .map(|s| s.virtual_address)
        .filter(|&va| va > section.virtual_address)
        .min();
    let room = next.map_or(section.raw_size, |va| {
        section
            .raw_size
            .min(va.saturating_sub(section.virtual_address))
    });
    Ok(Room {
        header: section.header,
        raw_offset: section.raw_offset as usize,
        raw_size: section.raw_size as usize,
        virtual_size: section.virtual_size as usize,
        room: room as usize,
    })
}

/// The `.cmdline` section's geometry, without moving another PE section.
///
/// # Errors
/// Not a PE, or no `.cmdline` section.
pub fn find_cmdline_room(data: &[u8]) -> Result<Room> {
    find_room(
        data,
        ".cmdline",
        "this PE has no .cmdline section, so it is not a UKI",
    )
}

fn find_osrel(data: &[u8]) -> Result<Room> {
    find_room(
        data,
        ".osrel",
        "this PE has no .osrel section, so systemd-boot would have no title to show for it",
    )
}

/// Replaces a section's content with `new` padded with NULs to its raw size,
/// and sets its virtual size to `new`'s length. Nothing else moves.
///
/// # Errors
/// The raw section reaches past the end of the file.
pub fn write_section(data: &mut [u8], room: &Room, new: &[u8]) -> Result<()> {
    let length =
        u32::try_from(new.len()).map_err(|_| Error::new("section content is too large"))?;
    let raw = room
        .raw_offset
        .checked_add(room.raw_size)
        .and_then(|end| data.get_mut(room.raw_offset..end))
        .ok_or_else(|| Error::new("the section reaches past the end of the file"))?;
    let (content, padding) = raw
        .split_at_mut_checked(new.len())
        .ok_or_else(|| Error::new("the content does not fit the raw section"))?;
    content.copy_from_slice(new);
    padding.fill(0);
    // The section's VirtualSize is what systemd reads the section's length
    // from; the raw size is unchanged, so nothing after this section moves
    // by a single byte.
    room.header
        .checked_add(8)
        .and_then(|at| Some(at..at.checked_add(4)?))
        .and_then(|range| data.get_mut(range))
        .ok_or_else(|| Error::new("truncated PE headers"))?
        .copy_from_slice(&length.to_le_bytes());
    Ok(())
}

/// `bytes.rstrip(chars)`.
fn rstrip<'a>(bytes: &'a [u8], chars: &[u8]) -> &'a [u8] {
    let end = bytes
        .iter()
        .rposition(|b| !chars.contains(b))
        .map_or(0, |at| at.saturating_add(1));
    bytes.get(..end).unwrap_or_default()
}

/// `bytes.split()`: runs of ASCII whitespace separate.
fn words(bytes: &[u8]) -> Vec<&[u8]> {
    bytes
        .split(|b| b" \t\n\r\x0b\x0c".contains(b))
        .filter(|w| !w.is_empty())
        .collect()
}

fn is_key(word: &[u8], bare: &[u8]) -> bool {
    word == bare
        || word
            .strip_prefix(bare)
            .is_some_and(|rest| rest.first() == Some(&b'='))
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Reads `path`, lets `edit` change the bytes, and writes back only the
/// range that changed, in place: the file keeps its size and inode, as an
/// edit through a shared mapping would.
///
/// # Errors
/// The file cannot be read or written, or `edit` refuses (nothing is
/// written then).
pub fn edit_file<T>(path: &Path, edit: impl FnOnce(&mut [u8]) -> Result<T>) -> Result<T> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| Error::io(path, &e))?;
    let before = read_all(&mut file, path)?;
    let mut data = before.clone();
    let value = edit(&mut data)?;
    let first = before.iter().zip(&data).position(|(a, b)| a != b);
    let last = before.iter().zip(&data).rposition(|(a, b)| a != b);
    if let (Some(first), Some(last)) = (first, last) {
        let changed = data
            .get(first..=last)
            .ok_or_else(|| Error::new("edit range is outside the file"))?;
        file.write_all_at(changed, first as u64)
            .and_then(|()| file.sync_data())
            .map_err(|e| Error::io(path, &e))?;
    }
    Ok(value)
}

fn read_all(file: &mut File, path: &Path) -> Result<Vec<u8>> {
    use std::io::Read as _;
    let mut data = Vec::new();
    file.read_to_end(&mut data)
        .map_err(|e| Error::io(path, &e))?;
    Ok(data)
}

fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|e| Error::io(path, &e))
}

/// Appends one `KEY` or `KEY=VALUE` token to the sealed command line in
/// place; returns the bytes used and the room there was.
///
/// # Errors
/// A malformed token, the key already present, or no room for it.
pub fn append_cmdline(path: &Path, token: &[u8]) -> Result<(usize, usize)> {
    let text = String::from_utf8_lossy(token);
    if token.is_empty() || token.starts_with(b"=") || text.chars().any(tilo_pycompat::is_space) {
        return Err(Error::new(
            "append takes one complete KEY or KEY=VALUE token",
        ));
    }
    edit_file(path, |data| {
        let room = find_cmdline_room(data)?;
        let current = rstrip(
            clamp(data, room.raw_offset, room.raw_size.min(room.virtual_size)),
            b"\0 \n",
        )
        .to_vec();
        let bare = token.split(|&b| b == b'=').next().unwrap_or_default();
        if words(&current).iter().any(|w| is_key(w, bare)) {
            return Err(Error::new(format!(
                "{} is already on the sealed command line",
                lossy(bare)
            )));
        }
        let mut new = current;
        if !new.is_empty() {
            new.push(b' ');
        }
        new.extend_from_slice(token);
        if new.len() > room.room {
            return Err(Error::new(format!(
                ".cmdline would need {} bytes and only {} are available without moving another PE section",
                new.len(),
                room.room
            )));
        }
        write_section(data, &room, &new)?;
        Ok((new.len(), room.room))
    })
}

/// Removes the one `KEY` or `KEY=...` token from the sealed command line in
/// place; returns the removed token, the bytes now used and the room.
///
/// # Errors
/// A malformed key, or not exactly one token with it.
pub fn remove_cmdline(path: &Path, key: &[u8]) -> Result<(Vec<u8>, usize, usize)> {
    let text = String::from_utf8_lossy(key);
    if key.is_empty() || key.contains(&b'=') || text.chars().any(tilo_pycompat::is_space) {
        return Err(Error::new("remove takes one KEY without = or whitespace"));
    }
    edit_file(path, |data| {
        let room = find_cmdline_room(data)?;
        let current = rstrip(
            clamp(data, room.raw_offset, room.raw_size.min(room.virtual_size)),
            b"\0 \n",
        );
        let parts = words(current);
        let matches: Vec<&[u8]> = parts.iter().copied().filter(|w| is_key(w, key)).collect();
        let [removed] = matches.as_slice() else {
            return Err(Error::new(format!(
                "expected exactly one {text} token, found {}",
                matches.len()
            )));
        };
        let removed = removed.to_vec();
        let new = parts
            .iter()
            .copied()
            .filter(|w| !is_key(w, key))
            .collect::<Vec<_>>()
            .join(&b' ');
        write_section(data, &room, &new)?;
        Ok((removed, new.len(), room.room))
    })
}

/// The `.osrel` section's text.
///
/// # Errors
/// Not a PE, or no `.osrel` section.
pub fn read_osrel(data: &[u8]) -> Result<String> {
    let room = find_osrel(data)?;
    let content = clamp(data, room.raw_offset, room.raw_size.min(room.virtual_size));
    Ok(lossy(rstrip(content, b"\0")))
}

/// Sets `KEY=VALUE` lines in `.osrel`, in place, without moving any
/// section; returns the bytes used and the room there was.
///
/// # Errors
/// An assignment without `=`, a section that is not UTF-8, or content that
/// does not fit in place.
pub fn rewrite_osrel(path: &Path, assignments: &[String]) -> Result<(usize, usize)> {
    edit_file(path, |data| {
        let room = find_osrel(data)?;
        let content = rstrip(
            clamp(data, room.raw_offset, room.raw_size.min(room.virtual_size)),
            b"\0",
        );
        let text = std::str::from_utf8(content)
            .map_err(|e| Error::new(format!("'utf-8' codec can't decode .osrel: {e}")))?;
        let mut lines: Vec<String> = tilo_pycompat::splitlines(text)
            .into_iter()
            .map(str::to_owned)
            .collect();
        for assign in assignments {
            let Some((key, _)) = assign.split_once('=') else {
                return Err(Error::new(format!(
                    "set-osrel takes KEY=VALUE, got '{assign}'"
                )));
            };
            let existing = lines
                .iter_mut()
                .find(|line| line.split('=').next() == Some(key));
            match existing {
                Some(line) => line.clone_from(assign),
                None => lines.push(assign.clone()),
            }
        }
        let new = format!("{}\n", lines.join("\n")).into_bytes();
        if new.len() > room.room {
            return Err(Error::new(format!(
                ".osrel would need {} bytes and only {} are available in place (raw size {}). \
                 Growing the section would move every following one and produce a PE that does not load.",
                new.len(),
                room.room,
                room.raw_size
            )));
        }
        write_section(data, &room, &new)?;
        Ok((new.len(), room.room))
    })
}

fn efi_files(directory: &Path, found: &mut Vec<PathBuf>, descend: bool) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.as_bytes().starts_with(b".") {
            continue;
        }
        let path = entry.path();
        if name.as_bytes().ends_with(b".efi") {
            found.push(path.clone());
        }
        if descend && path.is_dir() {
            efi_files(&path, found, false);
        }
    }
}

/// The single UKI under `/boot/EFI/Linux`, in either layout.
fn default_uki() -> Result<PathBuf> {
    // Two layouts, both real:
    //   /boot/EFI/Linux/<kver>.efi                           as the image build writes it
    //   /boot/EFI/Linux/bootc/bootc_composefs-<digest>.efi   as `bootc install` writes it
    // The second is what an installed machine has — bootc renames the UKI
    // after the composefs digest and nests it a directory deeper — so a
    // single-level search finds nothing on exactly the system this is meant
    // to be run on.
    //
    // /boot is an autofs mount on the sealed layout, automounted on access.
    // Listing a directory below it does not always trigger it, so stat the
    // mount point first.
    let _ = fs::metadata("/boot");
    let mut found = Vec::new();
    efi_files(Path::new("/boot/EFI/Linux"), &mut found, true);
    found.sort_by(|a, b| a.as_os_str().as_bytes().cmp(b.as_os_str().as_bytes()));
    found.dedup();
    match found.as_slice() {
        [] => Err(Error::new(
            "no UKI under /boot/EFI/Linux — on the sealed layout the ESP is mounted \
             at /boot, so either this is not a sealed install or /boot is not mounted",
        )),
        [one] => Ok(one.clone()),
        many => Err(Error::new(format!(
            "more than one UKI under /boot/EFI/Linux; name the one to act on:\n  {}",
            many.iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n  ")
        ))),
    }
}

fn path_or_default(arg: Option<&OsString>) -> Result<PathBuf> {
    arg.map_or_else(default_uki, |path| Ok(PathBuf::from(path)))
}

fn shown(path: &Path) -> String {
    path.display().to_string()
}

fn cmdline_text(data: &[u8]) -> Result<String> {
    let (offset, size) = find_cmdline(data)?;
    Ok(lossy(rstrip(clamp(data, offset, size), b"\0")))
}

/// Overwrites the token starting with `key=` in place: with spaces
/// (`replacement` `None`) or with a replacement of exactly its length.
/// `expected`, when given, is the token that must be there.
fn rewrite_token(
    path: &Path,
    key: &[u8],
    replacement: Option<&[u8]>,
    expected: Option<&[u8]>,
) -> Result<Vec<u8>> {
    edit_file(path, |data| {
        let (offset, size) = find_cmdline(data)?;
        let cmdline = clamp(data, offset, size).to_vec();
        // The token to replace, taken from the command line as it stands
        // rather than assembled here: its length is what has to be
        // preserved, and only the file knows it.
        let mut needle = key.to_vec();
        needle.push(b'=');
        let start = cmdline
            .windows(needle.len())
            .position(|w| w == needle.as_slice())
            .ok_or_else(|| {
                Error::new(format!(
                    "{}= is not on the sealed command line:\n  {}",
                    lossy(key),
                    lossy(rstrip(&cmdline, b"\0"))
                ))
            })?;
        let tail = cmdline.get(start..).unwrap_or_default();
        let length = tail
            .iter()
            .position(|b| b" \0\n".contains(b))
            .unwrap_or(tail.len());
        let token = tail.get(..length).unwrap_or_default().to_vec();
        if let Some(old) = expected
            && token != old
        {
            return Err(Error::new(format!(
                "refusing to replace an unexpected token\n  have: {}\n  expected: {}",
                lossy(&token),
                lossy(old)
            )));
        }
        let new = match replacement {
            None => vec![b' '; token.len()],
            // Length has to match exactly. Growing the section would move
            // every section after it and produce a PE that does not load;
            // padding a shorter value with spaces would work but hides a
            // value that is not the shape expected, so it is refused instead.
            Some(replacement) if replacement.len() != token.len() => {
                return Err(Error::new(format!(
                    "replacement is {} bytes and the existing token is {}; a length-changing \
                     edit would move every following PE section\n  have: {}\n  want: {}",
                    replacement.len(),
                    token.len(),
                    lossy(&token),
                    lossy(replacement)
                )));
            }
            Some(replacement) => replacement.to_vec(),
        };
        let at = offset.saturating_add(start);
        at.checked_add(new.len())
            .and_then(|end| data.get_mut(at..end))
            .ok_or_else(|| Error::new("the command line reaches past the end of the file"))?
            .copy_from_slice(&new);
        Ok(token)
    })
}

fn run(args: &[OsString], out: &mut dyn std::io::Write) -> Result<()> {
    let write = |out: &mut dyn std::io::Write, text: String| {
        writeln!(out, "{text}").map_err(|e| Error::new(e.to_string()))
    };
    let action = args
        .first()
        .and_then(|a| a.to_str())
        .filter(|a| ACTIONS.contains(a))
        .ok_or_else(|| Error::new(USAGE))?;
    let arg = |index: usize| args.get(index);
    let bytes = |index: usize| arg(index).map_or(&[][..], |a| a.as_bytes());
    let count = args.len().saturating_add(1);

    match action {
        "boot-digest" => {
            let [_, path] = args else {
                return Err(Error::new("boot-digest needs one UKI path"));
            };
            return write(out, boot_digest(&read(Path::new(path))?)?);
        }
        "show" => {
            let path = path_or_default(arg(1))?;
            return write(out, cmdline_text(&read(&path)?)?);
        }
        "osrel" => {
            let path = path_or_default(arg(1))?;
            return write(out, read_osrel(&read(&path)?)?);
        }
        "set-osrel" => {
            let [_, assignments @ .., path] = args else {
                return Err(Error::new(
                    "set-osrel needs at least one KEY=VALUE and a UKI path",
                ));
            };
            if assignments.is_empty() {
                return Err(Error::new(
                    "set-osrel needs at least one KEY=VALUE and a UKI path",
                ));
            }
            let path = Path::new(path);
            let assignments: Vec<String> = assignments
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let (used, room) = rewrite_osrel(path, &assignments)?;
            write(
                out,
                format!(
                    "rewrote .osrel in {} ({used} of {room} bytes available in place)",
                    shown(path)
                ),
            )?;
            write(out, "it now reads:".into())?;
            for line in tilo_pycompat::splitlines(&read_osrel(&read(path)?)?) {
                write(out, format!("  {line}"))?;
            }
            return Ok(());
        }
        "append" => {
            if !(3..=4).contains(&count) {
                return Err(Error::new("append needs KEY[=VALUE] [UKI]"));
            }
            let path = path_or_default(arg(2))?;
            let (used, room) = append_cmdline(&path, bytes(1))?;
            return write(
                out,
                format!(
                    "appended {} to {} ({used} of {room} bytes)",
                    lossy(bytes(1)),
                    shown(&path)
                ),
            );
        }
        "remove" => {
            if !(3..=4).contains(&count) {
                return Err(Error::new("remove needs KEY [UKI]"));
            }
            let path = path_or_default(arg(2))?;
            let (removed, used, room) = remove_cmdline(&path, bytes(1))?;
            return write(
                out,
                format!(
                    "removed {} from {} ({used} of {room} bytes)",
                    lossy(&removed),
                    shown(&path)
                ),
            );
        }
        _ => {}
    }

    rewrite(action, args, out)
}

/// `blank`, `set` and `replace`: the length-preserving edits.
fn rewrite(action: &str, args: &[OsString], out: &mut dyn std::io::Write) -> Result<()> {
    let write = |out: &mut dyn std::io::Write, text: String| {
        writeln!(out, "{text}").map_err(|e| Error::new(e.to_string()))
    };
    let arg = |index: usize| args.get(index);
    let bytes = |index: usize| arg(index).map_or(&[][..], |a| a.as_bytes());
    let count = args.len().saturating_add(1);
    let (key, replacement, expected, path) = if action == "replace" {
        if !(4..=5).contains(&count) {
            return Err(Error::new("replace needs OLD=VALUE NEW=VALUE [UKI]"));
        }
        let (old, new) = (bytes(1), bytes(2));
        if !old.contains(&b'=') || !new.contains(&b'=') {
            return Err(Error::new(
                "replace arguments must both be complete KEY=VALUE tokens",
            ));
        }
        if old.len() != new.len() {
            return Err(Error::new(format!(
                "replacement is {} bytes and the existing token is {}; a length-changing edit \
                 would move following PE sections",
                new.len(),
                old.len()
            )));
        }
        let key = old.split(|&b| b == b'=').next().unwrap_or_default();
        (key, Some(new), Some(old), path_or_default(arg(3))?)
    } else if count < 3 {
        let example = if action == "set" { "=7`" } else { "`" };
        return Err(Error::new(format!(
            "{action} needs an argument, e.g. `{action} tilo.generation{example}"
        )));
    } else if action == "set" {
        let assignment = bytes(1);
        if !assignment.contains(&b'=') {
            return Err(Error::new(
                "set takes KEY=VALUE, e.g. `set composefs=<digest>`",
            ));
        }
        let key = assignment.split(|&b| b == b'=').next().unwrap_or_default();
        (key, Some(assignment), None, path_or_default(arg(2))?)
    } else {
        (bytes(1), None, None, path_or_default(arg(2))?)
    };

    let token = rewrite_token(&path, key, replacement, expected)?;
    if replacement.is_none() {
        write(
            out,
            format!(
                "blanked {} ({} bytes -> {} spaces) in {}",
                lossy(&token),
                token.len(),
                token.len(),
                shown(&path)
            ),
        )?;
    } else {
        write(
            out,
            format!(
                "rewrote {}= in {} ({} bytes, unchanged)",
                lossy(key),
                shown(&path),
                token.len()
            ),
        )?;
    }
    write(out, "sealed cmdline is now:".into())?;
    write(out, format!("  {}", cmdline_text(&read(&path)?)?))
}

/// Whether `argv0` names this tool rather than `tilo-esp-write`.
pub fn invoked_as(argv0: &OsStr) -> bool {
    Path::new(argv0).file_name() == Some(OsStr::new(NAME))
}

/// The command line after the program name.
pub fn main(args: &[OsString]) -> ExitCode {
    let mut stdout = std::io::stdout().lock();
    match run(args, &mut stdout) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = stdout.flush();
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
