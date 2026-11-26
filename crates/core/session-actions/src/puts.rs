//! Uploads into another device's workspace content store, through this
//! device's session: each [`ContentPut`] step is a
//! [`ContentPutStep`] the device answers, its bytes split into pieces a
//! frame carries.
//!
//! A piece's bytes are split by [`MAX_CONTENT_FRAME_BYTES`], and the pieces of
//! an upload travel to the device without waiting, [`FLIGHT_BYTES`] of them at
//! once: the device applies them in the order they arrive, so what bounds an
//! upload is the link's bandwidth or the device's disk, not one round trip a
//! frame.

use std::collections::VecDeque;

use tilo_actions::remote::{
    ActionCallError, Caller, ContentPut, ContentPutAnswer, ContentPutStep, ContentPuts,
    MAX_CONTENT_FRAME_BYTES, MAX_HAVE_HASHES, MAX_PUT_MANY_BYTES, MAX_PUT_MANY_FILES, Pending,
};

use super::{Answered, SessionActions, in_time};

/// How many bytes of an upload travel to the device before the oldest piece's
/// answer is waited for. A frame carries [`MAX_CONTENT_FRAME_BYTES`], which a
/// link with real latency would otherwise cost a round trip each.
const FLIGHT_BYTES: usize = 8 * 1024 * 1024;

/// The device's content puts, asked through the session.
pub(super) struct SessionPuts(pub(super) SessionActions);

/// What the device answered where it should have answered something else.
fn unexpected(answer: &ContentPutAnswer) -> ActionCallError {
    ActionCallError::Unavailable {
        reason: format!("the device answered an upload step with {answer:?}"),
    }
}

impl ContentPuts for SessionPuts {
    fn put_many(&self, caller: &Caller, files: &[Vec<u8>]) -> Result<Vec<String>, ActionCallError> {
        let mut contents = Vec::with_capacity(files.len());
        let mut group: Vec<String> = Vec::new();
        let mut bytes_in_group = 0_usize;
        let mut files_in_group = 0_usize;
        for bytes in files {
            // One file too large for a step goes the three steps it takes.
            if bytes.len() > MAX_PUT_MANY_BYTES {
                if !group.is_empty() {
                    send_group(self, &mut group, &mut contents)?;
                    bytes_in_group = 0;
                    files_in_group = 0;
                }
                let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                let mut put = self.begin(caller, size)?;
                put.append(0, bytes)?;
                contents.push(put.finish()?);
                continue;
            }
            // A step carries at most so many bytes and so many files: the
            // device refuses a larger one.
            let full = bytes_in_group.saturating_add(bytes.len()) > MAX_PUT_MANY_BYTES
                || files_in_group >= MAX_PUT_MANY_FILES;
            if full && !group.is_empty() {
                send_group(self, &mut group, &mut contents)?;
                bytes_in_group = 0;
                files_in_group = 0;
            }
            bytes_in_group = bytes_in_group.saturating_add(bytes.len());
            files_in_group = files_in_group.saturating_add(1);
            group.push(hex::encode(bytes));
        }
        if !group.is_empty() {
            send_group(self, &mut group, &mut contents)?;
        }
        Ok(contents)
    }

    fn have(&self, _caller: &Caller, hashes: &[String]) -> Result<Vec<bool>, ActionCallError> {
        let mut present = Vec::with_capacity(hashes.len());
        for chunk in hashes.chunks(MAX_HAVE_HASHES) {
            match self.0.put_step_waited(ContentPutStep::Have {
                hashes: chunk.to_vec(),
            })? {
                ContentPutAnswer::Have { present: answered } if answered.len() == chunk.len() => {
                    present.extend(answered);
                }
                other => return Err(unexpected(&other)),
            }
        }
        Ok(present)
    }

