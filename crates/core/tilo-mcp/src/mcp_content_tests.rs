//! The put and get tools against a host whose content store records what reached it.
#![allow(
    clippy::significant_drop_tightening,
    clippy::arithmetic_side_effects,
    reason = "fixtures hold the fake store's log while asserting on it, and count bytes as an agent does"
)]
use super::super::get::{GET, MAX_GET_CALL_BYTES};
use super::*;
use tilo_actions::remote::{
    Caller, CataloguePage, CatalogueQuery, ContentGetAnswer, ContentGets, ContentPiece,
    ContentPuts, InvocationReceipt, InvocationState, InvokeRequest, Pending,
};

#[derive(Default)]
struct Log {
    /// The workspace of each upload begun.
    begun: Vec<Option<String>>,
    stored: Vec<Vec<u8>>,
    aborted: usize,
    /// The host has no content store.
    no_store: bool,
}

#[derive(Clone, Default)]
struct Shelf(Arc<Mutex<Log>>);

impl ActionProvider for Shelf {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        _request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn observe(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Option<InvocationState> {
        None
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionCallError>> {
        tilo_actions::remote::Pending::ready(Err(ActionCallError::UnknownInvocation))
    }

    fn content_puts(&self) -> Option<Arc<dyn ContentPuts>> {
        let offered: Arc<dyn ContentPuts> = Arc::new(self.clone());
        (!self.0.lock_recover().no_store).then_some(offered)
    }

    fn content_gets(&self) -> Option<Arc<dyn ContentGets>> {
        let offered: Arc<dyn ContentGets> = Arc::new(self.clone());
        (!self.0.lock_recover().no_store).then_some(offered)
    }
}

impl ContentPuts for Shelf {
    fn begin(&self, caller: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionCallError> {
        self.0
            .lock_recover()
            .begun
            .push(caller.workspace().map(str::to_owned));
        Ok(Box::new(Put {
            log: Arc::clone(&self.0),
            size,
            bytes: Vec::new(),
            done: false,
        }))
    }
}

struct Put {
    log: Arc<Mutex<Log>>,
    size: u64,
    bytes: Vec<u8>,
    done: bool,
}

impl ContentPut for Put {
    fn size(&self) -> u64 {
        self.size
    }

    fn written(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionCallError> {
        if offset != self.written() {
            return Err(ActionCallError::InvalidRequest {
                reason: format!("append at offset {}", self.written()),
            });
        }
        self.bytes.extend_from_slice(chunk);
        Ok(self.written())
    }

    fn finish(mut self: Box<Self>) -> Result<String, ActionCallError> {
        self.done = true;
        let hash = blake3::hash(&self.bytes).to_hex().to_string();
        self.log
            .lock_recover()
            .stored
            .push(std::mem::take(&mut self.bytes));
        Ok(format!("tilo.content:{hash}"))
    }
}

impl Drop for Put {
    fn drop(&mut self) {
        if !self.done {
            let mut log = self.log.lock_recover();
            log.aborted = log.aborted.saturating_add(1);
        }
    }
}

impl ContentGets for Shelf {
    fn get(
        &self,
        _caller: &Caller,
        content: &str,
        offset: u64,
        length: u64,
    ) -> Result<ContentPiece, ActionCallError> {
        let hash =
            tilo_actions::content_hash(content).ok_or_else(|| ActionCallError::InvalidRequest {
                reason: format!("{content} is not a tilo.content:<hash> reference"),
            })?;
        let log = self.0.lock_recover();
        let bytes = log
            .stored
            .iter()
            .find(|bytes| blake3::hash(bytes).to_hex().as_str() == hash)
            .ok_or_else(|| ActionCallError::InvalidRequest {
                reason: format!("this workspace's store holds no {content}"),
            })?;
        let size = bytes.len() as u64;
        if offset > size {
            return Err(ActionCallError::InvalidRequest {
                reason: format!(
                    "the content is {size} bytes: an offset of {offset} is past its end"
                ),
            });
        }
        let length = if length == 0 {
            MAX_GET_CALL_BYTES
        } else {
            length
        };
        let from = usize::try_from(offset).unwrap_or(usize::MAX);
        let to = usize::try_from(offset.saturating_add(length).min(size)).unwrap_or(usize::MAX);
        Ok(ContentPiece {
            size,
            offset,
            bytes: bytes[from..to].to_vec(),
        })
    }
}

fn run<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

/// A put tool's answer as JSON, and whether it is an error (its text then).
async fn put(server: &McpServer<Shelf>, name: &str, arguments: Value) -> Result<Value, String> {
    let reply = server
        .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}))
        .await
        .unwrap();
    let result = &reply["result"];
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text in {reply}"))
        .to_owned();
    if result["isError"] == true {
        Err(text)
    } else {
        Ok(serde_json::from_str(&text).unwrap())
    }
}

fn encoded(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// A call's answer: its text, and its typed value where it carries one.
async fn call(
    server: &McpServer<Shelf>,
    name: &str,
    arguments: Value,
    structured: bool,
) -> Result<(String, Option<Value>), String> {
    let reply = server
        .handle(json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"tools/call",
            "params":{
                "name":name,
                "arguments":arguments,
                "_meta":{"so.tilo.mcp":{"structured":structured}}
            }
        }))
        .await
        .unwrap();
    let result = &reply["result"];
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text in {reply}"))
        .to_owned();
    if result["isError"] == true {
        Err(text)
    } else {
        Ok((text, result.get("structuredContent").cloned()))
    }
}

