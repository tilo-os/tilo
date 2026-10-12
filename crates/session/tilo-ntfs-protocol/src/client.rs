//! Authenticated Linux client; owns only an IPC socket, never an NTFS device FD.
use crate::{CHUNK_BYTES, Identity, Issue, Operation, Reply, Request, Response, SCHEMA, socket};
use rustix::{
    fs::{Mode, OFlags, open, openat},
    process::geteuid,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io,
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::fs::MetadataExt,
    },
    path::{Component, Path},
    time::{Duration, Instant},
};
const DIRECTORY: OFlags = OFlags::PATH
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
fn bad() -> io::Error {
    io::Error::other("invalidated administrative NTFS client")
}
fn deadline() -> io::Result<Instant> {
    Instant::now()
        .checked_add(Duration::from_secs(20))
        .ok_or_else(bad)
}
pub struct Client {
    fd: Option<OwnedFd>,
    nonce: String,
    next: u64,
    identity: Identity,
}
pub use crate::Manifest;
impl Client {
    /// Connect only through root-owned, non-writable directories; authenticate the root broker.
    /// # Errors
    /// Refuses symlinks, unsafe ownership, fake brokers, identity or protocol disagreement.
    pub fn connect(path: &Path, nonce: String, expected: Identity) -> io::Result<Self> {
        if path.as_os_str().len() > 4096
            || path.components().count() > 64
            || !path.is_absolute()
            || nonce.len() != 64
            || !nonce.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(bad());
        }
        let mut directory = File::from(open("/", DIRECTORY, Mode::empty())?);
        let mut components = path.components().peekable();
        while let Some(component) = components.next() {
            if component == Component::RootDir {
                continue;
            }
            let Component::Normal(name) = component else {
                return Err(bad());
            };
            if components.peek().is_none() {
                if name != "read.sock" {
                    return Err(bad());
                }
                break;
            }
            directory = File::from(openat(&directory, name, DIRECTORY, Mode::empty())?);
            let m = directory.metadata()?;
            if m.uid() != 0 || m.mode() & 0o022 != 0 {
                return Err(bad());
            }
        }
        let fd = socket::connect(&format!(
            "/proc/self/fd/{}/read.sock",
            directory.as_raw_fd()
        ))?;
        if socket::peer_uid(&fd)? != 0 {
            return Err(bad());
        }
        let mut client = Self {
            fd: Some(fd),
            nonce,
            next: 0,
            identity: expected,
        };
        let (request, end) = client.request(Operation::Hello)?;
        match client.receive(&request, end)? {
            Response::Hello {
                identity,
                client_uid,
                backend,
                acceptance,
            } if identity == client.identity
                && client_uid == geteuid().as_raw()
                && backend == "native-exclusive-read"
                && acceptance == "native-runtime-unverified" =>
            {
                Ok(client)
            }
            _ => Err(bad()),
        }
    }
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    pub fn nonce(&self) -> &str {
        &self.nonce
    }
    fn fd(&self) -> io::Result<&OwnedFd> {
        self.fd.as_ref().ok_or_else(bad)
    }
    fn request(&mut self, operation: Operation) -> io::Result<(Request, Instant)> {
        let request = Request {
            schema: SCHEMA,
            nonce: self.nonce.clone(),
            id: self.next,
            operation,
        };
        self.next = self.next.checked_add(1).ok_or_else(bad)?;
        let end = deadline()?;
        if let Err(e) = socket::send(self.fd()?, &request, end) {
            self.fd = None;
            return Err(e);
        }
        Ok((request, end))
    }
    fn receive(&mut self, request: &Request, end: Instant) -> io::Result<Response> {
        let result = socket::receive::<Reply>(self.fd()?, end);
        match result {
            Ok(reply)
                if reply.schema == SCHEMA
                    && reply.nonce == self.nonce
                    && reply.id == request.id =>
            {
                Ok(reply.result)
            }
            Err(error) => {
                self.fd = None;
                Err(error)
            }
            _ => {
                self.fd = None;
                Err(bad())
            }
        }
    }
    /// # Errors
    /// Permanently closes on malformed, partial or unauthenticated manifest data.
    pub fn begin(&mut self) -> io::Result<Manifest> {
        let result = self.begin_inner();
        if result.is_err() {
            self.fd = None;
        }
        result
    }
    fn begin_inner(&mut self) -> io::Result<Manifest> {
        let (r, end) = self.request(Operation::BeginObservation)?;
        let Response::Begun { generation } = self.receive(&r, end)? else {
            return Err(bad());
        };
        let mut m = Manifest {
            generation,
            entries: vec![],
            issues: vec![],
            upcase: vec![],
        };
        let mut payload = 0usize;
        for _ in 0..65_536 {
            let (r, end) = self.request(Operation::NextManifestPage)?;
            let Response::Page {
                entries,
                issues,
                upcase,
                done,
            } = self.receive(&r, end)?
            else {
                return Err(bad());
            };
            payload = payload
                .saturating_add(serde_json::to_vec(&entries)?.len())
                .saturating_add(serde_json::to_vec(&issues)?.len())
                .saturating_add(serde_json::to_vec(&upcase)?.len())
                .saturating_add(256);
            if payload > 4 * 1024 * 1024 {
                return Err(bad());
            }
            m.entries.extend(entries);
            m.issues.extend(issues);
            m.upcase.extend(upcase);
            if m.entries.len() > 4096 || m.issues.len() > 65_536 || m.upcase.len() > 65_536 {
                return Err(bad());
            }
            if done {
                if m.upcase.len() != 65_536 {
                    return Err(bad());
                }
                let bytes: Vec<u8> = m.upcase.iter().flat_map(|u| u.to_le_bytes()).collect();
                if hex::encode(Sha256::digest(bytes)) != self.identity.upcase {
                    return Err(bad());
                }
                let mut tokens = std::collections::BTreeSet::new();
                for e in &m.entries {
                    if e.path.len() > 64
                        || e.path.iter().map(Vec::len).sum::<usize>() > 4096
                        || e.path.iter().any(|c| c.is_empty() || c.len() > 255)
                        || !tokens.insert(&e.token)
                        || !e.copy_times_valid()
                    {
                        return Err(bad());
                    }
                    let key: Vec<Vec<u16>> = e
                        .path
                        .iter()
                        .map(|c| {
                            c.iter()
                                .map(|u| m.upcase.get(usize::from(*u)).copied().ok_or_else(bad))
                                .collect()
                        })
                        .collect::<io::Result<_>>()?;
                    if e.key != key {
                        return Err(bad());
                    }
                }
                return Ok(m);
            }
        }
        Err(bad())
    }
    /// # Errors
    /// Refuses partial, duplicate, gapped, wrong-session or oversized content replies.
    pub fn read_chunk(&mut self, token: &str, offset: u64, length: u32) -> io::Result<Vec<u8>> {
        let result = self.read_inner(token, offset, length);
        if result.is_err() {
            self.fd = None;
        }
        result
    }
    fn read_inner(&mut self, token: &str, offset: u64, length: u32) -> io::Result<Vec<u8>> {
        if length == 0 || length > CHUNK_BYTES {
            return Err(bad());
        }
        let (r, end) = self.request(Operation::ReadChunk {
            token: token.to_owned(),
            offset,
            length,
        })?;
        let mut bytes = Vec::with_capacity(length as usize);
        loop {
            match self.receive(&r, end)? {
                Response::Data {
                    offset,
                    bytes: part,
                } => {
                    if offset as usize != bytes.len()
                        || part.is_empty()
                        || part.len() > 4096
                        || bytes.len().saturating_add(part.len()) > length as usize
                    {
                        return Err(bad());
                    }
                    bytes.extend(part);
                }
                Response::ReadDone {
                    length: actual,
                    sha256,
                } => {
                    if actual != length
                        || bytes.len() != length as usize
                        || hex::encode(Sha256::digest(&bytes)) != sha256
                    {
                        return Err(bad());
                    }
                    return Ok(bytes);
                }
                _ => return Err(bad()),
            }
        }
    }
    /// Full postflight is mandatory before returned hashes constitute observation evidence.
    /// # Errors
    /// Refuses missing, duplicate or oversized completion data.
    pub fn finish(&mut self) -> io::Result<(BTreeMap<String, String>, Vec<Issue>)> {
        let result = self.finish_inner();
        if result.is_err() {
            self.fd = None;
        }
        result
    }
    fn finish_inner(&mut self) -> io::Result<(BTreeMap<String, String>, Vec<Issue>)> {
        let (r, end) = self.request(Operation::FinishObservation)?;
        let mut hashes = BTreeMap::new();
        let mut pending = Vec::new();
        loop {
            let Response::Finished {
                hashes: part,
                issues,
                done,
            } = self.receive(&r, end)?
            else {
                return Err(bad());
            };
            for (token, hash) in part {
                if hashes.insert(token, hash).is_some() {
                    return Err(bad());
                }
            }
            pending.extend(issues);
            if hashes.len() > 4096 || pending.len() > 4096 {
                return Err(bad());
            }
            if done {
                return Ok((hashes, pending));
            }
        }
    }
    pub fn cancel(&mut self) {
        if let Ok((r, end)) = self.request(Operation::Cancel) {
            let _ = self.receive(&r, end);
        }
        self.fd = None;
    }
}
