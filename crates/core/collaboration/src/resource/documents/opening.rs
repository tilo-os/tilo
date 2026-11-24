//! A record read off the frame: a window that can show before its record opens with the record on
//! its way, and takes it in when it is here.
//!
//! A record is on its way while the workspace has not answered for it (its service still starting,
//! a large workspace loading) and while it is decoded: decoding a large record takes long
//! (hundreds of milliseconds at ten thousand items, seconds at a hundred thousand). A window that
//! waited for either before its first frame would show nothing all that time, or nothing at all
//! once it gave up. A window opens with its record [opening](ResourceStore::begin_opening)
//! instead: it is asked of the workspace at once, its bytes are decoded on a thread of their own
//! once they are here, and the window shows what it can meanwhile (what a tilolang app keeps in its
//! index, or the shape of the page on its way).
//! Nothing reads or writes the record meanwhile: it is not open until it is
//! [taken in](ResourceOpening::take_in), and every open of it takes it in first, waiting for its
//! decoding when it must; one while the workspace has not answered is
//! [`ResourceOpenError::Arriving`]. A write is therefore never lost or reordered: it waits for the
//! record, then lands on it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex};

use automerge::AutoCommit;
use tilo_lock::MutexRecover as _;

use super::{
    DocumentAsk, DocumentCheckout, DocumentId, DocumentRead, Parked, ResourceOpenError,
    ResourceSet, ResourceStore, WorkspaceAnswer,
};

type Answer = Result<Option<AutoCommit>, ResourceOpenError>;

type Wakes = Arc<Mutex<Vec<crate::WakeHandle>>>;

/// A record whose read is on its way.
pub(super) struct Opening {
    checkout: DocumentCheckout,
    read: Read,
    /// The windows to wake when it is here.
    wakes: Wakes,
    /// Whether they are woken once the workspace answers ([`ResourceSet::await_answer`]).
    awaits_answer: Arc<AtomicBool>,
}

/// Where a record opening off the frame is.
enum Read {
    /// The workspace has not answered for it: the port holds its answer for the next read.
    Asked,
    /// Its bytes are here, and decoded on a thread of their own.
    Decoding(mpsc::Receiver<Answer>),
}

/// What [`ResourceSet::opened`] found of a record.
pub(super) enum Opened {
    /// It is not opening off the frame.
    NotOpening,
    /// The workspace has not answered for it, or (when not waited for) it is still decoded.
    OnItsWay,
    /// What its read answered: it is not opening any more.
    Here(Answer),
}

impl ResourceSet {
    fn begin_opening(&self, want: &DocumentCheckout) -> Result<bool, ResourceOpenError> {
        if self.open.contains_key(&want.id)
            || self.pending.contains_key(&want.id)
            || self.opening.borrow().contains_key(&want.id)
            || !want.is_valid_local_storage_request()
            || self.answered.borrow().get(&want.id) == Some(&WorkspaceAnswer::HoldsNone)
        {
            return Ok(false);
        }
        let mut opening = Opening {
            checkout: want.clone(),
            read: Read::Asked,
            wakes: Wakes::default(),
            awaits_answer: Arc::default(),
        };
        match self.answer_of(&opening)? {
            Some(DocumentRead::Pending | DocumentRead::Later) => {}
            Some(DocumentRead::Whole {
                heads, bytes, hint, ..
            }) => opening.read = Read::Decoding(decode(heads, bytes, hint, &opening.wakes)?),
            // A record the workspace holds none of opens as before, and so does an answer that
            // does not read: opening it says why.
            Some(DocumentRead::Absent) => {
                self.settle(want, Ok(None))?;
                return Ok(false);
            }
            _ => return Ok(false),
        }
        self.opening.borrow_mut().insert(want.id.clone(), opening);
        Ok(true)
    }

    /// What the workspace answers now for the record `opening` asked it for, its windows woken once
    /// it answers when it has not. An answer that came between the read and the wake is read
    /// again rather than left unwoken.
    fn answer_of(&self, opening: &Opening) -> Result<Option<DocumentRead>, ResourceOpenError> {
        let mut read = self.ask(&opening.checkout)?;
        for _ in 0..2 {
            if !matches!(read, Some(DocumentRead::Pending | DocumentRead::Later))
                || self.await_answer(opening)
            {
                break;
            }
            read = self.ask(&opening.checkout)?;
        }
        Ok(read)
    }

