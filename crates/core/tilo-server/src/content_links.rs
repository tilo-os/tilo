//! Links to a workspace's files, served at this host's web origin
//! ([`tilo_actions::remote::ContentLink`]).
//!
//! A member's page asks for a link over its session; the workspace actions
//! service checks that they may read the workspace and mints one here
//! ([`ContentLinks::mint`]). The link names the files and the store they are
//! in, and the browser listener serves them at it ([`serve`]) for
//! [`CONTENT_LINK_TTL_SECS`]: the browser's own download manager fetches it
//! when a file is dragged out of the page onto the desktop, and the page
//! streams it into the file a person saves to. Nothing of it passes through
//! the page's memory, so a file of a gigabyte saves as a small one does.
//!
//! Bytes are read from the store as they are sent, a buffer at a time: a
//! whole file from its one blob, a file the folder mount wrote chunk by
//! chunk from the manifest its node names. Several files, a folder, go as
//! one zip, stored rather than compressed, written as it streams: each
//! member's checksum follows its bytes in a data descriptor, and its size,
//! which the store knows beforehand, is in its header, so the length of the
//! whole zip is known before the first byte goes.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tilo_actions::remote::{CONTENT_LINK_TTL_SECS, ContentLink, ContentLinkAsk, ContentLinkFile};
use tilo_content::ContentStore;
use tilo_lock::MutexRecover as _;

/// Where the browser listener serves links, below [`tilo_server_protocol::web::API_PREFIX`].
pub const ROUTE: &str = "/content/";

/// How many links are open at once: past this the one closest to expiring
/// gives way.
const MAX_OPEN: usize = 4096;

/// The bytes of a manifest's first entry, which say it is one
/// ([`fs_content::is_manifest`]).
const MANIFEST_HEAD: usize = 32;

/// How much of a file is read from the store before it is sent.
pub(crate) const BUFFER: usize = 256 * 1024;

/// One link: what it serves, from which store, until when.
#[derive(Clone)]
pub(crate) struct Grant {
    pub(crate) store: ContentStore,
    pub(crate) ask: ContentLinkAsk,
    until: Instant,
}

/// The links this host serves, by token.
#[derive(Default)]
pub struct ContentLinks {
    open: Mutex<HashMap<[u8; 32], Grant>>,
}

impl ContentLinks {
    /// A link to `ask`'s files in `store`: the caller was found to read the
    /// workspace `store` is.
    pub fn mint(&self, store: ContentStore, ask: ContentLinkAsk) -> ContentLink {
        let token: [u8; 32] = rand::random();
        let now = Instant::now();
        let ttl = Duration::from_secs(CONTENT_LINK_TTL_SECS);
        let path = format!(
            "{}{ROUTE}{}/{}",
            tilo_server_protocol::web::API_PREFIX,
            hex::encode(token),
            path_segment(&ask.name)
        );
        let mut open = self.open.lock_recover();
        open.retain(|_, grant| grant.until > now);
        while open.len() >= MAX_OPEN {
            let Some(soonest) = open
                .iter()
                .min_by_key(|(_, grant)| grant.until)
                .map(|(token, _)| *token)
            else {
                break;
            };
            open.remove(&soonest);
        }
        open.insert(
            token,
            Grant {
                store,
                ask,
                until: now.checked_add(ttl).unwrap_or(now),
            },
        );
        drop(open);
        ContentLink {
            path,
            expires_in_secs: CONTENT_LINK_TTL_SECS,
        }
    }

    /// What the link `route` (below [`ROUTE`]: `<token>/<name>`) names,
    /// while it is good.
    pub(crate) fn grant(&self, route: &str) -> Option<Grant> {
        let token = route.split('/').next()?;
        let token: [u8; 32] = hex::decode(token).ok()?.try_into().ok()?;
        let now = Instant::now();
        let mut open = self.open.lock_recover();
        match open.get(&token) {
            Some(grant) if grant.until > now => Some(grant.clone()),
            Some(_) => {
                open.remove(&token);
                None
            }
            None => None,
        }
    }
}

