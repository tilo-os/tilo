//! Moving a workspace's VM between machines: its disk, its snapshot and its meter as
//! one streamed transfer that keeps holes holes and can be picked up where it
//! stopped.
//!
//! The two machines speak [`Frame`]s over the connection they already keep. The machine
//! that has the VM offers what it holds — each file's length and the digest of its
//! blocks — and the machine taking it answers with what it already has, so a transfer
//! cut short continues from its last whole block, and a copy to a replica machine
//! carries only the blocks that changed since the last one. A block is 4 MiB and its
//! digest is BLAKE3 of its bytes; a block of zeros is sent as a hole, and the
//! receiving filesystem keeps the file sparse. A file is put in place only once
//! every block matched the digest offered for it and the digest of those blocks is
//! the file's. Nothing half-arrived is ever a VM.
//!
//! A replica machine never pairs an old snapshot with a newer disk: the first block of
//! a changed disk removes the snapshot's saved state, so a restore after a lost machine
//! boots the disk afresh.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::FileExt as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use wire_frame::Framing;

use super::firecracker::VmFiles;
use super::valid_name;

/// The size of one block of a file on the wire.
pub const BLOCK_BYTES: u64 = 4 << 20;

/// The largest frame: one block and the fields around it.
/// One block, and the fields of the frame it travels in.
const MAX_FRAME_BYTES: usize = 5 << 20;

const FRAMING: Framing = Framing::little_endian(MAX_FRAME_BYTES);

/// A two-way byte stream to another machine: the connection the two already have.
///
/// A transfer opens no connection of its own: whoever holds the lease passes the
/// stream it keeps to the machine the lease names, so the bytes travel where the
/// machines' own authentication already reaches.
pub trait Link: Read + Write {}

impl<T: Read + Write + ?Sized> Link for T {}

#[derive(Debug, Error)]
pub enum MoveError {
    #[error("{0}")]
    Io(#[from] io::Error),
    /// The other machine will not take it, in words.
    #[error("{0}")]
    Refused(String),
    /// The other machine answered something this transfer does not expect.
    #[error("the transfer: {0}")]
    Wire(String),
    /// What arrived does not hash to what was offered.
    #[error("{0}")]
    Corrupt(String),
    /// The request names something a transfer cannot carry.
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Vm(#[from] super::vm::VmError),
}

/// Why a VM's state is being sent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Kind {
    /// The VM moves: the machine that sent it forgets it once this one has all of it.
    Take,
    /// A copy kept for a lost machine: the machine that sent it goes on running the VM.
    Replica,
}

/// One file as a machine holds it: how long it is, the digest of its blocks, and the
/// blocks themselves when the machine that holds them says so.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileState {
    pub len: u64,
    pub digest: [u8; 32],
    /// The digests of the blocks this machine holds, in order. A transfer cut short
    /// holds a prefix of them; a replica holds them all; an offer leaves them out.
    pub blocks: Vec<[u8; 32]>,
}

/// What the sending machine has of one VM.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Offer {
    /// `<workspace>/<environment>`.
    pub vm: String,
    pub kind: Kind,
    pub files: BTreeMap<String, FileState>,
}

impl Offer {
    /// The workspace and environment the offer names.
    pub fn parts(&self) -> Result<(String, String), MoveError> {
        let (workspace, environment) = self
            .vm
            .split_once('/')
            .ok_or_else(|| MoveError::Invalid(format!("{:?} is not a workspace's VM", self.vm)))?;
        if !valid_name(workspace) || !valid_name(environment) {
            return Err(MoveError::Invalid(format!(
                "{:?} is not a workspace's VM",
                self.vm
            )));
        }
        Ok((workspace.to_owned(), environment.to_owned()))
    }
}

/// The parts of a VM's state a transfer carries, as the names they have in its
/// state directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Part {
    Disk,
    /// The guest's memory: the base a snapshot restores from.
    Memory,
    /// The vCPU and device state: present only while the snapshot is valid.
    State,
    Config,
    /// The VM's identity, size and held snapshot, so it is the same VM wherever it lives.
    Meter,
}