/// The object a get's text answer leads with.
fn answer(text: &str) -> Value {
    serde_json::Deserializer::from_str(text)
        .into_iter::<Value>()
        .next()
        .and_then(Result::ok)
        .unwrap_or_else(|| panic!("no object in {text}"))
}

fn bytes_of(answer: &Value) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(
            answer["bytes"]
                .as_str()
                .unwrap_or_else(|| panic!("no bytes in {answer}")),
        )
        .unwrap()
}

/// Put `bytes` through the session's put tools; answers the reference.
async fn upload(server: &McpServer<Shelf>, bytes: &[u8]) -> String {
    use base64::Engine as _;
    let begun = answer(
        &call(
            server,
            "tilo_put_begin",
            json!({"size": bytes.len()}),
            false,
        )
        .await
        .unwrap()
        .0,
    );
    let upload = begun["upload"].clone();
    let mut offset = 0;
    for piece in bytes.chunks(256 * 1024) {
        let chunk = base64::engine::general_purpose::STANDARD.encode(piece);
        call(
            server,
            "tilo_put_append",
            json!({"upload": upload, "offset": offset, "chunk": chunk}),
            false,
        )
        .await
        .unwrap();
        offset += piece.len();
    }
    let finished = call(server, "tilo_put_finish", json!({"upload": upload}), false)
        .await
        .unwrap();
    answer(&finished.0)["content"]
        .as_str()
        .expect("a reference")
        .to_owned()
}