/// `name` as one path segment: every byte but the unreserved ones
/// percent-encoded, so the name a browser saves under reads back exactly.
fn path_segment(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// The `Content-Disposition` that shows the response as `name` in the
/// browser's own tab, where [`tilo_actions::remote::viewable`] says it may be.
pub(crate) fn inline_disposition(name: &str) -> String {
    disposition(name).replacen("attachment", "inline", 1)
}

/// The `Content-Disposition` that saves the response as `name`: an ASCII
/// fallback, and the exact name for every browser that reads `filename*`.
pub(crate) fn disposition(name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "attachment; filename=\"{fallback}\"; filename*=UTF-8''{}",
        path_segment(name)
    )
}

/// How one file's bytes are read out of the store.
enum Layout {
    /// One blob, under the node's hash.
    Whole,
    /// The chunks a manifest under the node's hash lists, in order.
    Chunks(Vec<String>),
}

/// A file ready to stream: how it is laid out in the store and its length.
struct Planned {
    path: String,
    hash: String,
    layout: Layout,
    size: u64,
}

/// Why a link's files cannot be served.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// A file is not in the store (yet): the host has not kept it.
    Missing(String),
    /// The files are past what a zip without ZIP64 holds.
    TooLarge,
}

impl Refusal {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Missing(path) => format!("this host does not keep {path} yet; try again soon"),
            Self::TooLarge => {
                "this folder is too large to save as one zip; save the files in it instead".into()
            }
        }
    }
}

/// What reading `file` out of `store` takes, and its length.
fn plan(store: &ContentStore, file: &ContentLinkFile) -> Result<Planned, Refusal> {
    let missing = || Refusal::Missing(file.path.clone());
    // A manifest is small: read whole to tell it from a file whose bytes
    // happen to start the same way, which it would not decode as.
    let head = {
        // The manifest's first entry says it is one: a file shorter than that
        // is not.
        let reader = store.open(&file.hash).ok_or_else(missing)?;
        let mut head = Vec::with_capacity(MANIFEST_HEAD);
        reader
            .take(MANIFEST_HEAD as u64)
            .read_to_end(&mut head)
            .map_err(|_| missing())?;
        head
    };
    if fs_content::is_manifest(&head)
        && let Some(manifest) = store
            .read(&file.hash)
            .and_then(|bytes| fs_content::Manifest::decode(&bytes).ok())
    {
        let chunks: Vec<String> = manifest
            .chunks()
            .iter()
            .map(|chunk| hex::encode(chunk.hash))
            .collect();
        if let Some(gone) = chunks.iter().find(|chunk| !store.has(chunk)) {
            tracing::debug!(chunk = %gone, path = %file.path, "a chunk of a linked file is not kept");
            return Err(missing());
        }
        return Ok(Planned {
            path: file.path.clone(),
            hash: file.hash.clone(),
            layout: Layout::Chunks(chunks),
            size: manifest.total_len(),
        });
    }
    let size = if let Some(size) = store.size(&file.hash) {
        size
    } else {
        // A store that does not know a blob's length without reading it:
        // read it through once, holding nothing.
        let mut reader = store.open(&file.hash).ok_or_else(missing)?;
        std::io::copy(&mut reader, &mut std::io::sink()).map_err(|_| missing())?
    };
    Ok(Planned {
        path: file.path.clone(),
        hash: file.hash.clone(),
        layout: Layout::Whole,
        size,
    })
}

/// Bytes of the link, as they are read: where [`stream`] writes them.
pub(crate) trait Sink {
    /// Send `bytes` on; `false` when whoever reads stopped reading.
    fn send(&mut self, bytes: Vec<u8>) -> bool;
}

/// What streaming a link's files takes: each file planned, and the length of
/// the response.
pub(crate) struct Streaming {
    files: Vec<Planned>,
    zip: bool,
    /// The response's length.
    pub(crate) length: u64,
}

/// Plan serving `ask`'s files out of `store`.
///
/// # Errors
///
/// A file the store does not hold, or a folder too large for a zip.
pub(crate) fn prepare(store: &ContentStore, ask: &ContentLinkAsk) -> Result<Streaming, Refusal> {
    let files = ask
        .files
        .iter()
        .map(|file| plan(store, file))
        .collect::<Result<Vec<_>, _>>()?;
    if !ask.zip {
        let length = files.first().map_or(0, |file| file.size);
        return Ok(Streaming {
            files,
            zip: false,
            length,
        });
    }
    let length = zip::length(files.iter().map(|file| (file.path.as_str(), file.size)))
        .ok_or(Refusal::TooLarge)?;
    Ok(Streaming {
        files,
        zip: true,
        length,
    })
}