impl Part {
    /// Every part, the disk first: a receiver that takes a changed disk drops the
    /// snapshot it held, so a snapshot sent after it in the same transfer is the one
    /// that matches.
    pub const ALL: [Self; 5] = [
        Self::Disk,
        Self::Memory,
        Self::State,
        Self::Config,
        Self::Meter,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Disk => "disk.raw",
            Self::Memory => "vm.mem",
            Self::State => "vm.state",
            Self::Config => "firecracker.json",
            Self::Meter => "meter.json",
        }
    }

    fn of(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|part| part.name() == name)
    }

    fn path(self, files: &VmFiles) -> PathBuf {
        match self {
            Self::Disk => files.disk(),
            Self::Memory => files.snapshot().memory,
            Self::State => files.snapshot().state,
            Self::Config => files.config(),
            Self::Meter => files.meter(),
        }
    }

    /// Where it lives in the VM's state directory once it is the VM's.
    fn placed(self, state: &Path) -> PathBuf {
        match self {
            Self::Disk | Self::Memory | Self::State | Self::Config => {
                state.join("vmm").join(self.name())
            }
            Self::Meter => state.join("meter.json"),
        }
    }
}

#[derive(Serialize, Deserialize)]
enum Frame {
    Offer(Offer),
    Resume {
        files: BTreeMap<String, FileState>,
    },
    Block {
        file: String,
        index: u32,
        digest: [u8; 32],
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    Hole {
        file: String,
        index: u32,
    },
    End {
        file: String,
    },
    Kept {
        file: String,
    },
    Retry {
        file: String,
        reason: String,
    },
    Done,
    Refuse {
        reason: String,
    },
}

/// The frame as postcard carries it; [`Frame`] is written and read through it.
#[derive(Serialize, Deserialize)]
struct Wire(Frame);

impl Frame {
    fn written<W: Write + ?Sized>(self, link: &mut W) -> io::Result<()> {
        FRAMING
            .write_value(link, &Wire(self))
            .map_err(io::Error::from)
    }

    fn read<R: Read + ?Sized>(link: &mut R) -> io::Result<Option<Self>> {
        FRAMING
            .read_value::<_, Wire>(link)
            .map(|frame| frame.map(|Wire(frame)| frame))
            .map_err(io::Error::from)
    }
}

fn frame_or_end(frame: Option<Frame>) -> Result<Frame, MoveError> {
    frame.ok_or_else(|| MoveError::Io(io::Error::from(io::ErrorKind::UnexpectedEof)))
}

fn unexpected() -> MoveError {
    MoveError::Wire("the other machine answered something else".to_owned())
}

/// The digest of `bytes`.
fn digest_of(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// The digest of a block of zeros: what a hole is.
fn zero_digest() -> [u8; 32] {
    static ZERO: OnceLock<[u8; 32]> = OnceLock::new();
    *ZERO.get_or_init(|| {
        let zeros = vec![0_u8; usize::try_from(BLOCK_BYTES).unwrap_or(usize::MAX)];
        digest_of(&zeros)
    })
}

/// One file's digest from its blocks': what a file is verified against without
/// reading it again.
fn digest_of_blocks(blocks: &[[u8; 32]]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for block in blocks {
        hasher.update(block);
    }
    *hasher.finalize().as_bytes()
}

fn block_start(index: usize) -> u64 {
    u64::try_from(index)
        .unwrap_or(u64::MAX)
        .saturating_mul(BLOCK_BYTES)
}

fn block_len(index: usize, len: u64) -> u64 {
    len.saturating_sub(block_start(index)).min(BLOCK_BYTES)
}

fn block_count(len: u64) -> usize {
    usize::try_from(len.div_ceil(BLOCK_BYTES)).unwrap_or(usize::MAX)
}

/// The digests of `path`'s blocks, by what the filesystem stores: a block the file
/// does not hold is zeros, and only its data extents are read.
fn blocks_of(path: &Path) -> io::Result<Vec<[u8; 32]>> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut blocks = vec![zero_digest(); block_count(len)];
    let mut buffer = vec![0_u8; usize::try_from(BLOCK_BYTES).unwrap_or(usize::MAX)];
    let mut offset = 0_u64;
    loop {
        // The next data extent at or after `offset`; none after it ends the file's
        // data, and every block past it is a hole already.
        let data = match rustix::fs::seek(&file, rustix::fs::SeekFrom::Data(offset)) {
            Ok(data) => data,
            Err(rustix::io::Errno::NXIO) => break,
            Err(error) => return Err(error.into()),
        };
        let hole = rustix::fs::seek(&file, rustix::fs::SeekFrom::Hole(data))?;
        let first = usize::try_from(data / BLOCK_BYTES).unwrap_or(usize::MAX);
        let last = usize::try_from(hole.saturating_sub(1) / BLOCK_BYTES).unwrap_or(usize::MAX);
        for index in first..=last {
            let size = block_len(index, len);
            if size == 0 {
                break;
            }
            let size = usize::try_from(size).unwrap_or(usize::MAX);
            let wanted = buffer.get_mut(..size).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "a block is longer than one block",
                )
            })?;
            file.read_exact_at(wanted, block_start(index))?;
            if let Some(held) = blocks.get_mut(index) {
                *held = digest_of(wanted);
            }
        }
        offset = hole;
    }
    Ok(blocks)
}

