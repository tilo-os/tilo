//! Immutable bounded plan files, committed by the existing private journal.
//! The semantic Plan and its canonical digest remain v1; only local persistence
//! gains a segmented envelope. No path supplied by an app selects a segment.
use super::*;
use crate::migration_review::storage::scratch::MetadataReservation;
use std::os::unix::fs::MetadataExt as _;
const SEGMENTS: usize = 2048;
const HEADER: u64 = 1024 * 1024;
pub(super) const TOTAL: u64 = MAX as u64 * SEGMENTS as u64;
const BUFFER: usize = 64 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    version: u32,
    nonce: String,
    bytes: u64,
    sha256: [u8; 32],
    segments: Vec<Segment>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Segment {
    bytes: u64,
    sha256: [u8; 32],
}
fn directory_name(nonce: &str) -> io::Result<String> {
    let uuid = uuid::Uuid::parse_str(nonce).map_err(io::Error::other)?;
    if uuid.to_string() != nonce {
        return Err(changed("Adoption segment namespace is not canonical"));
    }
    Ok(format!(".tilo-adoption-plan-{nonce}"))
}
fn name(index: usize) -> String {
    format!("segment-{index:04}")
}
pub(super) fn is_segmented(prefix: &[u8]) -> bool {
    prefix.starts_with(b"{\"version\":2,")
}
pub(super) struct Pending {
    directory: Arc<PinnedDirectory>,
    parent: Arc<PinnedDirectory>,
    nonce: String,
    segments: Vec<Segment>,
    temporary: Option<OwnedTemporary>,
    current: u64,
    flushed: u64,
    total: u64,
    buffer: Vec<u8>,
    hash: Sha256,
    segment_hash: Sha256,
    reservation: Option<MetadataReservation>,
    committed: bool,
}
impl Pending {
    pub(super) fn write(
        parent: &Arc<PinnedDirectory>,
        plan: &Plan,
    ) -> io::Result<(Self, Manifest)> {
        plan.entries
            .store()
            .scratch()
            .validate_metadata_destination(parent)?;
        let nonce = uuid::Uuid::new_v4().to_string();
        let directory = parent.create_child(OsStr::new(&directory_name(&nonce)?))?;
        let mut value = Self {
            directory,
            parent: parent.clone(),
            nonce,
            segments: Vec::new(),
            temporary: None,
            current: 0,
            flushed: 0,
            total: 0,
            buffer: Vec::with_capacity(BUFFER),
            hash: Sha256::new(),
            segment_hash: Sha256::new(),
            reservation: Some(plan.entries.store().scratch().reserve_metadata()?),
            committed: false,
        };
        serde_json::to_writer(&mut value, plan)?;
        value.write_all(b"\n")?;
        value.finish_segment()?;
        durable::sync_directory(&value.directory.file)?;
        value
            .reservation
            .as_ref()
            .ok_or_else(|| changed("Adoption quota reservation missing"))?
            .check()?;
        let manifest = Manifest {
            version: 2,
            nonce: value.nonce.clone(),
            bytes: value.total,
            sha256: value.hash.clone().finalize().into(),
            segments: value
                .segments
                .iter()
                .map(|s| Segment {
                    bytes: s.bytes,
                    sha256: s.sha256,
                })
                .collect(),
        };
        manifest.validate()?;
        Ok((value, manifest))
    }
    fn flush_buffer(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let bytes = self
            .flushed
            .checked_add(self.buffer.len() as u64)
            .ok_or_else(|| changed("Adoption segment accounting overflow"))?;
        self.reservation
            .as_mut()
            .ok_or_else(|| changed("Adoption quota reservation missing"))?
            .grow(bytes)?;
        self.temporary
            .as_mut()
            .ok_or_else(|| changed("Adoption segment writer missing"))?
            .file
            .write_all(&self.buffer)?;
        self.flushed = bytes;
        self.buffer.clear();
        self.reservation
            .as_ref()
            .ok_or_else(|| changed("Adoption quota reservation missing"))?
            .check()
    }
    fn finish_segment(&mut self) -> io::Result<()> {
        if self.current == 0 {
            return Ok(());
        }
        self.flush_buffer()?;
        let temporary = self
            .temporary
            .take()
            .ok_or_else(|| changed("Adoption segment missing"))?;
        temporary.validate()?;
        if temporary.file.metadata()?.len() != self.current {
            return Err(changed("Adoption segment length changed"));
        }
        durable::sync_file(&temporary.file)?;
        publish_temporary(
            &self.directory,
            &temporary,
            OsStr::new(&name(self.segments.len())),
        )?;
        self.segments.push(Segment {
            bytes: self.current,
            sha256: self.segment_hash.clone().finalize().into(),
        });
        drop(temporary);
        self.current = 0;
        self.segment_hash = Sha256::new();
        durable::sync_directory(&self.directory.file)?;
        #[cfg(test)]
        crate::migration_journal::crash_injection::boundary("adoption_segment_published");
        Ok(())
    }
    pub(super) fn commit(mut self) -> io::Result<MetadataReservation> {
        self.committed = true;
        self.reservation
            .take()
            .ok_or_else(|| changed("Committed adoption quota reservation missing"))
    }
}
impl Pending {
    fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        if self.current == MAX as u64 {
            self.finish_segment()?;
        }
        if self.temporary.is_none() {
            if self.segments.len() == SEGMENTS {
                return Err(changed("Adoption plan segment count exceeds bounds"));
            }
            self.temporary = Some(OwnedTemporary::create(
                &self.directory,
                OsStr::new(&name(self.segments.len())),
            )?);
        }
        let count = bytes
            .len()
            .min((MAX as u64 - self.current) as usize)
            .min(BUFFER - self.buffer.len());
        self.total = self
            .total
            .checked_add(count as u64)
            .filter(|n| *n <= TOTAL)
            .ok_or_else(|| changed("Adoption segmented plan exceeds bounds"))?;
        self.current += count as u64;
        self.buffer.extend_from_slice(&bytes[..count]);
        self.hash.update(&bytes[..count]);
        self.segment_hash.update(&bytes[..count]);
        if self.buffer.len() == BUFFER {
            self.flush_buffer()?;
        }
        Ok(count)
    }
}
fn terminal(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::Interrupted {
        io::Error::other(error)
    } else {
        error
    }
}
impl Write for Pending {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.write_bytes(bytes).map_err(terminal)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.flush_buffer().map_err(terminal)
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        // Before root-journal publication no move is admitted. Keep ambiguous
        // custody rather than unlinking a replacement or unrelated namespace.
        self.temporary.take();
        if self.directory.validate_destination().is_err() {
            return;
        }
        for (index, segment) in self.segments.iter().enumerate() {
            let result = (|| -> io::Result<()> {
                let file = open_segment(&self.directory, index, segment)?;
                verify_segment(&file, segment)?;
                let named = cap_primitives::fs::open(
                    &self.directory.file,
                    Path::new(&name(index)),
                    OpenOptions::new()
                        .read(true)
                        .follow(FollowSymlinks::No)
                        .nonblock(true),
                )?;
                if durable::identity(&named)? != durable::identity(&file)? {
                    return Err(changed("Uncommitted adoption segment was replaced"));
                }
                cap_primitives::fs::remove_file(&self.directory.file, Path::new(&name(index)))
            })();
            if result.is_err() {
                return;
            }
        }
        if self.directory.validate_destination().is_ok() {
            if let Ok(basename) = directory_name(&self.nonce) {
                let _ = cap_primitives::fs::remove_dir(&self.parent.file, Path::new(&basename));
                let _ = durable::sync_directory(&self.parent.file);
            }
        }
    }
}
impl Manifest {
    fn validate(&self) -> io::Result<()> {
        directory_name(&self.nonce)?;
        if self.version != 2
            || self.segments.is_empty()
            || self.segments.len() > SEGMENTS
            || self.bytes > TOTAL
        {
            return Err(changed("Adoption segmented manifest exceeds bounds"));
        }
        let mut total = 0u64;
        for (index, segment) in self.segments.iter().enumerate() {
            if segment.bytes == 0
                || segment.bytes > MAX as u64
                || index + 1 < self.segments.len() && segment.bytes != MAX as u64
            {
                return Err(changed("Adoption plan segment size differs"));
            }
            total = total
                .checked_add(segment.bytes)
                .ok_or_else(|| changed("Adoption segment sum overflow"))?;
        }
        if total != self.bytes {
            return Err(changed("Adoption segment total differs"));
        }
        Ok(())
    }
    pub(super) fn read<R: BufRead>(reader: &mut R) -> io::Result<(Self, u64)> {
        let mut bytes = Vec::new();
        reader.take(HEADER + 1).read_until(b'\n', &mut bytes)?;
        if bytes.len() as u64 > HEADER || bytes.last() != Some(&b'\n') {
            return Err(changed("Adoption segmented commit is incomplete"));
        }
        let manifest: Self = serde_json::from_slice(&bytes)?;
        manifest.validate()?;
        Ok((manifest, bytes.len() as u64))
    }
    pub(super) fn reader(
        self,
        parent: &Arc<PinnedDirectory>,
        scratch: &Arc<crate::migration_review::InventoryScratch>,
        reservation: Option<MetadataReservation>,
    ) -> io::Result<Reader> {
        self.validate()?;
        scratch.validate_metadata_destination(parent)?;
        let existing = reservation.is_none();
        let reservation = match reservation {
            Some(value) => value,
            None => scratch.reserve_metadata()?,
        };
        let basename = directory_name(&self.nonce)?;
        let metadata = metadata_nofollow(&parent.file, Path::new(&basename))?;
        if !metadata.is_dir() || metadata.is_symlink() {
            return Err(changed("Adoption segment directory changed"));
        }
        let directory = parent.open_child(OsStr::new(&basename), &metadata)?;
        destination_security::check_private_file(&directory.file)?;
        Ok(Reader {
            manifest: self,
            directory,
            index: 0,
            file: None,
            offset: 0,
            hash: Sha256::new(),
            segment_hash: Sha256::new(),
            complete: false,
            reservation,
            existing,
        })
    }
}
fn open_segment(
    directory: &Arc<PinnedDirectory>,
    index: usize,
    expected: &Segment,
) -> io::Result<File> {
    directory.validate_destination()?;
    let file = cap_primitives::fs::open(
        &directory.file,
        Path::new(&name(index)),
        OpenOptions::new()
            .read(true)
            .follow(FollowSymlinks::No)
            .nonblock(true),
    )?;
    destination_security::check_private_file(&file)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() != expected.bytes {
        return Err(changed("Adoption segment file changed"));
    }
    Ok(file)
}
fn verify_segment(file: &File, expected: &Segment) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    let mut bytes = vec![0; BUFFER];
    let mut offset = 0u64;
    let mut hash = Sha256::new();
    while offset < expected.bytes {
        let count = bytes.len().min((expected.bytes - offset) as usize);
        file.read_exact_at(&mut bytes[..count], offset)?;
        hash.update(&bytes[..count]);
        offset += count as u64;
    }
    if <[u8; 32]>::from(hash.finalize()) != expected.sha256 {
        return Err(changed("Adoption segment digest changed"));
    }
    Ok(())
}
pub(super) struct Reader {
    manifest: Manifest,
    directory: Arc<PinnedDirectory>,
    index: usize,
    file: Option<File>,
    offset: u64,
    hash: Sha256,
    segment_hash: Sha256,
    complete: bool,
    reservation: MetadataReservation,
    existing: bool,
}
impl Reader {
    pub(super) fn into_reservation(self) -> io::Result<MetadataReservation> {
        self.verify_complete()?;
        Ok(self.reservation)
    }
    pub(super) fn verify_complete(&self) -> io::Result<()> {
        if !self.complete {
            return Err(changed("Adoption segmented plan was not fully verified"));
        }
        self.directory.validate_destination()?;
        self.reservation.check()
    }
}
impl Reader {
    fn read_bytes(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() || self.complete {
            return Ok(0);
        }
        if self.index == self.manifest.segments.len() {
            if <[u8; 32]>::from(self.hash.clone().finalize()) != self.manifest.sha256 {
                return Err(changed("Adoption whole-plan digest changed"));
            }
            self.complete = true;
            return Ok(0);
        }
        let segment = &self.manifest.segments[self.index];
        if self.file.is_none() {
            self.file = Some(open_segment(&self.directory, self.index, segment)?);
        }
        let count = bytes.len().min((segment.bytes - self.offset) as usize);
        self.file
            .as_mut()
            .ok_or_else(|| changed("Adoption segment absent"))?
            .read_exact(&mut bytes[..count])?;
        self.hash.update(&bytes[..count]);
        self.segment_hash.update(&bytes[..count]);
        self.offset += count as u64;
        if self.offset == segment.bytes {
            if <[u8; 32]>::from(self.segment_hash.clone().finalize()) != segment.sha256 {
                return Err(changed("Adoption segment digest changed"));
            }
            if self.existing {
                self.reservation.retain_file(
                    self.file
                        .as_ref()
                        .ok_or_else(|| changed("Adoption segment absent"))?,
                )?;
            }
            self.file.take();
            self.index += 1;
            self.offset = 0;
            self.segment_hash = Sha256::new();
            self.directory.validate_destination()?;
        }
        Ok(count)
    }
}

impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        // An intentional custody/cancellation interruption must be terminal;
        // Read::read_exact may retry OS Interrupted forever otherwise.
        self.read_bytes(bytes).map_err(terminal)
    }
}