/// Read every planned file out of `store` into `sink`, as a zip when asked.
/// Stops early, without error, when the sink stops reading.
///
/// # Errors
///
/// A file whose bytes could not be read, or did not match their hash.
pub(crate) fn stream(
    store: &ContentStore,
    streaming: &Streaming,
    sink: &mut dyn Sink,
) -> Result<(), String> {
    if !streaming.zip {
        if let Some(file) = streaming.files.first() {
            copy(store, file, sink, &mut |_| {})?;
        }
        return Ok(());
    }
    let mut writer = zip::Writer::default();
    for file in &streaming.files {
        if !sink.send(writer.begin(&file.path, file.size)) {
            return Ok(());
        }
        let mut crc = crc32fast::Hasher::new();
        let sent = copy(store, file, sink, &mut |bytes| crc.update(bytes))?;
        if !sent {
            return Ok(());
        }
        if !sink.send(writer.end(crc.finalize())) {
            return Ok(());
        }
    }
    sink.send(writer.finish());
    Ok(())
}

/// Copy `file`'s bytes out of `store` into `sink`, each buffer also handed
/// to `seen`. `Ok(false)` when the sink stopped reading.
fn copy(
    store: &ContentStore,
    file: &Planned,
    sink: &mut dyn Sink,
    seen: &mut dyn FnMut(&[u8]),
) -> Result<bool, String> {
    let blobs: Vec<&str> = match &file.layout {
        Layout::Whole => vec![file.hash.as_str()],
        Layout::Chunks(chunks) => chunks.iter().map(String::as_str).collect(),
    };
    let mut sent = 0_u64;
    for blob in blobs {
        let mut reader = store
            .open(blob)
            .ok_or_else(|| format!("{} is no longer kept", file.path))?;
        loop {
            let mut buffer = vec![0_u8; BUFFER];
            let read = reader
                .read(&mut buffer)
                .map_err(|error| format!("{}: {error}", file.path))?;
            if read == 0 {
                break;
            }
            buffer.truncate(read);
            sent = sent.saturating_add(buffer.len() as u64);
            if sent > file.size {
                return Err(format!(
                    "{} holds more than the {} bytes its node names",
                    file.path, file.size
                ));
            }
            seen(&buffer);
            if !sink.send(buffer) {
                return Ok(false);
            }
        }
    }
    if sent != file.size {
        return Err(format!(
            "{} read {sent} bytes of the {} its node names",
            file.path, file.size
        ));
    }
    Ok(true)
}

/// A zip written as its members stream: stored members, each with a data
/// descriptor after its bytes.
mod zip {
    /// Names are UTF-8 (bit 11); the checksum follows the bytes (bit 3).
    const FLAGS: u16 = (1 << 11) | (1 << 3);
    /// Version 2.0: what reading a stored member needs.
    const VERSION: u16 = 20;
    /// 1980-01-01, the earliest date a zip holds.
    const DATE: u16 = 0x0021;
    const LOCAL: usize = 30;
    const DESCRIPTOR: usize = 16;
    const CENTRAL: usize = 46;
    const END: usize = 22;

