//! A file put into a workspace through this device's session, and landed in the app that opens
//! it: the same steps the workspace's API server and an agent's MCP session take, so a file a
//! person drops on a page arrives exactly as one a script or an agent hands over.
//!
//! The bytes go up in pieces a frame carries ([`MAX_CONTENT_FRAME_BYTES`]), each read from its
//! [`Source`] only when it is sent, so a file of hundreds of megabytes never sits whole in the
//! sender's memory. A few pieces travel at once ([`IN_FLIGHT`]), since each answer waits on a
//! round trip. A piece the host did not take where it should have (a lost frame, an answer that
//! came too late) stops the ones behind it; the upload asks the host where it stands and goes on
//! from there, a few times at most ([`RESUMES`]).
//!
//! What the file is, read from its first bytes and its name ([`tilo_content_types::landing`]),
//! decides the app it lands in; the workspace's own `open` of that app takes it
//! ([`landing_request`]). A file no app opens stays content of the workspace.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;

use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionHost, ActionHostError, Caller,
    ContentPutAnswer, ContentPutStep, InvokeArgument, InvokeRequest, MAX_CONTENT_FRAME_BYTES,
    MAX_REMOTE_WAIT, Pending, ReceiptStatus, RemoteActionTarget, settle_with, workspace_provider,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};

use super::SessionActions;

/// How much of a file's start is read to tell what it is.
pub const HEAD_BYTES: usize = tilo_content_types::HEAD_BYTES;

/// How many pieces travel to the host before the first of them is answered.
pub const IN_FLIGHT: usize = 4;

/// How often an upload resumes after a piece went astray before it gives up.
pub const RESUMES: usize = 3;

/// How long a landing may run on the host before the upload stops waiting for it: a large
/// document takes its app a while to read.
const LANDING_WAITS: u32 = 30;

/// Whoever answers an upload's steps: the workspace's host, over this device's session.
pub trait PutSteps {
    /// Ask for `step`: the answer, once it comes.
    fn put_step(&self, step: ContentPutStep) -> Pending<Result<ContentPutAnswer, ActionHostError>>;
}

impl PutSteps for SessionActions {
    fn put_step(&self, step: ContentPutStep) -> Pending<Result<ContentPutAnswer, ActionHostError>> {
        Self::put_step(self, step)
    }
}

/// Where an upload's bytes come from, read a range at a time.
pub trait Source {
    /// How many bytes there are.
    fn size(&self) -> u64;
    /// The bytes from `from` up to `to`.
    fn read(
        &self,
        from: u64,
        to: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + '_>>;
}

/// Bytes already in memory, as a source.
impl Source for [u8] {
    fn size(&self) -> u64 {
        self.len() as u64
    }

    fn read(
        &self,
        from: u64,
        to: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + '_>> {
        let range = usize::try_from(from).ok().zip(usize::try_from(to).ok());
        let piece = range
            .and_then(|(from, to)| self.get(from..to))
            .map(<[u8]>::to_vec)
            .ok_or_else(|| format!("there are no bytes {from}..{to}"));
        Box::pin(std::future::ready(piece))
    }
}

/// Why an upload stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadError {
    /// The person stopped it.
    Stopped,
    /// Its bytes could not be read where they are.
    Read(String),
    /// The workspace refused it, or never answered.
    Refused(ActionHostError),
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stopped => f.write_str("stopped"),
            Self::Read(why) => write!(f, "it could not be read: {why}"),
            Self::Refused(error) => error.fmt(f),
        }
    }
}

/// What an answer that never came reads as.
fn answered<T>(answer: Option<Result<T, ActionHostError>>) -> Result<T, ActionHostError> {
    answer.unwrap_or_else(|| {
        Err(ActionHostError::Unavailable {
            reason: "the workspace's answer was lost".into(),
        })
    })
}

/// What the host answered where it should have answered something else.
fn unexpected(answer: &ContentPutAnswer) -> UploadError {
    UploadError::Refused(ActionHostError::Unavailable {
        reason: format!("the workspace answered an upload step with {answer:?}"),
    })
}

