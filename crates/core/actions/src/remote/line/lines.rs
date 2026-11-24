//! A whole connection of the line wire, answered as one caller: what a local socket that keeps
//! state between lines serves.

use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_lock::MutexRecover as _;

use super::{answer_message, frame_line, line_refusal};
use crate::remote::refuse;
use crate::remote::{
    ActionCallError, ActionProvider, Caller, ContentGetAnswer, HostedContentPuts,
    MAX_REMOTE_ACTION_FRAME_BYTES, RemoteActionFrame, RemoteActionMessage,
};

/// How many requests that wait one connection keeps answering at once; one past them is answered
/// in turn, so a caller that floods waits slows only itself.
const WAITING_AT_ONCE: usize = 64;

/// Answer every line `input` reads as `tree` answers the frame it holds, `first` before them,
/// writing each answer to `output` until `input` ends or `output` closes.
///
/// `tree` names, for each frame, the provider it is asked of and the caller it is asked as: a
/// connection that serves every workspace its device holds answers each frame in the workspace
/// it names ([`RemoteActionFrame::workspace`]). A frame `tree` refuses is answered with that
/// refusal, and the connection goes on.
///
/// Uploads stay open between lines in `uploads` ([`HostedContentPuts`]), each answering only the
/// caller that began it: kept beyond the connection, one a lost connection cut is taken up on the
/// next. Content is read through the provider's [`ActionProvider::content_gets`]. A request that waits
/// ([`RemoteActionMessage::waits`]) is answered on a thread of its own, so the lines after it are
/// answered meanwhile; each answer names what it answers, so the order answers go out in is
/// theirs. A line that is no frame is refused, and the connection goes on.
pub fn serve_lines<H: ActionProvider + Clone + Send + Sync + 'static>(
    tree: impl FnMut(&RemoteActionFrame) -> Result<(H, Caller), ActionCallError>,
    uploads: &HostedContentPuts,
    first: Option<Vec<u8>>,
    input: impl BufRead,
    output: impl Write + Send + 'static,
    slack: Duration,
) {
    serve_lines_with(tree, uploads, first, input, output, slack, None);
}

/// What a host's own lane did with a frame.
pub enum Take {
    /// The frame is not the lane's: the provider answers it as any other.
    NotCall,
    /// The lane took it and answers nothing.
    Silent,
    /// The lane's answer, as the line to write.
    Answer(String),
}

/// A host's own frames on a connection it serves, beside the action wire: a call lane on an
/// app's connection ([`crate::remote::call`]). The host answers them itself and may write
/// pushes of its own to the same connection; a frame it does not take goes to the provider.
pub trait CallLanes: Send + Sync {
    /// Take `message` as the lane's, or say it is the provider's.
    fn take(&self, message: &RemoteActionMessage) -> Take;
}

