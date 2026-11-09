//! Uploads into another device's workspace content store, through this
//! device's session: each [`ContentPut`] step is a
//! [`ContentPutStep`] the device answers, its bytes split into pieces a
//! frame carries.

use tilo_actions::remote::{
    ActionHostError, Caller, ContentPut, ContentPutAnswer, ContentPutStep, ContentPuts,
    MAX_CONTENT_FRAME_BYTES,
};

use super::SessionActions;

/// The device's content puts, asked through the session.
pub(super) struct SessionPuts(pub(super) SessionActions);

/// What the device answered where it should have answered something else.
fn unexpected(answer: &ContentPutAnswer) -> ActionHostError {
    ActionHostError::Unavailable {
        reason: format!("the device answered an upload step with {answer:?}"),
    }
}

impl ContentPuts for SessionPuts {
    fn begin(&self, _caller: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionHostError> {
        match self.0.put_step(ContentPutStep::Begin { size })? {
            ContentPutAnswer::Begun { upload } => Ok(Box::new(SessionPut {
                actions: self.0.clone(),
                upload,
                size,
                written: 0,
            })),
            other => Err(unexpected(&other)),
        }
    }
}

/// One upload the device keeps open for this session.
struct SessionPut {
    actions: SessionActions,
    upload: u64,
    size: u64,
    written: u64,
}

impl ContentPut for SessionPut {
    fn size(&self) -> u64 {
        self.size
    }

    fn written(&self) -> u64 {
        self.written
    }

    fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionHostError> {
        if offset != self.written {
            return Err(ActionHostError::InvalidRequest {
                reason: format!(
                    "the upload has {} bytes: append at offset {}",
                    self.written, self.written
                ),
            });
        }
        for piece in chunk.chunks(MAX_CONTENT_FRAME_BYTES) {
            let step = ContentPutStep::append(self.upload, self.written, piece);
            match self.actions.put_step(step)? {
                ContentPutAnswer::Appended { written } => self.written = written,
                other => return Err(unexpected(&other)),
            }
        }
        Ok(self.written)
    }

    fn finish(self: Box<Self>) -> Result<String, ActionHostError> {
        match self.actions.put_step(ContentPutStep::Finish {
            upload: self.upload,
        })? {
            ContentPutAnswer::Finished { content } => Ok(content),
            other => Err(unexpected(&other)),
        }
    }
}