/// Put `source` into the workspace's content store through `steps`, telling `told` how many
/// bytes the workspace holds as that grows, and stopping between pieces once `stopped` says so.
/// Answers the content's reference, `tilo.content:<hash>`.
///
/// # Errors
///
/// The person stopped it, its bytes could not be read, or the workspace refused it or stopped
/// answering, past [`RESUMES`] resumes.
#[expect(
    clippy::future_not_send,
    reason = "a source reads where its bytes are kept: a page's file, on the page's one thread"
)]
pub async fn put(
    steps: &impl PutSteps,
    source: &(impl Source + ?Sized),
    told: &mut dyn FnMut(u64),
    stopped: &dyn Fn() -> bool,
) -> Result<String, UploadError> {
    let size = source.size();
    let upload = match answered(steps.put_step(ContentPutStep::Begin { size }).await)
        .map_err(UploadError::Refused)?
    {
        ContentPutAnswer::Begun { upload } => upload,
        other => return Err(unexpected(&other)),
    };
    let piece = MAX_CONTENT_FRAME_BYTES as u64;
    let mut sent = 0_u64;
    let mut resumed = 0_usize;
    let mut flight: VecDeque<(u64, Pending<Result<ContentPutAnswer, ActionHostError>>)> =
        VecDeque::new();
    loop {
        if stopped() {
            return Err(UploadError::Stopped);
        }
        while sent < size && flight.len() < IN_FLIGHT {
            let end = sent.saturating_add(piece).min(size);
            let bytes = source.read(sent, end).await.map_err(UploadError::Read)?;
            if bytes.len() as u64 != end.saturating_sub(sent) {
                return Err(UploadError::Read(format!(
                    "{} bytes came where {} were asked for",
                    bytes.len(),
                    end.saturating_sub(sent)
                )));
            }
            flight.push_back((
                end,
                steps.put_step(ContentPutStep::append(upload, sent, &bytes)),
            ));
            sent = end;
        }
        let Some((end, answer)) = flight.pop_front() else {
            break;
        };
        let why = match answered(answer.await) {
            Ok(ContentPutAnswer::Appended { written }) if written == end => {
                resumed = 0;
                told(written);
                continue;
            }
            Ok(other) => unexpected(&other),
            Err(error) => UploadError::Refused(error),
        };
        // The pieces behind it were sent for a place the upload never reached: let them be
        // answered (refused) before asking where it stands, so none lands after the resume.
        while let Some((_, behind)) = flight.pop_front() {
            drop(behind.await);
        }
        resumed = resumed.saturating_add(1);
        if resumed > RESUMES {
            return Err(why);
        }
        sent = match answered(steps.put_step(ContentPutStep::Standing { upload }).await) {
            Ok(ContentPutAnswer::Appended { written }) => written,
            Ok(other) => return Err(unexpected(&other)),
            // The upload is gone (the store refused its bytes): what went wrong first says why.
            Err(_) => return Err(why),
        };
        told(sent);
    }
    match answered(steps.put_step(ContentPutStep::Finish { upload }).await)
        .map_err(UploadError::Refused)?
    {
        ContentPutAnswer::Finished { content } => Ok(content),
        other => Err(unexpected(&other)),
    }
}

/// The package of the app a file lands in, as the workspace's actions name it.
#[must_use]
pub fn landing_app(landing: &tilo_content_types::Landing) -> String {
    format!("so.tilo.{}", landing.app)
}

/// The request that lands `content` — a file called `name` (its path in what was uploaded, as
/// the person sees it) whose first bytes are `head` — in the app that opens it, in `workspace`:
/// the workspace's own `open` of that app, which keeps the file in a record of the app. `None`
/// when no app opens what the file is: it stays content of the workspace.
#[must_use]
pub fn landing_request(
    workspace: &str,
    head: &[u8],
    name: &str,
    content: &str,
) -> Option<InvokeRequest> {
    let file = name.rsplit('/').next().unwrap_or(name);
    let landing = tilo_content_types::landing(head, file)?;
    let mut argument = serde_json::Map::new();
    argument.insert("content".into(), content.into());
    argument.insert("name".into(), name.into());
    Some(InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: workspace_provider(workspace),
            address: ActionAddress::new(
                format!("{APPLICATION_RESOURCE_PREFIX}{}", landing_app(&landing)),
                APPLICATION_OPEN_ACTION,
            ),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: u32::try_from(MAX_REMOTE_WAIT.as_millis()).ok(),
        appearance: None,
    })
}

/// Where a landed file is kept: the app's record, as the workspace answered it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Landed {
    /// The app's package.
    pub app: String,
    /// The record it keeps the file in.
    pub record: String,
}

