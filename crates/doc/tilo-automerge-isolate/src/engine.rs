//! The Automerge work itself, identical wherever it runs: in the worker
//! process on untrusted bytes, and in the caller once the worker has shown
//! the same work to finish within its limits.

#[cfg(target_os = "linux")]
use automerge::ChangeHash;
use automerge::{AutoCommit, LoadOptions, OnPartialLoad, VerificationMode};
use tilo_automerge::ApplyIncoming as _;

use crate::{Applied, Dependencies, Rejected};

/// Load a complete document, refusing input with a malformed tail rather
/// than keeping the part before it.
pub(crate) fn load(bytes: &[u8]) -> Result<AutoCommit, Rejected> {
    AutoCommit::load_with_options(bytes, strict()).map_err(|e| Rejected::Invalid(e.to_string()))
}

/// [`load`] of bytes a worker has already loaded, taking each change's hash
/// from that worker rather than rebuilding and hashing every change, which is
/// about half the cost of loading a large document. `hashes` are what
/// [`document_change_hashes`] gave for exactly these bytes in the worker;
/// none (or any Automerge finds unfit) loads as [`load`] does.
///
/// The document is the one [`load`] builds: Automerge uses the hashes only
/// in place of the ones it would compute, and only if they are one per
/// change, distinct, and have the document's stored heads as the changes
/// nothing depends on, heads the worker's [`load`] checked against the
/// changes themselves.
#[cfg(target_os = "linux")]
pub(crate) fn load_validated(bytes: &[u8], hashes: &[ChangeHash]) -> Result<AutoCommit, Rejected> {
    if hashes.is_empty() {
        return load(bytes);
    }
    AutoCommit::load_with_options(bytes, strict().known_change_hashes(hashes))
        .map_err(|e| Rejected::Invalid(e.to_string()))
}

/// The hash of every change of `doc`, which [`load`] built from `bytes`, in
/// the order [`load_validated`] takes them: only when `bytes` is a single
/// document chunk, whose changes are then exactly the document's, listed in
/// the chunk's order. Otherwise none.
#[cfg(target_os = "linux")]
pub(crate) fn document_change_hashes(bytes: &[u8], doc: &mut AutoCommit) -> Vec<ChangeHash> {
    let mut chunks = crate::precheck::chunks(bytes);
    let single_document = chunks.next().is_some_and(|chunk| {
        chunk.kind == crate::precheck::DOCUMENT && chunk.bytes.len() == bytes.len()
    }) && chunks.next().is_none();
    if !single_document {
        return Vec::new();
    }
    doc.document().change_hashes().to_vec()
}

/// Apply `incoming` to `doc` as the caller will.
///
/// `load_incremental` keeps the chunks before a malformed one, so every
/// chunk is first parsed on its own and the input refused unless all of them
/// parse. With [`Dependencies::RequireComplete`] the result is
/// [`Applied::MissingDependencies`] when any change is left waiting for a
/// dependency; `doc` has then been changed and must be discarded.
///
/// Into a blank document this is one strict load, the pass Automerge itself
/// makes there, rather than a parse followed by the same load.
pub(crate) fn apply(
    doc: &mut AutoCommit,
    incoming: &[u8],
    dependencies: Dependencies,
) -> Result<Applied, Rejected> {
    if is_blank(doc) {
        let loaded = match AutoCommit::load_with_options(
            incoming,
            strict().text_encoding(doc.document().text_encoding()),
        ) {
            Ok(loaded) => loaded,
            // Strict loading reports this only after every chunk parsed and
            // every decoded change was applied or queued. Repeating that
            // validated input with Ignore retains the queue; no malformed
            // tail can reach this branch. Both passes run inside the worker.
            Err(automerge::AutomergeError::MissingDeps) => {
                if dependencies == Dependencies::RequireComplete {
                    return Ok(Applied::MissingDependencies);
                }
                AutoCommit::load_with_options(
                    incoming,
                    strict()
                        .text_encoding(doc.document().text_encoding())
                        .on_partial_load(OnPartialLoad::Ignore),
                )
                .map_err(|e| Rejected::Invalid(e.to_string()))?
            }
            Err(error) => return Err(Rejected::Invalid(error.to_string())),
        };
        let actor = doc.get_actor().clone();
        *doc = loaded.with_actor(actor);
        if dependencies == Dependencies::RequireComplete && !doc.get_missing_deps(&[]).is_empty() {
            return Ok(Applied::MissingDependencies);
        }
        return Ok(Applied::Changed(!doc.get_heads().is_empty()));
    }
    for chunk in crate::precheck::chunks(incoming) {
        parse_alone(chunk)?;
    }
    apply_trusted(doc, incoming, dependencies)
}

/// A document with no changes, applied or queued.
pub(crate) fn is_blank(doc: &mut AutoCommit) -> bool {
    doc.get_heads().is_empty() && doc.get_missing_deps(&[]).is_empty()
}