/// What the sending machine holds of `files`, in the parts it sends. A part that is
/// not there yet — no disk before the first boot, no snapshot while the VM is awake —
/// is left out.
fn manifest(files: &VmFiles, parts: &[Part]) -> io::Result<BTreeMap<String, FileState>> {
    let mut held = BTreeMap::new();
    for part in parts {
        let path = part.path(files);
        if !path.is_file() {
            continue;
        }
        let len = std::fs::metadata(&path)?.len();
        let blocks = blocks_of(&path)?;
        held.insert(
            part.name().to_owned(),
            FileState {
                len,
                digest: digest_of_blocks(&blocks),
                blocks,
            },
        );
    }
    Ok(held)
}

/// Offer `files`' `parts` as workspace `vm`'s state and send what the other machine
/// does not have. `Ok(bytes)`: the block bytes written.
pub fn send<L: Link + ?Sized>(
    files: &VmFiles,
    vm: &str,
    parts: &[Part],
    kind: Kind,
    link: &mut L,
) -> Result<u64, MoveError> {
    let mut sources = manifest(files, parts)?;
    if sources.is_empty() {
        return Err(MoveError::Invalid(format!("{vm} has no state to move")));
    }
    let offered: BTreeMap<String, FileState> = sources
        .iter()
        .map(|(name, state)| {
            let state = FileState {
                len: state.len,
                digest: state.digest,
                blocks: Vec::new(),
            };
            (name.clone(), state)
        })
        .collect();
    Frame::Offer(Offer {
        vm: vm.to_owned(),
        kind,
        files: offered,
    })
    .written(link)?;
    let have = match frame_or_end(Frame::read(link)?)? {
        Frame::Resume { files } => files,
        Frame::Refuse { reason } => return Err(MoveError::Refused(reason)),
        _ => return Err(unexpected()),
    };
    let mut sent = 0_u64;
    let mut buffer = vec![0_u8; usize::try_from(BLOCK_BYTES).unwrap_or(usize::MAX)];
    let mut names: Vec<String> = sources.keys().cloned().collect();
    names.sort();
    for name in names {
        let Some((len, blocks)) = sources.remove(&name).map(|state| (state.len, state.blocks))
        else {
            continue;
        };
        let path = Part::of(&name)
            .ok_or_else(|| MoveError::Invalid(format!("{name:?} is not a VM's file")))?
            .path(files);
        let file = File::open(path)?;
        // A transfer cut short holds a prefix; a replica holds every block and is
        // compared one by one.
        let known = have.get(&name);
        let held = match kind {
            Kind::Take => known.map_or(0, |state| state.blocks.len()),
            Kind::Replica => 0,
        };
        for (index, digest) in blocks.iter().enumerate().skip(held) {
            if kind == Kind::Replica
                && known.and_then(|state| state.blocks.get(index)) == Some(digest)
            {
                continue;
            }
            let size = usize::try_from(block_len(index, len)).unwrap_or(usize::MAX);
            let wanted = buffer.get_mut(..size).ok_or_else(|| {
                MoveError::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "a block is longer than one block",
                ))
            })?;
            file.read_exact_at(wanted, block_start(index))?;
            let index = u32::try_from(index)
                .map_err(|_| MoveError::Invalid(format!("{name} has too many blocks")))?;
            if *digest == zero_digest() {
                Frame::Hole {
                    file: name.clone(),
                    index,
                }
                .written(link)?;
                continue;
            }
            // Only up to the block's last written byte: what follows it is a hole at
            // the receiver too, so a block of mostly zeros travels as what it holds.
            let keep = wanted
                .iter()
                .rposition(|byte| *byte != 0)
                .map_or(0, |last| last.saturating_add(1));
            Frame::Block {
                file: name.clone(),
                index,
                digest: *digest,
                data: wanted.get(..keep).unwrap_or_default().to_vec(),
            }
            .written(link)?;
            sent = sent.saturating_add(u64::try_from(keep).unwrap_or(u64::MAX));
        }
        Frame::End { file: name.clone() }.written(link)?;
        match frame_or_end(Frame::read(link)?)? {
            Frame::Kept { .. } => {}
            Frame::Retry { reason, .. } => return Err(MoveError::Corrupt(reason)),
            _ => return Err(unexpected()),
        }
    }
    match frame_or_end(Frame::read(link)?)? {
        Frame::Done => Ok(sent),
        _ => Err(unexpected()),
    }
}