impl Landed {
    /// Where an answer of the workspace's `open` says the file landed: its `provider`,
    /// `record:<app>:<record>`.
    #[must_use]
    pub fn of(value: &serde_json::Value) -> Option<Self> {
        let provider = value.get("provider")?.as_str()?;
        let (app, record) = provider
            .strip_prefix(tilo_actions::remote::RECORD_PROVIDER)?
            .strip_prefix(':')?
            .split_once(':')?;
        Some(Self {
            app: app.to_owned(),
            record: record.to_owned(),
        })
    }
}

/// The request that shows `landed` on `workspace`'s shared desk: its app's `open` on the record,
/// which the workspace's host answers by adding a window for it beside the work on the desk, as
/// it does for an agent's work. Everyone in the workspace sees it arrive; nobody's view moves.
#[must_use]
pub fn show_request(workspace: &str, landed: &Landed) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: tilo_actions::remote::application_record_target(
            workspace,
            &landed.app,
            &landed.record,
            format!("{APPLICATION_RESOURCE_PREFIX}{}", landed.app),
            APPLICATION_OPEN_ACTION,
        ),
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: u32::try_from(MAX_REMOTE_WAIT.as_millis()).ok(),
        appearance: None,
    }
}

/// Run `request` on `host` as `caller` and wait until it settles: what it answered.
///
/// # Errors
///
/// The host refused it, it failed or was stopped there, or it did not settle in the minutes a
/// landing may take.
pub async fn settled<H: ActionHost + Clone + Send + Sync + 'static>(
    host: &H,
    caller: &Caller,
    request: InvokeRequest,
) -> Result<Option<serde_json::Value>, String> {
    let invocation = request.invocation;
    let receipt =
        answered(host.invoke(caller, request).await).map_err(|error| error.to_string())?;
    let mut update = match receipt.status {
        ReceiptStatus::Completed { output } => return Ok(output.value),
        ReceiptStatus::Failed { error } => return Err(error.message),
        ReceiptStatus::Rejected { error } => return Err(error.to_string()),
        ReceiptStatus::Cancelled { message } => {
            return Err(message.unwrap_or_else(|| "it was stopped".into()));
        }
        ReceiptStatus::Accepted => InvocationUpdate::Accepted { invocation },
    };
    for _ in 0..LANDING_WAITS {
        match update {
            InvocationUpdate::Succeeded { output, .. } => return Ok(output.value),
            InvocationUpdate::Failed { error, .. } => return Err(error.message),
            InvocationUpdate::Cancelled { message, .. } => {
                return Err(message.unwrap_or_else(|| "it was stopped".into()));
            }
            _ => {}
        }
        let now = web_time::Instant::now();
        let until = now.checked_add(MAX_REMOTE_WAIT).unwrap_or(now);
        update = settle_with(host, caller, invocation, None, until)
            .await
            .flatten()
            .ok_or_else(|| "the workspace lost track of it".to_owned())?
            .update;
    }
    Err("it did not finish in time".into())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;

    #[test]
    fn what_a_file_is_decides_where_it_lands() {
        let pdf =
            landing_request("ws", b"%PDF-1.7\n", "docs/2026/scan.md", "tilo.content:a").unwrap();
        assert_eq!(pdf.target.provider, workspace_provider("ws"));
        assert_eq!(pdf.target.address.resource, "app.so.tilo.pdf-viewer");
        assert_eq!(pdf.target.address.action, APPLICATION_OPEN_ACTION);
        assert_eq!(
            pdf.argument,
            InvokeArgument::Json(
                serde_json::json!({"content": "tilo.content:a", "name": "docs/2026/scan.md"})
                    .as_object()
                    .unwrap()
                    .clone()
            ),
            "the file keeps its path in what was uploaded"
        );
        let note = landing_request("ws", b"# Plan\n", "docs/plan.md", "tilo.content:b").unwrap();
        assert_eq!(note.target.address.resource, "app.so.tilo.notes");
        assert!(
            landing_request("ws", &[0, 1, 2, 3], "docs/blob.md", "tilo.content:c").is_none(),
            "bytes no app opens stay content, whatever the name"
        );
    }

    #[test]
    fn a_landing_answer_names_the_record() {
        let answer = serde_json::json!({"provider": "record:so.tilo.pdf-viewer:abc", "record": {}});
        assert_eq!(
            Landed::of(&answer),
            Some(Landed {
                app: "so.tilo.pdf-viewer".into(),
                record: "abc".into()
            })
        );
        assert_eq!(Landed::of(&serde_json::json!({})), None);
    }
}
