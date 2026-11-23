//! A large document verified away from the thread that reads it.
//!
//! Loading a document someone else saved rebuilds and hashes every change it
//! holds, since the hashes a peer names are not taken on trust: on a page,
//! whose one thread also draws and answers input, that is seconds for a
//! company's tracker. A host that can run that work elsewhere (the page's
//! Web Worker) installs a [`Verifier`]. A read made within [`deferring`] of a
//! large document not verified yet then starts its verification and is
//! answered [`Rejected::Verifying`], and the reader asks again once
//! [`verified`] moved; that read, and every later load of the same bytes on
//! this thread, takes the hashes the verification computed instead of
//! computing them again ([`crate::engine::load_validated`]), which leaves
//! about half of the work. The host may do that half a step at a time first
//! ([`Loading`]), and the read then takes a copy of what it loaded. A
//! verification that fails says nothing about the bytes: they load here as
//! before, and are refused here if they are bad.
//!
//! Outside [`deferring`], and without a verifier, a load does all of the
//! work in place as it always did, with the hashes of a finished
//! verification when there are some.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use automerge::ChangeHash;

use crate::Rejected;

/// Below this a document loads in a few milliseconds where it is read.
pub const MIN_BYTES: usize = 64 * 1024;

/// Most verifications whose outcome is kept on one thread.
const MAX_KEPT: usize = 64;

/// Verifies documents away from the thread that installed it.
pub trait Verifier {
    /// Verify `bytes`, exactly one document chunk, somewhere else, as
    /// [`crate::verified_hashes`] does, and hand `done` its answer on this
    /// thread: the hash of every change in the order the document lists
    /// them, or why there is none.
    fn verify(&self, bytes: Vec<u8>, done: Box<dyn FnOnce(Result<Vec<ChangeHash>, String>)>);
}

type Digest = (usize, [u8; 32]);

/// What a finished verification said: the hashes, or none where it failed
/// and the bytes load in place.
type Outcome = Option<Rc<[ChangeHash]>>;

thread_local! {
    static VERIFIER: RefCell<Option<Rc<dyn Verifier>>> = const { RefCell::new(None) };
    /// What finished verifications said, oldest first.
    static KEPT: RefCell<Vec<(Digest, Outcome)>> = const { RefCell::new(Vec::new()) };
    static IN_FLIGHT: RefCell<Vec<Digest>> = const { RefCell::new(Vec::new()) };
    static DEFERRING: Cell<u32> = const { Cell::new(0) };
    static VERIFIED: Cell<u64> = const { Cell::new(0) };
}

fn digest(bytes: &[u8]) -> Digest {
    (bytes.len(), *blake3::hash(bytes).as_bytes())
}

/// Verify large documents read on this thread with `verifier`.
pub fn verify_with(verifier: Rc<dyn Verifier>) {
    VERIFIER.with_borrow_mut(|installed| *installed = Some(verifier));
}

/// Run `read`, in which a large document not verified yet is not loaded in
/// place: its verification starts away from this thread and the load is
/// answered [`Rejected::Verifying`].
pub fn deferring<T>(read: impl FnOnce() -> T) -> T {
    struct Scope;
    impl Drop for Scope {
        fn drop(&mut self) {
            DEFERRING.set(DEFERRING.get().saturating_sub(1));
        }
    }
    DEFERRING.set(DEFERRING.get().saturating_add(1));
    let _scope = Scope;
    read()
}

/// Whether a verification this thread started has not finished.
#[must_use]
pub fn verifying() -> bool {
    IN_FLIGHT.with_borrow(|in_flight| !in_flight.is_empty())
}

/// How many verifications this thread started have finished: a reader
/// answered [`Rejected::Verifying`] asks again once this moved.
#[must_use]
pub fn verified() -> u64 {
    VERIFIED.get()
}

/// The hashes to load `bytes` with: those a finished verification computed,
/// or none where they load in place. [`Rejected::Verifying`] when this
/// started their verification, or one is still running, within
/// [`deferring`].
pub(crate) fn hashes_for(bytes: &[u8]) -> Result<Outcome, Rejected> {
    if bytes.len() < MIN_BYTES || !crate::engine::single_document(bytes) {
        return Ok(None);
    }
    let key = digest(bytes);
    if let Some(kept) = KEPT.with_borrow(|kept| {
        kept.iter()
            .find(|(digest, _)| *digest == key)
            .map(|(_, hashes)| hashes.clone())
    }) {
        return Ok(kept);
    }
    if DEFERRING.get() == 0 {
        return Ok(None);
    }
    let Some(verifier) = VERIFIER.with_borrow(Clone::clone) else {
        return Ok(None);
    };
    let started = IN_FLIGHT.with_borrow_mut(|in_flight| {
        if in_flight.contains(&key) {
            false
        } else {
            in_flight.push(key);
            true
        }
    });
    if started {
        verifier.verify(
            bytes.to_vec(),
            Box::new(move |answer| finish(key, answer.ok())),
        );
    }
    // A verifier may answer at once (one that cannot start): the bytes then
    // load now.
    match KEPT.with_borrow(|kept| {
        kept.iter()
            .find(|(digest, _)| *digest == key)
            .map(|(_, hashes)| hashes.clone())
    }) {
        Some(kept) => Ok(kept),
        None => Err(Rejected::Verifying),
    }
}

fn finish(key: Digest, hashes: Option<Vec<ChangeHash>>) {
    IN_FLIGHT.with_borrow_mut(|in_flight| in_flight.retain(|other| *other != key));
    KEPT.with_borrow_mut(|kept| {
        kept.retain(|(digest, _)| *digest != key);
        kept.push((key, hashes.map(Rc::from)));
        if kept.len() > MAX_KEPT {
            kept.remove(0);
        }
    });
    VERIFIED.set(VERIFIED.get().wrapping_add(1));
}

/// Forget every verification and the verifier: a test's fresh start.
#[doc(hidden)]
pub fn reset() {
    VERIFIER.with_borrow_mut(|installed| *installed = None);
    KEPT.with_borrow_mut(Vec::clear);
    IN_FLIGHT.with_borrow_mut(Vec::clear);
}

/// Bytes a verification answered for, loading a number of ops at a time on
/// this thread, between its frames: what is left of a load once the hashes
/// are known (building the document's indexes), spread so that no step holds
/// the thread long. When done, the document is kept for the read that follows
/// (`tilo_automerge::shared`), which then takes a copy of it.
#[derive(Debug)]
pub struct Loading {
    load: automerge::LoadInSlices,
    bytes: Vec<u8>,
}

impl Loading {
    /// Start loading `bytes` with `hashes`; none when they cannot load that
    /// way (not one document, or hashes that do not fit), and the read loads
    /// them as before.
    #[must_use]
    pub fn new(bytes: Vec<u8>, hashes: Vec<ChangeHash>) -> Option<Self> {
        if bytes.len() < MIN_BYTES {
            return None;
        }
        let load = automerge::Automerge::load_in_slices(&bytes, hashes).ok()??;
        Some(Self { load, bytes })
    }

    /// Load up to `ops` more ops, or the next pass that finishes the
    /// document. True once done, or when it cannot be: the read that follows
    /// then loads the bytes as before.
    pub fn step(&mut self, ops: usize) -> bool {
        self.load.step(ops)
    }

    /// Keep the loaded document for the next read of the same bytes.
    pub fn finish(self) {
        if let Ok(doc) = self.load.finish() {
            tilo_automerge::shared::hold(&self.bytes, &automerge::AutoCommit::from_document(doc));
        }
    }
}