/// Read the offer at the other end of `link`; the machine receiving a VM answers it.
pub fn hello<L: Link + ?Sized>(link: &mut L) -> Result<Offer, MoveError> {
    match frame_or_end(Frame::read(link)?)? {
        Frame::Offer(offer) => {
            if let Some(name) = offer.files.keys().find(|name| Part::of(name).is_none()) {
                return Err(decline(
                    link,
                    format!("{:?} is not a VM's file", name.clone()),
                ));
            }
            if offer.files.is_empty() {
                return Err(decline(link, format!("{} has no state to take", offer.vm)));
            }
            Ok(offer)
        }
        _ => Err(unexpected()),
    }
}

/// Tell the sending machine why nothing will be taken, and stop.
pub fn decline<L: Link + ?Sized>(link: &mut L, reason: String) -> MoveError {
    let _ = Frame::Refuse {
        reason: reason.clone(),
    }
    .written(link);
    MoveError::Refused(reason)
}

/// The staging directory a take receives into: inside the VM's own state
/// directory, so the last step is a rename and nothing sits outside it.
fn incoming(state: &Path) -> PathBuf {
    state.join("incoming")
}

/// Where a replica machine remembers the blocks it holds, so the next copy carries
/// only what changed.
fn replica_manifest(state: &Path) -> PathBuf {
    state.join("replica.json")
}

/// What the receiving machine already has of `offer`'s files.
fn held(offer: &Offer, files: &VmFiles) -> BTreeMap<String, FileState> {
    match offer.kind {
        Kind::Take => staged(offer, &files.state),
        Kind::Replica => replica(offer, &files.state),
    }
}

/// The staged files of a take that was cut short: each file's journal names the
/// blocks it has, in order.
fn staged(offer: &Offer, state: &Path) -> BTreeMap<String, FileState> {
    let mut have = BTreeMap::new();
    let dir = incoming(state);
    if !dir.is_dir() {
        return have;
    }
    for (name, offered) in &offer.files {
        let blocks = journal(&dir.join(format!("{name}.blocks")));
        let whole = blocks.len().min(block_count(offered.len));
        let blocks = blocks
            .get(..whole)
            .map(<[[u8; 32]]>::to_vec)
            .unwrap_or_default();
        if blocks.is_empty() {
            continue;
        }
        // Whole blocks only: the length of what is held, not of what was asked.
        let len = u64::try_from(blocks.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(BLOCK_BYTES)
            .min(offered.len);
        have.insert(
            name.clone(),
            FileState {
                len,
                digest: digest_of_blocks(&blocks),
                blocks,
            },
        );
    }
    have
}

/// The blocks a replica machine holds, as its last copy wrote them. A file whose
/// length is not the one recorded is not held.
fn replica(offer: &Offer, state: &Path) -> BTreeMap<String, FileState> {
    let Ok(bytes) = std::fs::read(replica_manifest(state)) else {
        return BTreeMap::new();
    };
    let Ok(mut recorded) = serde_json::from_slice::<BTreeMap<String, FileState>>(&bytes) else {
        return BTreeMap::new();
    };
    recorded.retain(|name, held| {
        offer
            .files
            .get(name)
            .is_some_and(|offered| offered.len == held.len)
            && Part::of(name).is_some_and(|part| {
                std::fs::metadata(part.placed(state)).is_ok_and(|meta| meta.len() == held.len)
            })
    });
    recorded
}

/// The blocks `path`'s journal names, in order, up to the first that does not read.
fn journal(path: &Path) -> Vec<[u8; 32]> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .map_while(|line| {
            let mut digest = [0_u8; 32];
            let bytes = line.trim().as_bytes();
            if bytes.len() != 64 {
                return None;
            }
            for (index, pair) in bytes.chunks(2).enumerate() {
                let pair = std::str::from_utf8(pair).ok()?;
                let byte = u8::from_str_radix(pair, 16).ok()?;
                *digest.get_mut(index)? = byte;
            }
            Some(digest)
        })
        .collect()
}

