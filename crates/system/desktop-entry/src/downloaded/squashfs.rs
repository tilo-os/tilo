//! Reading a few small files out of a squashfs 4.0 image, without mounting,
//! extracting or running anything.
//!
//! An `AppImage` is an ELF runtime with a squashfs image appended; its name
//! and icon live in that image. Mounting it means FUSE or running the
//! runtime, and both mean trusting the file first. So this reads the format
//! directly: the superblock, the inode and directory tables, and the data
//! blocks (and tail fragment) of the one file asked for. Everything is
//! bounded, since the image is untrusted: a file larger than the caller's
//! limit, a directory with absurdly many entries, a symlink chain or a path
//! deeper than a few levels is refused rather than followed.
//!
//! Compression: gzip (zlib), xz and zstd, which are what `AppImage` runtimes
//! produce. Anything else is reported as unsupported.

use std::io::{self, Read, Seek, SeekFrom};

const MAGIC: u32 = 0x7371_7368;
const METADATA_MAX: usize = 8192;
const METADATA_UNCOMPRESSED: u16 = 0x8000;
const DATA_UNCOMPRESSED: u32 = 1 << 24;
const NO_FRAGMENT: u32 = 0xFFFF_FFFF;
/// Entries one directory may have before it is refused as hostile.
const MAX_DIRECTORY_ENTRIES: usize = 65_536;
/// Symlinks followed while resolving one path.
const MAX_SYMLINKS: usize = 8;
/// Components one path may have.
const MAX_DEPTH: usize = 32;
/// Largest data block the format allows (1 MiB).
const MAX_BLOCK: u32 = 1 << 20;

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("squashfs: {what}"))
}

/// Little-endian reads over a byte slice that fail instead of panicking.
struct Bytes<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Bytes<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(n)
            .ok_or_else(|| invalid("length overflow"))?;
        let out = self
            .buf
            .get(self.at..end)
            .ok_or_else(|| invalid("truncated record"))?;
        self.at = end;
        Ok(out)
    }

    fn u16(&mut self) -> io::Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([
            *b.first().unwrap_or(&0),
            *b.get(1).unwrap_or(&0),
        ]))
    }

    fn u32(&mut self) -> io::Result<u32> {
        let mut out = [0u8; 4];
        out.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(out))
    }

    fn u64(&mut self) -> io::Result<u64> {
        let mut out = [0u8; 8];
        out.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(out))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compressor {
    Gzip,
    Xz,
    Zstd,
}

#[derive(Debug, Clone, Copy)]
struct Superblock {
    block_size: u32,
    compressor: Compressor,
    root_inode: u64,
    bytes_used: u64,
    inode_table: u64,
    directory_table: u64,
    fragment_table: u64,
}

/// What an inode is, as far as finding a file needs.
#[derive(Debug, Clone)]
enum Inode {
    Directory {
        block_index: u32,
        block_offset: u16,
        listing_size: u32,
    },
    File {
        blocks_start: u64,
        size: u64,
        fragment: u32,
        fragment_offset: u32,
        block_sizes: Vec<u32>,
    },
    Symlink {
        target: Vec<u8>,
    },
    Other,
}

/// One name in a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    inode: u64,
}

/// A squashfs image inside `file`, starting `base` bytes in.
pub struct Image<R> {
    file: R,
    base: u64,
    superblock: Superblock,
}