    fn begin(&self, _caller: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionCallError> {
        match self
            .0
            .put_step_waited(ContentPutStep::Begin { size, target: None })?
        {
            ContentPutAnswer::Begun { upload } => Ok(Box::new(SessionPut {
                actions: self.0.clone(),
                upload,
                size,
                sent: 0,
                flight: VecDeque::new(),
                flight_bytes: 0,
            })),
            other => Err(unexpected(&other)),
        }
    }
}

/// Send `group` as one many-files step and keep its answers.
fn send_group(
    puts: &SessionPuts,
    group: &mut Vec<String>,
    contents: &mut Vec<String>,
) -> Result<(), ActionCallError> {
    let files = std::mem::take(group);
    match puts.0.put_step_waited(ContentPutStep::Many {
        files,
        target: None,
    }) {
        Ok(ContentPutAnswer::Many { contents: answered }) => {
            contents.extend(answered);
            Ok(())
        }
        Ok(other) => Err(unexpected(&other)),
        Err(error) => Err(error),
    }
}

/// One upload the device keeps open for this session.
struct SessionPut {
    actions: SessionActions,
    upload: u64,
    size: u64,
    /// Bytes handed to the session, answered or not.
    sent: u64,
    /// The pieces on their way to the device, oldest first: where each ends, how
    /// many bytes it carries, and its answer.
    flight: VecDeque<(u64, usize, Pending<Answered<ContentPutAnswer>>)>,
    /// The bytes of the pieces on their way.
    flight_bytes: usize,
}

impl SessionPut {
    /// Take the answers of the pieces on their way, oldest first, until their
    /// bytes fit within [`FLIGHT_BYTES`] again; or, when `all` is said, until
    /// every one of them is answered.
    fn settle(&mut self, all: bool) -> Result<(), ActionCallError> {
        while self
            .flight
            .front()
            .is_some_and(|_| all || self.flight_bytes > FLIGHT_BYTES)
        {
            let Some((end, len, answer)) = self.flight.pop_front() else {
                break;
            };
            self.flight_bytes = self.flight_bytes.saturating_sub(len);
            match in_time(answer).unwrap_or(Err(ActionCallError::Unanswered))? {
                ContentPutAnswer::Appended { written } => {
                    // Pieces travel in the order they are sent, so the device
                    // holds every byte of this one when it answers.
                    if written < end {
                        return Err(ActionCallError::Unavailable {
                            reason: format!(
                                "the workspace holds {written} of the {end} bytes sent"
                            ),
                        });
                    }
                }
                other => return Err(unexpected(&other)),
            }
        }
        Ok(())
    }
}

impl ContentPut for SessionPut {
    fn size(&self) -> u64 {
        self.size
    }

    fn written(&self) -> u64 {
        // What was sent: the pieces on their way hold the bytes just as the ones
        // the device answered do, and only the upload's finish needs to know
        // that every one of them was taken.
        self.sent
    }

    fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionCallError> {
        if offset != self.sent {
            return Err(ActionCallError::InvalidRequest {
                reason: format!(
                    "the upload has {} bytes: append at offset {}",
                    self.sent, self.sent
                ),
            });
        }
        for piece in chunk.chunks(MAX_CONTENT_FRAME_BYTES) {
            let end = self
                .sent
                .saturating_add(u64::try_from(piece.len()).unwrap_or(u64::MAX));
            let step = ContentPutStep::append(self.upload, self.sent, piece);
            self.flight
                .push_back((end, piece.len(), self.actions.put_step(step)));
            self.flight_bytes = self.flight_bytes.saturating_add(piece.len());
            self.sent = end;
            // The oldest pieces are waited for only once the window is full, so
            // a link carries all of them at once.
            self.settle(false)?;
        }
        Ok(self.sent)
    }

    fn finish(mut self: Box<Self>) -> Result<String, ActionCallError> {
        self.settle(true)?;
        match self.actions.put_step_waited(ContentPutStep::Finish {
            upload: self.upload,
        })? {
            ContentPutAnswer::Finished { content } => Ok(content),
            other => Err(unexpected(&other)),
        }
    }
}
