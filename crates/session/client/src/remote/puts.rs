//! Uploads into another device's workspace content store, through this
//! device's session: each [`ContentPut`] step is a
//! [`ContentPutStep`] the device answers, its bytes split into pieces a
//! frame carries.

use tilo_actions::remote::{
    ActionCallError, Caller, ContentPut, ContentPutAnswer, ContentPutStep, ContentPuts,
    MAX_CONTENT_FRAME_BYTES, MAX_HAVE_HASHES, MAX_PUT_MANY_BYTES, MAX_PUT_MANY_FILES,
};

use super::SessionActions;

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
            if bytes_in_group.saturating_add(bytes.len()) > MAX_PUT_MANY_BYTES && !group.is_empty()
            {
                send_group(self, &mut group, &mut contents)?;
                bytes_in_group = 0;
            }
            bytes_in_group = bytes_in_group.saturating_add(bytes.len());
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
        match self.0.put_step_waited(ContentPutStep::Begin { size })? {
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

/// Send `group` as one many-files step and keep its answers.
fn send_group(
    puts: &SessionPuts,
    group: &mut Vec<String>,
    contents: &mut Vec<String>,
) -> Result<(), ActionCallError> {
    let files = std::mem::take(group);
    match puts.0.put_step_waited(ContentPutStep::Many { files }) {
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
    written: u64,
}

impl ContentPut for SessionPut {
    fn size(&self) -> u64 {
        self.size
    }

    fn written(&self) -> u64 {
        self.written
    }

    fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionCallError> {
        if offset != self.written {
            return Err(ActionCallError::InvalidRequest {
                reason: format!(
                    "the upload has {} bytes: append at offset {}",
                    self.written, self.written
                ),
            });
        }
        for piece in chunk.chunks(MAX_CONTENT_FRAME_BYTES) {
            let step = ContentPutStep::append(self.upload, self.written, piece);
            match self.actions.put_step_waited(step)? {
                ContentPutAnswer::Appended { written } => self.written = written,
                other => return Err(unexpected(&other)),
            }
        }
        Ok(self.written)
    }

    fn finish(self: Box<Self>) -> Result<String, ActionCallError> {
        match self.actions.put_step_waited(ContentPutStep::Finish {
            upload: self.upload,
        })? {
            ContentPutAnswer::Finished { content } => Ok(content),
            other => Err(unexpected(&other)),
        }
    }
}