    /// `want` as the workspace answers it now; `None` without a port to ask.
    fn ask(&self, want: &DocumentCheckout) -> Result<Option<DocumentRead>, ResourceOpenError> {
        let Some(port) = &self.port else {
            return Ok(None);
        };
        let workspace = self.cluster()?.to_string();
        let ask = DocumentAsk {
            id: want.id.route().to_owned(),
            since: None,
        };
        Ok(port
            .read(self.app_id.as_str(), &workspace, Some(vec![ask]))
            .map_err(ResourceOpenError::Unreachable)?
            .pop()
            .map(|(_, read)| read))
    }

    /// Wake `opening`'s windows once the workspace answers for it, which the port holds for the
    /// next read of it. Whether they will be: not when the port has nothing on its way, its answer
    /// having come since it was read.
    fn await_answer(&self, opening: &Opening) -> bool {
        let Some(port) = &self.port else {
            return false;
        };
        if opening.awaits_answer.swap(true, Ordering::AcqRel) {
            return true;
        }
        let (wakes, awaits, readers) = (
            Arc::clone(&opening.wakes),
            Arc::clone(&opening.awaits_answer),
            Arc::clone(&self.readers),
        );
        let woken: super::ReadyWake = Arc::new(move || {
            awaits.store(false, Ordering::Release);
            // Kept: they are woken again once it is decoded. The store's readers too: an open of
            // it while it was on its way said so, and opens it now.
            let readers = readers.lock_recover().clone();
            for wake in wakes.lock_recover().iter().chain(&readers) {
                wake.wake();
            }
        });
        let awaits = port.when_answered(woken);
        if !awaits {
            opening.awaits_answer.store(false, Ordering::Release);
        }
        awaits
    }

    /// What the read of `id` begun off the frame answered, waiting for its decoding with `wait`
    /// (never for the workspace's answer).
    pub(super) fn opened(&self, id: &DocumentId, wait: bool) -> Opened {
        let answer = {
            let mut openings = self.opening.borrow_mut();
            let Some(opening) = openings.get_mut(id) else {
                return Opened::NotOpening;
            };
            let asked = if matches!(opening.read, Read::Asked) {
                match self.answer_of(opening) {
                    Ok(Some(DocumentRead::Whole {
                        heads, bytes, hint, ..
                    })) => match decode(heads, bytes, hint, &opening.wakes) {
                        Ok(decoding) => {
                            opening.read = Read::Decoding(decoding);
                            None
                        }
                        Err(error) => Some(Err(error)),
                    },
                    Ok(Some(DocumentRead::Pending | DocumentRead::Later)) => {
                        return Opened::OnItsWay;
                    }
                    Ok(read) => Some(super::decode_read(read)),
                    Err(error) => Some(Err(error)),
                }
            } else {
                None
            };
            match (asked, &opening.read) {
                (Some(answer), _) => Some(answer),
                (None, Read::Decoding(answer)) if wait => {
                    Some(answer.recv().unwrap_or_else(|_| Err(ended())))
                }
                (None, Read::Decoding(answer)) => match answer.try_recv() {
                    Ok(answer) => Some(answer),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => Some(Err(ended())),
                },
                (None, Read::Asked) => None,
            }
        };
        let Some(answer) = answer else {
            return Opened::OnItsWay;
        };
        let Some(opening) = self.opening.borrow_mut().remove(id) else {
            return Opened::NotOpening;
        };
        // Whatever waited for it sees it now, here or refused.
        for wake in opening.wakes.lock_recover().drain(..) {
            wake.wake();
        }
        Opened::Here(self.settle(&opening.checkout, answer))
    }

    /// Take in the record `id` was opening once it is decoded (waiting for its decoding with
    /// `wait`): open from then on, as [`Self::try_open_resource`] opens it. Whether it is open;
    /// not while the workspace has not answered for it.
    pub(super) fn take_in_opening(
        &mut self,
        id: &DocumentId,
        wait: bool,
    ) -> Result<bool, ResourceOpenError> {
        let Some(checkout) = self
            .opening
            .borrow()
            .get(id)
            .map(|opening| opening.checkout.clone())
        else {
            return Ok(true);
        };
        let answer = match self.opened(id, wait) {
            Opened::NotOpening => return Ok(true),
            Opened::OnItsWay => return Ok(false),
            Opened::Here(answer) => answer,
        };
        let mut doc = answer?.ok_or(ResourceOpenError::MissingResource)?;
        let heads = doc.get_heads();
        self.open.insert(
            id.clone(),
            Parked::new(doc, checkout.kind.clone(), Some(heads)),
        );
        Ok(true)
    }
}

fn ended() -> ResourceOpenError {
    ResourceOpenError::Initialization("the record's read ended without it".into())
}