impl<R: Read + Seek> Image<R> {
    /// Read the superblock at `base`.
    ///
    /// # Errors
    ///
    /// Not a squashfs 4.0 image, or one compressed with a codec this reader
    /// does not decode.
    pub fn open(mut file: R, base: u64) -> io::Result<Self> {
        let mut raw = [0u8; 96];
        file.seek(SeekFrom::Start(base))?;
        file.read_exact(&mut raw)?;
        let mut b = Bytes::new(&raw);
        if b.u32()? != MAGIC {
            return Err(invalid("no squashfs magic"));
        }
        let _inode_count = b.u32()?;
        let _mtime = b.u32()?;
        let block_size = b.u32()?;
        let _fragment_count = b.u32()?;
        let compressor = match b.u16()? {
            1 => Compressor::Gzip,
            4 => Compressor::Xz,
            6 => Compressor::Zstd,
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("squashfs: compressor {other} is not supported"),
                ));
            }
        };
        let _block_log = b.u16()?;
        let _flags = b.u16()?;
        let _id_count = b.u16()?;
        let major = b.u16()?;
        let _minor = b.u16()?;
        if major != 4 {
            return Err(invalid("not version 4"));
        }
        if block_size == 0 || block_size > MAX_BLOCK || !block_size.is_power_of_two() {
            return Err(invalid("block size"));
        }
        let root_inode = b.u64()?;
        let bytes_used = b.u64()?;
        let _id_table = b.u64()?;
        let _xattr_table = b.u64()?;
        let inode_table = b.u64()?;
        let directory_table = b.u64()?;
        let fragment_table = b.u64()?;
        Ok(Self {
            file,
            base,
            superblock: Superblock {
                block_size,
                compressor,
                root_inode,
                bytes_used,
                inode_table,
                directory_table,
                fragment_table,
            },
        })
    }

    fn absolute(&self, offset: u64) -> io::Result<u64> {
        if offset > self.superblock.bytes_used {
            return Err(invalid("offset past the image"));
        }
        self.base
            .checked_add(offset)
            .ok_or_else(|| invalid("offset overflow"))
    }

    fn decompress(&self, compressed: &[u8], limit: usize) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        let cap = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
        match self.superblock.compressor {
            Compressor::Gzip => {
                flate2::read::ZlibDecoder::new(compressed)
                    .take(cap)
                    .read_to_end(&mut out)?;
            }
            Compressor::Xz => {
                lzma_rust2::XzReader::new(compressed, true)
                    .take(cap)
                    .read_to_end(&mut out)?;
            }
            Compressor::Zstd => {
                ruzstd::decoding::StreamingDecoder::new(compressed)
                    .map_err(|error| invalid(&error.to_string()))?
                    .take(cap)
                    .read_to_end(&mut out)?;
            }
        }
        if out.len() > limit {
            return Err(invalid("block decompresses past its limit"));
        }
        Ok(out)
    }

    /// One metadata block at absolute `position`: its bytes and where the
    /// next one starts.
    fn metadata_block(&mut self, position: u64) -> io::Result<(Vec<u8>, u64)> {
        let mut header = [0u8; 2];
        self.file.seek(SeekFrom::Start(position))?;
        self.file.read_exact(&mut header)?;
        let header = u16::from_le_bytes(header);
        let size = header & !METADATA_UNCOMPRESSED;
        if size == 0 || usize::from(size) > METADATA_MAX {
            return Err(invalid("metadata block size"));
        }
        let mut raw = vec![0u8; usize::from(size)];
        self.file.read_exact(&mut raw)?;
        let next = position
            .checked_add(2)
            .and_then(|p| p.checked_add(u64::from(size)))
            .ok_or_else(|| invalid("metadata overflow"))?;
        let data = if header & METADATA_UNCOMPRESSED == 0 {
            self.decompress(&raw, METADATA_MAX)?
        } else {
            raw
        };
        Ok((data, next))
    }

    /// `len` bytes of metadata starting `offset` bytes into the block at
    /// `table + block`.
    fn metadata(
        &mut self,
        table: u64,
        block: u64,
        offset: usize,
        len: usize,
    ) -> io::Result<Vec<u8>> {
        let start = table
            .checked_add(block)
            .ok_or_else(|| invalid("metadata overflow"))?;
        let mut position = self.absolute(start)?;
        let mut skip = offset;
        let mut out = Vec::with_capacity(len.min(1 << 20));
        while out.len() < len {
            let (data, next) = self.metadata_block(position)?;
            let from = data.get(skip..).ok_or_else(|| invalid("metadata offset"))?;
            skip = 0;
            let want = len.saturating_sub(out.len()).min(from.len());
            out.extend_from_slice(from.get(..want).unwrap_or_default());
            if next > self.absolute(self.superblock.bytes_used)? {
                break;
            }
            position = next;
        }
        if out.len() < len {
            return Err(invalid("metadata runs past the image"));
        }
        Ok(out)
    }

    fn inode(&mut self, reference: u64) -> io::Result<Inode> {
        let block = reference >> 16;
        let offset = usize::try_from(reference & 0xFFFF).map_err(|_| invalid("inode reference"))?;
        let table = self.superblock.inode_table;
        // The longest fixed part of any inode read here, header included.
        let head = self.metadata(table, block, offset, 16)?;
        let kind = Bytes::new(&head).u16()?;
        match kind {
            1 => {
                let raw = self.metadata(table, block, offset, 32)?;
                let mut b = Bytes::new(&raw);
                b.take(16)?;
                let block_index = b.u32()?;
                let _links = b.u32()?;
                let file_size = b.u16()?;
                let block_offset = b.u16()?;
                Ok(Inode::Directory {
                    block_index,
                    block_offset,
                    listing_size: u32::from(file_size).saturating_sub(3),
                })
            }
            8 => {
                let raw = self.metadata(table, block, offset, 40)?;
                let mut b = Bytes::new(&raw);
                b.take(16)?;
                let _links = b.u32()?;
                let file_size = b.u32()?;
                let block_index = b.u32()?;
                let _parent = b.u32()?;
                let _index_count = b.u16()?;
                let block_offset = b.u16()?;
                Ok(Inode::Directory {
                    block_index,
                    block_offset,
                    listing_size: file_size.saturating_sub(3),
                })
            }
            2 | 9 => self.file_inode(kind, block, offset),
            3 | 10 => {
                let raw = self.metadata(table, block, offset, 24)?;
                let mut b = Bytes::new(&raw);
                b.take(16)?;
                let _links = b.u32()?;
                let target_size = usize::try_from(b.u32()?).map_err(|_| invalid("symlink"))?;
                if target_size > 4096 {
                    return Err(invalid("symlink target too long"));
                }
                let raw =
                    self.metadata(table, block, offset, 24usize.saturating_add(target_size))?;
                let target = raw.get(24..).unwrap_or_default().to_vec();
                Ok(Inode::Symlink { target })
            }
            _ => Ok(Inode::Other),
        }
    }

    fn file_inode(&mut self, kind: u16, block: u64, offset: usize) -> io::Result<Inode> {
        let table = self.superblock.inode_table;
        let fixed = if kind == 2 { 32 } else { 56 };
        let raw = self.metadata(table, block, offset, fixed)?;
        let mut b = Bytes::new(&raw);
        b.take(16)?;
        let (blocks_start, fragment, fragment_offset, size) = if kind == 2 {
            let start = u64::from(b.u32()?);
            let fragment = b.u32()?;
            let fragment_offset = b.u32()?;
            let size = u64::from(b.u32()?);
            (start, fragment, fragment_offset, size)
        } else {
            let start = b.u64()?;
            let size = b.u64()?;
            let _sparse = b.u64()?;
            let _links = b.u32()?;
            let fragment = b.u32()?;
            let fragment_offset = b.u32()?;
            (start, fragment, fragment_offset, size)
        };
        let block_size = u64::from(self.superblock.block_size);
        let full = size.checked_div(block_size).unwrap_or(0);
        let count = if fragment == NO_FRAGMENT && size.checked_rem(block_size).unwrap_or(0) != 0 {
            full.saturating_add(1)
        } else {
            full
        };
        let count = usize::try_from(count).map_err(|_| invalid("block count"))?;
        let list_len = count.checked_mul(4).ok_or_else(|| invalid("block count"))?;
        let total = fixed
            .checked_add(list_len)
            .ok_or_else(|| invalid("block count"))?;
        let raw = self.metadata(table, block, offset, total)?;
        let mut b = Bytes::new(&raw);
        b.take(fixed)?;
        let mut block_sizes = Vec::with_capacity(count);
        for _ in 0..count {
            block_sizes.push(b.u32()?);
        }
        Ok(Inode::File {
            blocks_start,
            size,
            fragment,
            fragment_offset,
            block_sizes,
        })
    }

    fn list(
        &mut self,
        block_index: u32,
        block_offset: u16,
        size: u32,
    ) -> io::Result<Vec<DirEntry>> {
        let len = usize::try_from(size).map_err(|_| invalid("directory size"))?;
        if len == 0 {
            return Ok(Vec::new());
        }
        let raw = self.metadata(
            self.superblock.directory_table,
            u64::from(block_index),
            usize::from(block_offset),
            len,
        )?;
        let mut b = Bytes::new(&raw);
        let mut out = Vec::new();
        while b.at < raw.len() {
            let count = b.u32()?.saturating_add(1);
            let start = b.u32()?;
            let _inode_number = b.u32()?;
            for _ in 0..count {
                let offset = b.u16()?;
                let _inode_delta = b.u16()?;
                let _kind = b.u16()?;
                let name_len = usize::from(b.u16()?).saturating_add(1);
                let name = String::from_utf8_lossy(b.take(name_len)?).into_owned();
                out.push(DirEntry {
                    name,
                    inode: (u64::from(start) << 16) | u64::from(offset),
                });
                if out.len() > MAX_DIRECTORY_ENTRIES {
                    return Err(invalid("directory has too many entries"));
                }
            }
        }
        Ok(out)
    }

    fn directory(&mut self, inode: &Inode) -> io::Result<Vec<DirEntry>> {
        match *inode {
            Inode::Directory {
                block_index,
                block_offset,
                listing_size,
            } => self.list(block_index, block_offset, listing_size),
            _ => Err(invalid("not a directory")),
        }
    }

    /// The names in the image's top directory.
    ///
    /// # Errors
    ///
    /// The image is damaged.
    pub fn root_names(&mut self) -> io::Result<Vec<String>> {
        let root = self.inode(self.superblock.root_inode)?;
        Ok(self
            .directory(&root)?
            .into_iter()
            .map(|entry| entry.name)
            .collect())
    }

    /// The inode at `path` (relative to the image's root), symlinks inside
    /// the image followed.
    fn resolve(&mut self, path: &str) -> io::Result<Option<Inode>> {
        let mut pending: Vec<String> = components(path).into_iter().rev().collect();
        let mut stack: Vec<Inode> = Vec::new();
        let mut current = self.inode(self.superblock.root_inode)?;
        let mut symlinks = 0usize;
        let mut steps = 0usize;
        while let Some(name) = pending.pop() {
            steps = steps.saturating_add(1);
            if steps > MAX_DEPTH.saturating_mul(MAX_SYMLINKS.saturating_add(1)) {
                return Err(invalid("path too deep"));
            }
            if name == ".." {
                if let Some(parent) = stack.pop() {
                    current = parent;
                }
                continue;
            }
            let entries = self.directory(&current)?;
            let Some(entry) = entries.into_iter().find(|entry| entry.name == name) else {
                return Ok(None);
            };
            let next = self.inode(entry.inode)?;
            if let Inode::Symlink { target } = &next {
                symlinks = symlinks.saturating_add(1);
                if symlinks > MAX_SYMLINKS {
                    return Err(invalid("too many symlinks"));
                }
                let target = String::from_utf8_lossy(target).into_owned();
                if target.starts_with('/') {
                    // An absolute target names the host, not the image.
                    return Ok(None);
                }
                for part in components(&target).into_iter().rev() {
                    pending.push(part);
                }
                continue;
            }
            stack.push(std::mem::replace(&mut current, next));
        }
        Ok(Some(current))
    }

    fn fragment_block(&mut self, index: u32) -> io::Result<Vec<u8>> {
        let table_entry = u64::from(index / 512)
            .checked_mul(8)
            .and_then(|o| o.checked_add(self.superblock.fragment_table))
            .ok_or_else(|| invalid("fragment index"))?;
        let mut pointer = [0u8; 8];
        let position = self.absolute(table_entry)?;
        self.file.seek(SeekFrom::Start(position))?;
        self.file.read_exact(&mut pointer)?;
        let block = u64::from_le_bytes(pointer);
        let offset = usize::try_from(index % 512)
            .ok()
            .and_then(|i| i.checked_mul(16))
            .ok_or_else(|| invalid("fragment index"))?;
        let raw = self.metadata(0, block, offset, 16)?;
        let mut b = Bytes::new(&raw);
        let start = b.u64()?;
        let size = b.u32()?;
        self.data_block(start, size)
    }

    fn data_block(&mut self, start: u64, word: u32) -> io::Result<Vec<u8>> {
        let size = word & !DATA_UNCOMPRESSED;
        let block_size =
            usize::try_from(self.superblock.block_size).map_err(|_| invalid("block"))?;
        if size == 0 {
            return Ok(vec![0u8; block_size]);
        }
        if size > MAX_BLOCK {
            return Err(invalid("data block size"));
        }
        let mut raw = vec![0u8; usize::try_from(size).map_err(|_| invalid("block"))?];
        let position = self.absolute(start)?;
        self.file.seek(SeekFrom::Start(position))?;
        self.file.read_exact(&mut raw)?;
        if word & DATA_UNCOMPRESSED == 0 {
            self.decompress(&raw, block_size)
        } else {
            Ok(raw)
        }
    }

    /// The contents of the regular file at `path`, symlinks inside the image
    /// followed; `None` when there is no such file. A file larger than
    /// `limit` bytes is an error, never read.
    ///
    /// # Errors
    ///
    /// The image is damaged or the file is larger than `limit`.
    pub fn read_file(&mut self, path: &str, limit: usize) -> io::Result<Option<Vec<u8>>> {
        let Some(inode) = self.resolve(path)? else {
            return Ok(None);
        };
        let Inode::File {
            blocks_start,
            size,
            fragment,
            fragment_offset,
            block_sizes,
        } = inode
        else {
            return Ok(None);
        };
        let size = usize::try_from(size).map_err(|_| invalid("file size"))?;
        if size > limit {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "squashfs: file larger than allowed",
            ));
        }
        let mut out = Vec::with_capacity(size);
        let mut position = blocks_start;
        for word in block_sizes {
            let block = self.data_block(position, word)?;
            let want = size.saturating_sub(out.len()).min(block.len());
            out.extend_from_slice(block.get(..want).unwrap_or_default());
            position = position
                .checked_add(u64::from(word & !DATA_UNCOMPRESSED))
                .ok_or_else(|| invalid("data overflow"))?;
        }
        if out.len() < size && fragment != NO_FRAGMENT {
            let block = self.fragment_block(fragment)?;
            let from = usize::try_from(fragment_offset).map_err(|_| invalid("fragment offset"))?;
            let want = size.saturating_sub(out.len());
            let end = from.checked_add(want).ok_or_else(|| invalid("fragment"))?;
            out.extend_from_slice(block.get(from..end).ok_or_else(|| invalid("fragment"))?);
        }
        if out.len() != size {
            return Err(invalid("file shorter than its size"));
        }
        Ok(Some(out))
    }
}