tilo_test_env::scenario! {
    fn an_agent_puts_content_into_its_workspace_in_bounded_chunks;

    phase an_agent_uploads_in_chunks_into_its_workspace_and_gets_a_reference;
    {
        let host = Shelf::default();
        run(async {
            let server = McpServer::for_agent(host.clone(), "workspace".into(), "claude-code", "ada");
            let listed = server
                .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
                .await
                .unwrap();
            let names: Vec<_> = listed["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .collect();
            for name in [BEGIN, APPEND, FINISH] {
                assert!(names.contains(&name), "{names:?}");
            }
            let bytes: Vec<u8> = (0..10_000_u32).map(|n| (n % 251) as u8).collect();
            let begun = put(&server, BEGIN, json!({"size": bytes.len()}))
                .await
                .unwrap();
            let upload = begun["upload"].clone();
            let mut offset = 0;
            for chunk in bytes.chunks(4096) {
                let appended = put(
                    &server,
                    APPEND,
                    json!({"upload": upload, "offset": offset, "chunk": encoded(chunk)}),
                )
                .await
                .unwrap();
                offset += chunk.len();
                assert_eq!(appended["written"], offset);
            }
            // A chunk sent again is refused with where to resume.
            let again = put(
                &server,
                APPEND,
                json!({"upload": upload, "offset": 0, "chunk": encoded(&bytes[..4096])}),
            )
            .await
            .unwrap_err();
            assert_eq!(again, "the upload has 10000 bytes: append at offset 10000");
            let finished = put(&server, FINISH, json!({"upload": upload}))
                .await
                .unwrap();
            assert_eq!(
                finished["content"],
                format!("tilo.content:{}", blake3::hash(&bytes).to_hex())
            );
            {
                let log = host.0.lock_recover();
                assert_eq!(log.begun, [Some("workspace".to_owned())]);
                assert_eq!(log.stored, [bytes]);
            }
            // Finished, it is no longer open.
            assert!(
                put(&server, FINISH, json!({"upload": upload}))
                    .await
                    .unwrap_err()
                    .contains("no upload")
            );
        });
    }

    phase a_session_holds_few_uploads_takes_bounded_chunks_and_drops_what_it_left_open;
    {
        let host = Shelf::default();
        run(async {
            let server = McpServer::for_agent(host.clone(), "workspace".into(), "claude-code", "ada");
            let mut uploads = Vec::new();
            for _ in 0..MAX_OPEN_CONTENT_PUTS {
                uploads.push(put(&server, BEGIN, json!({"size": 4})).await.unwrap()["upload"].clone());
            }
            let refused = put(&server, BEGIN, json!({"size": 4})).await.unwrap_err();
            assert!(refused.contains("finish one first"), "{refused}");

            let too_long = put(
                &server,
                APPEND,
                json!({"upload": uploads[0], "offset": 0, "chunk": encoded(&vec![0; MAX_CONTENT_PUT_CHUNK + 1])}),
            )
            .await
            .unwrap_err();
            assert_eq!(too_long, "a chunk carries at most 524288 bytes: split it");

            // Short of its size, an upload stays open to be completed.
            put(
                &server,
                APPEND,
                json!({"upload": uploads[0], "offset": 0, "chunk": encoded(b"ab")}),
            )
            .await
            .unwrap();
            let short = put(&server, FINISH, json!({"upload": uploads[0]}))
                .await
                .unwrap_err();
            assert_eq!(
                short,
                "the upload has 2 of 4 bytes: append the rest at offset 2"
            );
            put(
                &server,
                APPEND,
                json!({"upload": uploads[0], "offset": 2, "chunk": encoded(b"cd")}),
            )
            .await
            .unwrap();
            put(&server, FINISH, json!({"upload": uploads[0]}))
                .await
                .unwrap();
            assert_eq!(host.0.lock_recover().stored, [b"abcd".to_vec()]);

            drop(server);
            let log = host.0.lock_recover();
            assert_eq!(log.stored.len(), 1);
            assert_eq!(
                log.aborted,
                MAX_OPEN_CONTENT_PUTS - 1,
                "the session's open uploads end with it"
            );
            drop(log);
        });
    }

    phase chunks_sent_ahead_of_the_gap_arrive_intact_and_one_in_its_way_is_refused;
    {
        let store = Shelf::default();
        run(async {
            let server = McpServer::for_agent(store.clone(), "workspace".into(), "claude-code", "ada");
            // A few megabytes in pieces of the size one append carries, sent
            // several at a time: the ones ahead of the gap are held, and the
            // piece that fills it appends them all.
            let piece = MAX_CONTENT_PUT_CHUNK;
            let bytes: Vec<u8> = (0..piece * 4)
                .map(|n| u8::try_from(n % 251).unwrap())
                .collect();
            let upload = put(&server, BEGIN, json!({"size": bytes.len()}))
                .await
                .unwrap()["upload"]
                .clone();
            let mut answers = Vec::new();
            for index in [1, 2, 0, 3] {
                let chunk = &bytes[index * piece..(index + 1) * piece];
                let appended = put(
                    &server,
                    APPEND,
                    json!({"upload": upload, "offset": index * piece, "chunk": encoded(chunk)}),
                )
                .await
                .unwrap();
                answers.push(appended["written"].as_u64().unwrap());
            }
            assert_eq!(
                answers,
                [0, 0, (piece * 3) as u64, (piece * 4) as u64],
                "each answer is the bytes held contiguously from the start"
            );
            let finished = put(&server, FINISH, json!({"upload": upload}))
                .await
                .unwrap();
            assert_eq!(
                finished["content"],
                format!("tilo.content:{}", blake3::hash(&bytes).to_hex())
            );
            assert_eq!(store.0.lock_recover().stored, [bytes]);

            // A chunk already held, or one past the window, is refused.
            let held = put(&server, BEGIN, json!({"size": 12}))
                .await
                .unwrap()["upload"]
                .clone();
            put(
                &server,
                APPEND,
                json!({"upload": held, "offset": 4, "chunk": encoded(b"efgh")}),
            )
            .await
            .unwrap();
            let twice = put(
                &server,
                APPEND,
                json!({"upload": held, "offset": 4, "chunk": encoded(b"efgh")}),
            )
            .await
            .unwrap_err();
            assert_eq!(
                twice,
                "the upload already holds those bytes: append at offset 0"
            );
            let past = put(
                &server,
                BEGIN,
                json!({"size": MAX_CONTENT_PUT_AHEAD as u64 + 1}),
            )
            .await
            .unwrap()["upload"]
            .clone();
            let too_far = put(
                &server,
                APPEND,
                json!({"upload": past, "offset": MAX_CONTENT_PUT_AHEAD, "chunk": encoded(b"x")}),
            )
            .await
            .unwrap_err();
            assert!(
                too_far.contains(&format!(
                    "at most {MAX_CONTENT_PUT_AHEAD} bytes ahead of the 0 the upload holds"
                )),
                "{too_far}"
            );
        });
    }

    phase a_host_without_a_content_store_offers_no_put_or_get_tools;
    {
        let host = Shelf::default();
        host.0.lock_recover().no_store = true;
        run(async {
            let server = McpServer::for_agent(host.clone(), "workspace".into(), "claude-code", "ada");
            let listed = server
                .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
                .await
                .unwrap();
            assert!(
                listed["result"]["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|tool| !is_put_tool(tool["name"].as_str().unwrap()) && tool["name"] != GET),
                "{listed}"
            );
            let refused = put(&server, BEGIN, json!({"size": 4})).await.unwrap_err();
            assert_eq!(refused, "content uploads are not available here");
            let refused = call(
                &server,
                GET,
                json!({"content": format!("tilo.content:{}", "0".repeat(64))}),
                false,
            )
            .await
            .unwrap_err();
            assert_eq!(refused, "reading content is not available here");
        });
    }
}

tilo_test_env::scenario! {
    fn an_agent_gets_content_the_store_holds_in_pieces;

    phase an_agent_downloads_what_it_uploaded;
    {
        let host = Shelf::default();
        run(async {
            let server = McpServer::for_agent(host.clone(), "workspace".into(), "claude-code", "ada");
            let listed = server
                .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
                .await
                .unwrap();
            assert!(
                listed["result"]["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|tool| tool["name"] == GET),
                "{listed}"
            );
            let bytes: Vec<u8> = (0..10_000_u32).map(|n| (n % 251) as u8).collect();
            let content = upload(&server, &bytes).await;

            let (text, typed) = call(&server, GET, json!({"content": content}), true)
                .await
                .unwrap();
            let piece = answer(&text);
            assert_eq!(piece["content"], content);
            assert_eq!(piece["offset"], 0);
            assert_eq!(piece["length"], bytes.len());
            assert_eq!(piece["size"], bytes.len());
            assert_eq!(bytes_of(&piece), bytes);
            assert!(text.contains("that is all of it"), "{text}");
            assert_eq!(
                typed.as_ref().map(|typed| typed["bytes"].clone()),
                Some(piece["bytes"].clone()),
                "the object is the typed answer too"
            );

            // A session that asked for no typed answers reads the text alone.
            let (_, typed) = call(&server, GET, json!({"content": content}), false)
                .await
                .unwrap();
            assert_eq!(typed, None);
        });
    }

    phase a_large_download_pages_by_offset_and_says_where_the_next_piece_is;
    {
        let host = Shelf::default();
        run(async {
            let server = McpServer::for_agent(host.clone(), "workspace".into(), "claude-code", "ada");
            let bytes: Vec<u8> = (0..700_000_u32).map(|n| (n % 251) as u8).collect();
            let content = upload(&server, &bytes).await;

            let (text, _) = call(&server, GET, json!({"content": content}), false)
                .await
                .unwrap();
            let first = answer(&text);
            assert_eq!(
                first["length"],
                512 * 1024,
                "one call carries at most 512 KiB"
            );
            assert_eq!(first["size"], bytes.len());
            assert_eq!(bytes_of(&first), bytes[..512 * 1024]);
            assert!(
                text.contains("at offset 524288") && text.contains("call tilo_get again"),
                "{text}"
            );

            let (text, _) = call(
                &server,
                GET,
                json!({"content": content, "offset": 512 * 1024}),
                false,
            )
            .await
            .unwrap();
            let rest = answer(&text);
            assert_eq!(rest["offset"], 512 * 1024);
            assert_eq!(bytes_of(&rest), bytes[512 * 1024..]);
            assert!(text.contains("that is all of it"), "{text}");

            // A piece asked for by its length.
            let (text, _) = call(
                &server,
                GET,
                json!({"content": content, "offset": 10, "length": 20}),
                false,
            )
            .await
            .unwrap();
            let small = answer(&text);
            assert_eq!(small["length"], 20);
            assert_eq!(bytes_of(&small), bytes[10..30]);
            assert!(text.contains("at offset 30"), "{text}");
        });
    }

    phase content_the_store_does_not_hold_is_refused_and_nothing_names_anothers;
    {
        let host = Shelf::default();
        run(async {
            let server = McpServer::for_agent(host.clone(), "workspace".into(), "claude-code", "ada");
            let missing = format!("tilo.content:{}", blake3::hash(b"never put").to_hex());
            let refused = call(&server, GET, json!({"content": missing}), false)
                .await
                .unwrap_err();
            assert!(
                refused.contains("holds no") && refused.contains(&missing),
                "{refused}"
            );
            let refused = call(&server, GET, json!({"content": "not a reference"}), false)
                .await
                .unwrap_err();
            assert!(
                refused.contains("is not a tilo.content:<hash>"),
                "{refused}"
            );

            let bytes = b"a few bytes to page".to_vec();
            let content = upload(&server, &bytes).await;
            let refused = call(
                &server,
                GET,
                json!({"content": content, "offset": 100}),
                false,
            )
            .await
            .unwrap_err();
            assert!(
                refused.contains("is past its end"),
                "an offset past the end is refused: {refused}"
            );
        });
    }

    phase a_piece_travels_as_hex;
    // The answer type round-trips through the wire as the host builds it.
    {
        let answer = ContentGetAnswer::new(ContentPiece {
            size: 3,
            offset: 0,
            bytes: b"abc".to_vec(),
        })
        .unwrap();
        let encoded = serde_json::to_string(&answer).unwrap();
        assert!(encoded.contains("\"616263\""), "{encoded}");
        let read: ContentGetAnswer = serde_json::from_str(&encoded).unwrap();
        assert_eq!(read.bytes(), Some(b"abc".to_vec()));
    }
}