/// Decode the record whose bytes the workspace answered on a thread of their own, waking `wakes`
/// once it is: its answer comes on the receiver. A document a reader on this thread holds at
/// those heads is copied at once, with nothing to decode.
fn decode(
    heads: Vec<[u8; 32]>,
    bytes: Vec<u8>,
    hint: Option<Vec<u8>>,
    wakes: &Wakes,
) -> Result<mpsc::Receiver<Answer>, ResourceOpenError> {
    let (sent, answer) = mpsc::sync_channel(1);
    let heads: Vec<automerge::ChangeHash> = heads.into_iter().map(automerge::ChangeHash).collect();
    if let Some(doc) = tilo_automerge::shared::held_at(&heads) {
        let _ = sent.send(Ok(Some(doc)));
        return Ok(answer);
    }
    let waking = Arc::clone(wakes);
    tilo_rt::background::Background::current()
        .spawn("record-decode", move || {
            let answer = tilo_automerge::shared::load_hinted(&bytes, hint.as_deref())
                .map(Some)
                .map_err(|error| ResourceOpenError::Initialization(error.to_string()));
            let _ = sent.send(answer);
            for wake in waking.lock_recover().drain(..) {
                wake.wake();
            }
        })
        .map_err(|error| {
            ResourceOpenError::Initialization(format!("the record cannot be read: {error}"))
        })?;
    Ok(answer)
}

impl ResourceStore {
    /// Begin opening the record `want` off the frame: it is asked of the workspace now, and
    /// decoded on a thread of its own once its bytes are here. Until it is
    /// [taken in](ResourceOpening::take_in) it is not open, and the window that opened it shows
    /// what it can without it. Whether it began: not for a record open, opening or reserved
    /// already, nor one the workspace answered at once it holds none of (or that does not read),
    /// which opens as before.
    ///
    /// # Errors
    /// The workspace cannot be asked: this process has no connection to it, or the connection
    /// says it is gone.
    pub fn begin_opening(&self, want: &DocumentCheckout) -> Result<bool, ResourceOpenError> {
        self.0
            .try_borrow()
            .map_err(|_| ResourceOpenError::Busy)?
            .begin_opening(want)
    }

    /// Whether the record `id` is being opened off the frame and is not taken in yet.
    #[must_use]
    pub fn is_opening(&self, id: &DocumentId) -> bool {
        self.0
            .try_borrow()
            .is_ok_and(|set| set.opening.borrow().contains_key(id))
    }

    /// The record `want` as it opens off the frame.
    #[must_use]
    pub fn opening(&self, want: &DocumentCheckout) -> ResourceOpening {
        ResourceOpening {
            records: self.clone(),
            checkout: want.clone(),
        }
    }
}

/// A record opening off the frame ([`ResourceStore::begin_opening`]), for the window that shows
/// before it.
#[derive(Clone)]
pub struct ResourceOpening {
    records: ResourceStore,
    checkout: DocumentCheckout,
}

impl std::fmt::Debug for ResourceOpening {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceOpening")
            .field("record", &self.checkout.id)
            .finish_non_exhaustive()
    }
}

impl ResourceOpening {
    /// The record this is the opening of.
    #[must_use]
    pub fn checkout(&self) -> &DocumentCheckout {
        &self.checkout
    }

    /// Have `wake` called when the record is here, or the workspace answered for it (at once when
    /// it already did).
    pub fn wake_when_decoded(&self, wake: crate::WakeHandle) {
        let Ok(set) = self.records.0.try_borrow() else {
            return;
        };
        match set.opening.borrow().get(&self.checkout.id) {
            Some(opening) => opening.wakes.lock_recover().push(wake),
            None => wake.wake(),
        }
    }

    /// Take the record in when it is here, waiting for its decoding with `wait` (never for the
    /// workspace's answer). Whether it is open.
    ///
    /// # Errors
    /// The record cannot be read or decoded, the workspace holds none of it
    /// ([`ResourceOpenError::MissingResource`]), or it is not this workspace's.
    pub fn take_in(&self, wait: bool) -> Result<bool, ResourceOpenError> {
        self.records
            .0
            .try_borrow_mut()
            .map_err(|_| ResourceOpenError::Busy)?
            .take_in_opening(&self.checkout.id, wait)
    }

    /// The record's document as it is now, waiting for it to be taken in: what a window that
    /// showed before it reads once it needs the record (to write, or to read what it does not
    /// keep). `None` when it cannot be had, or the workspace has not answered for it yet.
    #[must_use]
    pub fn document(&self) -> Option<AutoCommit> {
        match self.take_in(true) {
            Ok(true) => {}
            Ok(false) => return None,
            Err(error) => {
                tracing::warn!(%error, "the record could not be taken in");
                return None;
            }
        }
        self.records
            .read_checked(&self.checkout.id, AutoCommit::clone)
            .ok()
    }
}