fn components(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::arithmetic_side_effects,
    reason = "a fixture writer for images of a few kilobytes"
)]
pub(crate) mod tests {
    use super::*;

    /// Writes a minimal gzip-compressed squashfs 4.0 image of regular files
    /// and symlinks in one root directory, plus directories one level deep.
    /// The tests build their `AppImage` fixtures with it, so no image tool
    /// and no download is needed.
    pub(crate) struct Writer {
        entries: Vec<(String, Node)>,
    }

    enum Node {
        File(Vec<u8>),
        Symlink(String),
        Dir(Vec<(String, Node)>),
    }

    impl Writer {
        pub(crate) fn new() -> Self {
            Self {
                entries: Vec::new(),
            }
        }

        pub(crate) fn file(mut self, path: &str, bytes: &[u8]) -> Self {
            insert(&mut self.entries, path, Node::File(bytes.to_vec()));
            self
        }

        pub(crate) fn symlink(mut self, path: &str, target: &str) -> Self {
            insert(&mut self.entries, path, Node::Symlink(target.to_owned()));
            self
        }

        /// The image's bytes. Files go in whole blocks (no fragments) except
        /// those under one block, which share one fragment block, so both
        /// read paths are exercised.
        pub(crate) fn build(self) -> Vec<u8> {
            const BLOCK: u32 = 4096;
            let mut data: Vec<u8> = vec![0u8; 96];
            let mut fragment: Vec<u8> = Vec::new();
            let mut inodes: Vec<u8> = Vec::new();
            let mut dirs: Vec<u8> = Vec::new();
            let mut number = 0u32;
            let root = write_dir(
                &self.entries,
                &mut data,
                &mut fragment,
                &mut inodes,
                &mut dirs,
                &mut number,
                BLOCK,
            );
            // The single fragment block, compressed.
            let fragment_start = data.len() as u64;
            let compressed = zlib(&fragment);
            data.extend_from_slice(&compressed);
            let fragment_entry = {
                let mut e = Vec::new();
                e.extend_from_slice(&fragment_start.to_le_bytes());
                e.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
                e.extend_from_slice(&0u32.to_le_bytes());
                e
            };
            let inode_table = data.len() as u64;
            data.extend_from_slice(&metadata(&inodes));
            let directory_table = data.len() as u64;
            data.extend_from_slice(&metadata(&dirs));
            let fragment_meta = data.len() as u64;
            data.extend_from_slice(&metadata(&fragment_entry));
            let fragment_table = data.len() as u64;
            data.extend_from_slice(&fragment_meta.to_le_bytes());
            let id_table_meta = data.len() as u64;
            data.extend_from_slice(&metadata(&0u32.to_le_bytes()));
            let id_table = data.len() as u64;
            data.extend_from_slice(&id_table_meta.to_le_bytes());
            let bytes_used = data.len() as u64;
            let mut sb = Vec::new();
            sb.extend_from_slice(&MAGIC.to_le_bytes());
            sb.extend_from_slice(&number.to_le_bytes());
            sb.extend_from_slice(&0u32.to_le_bytes());
            sb.extend_from_slice(&BLOCK.to_le_bytes());
            sb.extend_from_slice(&1u32.to_le_bytes());
            sb.extend_from_slice(&1u16.to_le_bytes()); // gzip
            sb.extend_from_slice(&12u16.to_le_bytes());
            sb.extend_from_slice(&0u16.to_le_bytes());
            sb.extend_from_slice(&1u16.to_le_bytes());
            sb.extend_from_slice(&4u16.to_le_bytes());
            sb.extend_from_slice(&0u16.to_le_bytes());
            sb.extend_from_slice(&root.to_le_bytes());
            sb.extend_from_slice(&bytes_used.to_le_bytes());
            sb.extend_from_slice(&id_table.to_le_bytes());
            sb.extend_from_slice(&u64::MAX.to_le_bytes());
            sb.extend_from_slice(&inode_table.to_le_bytes());
            sb.extend_from_slice(&directory_table.to_le_bytes());
            sb.extend_from_slice(&fragment_table.to_le_bytes());
            sb.extend_from_slice(&u64::MAX.to_le_bytes());
            data[..96].copy_from_slice(&sb);
            data
        }
    }