fn hex(digest: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(64);
    for byte in digest {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// Set down `digest` as the next block of `name` in the staging directory.
fn note_block(dir: &Path, name: &str, digest: &[u8; 32]) -> io::Result<()> {
    let mut journal = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{name}.blocks")))?;
    journal.write_all(format!("{}\n", hex(digest)).as_bytes())
}

/// The file `name` is whole in the receiver's hands: every block of it is there and
/// its digest is the offer's. A staged take is flushed; a replica's file is flushed
/// in place.
fn finish(
    offer: &Offer,
    name: &str,
    have: &BTreeMap<String, FileState>,
    files: &VmFiles,
    skip: bool,
) -> Result<(), String> {
    let offered = offer
        .files
        .get(name)
        .ok_or_else(|| format!("{name:?} was not offered"))?;
    let held = have
        .get(name)
        .ok_or_else(|| format!("nothing of {name} arrived"))?;
    let whole = block_count(offered.len);
    if held.blocks.len() != whole {
        return Err(format!(
            "{name} arrived with {} of {whole} blocks",
            held.blocks.len()
        ));
    }
    if digest_of_blocks(&held.blocks) != offered.digest {
        return Err(format!("{name} does not hash to what was offered"));
    }
    if skip {
        // What it held is the memory of a disk that is no longer on this machine.
        return Ok(());
    }
    let part = Part::of(name).ok_or_else(|| format!("{name:?} is not a VM's file"))?;
    let path = match offer.kind {
        Kind::Take => incoming(&files.state).join(name),
        Kind::Replica => part.placed(&files.state),
    };
    let handle = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .map_err(|error| format!("{name}: {error}"))?;
    handle
        .set_len(offered.len)
        .map_err(|error| format!("{name}: {error}"))?;
    handle
        .sync_all()
        .map_err(|error| format!("{name}: {error}"))?;
    Ok(())
}

/// How a transfer into this machine is going: whether the disk it takes replaces one it
/// already had, and whether that has happened, so the memory saved with the older disk
/// is not kept beside the newer one.
struct Pass {
    kind: Kind,
    replacing: bool,
    cleared: bool,
}

impl Pass {
    /// A replica pass that changes a disk this machine already had drops the memory that
    /// went with it: that snapshot can never restore the newer disk.
    fn clear(&mut self, files: &VmFiles) -> bool {
        let drop = self.kind == Kind::Replica && self.replacing && !self.cleared;
        if drop {
            let _ = std::fs::remove_file(files.snapshot().state);
            self.cleared = true;
        }
        self.cleared
    }

    /// Whether what is in `file` is left out: the memory of a disk that changed under it.
    fn dropped(&self, file: &str) -> bool {
        self.kind == Kind::Replica
            && self.cleared
            && matches!(Part::of(file), Some(Part::State | Part::Memory))
    }
}

/// Receive `offer`'s files into `files`, and take the VM over once all of them are
/// there. `Ok(bytes)`: the block bytes read. A replica is updated in place and stays
/// the sending machine's.
pub fn receive<L: Link + ?Sized>(
    offer: &Offer,
    files: &VmFiles,
    link: &mut L,
) -> Result<u64, MoveError> {
    offer.parts()?;
    let staging = incoming(&files.state);
    if offer.kind == Kind::Take {
        std::fs::create_dir_all(&staging)?;
        let marker = staging.join("vm");
        let named = std::fs::read_to_string(&marker).unwrap_or_default();
        if named.trim() != offer.vm {
            // Nothing of another VM, or of a transfer for another one, is resumed.
            clear(&staging)?;
            std::fs::create_dir_all(&staging)?;
            std::fs::write(&marker, format!("{}\n", offer.vm))?;
        }
    } else {
        std::fs::create_dir_all(files.vmm())?;
    }
    let mut have = held(offer, files);
    // A replica machine that already had this disk is taking a newer one: the memory it kept
    // cannot go with it.
    let mut pass = Pass {
        kind: offer.kind,
        replacing: offer.kind == Kind::Replica && files.disk().is_file(),
        cleared: false,
    };
    Frame::Resume {
        files: have.clone(),
    }
    .written(link)?;
    let mut received = 0_u64;
    let mut done = 0_usize;
    while let Some(frame) = Frame::read(link)? {
        match frame {
            Frame::End { file } => match finish(offer, &file, &have, files, pass.dropped(&file)) {
                Ok(()) => {
                    Frame::Kept { file: file.clone() }.written(link)?;
                    done = done.saturating_add(1);
                }
                Err(reason) => {
                    let _ = Frame::Retry {
                        file,
                        reason: reason.clone(),
                    }
                    .written(link);
                    return Err(MoveError::Corrupt(reason));
                }
            },
            frame => {
                received = received.saturating_add(block(
                    offer, files, &staging, &mut pass, &mut have, frame, link,
                )?);
            }
        }
        if done == offer.files.len() {
            commit(offer, files, &have)?;
            Frame::Done.written(link)?;
            return Ok(received);
        }
    }
    Err(MoveError::Io(io::Error::from(io::ErrorKind::UnexpectedEof)))
}

/// Take one block of a file into this machine, and say how many bytes it carried.
fn block<L: Link + ?Sized>(
    offer: &Offer,
    files: &VmFiles,
    staging: &Path,
    pass: &mut Pass,
    have: &mut BTreeMap<String, FileState>,
    frame: Frame,
    link: &mut L,
) -> Result<u64, MoveError> {
    let (file, index, data, digest) = match frame {
        Frame::Block {
            file,
            index,
            digest,
            data,
        } => (file, index, Some(data), digest),
        Frame::Hole { file, index } => (file, index, None, zero_digest()),
        _ => return Err(unexpected()),
    };
    let Some(offered) = offer.files.get(&file) else {
        return Err(decline(link, format!("{file:?} was not offered")));
    };
    let index = usize::try_from(index).unwrap_or(usize::MAX);
    if index >= block_count(offered.len) {
        return Err(decline(link, format!("{file} has no block {index}")));
    }
    let size = block_len(index, offered.len);
    if let Some(data) = &data {
        if u64::try_from(data.len()).unwrap_or(u64::MAX) > size {
            return Err(decline(
                link,
                format!("block {index} of {file} is the wrong size"),
            ));
        }
        // The digest is of the whole block: what the frame leaves out is zeros.
        if digest_of_padded(data, size) != digest {
            return Err(decline(
                link,
                format!("block {index} of {file} did not arrive whole"),
            ));
        }
    }
    if pass.kind == Kind::Take && have.get(&file).map_or(0, |state| state.blocks.len()) != index {
        return Err(decline(link, format!("{file} arrived out of order")));
    }
    let cleared = pass.clear(files);
    let received = u64::try_from(data.as_ref().map_or(0, Vec::len)).unwrap_or(u64::MAX);
    if !(pass.dropped(&file) && cleared) {
        let target = receiver_path(offer, &file, staging, &files.state)?;
        let handle = open_for_write(&target)?;
        match &data {
            Some(data) => {
                handle.write_all_at(data, block_start(index))?;
                // What the block does not hold is a hole here too.
                let written = u64::try_from(data.len()).unwrap_or(u64::MAX);
                if written < size {
                    hole(
                        &handle,
                        block_start(index).saturating_add(written),
                        size.saturating_sub(written),
                    )?;
                }
            }
            // A hole where there was data is punched away, not written over with zeros:
            // the file stays as sparse as what it holds.
            None => hole(&handle, block_start(index), size)?,
        }
    }
    if pass.kind == Kind::Take {
        note_block(staging, &file, &digest)?;
    }
    note(have, &file, index, digest, offered.len)?;
    Ok(received)
}

/// The digest of `data` and the zeros that fill its block.
fn digest_of_padded(data: &[u8], size: u64) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(data);
    let tail = usize::try_from(size.saturating_sub(u64::try_from(data.len()).unwrap_or(0)))
        .unwrap_or(usize::MAX);
    if tail > 0 {
        static ZEROS: [u8; 1 << 16] = [0_u8; 1 << 16];
        let mut left = tail;
        while left > 0 {
            let chunk = left.min(ZEROS.len());
            hasher.update(ZEROS.get(..chunk).unwrap_or_default());
            left = left.saturating_sub(chunk);
        }
    }
    *hasher.finalize().as_bytes()
}

/// Punch `len` bytes at `offset` of `file` away: a hole, where there was data.
fn hole(file: &File, offset: u64, len: u64) -> io::Result<()> {
    if len == 0 {
        return Ok(());
    }
    rustix::fs::fallocate(
        file,
        rustix::fs::FallocateFlags::PUNCH_HOLE | rustix::fs::FallocateFlags::KEEP_SIZE,
        offset,
        len,
    )?;
    Ok(())
}

/// Where the block of `file` is written: the staging directory of a take, the VM's
/// own file of a replica.
fn receiver_path(
    offer: &Offer,
    file: &str,
    staging: &Path,
    state: &Path,
) -> Result<PathBuf, MoveError> {
    match offer.kind {
        Kind::Take => Ok(staging.join(file)),
        Kind::Replica => Part::of(file)
            .map(|part| part.placed(state))
            .ok_or_else(|| MoveError::Invalid(format!("{file:?} is not a VM's file"))),
    }
}

fn open_for_write(path: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
}

/// Take `digest` as block `index` of `file`, and refuse one that leaves a gap.
fn note(
    have: &mut BTreeMap<String, FileState>,
    file: &str,
    index: usize,
    digest: [u8; 32],
    len: u64,
) -> Result<(), MoveError> {
    let mut blocks = have
        .get(file)
        .map(|state| state.blocks.clone())
        .unwrap_or_default();
    match blocks.len().cmp(&index) {
        std::cmp::Ordering::Greater => {
            if let Some(held) = blocks.get_mut(index) {
                *held = digest;
            }
        }
        std::cmp::Ordering::Equal => blocks.push(digest),
        std::cmp::Ordering::Less => {
            return Err(MoveError::Corrupt(format!(
                "{file} arrived out of order at block {index}"
            )));
        }
    }
    have.insert(
        file.to_owned(),
        FileState {
            len,
            digest: [0_u8; 32],
            blocks,
        },
    );
    Ok(())
}

/// Everything arrived: a take's files become the VM's, a replica's note of its copy.
fn commit(
    offer: &Offer,
    files: &VmFiles,
    have: &BTreeMap<String, FileState>,
) -> Result<(), MoveError> {
    let staging = incoming(&files.state);
    match offer.kind {
        Kind::Take => {
            std::fs::create_dir_all(files.vmm())?;
            // The meter last: a machine that stops here finds the disk without one and
            // no VM, never a VM without its disk.
            for part in Part::ALL
                .into_iter()
                .filter(|part| *part != Part::Meter)
                .chain([Part::Meter])
            {
                let staged = staging.join(part.name());
                if staged.is_file() {
                    std::fs::rename(&staged, part.placed(&files.state))?;
                }
            }
            let _ = std::fs::remove_file(files.state.join("moved.json"));
            let _ = std::fs::remove_dir_all(&staging);
        }
        Kind::Replica => {
            let bytes = serde_json::to_vec(have)
                .map_err(|error| MoveError::Wire(format!("the replica's note: {error}")))?;
            tilo_files::native::atomic::write_creating_parent(
                &replica_manifest(&files.state),
                &bytes,
            )?;
        }
    }
    Ok(())
}

fn clear(dir: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}