/// [`serve_lines`], with `lanes` answering the frames it owns before the provider sees them.
pub fn serve_lines_with<H: ActionProvider + Clone + Send + Sync + 'static>(
    mut tree: impl FnMut(&RemoteActionFrame) -> Result<(H, Caller), ActionCallError>,
    uploads: &HostedContentPuts,
    first: Option<Vec<u8>>,
    mut input: impl BufRead,
    output: impl Write + Send + 'static,
    slack: Duration,
    lanes: Option<&dyn CallLanes>,
) {
    let output = Arc::new(Mutex::new(output));
    let waiting = Arc::new(AtomicUsize::new(0));
    let mut next = first;
    loop {
        let line = match next.take() {
            Some(line) => line,
            None => match read_line(&mut input) {
                Ok(Some(line)) => line,
                Ok(None) | Err(_) => return,
            },
        };
        let frame = match RemoteActionFrame::decode(&line) {
            Ok(frame) => frame,
            Err(error) => {
                if !write_line(&output, &line_refusal("parse_error", &error.to_string())) {
                    return;
                }
                continue;
            }
        };
        if let Some(lanes) = lanes {
            match lanes.take(&frame.message) {
                Take::NotCall => {}
                Take::Silent => continue,
                Take::Answer(answer) => {
                    if !write_line(&output, &answer) {
                        return;
                    }
                    continue;
                }
            }
        }
        let (provider, caller) = match tree(&frame) {
            Ok(served) => served,
            Err(refusal) => {
                let answer = refuse(&frame.message, refusal).map_or_else(
                    || line_refusal("parse_error", "a frame sent here asks something"),
                    frame_line,
                );
                if !write_line(&output, &answer) {
                    return;
                }
                continue;
            }
        };
        let (provider, caller) = (&provider, &caller);
        let answer = match frame.message {
            RemoteActionMessage::ContentPutRequest { correlation, step } => {
                let result = provider
                    .content_puts()
                    .ok_or_else(no_content_store)
                    .and_then(|store| uploads.step(store.as_ref(), caller, step));
                frame_line(RemoteActionMessage::ContentPutResponse {
                    correlation,
                    result,
                })
            }
            RemoteActionMessage::ContentGetRequest {
                correlation,
                content,
                offset,
                length,
            } => {
                let result = provider
                    .content_gets()
                    .ok_or_else(no_content_store)
                    .and_then(|store| store.get(caller, &content, offset, length))
                    .and_then(ContentGetAnswer::new);
                frame_line(RemoteActionMessage::ContentGetResponse {
                    correlation,
                    result,
                })
            }
            message if message.waits() && waiting.load(Ordering::Relaxed) < WAITING_AT_ONCE => {
                waiting.fetch_add(1, Ordering::Relaxed);
                let (provider, caller) = (provider.clone(), caller.clone());
                let (output, waiting) = (Arc::clone(&output), Arc::clone(&waiting));
                let spawned = std::thread::Builder::new()
                    .name("tilo-line-wait".into())
                    .spawn(move || {
                        let answer = answer_message(&provider, &caller, message, slack);
                        write_line(&output, &answer);
                        waiting.fetch_sub(1, Ordering::Relaxed);
                    });
                if spawned.is_err() {
                    return;
                }
                continue;
            }
            message => answer_message(provider, caller, message, slack),
        };
        if !write_line(&output, &answer) {
            return;
        }
    }
}

/// What a connection is told whose provider keeps no content.
fn no_content_store() -> ActionCallError {
    ActionCallError::Unavailable {
        reason: "no content store is open here".into(),
    }
}

/// Write `line` and its end; whether the other side still reads.
fn write_line(output: &Mutex<impl Write>, line: &str) -> bool {
    let mut output = output.lock_recover();
    writeln!(output, "{line}")
        .and_then(|()| output.flush())
        .is_ok()
}