    /// The length of the zip of `members` (path, size), or `None` past what
    /// a zip without ZIP64 holds.
    pub(super) fn length<'a>(members: impl Iterator<Item = (&'a str, u64)>) -> Option<u64> {
        let mut count = 0_u64;
        let mut body = 0_u64;
        let mut central = 0_u64;
        for (path, size) in members {
            u32::try_from(size).ok()?;
            let name = u16::try_from(path.len()).ok()?;
            count = count.checked_add(1)?;
            // Each member must start where an offset of 32 bits reaches.
            u32::try_from(body).ok()?;
            body = body
                .checked_add((LOCAL + DESCRIPTOR) as u64)?
                .checked_add(u64::from(name))?
                .checked_add(size)?;
            central = central
                .checked_add(CENTRAL as u64)?
                .checked_add(u64::from(name))?;
        }
        u16::try_from(count).ok()?;
        u32::try_from(body).ok()?;
        u32::try_from(central).ok()?;
        body.checked_add(central)?.checked_add(END as u64)
    }

    /// One member, as the central directory names it.
    struct Entry {
        name: Vec<u8>,
        size: u32,
        crc: u32,
        offset: u32,
    }

    /// Where the zip stands: what went, and the members so far.
    #[derive(Default)]
    pub(super) struct Writer {
        at: u64,
        entries: Vec<Entry>,
    }

    impl Writer {
        /// The local header of the member `path`, `size` bytes long. The
        /// caller checked the zip's [`length`] first, which bounds every
        /// number here.
        pub(super) fn begin(&mut self, path: &str, size: u64) -> Vec<u8> {
            let name = path.as_bytes().to_vec();
            let size = u32::try_from(size).unwrap_or(u32::MAX);
            let name_len = u16::try_from(name.len()).unwrap_or(u16::MAX);
            let mut out = Vec::with_capacity(LOCAL.saturating_add(name.len()));
            out.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
            out.extend_from_slice(&VERSION.to_le_bytes());
            out.extend_from_slice(&FLAGS.to_le_bytes());
            out.extend_from_slice(&0_u16.to_le_bytes()); // stored
            out.extend_from_slice(&0_u16.to_le_bytes()); // time
            out.extend_from_slice(&DATE.to_le_bytes());
            // The checksum is in the descriptor; the sizes, known now, are
            // here too, for a reader that walks the local headers.
            out.extend_from_slice(&0_u32.to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&name_len.to_le_bytes());
            out.extend_from_slice(&0_u16.to_le_bytes());
            out.extend_from_slice(&name);
            self.entries.push(Entry {
                name,
                size,
                crc: 0,
                offset: u32::try_from(self.at).unwrap_or(u32::MAX),
            });
            self.at = self
                .at
                .saturating_add(out.len() as u64)
                .saturating_add(u64::from(size));
            out
        }

        /// The data descriptor that ends the member begun last, whose bytes
        /// have the checksum `crc`.
        pub(super) fn end(&mut self, crc: u32) -> Vec<u8> {
            let size = self.entries.last().map_or(0, |entry| entry.size);
            if let Some(entry) = self.entries.last_mut() {
                entry.crc = crc;
            }
            let mut out = Vec::with_capacity(DESCRIPTOR);
            out.extend_from_slice(&0x0807_4b50_u32.to_le_bytes());
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes());
            self.at = self.at.saturating_add(out.len() as u64);
            out
        }

        /// The central directory and its end record.
        pub(super) fn finish(&mut self) -> Vec<u8> {
            let mut out = Vec::new();
            for entry in &self.entries {
                let name_len = u16::try_from(entry.name.len()).unwrap_or(u16::MAX);
                out.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
                out.extend_from_slice(&VERSION.to_le_bytes());
                out.extend_from_slice(&VERSION.to_le_bytes());
                out.extend_from_slice(&FLAGS.to_le_bytes());
                out.extend_from_slice(&0_u16.to_le_bytes());
                out.extend_from_slice(&0_u16.to_le_bytes());
                out.extend_from_slice(&DATE.to_le_bytes());
                out.extend_from_slice(&entry.crc.to_le_bytes());
                out.extend_from_slice(&entry.size.to_le_bytes());
                out.extend_from_slice(&entry.size.to_le_bytes());
                out.extend_from_slice(&name_len.to_le_bytes());
                out.extend_from_slice(&[0; 12]); // extra, comment, disk, attributes
                out.extend_from_slice(&entry.offset.to_le_bytes());
                out.extend_from_slice(&entry.name);
            }
            let count = u16::try_from(self.entries.len()).unwrap_or(u16::MAX);
            let length = u32::try_from(out.len()).unwrap_or(u32::MAX);
            let start = u32::try_from(self.at).unwrap_or(u32::MAX);
            out.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
            out.extend_from_slice(&[0; 4]); // this disk, the directory's disk
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&length.to_le_bytes());
            out.extend_from_slice(&start.to_le_bytes());
            out.extend_from_slice(&0_u16.to_le_bytes());
            self.at = self.at.saturating_add(out.len() as u64);
            out
        }
    }
}

#[cfg(test)]
#[path = "content_links_tests.rs"]
mod tests;