    fn insert(entries: &mut Vec<(String, Node)>, path: &str, node: Node) {
        match path.split_once('/') {
            None => entries.push((path.to_owned(), node)),
            Some((head, rest)) => {
                if !entries.iter().any(|(name, _)| name == head) {
                    entries.push((head.to_owned(), Node::Dir(Vec::new())));
                }
                let Some((_, Node::Dir(children))) =
                    entries.iter_mut().find(|(name, _)| name == head)
                else {
                    panic!("{head} is not a directory");
                };
                insert(children, rest, node);
            }
        }
    }

    fn zlib(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write as _;
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    /// One table as a run of compressed metadata blocks. Every table here is
    /// written so that a reference's block part is always 0 or a real block
    /// start: the writer keeps each table under one block's size.
    fn metadata(bytes: &[u8]) -> Vec<u8> {
        assert!(
            bytes.len() <= METADATA_MAX,
            "fixture tables stay in one block"
        );
        let compressed = zlib(bytes);
        let mut out = Vec::new();
        out.extend_from_slice(&(compressed.len() as u16).to_le_bytes());
        out.extend_from_slice(&compressed);
        out
    }

    /// Writes `entries` and returns the directory's inode reference.
    fn write_dir(
        entries: &[(String, Node)],
        data: &mut Vec<u8>,
        fragment: &mut Vec<u8>,
        inodes: &mut Vec<u8>,
        dirs: &mut Vec<u8>,
        number: &mut u32,
        block: u32,
    ) -> u64 {
        let mut sorted: Vec<&(String, Node)> = entries.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        let mut children: Vec<(String, u64, u16)> = Vec::new();
        for (name, node) in sorted {
            let reference = match node {
                Node::Dir(inner) => write_dir(inner, data, fragment, inodes, dirs, number, block),
                Node::File(bytes) => {
                    let blocks_start = data.len() as u32;
                    let whole = bytes.len() / block as usize;
                    let mut sizes = Vec::new();
                    for chunk in bytes.chunks(block as usize).take(whole) {
                        let compressed = zlib(chunk);
                        sizes.push(compressed.len() as u32);
                        data.extend_from_slice(&compressed);
                    }
                    let tail = &bytes[whole * block as usize..];
                    let (frag, offset) = if tail.is_empty() {
                        (NO_FRAGMENT, 0)
                    } else {
                        let offset = fragment.len() as u32;
                        fragment.extend_from_slice(tail);
                        (0, offset)
                    };
                    *number += 1;
                    let at = inodes.len() as u64;
                    inodes.extend_from_slice(&2u16.to_le_bytes());
                    inodes.extend_from_slice(&0o644u16.to_le_bytes());
                    inodes.extend_from_slice(&[0, 0, 0, 0]);
                    inodes.extend_from_slice(&0u32.to_le_bytes());
                    inodes.extend_from_slice(&number.to_le_bytes());
                    inodes.extend_from_slice(&blocks_start.to_le_bytes());
                    inodes.extend_from_slice(&frag.to_le_bytes());
                    inodes.extend_from_slice(&offset.to_le_bytes());
                    inodes.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                    for size in sizes {
                        inodes.extend_from_slice(&size.to_le_bytes());
                    }
                    at
                }
                Node::Symlink(target) => {
                    *number += 1;
                    let at = inodes.len() as u64;
                    inodes.extend_from_slice(&3u16.to_le_bytes());
                    inodes.extend_from_slice(&0o777u16.to_le_bytes());
                    inodes.extend_from_slice(&[0, 0, 0, 0]);
                    inodes.extend_from_slice(&0u32.to_le_bytes());
                    inodes.extend_from_slice(&number.to_le_bytes());
                    inodes.extend_from_slice(&1u32.to_le_bytes());
                    inodes.extend_from_slice(&(target.len() as u32).to_le_bytes());
                    inodes.extend_from_slice(target.as_bytes());
                    at
                }
            };
            let kind = match node {
                Node::Dir(_) => 1,
                Node::File(_) => 2,
                Node::Symlink(_) => 3,
            };
            children.push((name.clone(), reference, kind));
        }
        let listing_at = dirs.len() as u32;
        if !children.is_empty() {
            dirs.extend_from_slice(&(children.len() as u32 - 1).to_le_bytes());
            dirs.extend_from_slice(&0u32.to_le_bytes());
            dirs.extend_from_slice(&1u32.to_le_bytes());
            for (name, reference, kind) in &children {
                dirs.extend_from_slice(&(*reference as u16).to_le_bytes());
                dirs.extend_from_slice(&0u16.to_le_bytes());
                dirs.extend_from_slice(&kind.to_le_bytes());
                dirs.extend_from_slice(&(name.len() as u16 - 1).to_le_bytes());
                dirs.extend_from_slice(name.as_bytes());
            }
        }
        let listing_size = dirs.len() as u32 - listing_at;
        *number += 1;
        let at = inodes.len() as u64;
        inodes.extend_from_slice(&1u16.to_le_bytes());
        inodes.extend_from_slice(&0o755u16.to_le_bytes());
        inodes.extend_from_slice(&[0, 0, 0, 0]);
        inodes.extend_from_slice(&0u32.to_le_bytes());
        inodes.extend_from_slice(&number.to_le_bytes());
        inodes.extend_from_slice(&0u32.to_le_bytes()); // block index
        inodes.extend_from_slice(&2u32.to_le_bytes());
        inodes.extend_from_slice(&((listing_size + 3) as u16).to_le_bytes());
        inodes.extend_from_slice(&(listing_at as u16).to_le_bytes());
        inodes.extend_from_slice(&0u32.to_le_bytes());
        at
    }

    #[test]
    fn files_symlinks_and_subdirectories_read_back() {
        let big: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let image = Writer::new()
            .file("app.desktop", b"[Desktop Entry]\nName=Thing\n")
            .file("usr/share/icon.png", &big)
            .symlink(".DirIcon", "usr/share/icon.png")
            .build();
        let mut image = Image::open(std::io::Cursor::new(image), 0).unwrap();
        let mut names = image.root_names().unwrap();
        names.sort();
        assert_eq!(names, [".DirIcon", "app.desktop", "usr"]);
        assert_eq!(
            image.read_file("app.desktop", 1024).unwrap().unwrap(),
            b"[Desktop Entry]\nName=Thing\n"
        );
        assert_eq!(image.read_file(".DirIcon", 1 << 20).unwrap().unwrap(), big);
        assert!(image.read_file("missing", 1024).unwrap().is_none());
        assert!(
            image.read_file("usr/share/icon.png", 100).is_err(),
            "a file over the limit is refused, never read"
        );
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(Image::open(std::io::Cursor::new(vec![0u8; 200]), 0).is_err());
        let mut image = Writer::new().file("a", b"x").build();
        // Corrupt the inode table pointer.
        image[64..72].copy_from_slice(&u64::MAX.to_le_bytes());
        if let Ok(mut opened) = Image::open(std::io::Cursor::new(image), 0) {
            assert!(opened.root_names().is_err());
        }
    }
}