/// Parse one chunk without applying it. A change costs only its own decode;
/// a document or bundle is loaded on its own, as it has no cheaper parse.
///
/// Parsing a change does not check its stored checksum, and into a document
/// that is not blank `load_incremental` stops at a chunk with a bad checksum
/// and keeps the chunks before it without an error. So a change's stored
/// checksum is compared with its hash here, and the whole input is refused.
/// For a compressed chunk that hash is the uncompressed change's, which
/// Automerge requires the compressed chunk's checksum to match as well.
fn parse_alone(chunk: crate::precheck::Chunk<'_>) -> Result<(), Rejected> {
    let invalid = |e: automerge::AutomergeError| Rejected::Invalid(e.to_string());
    match chunk.kind {
        crate::precheck::CHANGE | crate::precheck::COMPRESSED_CHANGE => {
            let change = automerge::Change::try_from(chunk.bytes)
                .map_err(|e| Rejected::Invalid(e.to_string()))?;
            if chunk.bytes.get(4..8) != change.hash().0.get(..4) {
                return Err(Rejected::Invalid("bad change checksum".into()));
            }
            Ok(())
        }
        // On its own, a change whose dependencies are elsewhere loads as
        // `MissingDeps`: every chunk parsed, which is all this asks.
        _ => match automerge::Automerge::load_with_options(chunk.bytes, strict()) {
            Ok(_) | Err(automerge::AutomergeError::MissingDeps) => Ok(()),
            Err(error) => Err(invalid(error)),
        },
    }
}

/// [`apply`] without the standalone parse: for bytes already shown to be
/// complete, or produced by this process.
pub(crate) fn apply_trusted(
    doc: &mut AutoCommit,
    incoming: &[u8],
    dependencies: Dependencies,
) -> Result<Applied, Rejected> {
    let before = doc.get_heads();
    doc.load_incoming(incoming)
        .map_err(|e| Rejected::Invalid(e.to_string()))?;
    if dependencies == Dependencies::RequireComplete && !doc.get_missing_deps(&[]).is_empty() {
        return Ok(Applied::MissingDependencies);
    }
    Ok(Applied::Changed(doc.get_heads() != before))
}

fn strict() -> LoadOptions<'static> {
    LoadOptions::new()
        .on_partial_load(OnPartialLoad::Error)
        .verification_mode(VerificationMode::Check)
}

/// Identifies a document's state: its heads, and the dependencies its queued
/// changes still wait for. Only complete states are cache identities: two
/// different orphan queues can share the same heads and missing dependencies.
/// Incomplete states must be rebuilt from the caller's exact saved document.
// Only the Linux worker pool compares document states.
#[cfg(target_os = "linux")]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Key(Vec<u8>);

#[cfg(target_os = "linux")]
impl Key {
    pub(crate) fn of(doc: &mut AutoCommit) -> Self {
        let mut heads = doc.get_heads();
        let mut missing = doc.get_missing_deps(&[]);
        heads.sort_unstable();
        missing.sort_unstable();
        let mut bytes = Vec::with_capacity(
            heads
                .len()
                .saturating_add(missing.len())
                .saturating_mul(32)
                .saturating_add(8),
        );
        push_hashes(&mut bytes, &heads);
        push_hashes(&mut bytes, &missing);
        Self(bytes)
    }

    /// The state of a document with no changes, applied or queued.
    pub(crate) fn is_blank(&self) -> bool {
        self.complete_heads().is_some_and(|heads| heads.is_empty())
    }

    pub(crate) fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The heads, when nothing is queued: a state the caller can extend
    /// with the changes it has made since.
    pub(crate) fn complete_heads(&self) -> Option<Vec<ChangeHash>> {
        let (heads, rest) = read_hashes(&self.0)?;
        let (missing, rest) = read_hashes(rest)?;
        (missing.is_empty() && rest.is_empty()).then_some(heads)
    }
}

#[cfg(target_os = "linux")]
fn push_hashes(out: &mut Vec<u8>, hashes: &[ChangeHash]) {
    let count = u32::try_from(hashes.len()).unwrap_or(u32::MAX);
    out.extend(count.to_le_bytes());
    for hash in hashes
        .iter()
        .take(usize::try_from(count).unwrap_or(usize::MAX))
    {
        out.extend(hash.0);
    }
}

#[cfg(target_os = "linux")]
fn read_hashes(bytes: &[u8]) -> Option<(Vec<ChangeHash>, &[u8])> {
    let (count, mut rest) = bytes.split_first_chunk::<4>()?;
    let count = usize::try_from(u32::from_le_bytes(*count)).ok()?;
    let mut hashes = Vec::with_capacity(count.min(rest.len() / 32));
    for _ in 0..count {
        let (hash, after) = rest.split_first_chunk::<32>()?;
        hashes.push(ChangeHash(*hash));
        rest = after;
    }
    Some((hashes, rest))
}