/// The next line of `input`, without its end; `None` once it ended. A line longer than any
/// frame is read past and answered as the empty line, which is no frame.
fn read_line(input: &mut impl BufRead) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let limit = u64::try_from(MAX_REMOTE_ACTION_FRAME_BYTES.saturating_add(1)).unwrap_or(u64::MAX);
    let read = std::io::Read::take(&mut *input, limit).read_until(b'\n', &mut line)?;
    if read == 0 {
        return Ok(None);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
        return Ok(Some(line));
    }
    if line.len() <= MAX_REMOTE_ACTION_FRAME_BYTES {
        // The input ended within its last line.
        return Ok(Some(line));
    }
    // Too long: what is left of it is read and dropped.
    let mut rest = Vec::new();
    loop {
        rest.clear();
        let read = std::io::Read::take(&mut *input, limit).read_until(b'\n', &mut rest)?;
        if read == 0 || rest.last() == Some(&b'\n') {
            return Ok(Some(Vec::new()));
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use crate::api::ActionAddress;
    use crate::outcome::{ActionOutput, InvocationId};
    use crate::remote::{
        CataloguePage, CatalogueQuery, ContentGets, ContentPiece, ContentPut, ContentPuts,
        InvocationReceipt, InvocationState, InvokeRequest, LineClient, Pending, ReceiptStatus,
        RemoteActionTarget, TestProvider,
    };
    use std::collections::HashMap;
    use std::io::BufReader;
    use std::os::unix::net::UnixStream;

    /// A provider that keeps content in memory, its actions [`TestProvider`]'s.
    #[derive(Clone)]
    struct Kept {
        actions: Arc<TestProvider>,
        store: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    }

    struct Upload {
        store: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        size: u64,
        bytes: Vec<u8>,
    }

    impl ContentPut for Upload {
        fn size(&self) -> u64 {
            self.size
        }
        fn written(&self) -> u64 {
            self.bytes.len() as u64
        }
        fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionCallError> {
            if offset != self.written() {
                return Err(ActionCallError::InvalidRequest {
                    reason: "a piece out of order".into(),
                });
            }
            self.bytes.extend_from_slice(chunk);
            Ok(self.written())
        }
        fn finish(self: Box<Self>) -> Result<String, ActionCallError> {
            if self.written() != self.size {
                return Err(ActionCallError::InvalidRequest {
                    reason: "an upload finished short".into(),
                });
            }
            let content = format!("tilo.content:{}", blake3::hash(&self.bytes).to_hex());
            self.store
                .lock_recover()
                .insert(content.clone(), self.bytes);
            Ok(content)
        }
    }

    impl ContentPuts for Kept {
        fn begin(&self, _: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionCallError> {
            Ok(Box::new(Upload {
                store: Arc::clone(&self.store),
                size,
                bytes: Vec::new(),
            }))
        }
        fn have(&self, _: &Caller, hashes: &[String]) -> Result<Vec<bool>, ActionCallError> {
            let store = self.store.lock_recover();
            Ok(hashes
                .iter()
                .map(|hash| store.contains_key(&format!("tilo.content:{hash}")))
                .collect())
        }
    }

    impl ContentGets for Kept {
        fn get(
            &self,
            _: &Caller,
            content: &str,
            offset: u64,
            length: u64,
        ) -> Result<ContentPiece, ActionCallError> {
            let bytes = self
                .store
                .lock_recover()
                .get(content)
                .cloned()
                .ok_or_else(|| ActionCallError::InvalidRequest {
                    reason: "no such content".into(),
                })?;
            let length = if length == 0 {
                300_000
            } else {
                length.min(300_000)
            };
            let start = usize::try_from(offset).unwrap().min(bytes.len());
            let end = start
                .saturating_add(usize::try_from(length).unwrap())
                .min(bytes.len());
            Ok(ContentPiece {
                size: bytes.len() as u64,
                offset,
                bytes: bytes[start..end].to_vec(),
            })
        }
    }

    impl ActionProvider for Kept {
        fn query(
            &self,
            caller: &Caller,
            query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            self.actions.query(caller, query)
        }
        fn invoke(
            &self,
            caller: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            self.actions.invoke(caller, request)
        }
        fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
            self.actions.observe(caller, id)
        }
        fn cancel(
            &self,
            caller: &Caller,
            id: &InvocationId,
        ) -> Pending<Result<(), ActionCallError>> {
            self.actions.cancel(caller, id)
        }
        fn content_puts(&self) -> Option<Arc<dyn ContentPuts>> {
            Some(Arc::new(self.clone()))
        }
        fn content_gets(&self) -> Option<Arc<dyn ContentGets>> {
            Some(Arc::new(self.clone()))
        }
    }

    /// A client of `provider`, served as `caller` on the other end of a socket.
    fn connected<H: ActionProvider + Clone + Send + Sync + 'static>(
        provider: H,
        caller: Caller,
    ) -> LineClient {
        let (near, far) = UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            let output = far.try_clone().unwrap();
            serve_lines(
                |_| Ok((provider.clone(), caller.clone())),
                &HostedContentPuts::default(),
                None,
                BufReader::new(far),
                output,
                Duration::ZERO,
            );
        });
        LineClient::of_stream(near).unwrap()
    }

    fn target() -> RemoteActionTarget {
        RemoteActionTarget {
            provider: "notes".into(),
            address: ActionAddress::new("notes", "create"),
        }
    }

    /// Asked from any thread at once, each answer reaches the ask it answers, decided as the
    /// caller the serving end names, whoever the client says it is.
    #[test]
    fn answers_reach_their_asks_as_the_serving_ends_caller() {
        let actions = Arc::new(
            TestProvider::new()
                .with_catalogue(|_, query| {
                    Ok(CataloguePage {
                        revision: "r1".into(),
                        ..CataloguePage::from_rows("r1".into(), Vec::new(), &query)
                    })
                })
                .completing(|request| ActionOutput {
                    value: Some(serde_json::json!({ "asked": request.invocation.0.to_string() })),
                    resources: Vec::new(),
                }),
        );
        let served = Caller::local_app("notes").in_workspace("w");
        let client = connected(
            Kept {
                actions: Arc::clone(&actions),
                store: Arc::default(),
            },
            served.clone(),
        );
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let client = client.clone();
                std::thread::spawn(move || {
                    let request = InvokeRequest::plain(target(), serde_json::Map::new());
                    let invocation = request.invocation;
                    let receipt = client
                        .invoke(&Caller::local_person("someone else"), request)
                        .wait_timeout(Duration::from_secs(10))
                        .ok()
                        .flatten()
                        .expect("answered")
                        .unwrap();
                    assert_eq!(receipt.invocation, invocation);
                    let ReceiptStatus::Completed { output } = receipt.status else {
                        panic!("not completed: {receipt:?}");
                    };
                    assert_eq!(
                        output.value.unwrap()["asked"],
                        invocation.0.to_string(),
                        "each receipt answers its own invocation"
                    );
                })
            })
            .collect();
        let page = client
            .query(
                &Caller::local_person("someone else"),
                CatalogueQuery::default(),
            )
            .wait_timeout(Duration::from_secs(10))
            .ok()
            .flatten()
            .expect("answered")
            .unwrap();
        assert_eq!(page.revision, "r1");
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(actions.asked().len(), 8);
        assert!(
            actions.callers().iter().all(|caller| *caller == served),
            "every call is the serving end's caller: {:?}",
            actions.callers()
        );
    }

    /// A file larger than a frame goes up in pieces on one open upload, small files go up
    /// together, and each comes back whole, read a piece at a time.
    #[test]
    fn content_goes_up_in_frames_and_comes_back_whole() {
        let kept = Kept {
            actions: Arc::new(TestProvider::new()),
            store: Arc::default(),
        };
        let client = connected(kept, Caller::local_person("ada"));
        let caller = Caller::local_person("ada");
        let large: Vec<u8> = (0..1_000_000_u32).map(|at| (at % 251) as u8).collect();
        let small = vec![b"one".to_vec(), b"two".to_vec(), large.clone()];
        let puts = client.content_puts().unwrap();
        let contents = puts.put_many(&caller, &small).unwrap();
        assert_eq!(contents.len(), 3);
        let present = puts
            .have(&caller, &[contents[0].clone(), "0".repeat(64)])
            .unwrap();
        assert_eq!(
            present,
            [true, false],
            "a put file is there, another is not"
        );
        let gets = client.content_gets().unwrap();
        let whole = gets
            .get(&caller, &contents[2], 0, large.len() as u64)
            .unwrap();
        assert_eq!(whole.size, large.len() as u64);
        assert!(whole.bytes == large, "the large file comes back whole");
        let first = gets.get(&caller, &contents[2], 10, 0).unwrap();
        assert_eq!(first.bytes.len(), 300_000, "zero asks for one piece");
        assert_eq!(first.bytes[..], large[10..300_010]);
        assert_eq!(gets.get(&caller, &contents[1], 0, 0).unwrap().bytes, b"two");
        let refused = gets.get(&caller, "tilo.content:nothing", 0, 0).unwrap_err();
        assert!(
            matches!(refused, ActionCallError::InvalidRequest { .. }),
            "{refused:?}"
        );
    }

    /// A connection the other end closes answers what waits on it, and every later ask, with
    /// why; a refusal line says it in the other end's words.
    #[test]
    fn a_closed_connection_answers_every_ask() {
        let (near, far) = UnixStream::pair().unwrap();
        let client = LineClient::of_stream(near).unwrap();
        let mut far_writer = far.try_clone().unwrap();
        let mut reader = BufReader::new(far);
        let asked = client.query(&Caller::local_person("ada"), CatalogueQuery::default());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(RemoteActionFrame::decode(line.trim_end().as_bytes()).is_ok());
        writeln!(far_writer, "{}", line_refusal("not_permitted", "not yours")).unwrap();
        let refused = asked
            .wait_timeout(Duration::from_secs(10))
            .ok()
            .flatten()
            .expect("answered")
            .unwrap_err();
        assert_eq!(
            refused,
            ActionCallError::Refused {
                reason: "not yours".into()
            }
        );
        let later = client
            .cancel(&Caller::local_person("ada"), &InvocationId::new())
            .wait_timeout(Duration::from_secs(10))
            .ok()
            .flatten()
            .expect("answered")
            .unwrap_err();
        assert_eq!(later, refused);
        // A client that goes closes its connection, so the other end hears it left.
        drop(client);
        let mut rest = String::new();
        assert_eq!(reader.read_line(&mut rest).unwrap(), 0, "{rest}");
    }
}
