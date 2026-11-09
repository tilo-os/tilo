//! The get tool against a host whose content store keeps what reached it.
#![allow(
    clippy::significant_drop_tightening,
    clippy::arithmetic_side_effects,
    reason = "fixtures hold the fake store's log while asserting on it, and count bytes as an agent does"
)]
use super::*;
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{
    Caller, CataloguePage, CatalogueQuery, ContentGetAnswer, ContentGets, ContentPiece, ContentPut,
    ContentPuts, InvocationReceipt, InvocationState, InvokeRequest, Pending,
};
use tilo_lock::MutexRecover as _;

#[derive(Default)]
struct Store {
    /// What the store holds, by hash.
    contents: std::collections::HashMap<String, Vec<u8>>,
    /// The host has no content store at all.
    no_store: bool,
}

#[derive(Clone, Default)]
struct Shelf(Arc<Mutex<Store>>);

impl ActionHost for Shelf {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        Pending::ready(Err(ActionHostError::UnknownAction))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        _request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        Pending::ready(Err(ActionHostError::UnknownAction))
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
    ) -> tilo_actions::remote::Pending<Result<(), ActionHostError>> {
        tilo_actions::remote::Pending::ready(Err(ActionHostError::UnknownInvocation))
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
    fn begin(&self, _caller: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionHostError> {
        Ok(Box::new(Put {
            store: Arc::clone(&self.0),
            size,
            bytes: Vec::new(),
        }))
    }
}

struct Put {
    store: Arc<Mutex<Store>>,
    size: u64,
    bytes: Vec<u8>,
}

impl ContentPut for Put {
    fn size(&self) -> u64 {
        self.size
    }

    fn written(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionHostError> {
        if offset != self.written() {
            return Err(ActionHostError::InvalidRequest {
                reason: format!("append at offset {}", self.written()),
            });
        }
        self.bytes.extend_from_slice(chunk);
        Ok(self.written())
    }

    fn finish(self: Box<Self>) -> Result<String, ActionHostError> {
        let hash = blake3::hash(&self.bytes).to_hex().to_string();
        self.store
            .lock_recover()
            .contents
            .insert(hash.clone(), self.bytes);
        Ok(format!("tilo.content:{hash}"))
    }
}

impl ContentGets for Shelf {
    fn get(
        &self,
        _caller: &Caller,
        content: &str,
        offset: u64,
        length: u64,
    ) -> Result<ContentPiece, ActionHostError> {
        let hash =
            tilo_actions::content_hash(content).ok_or_else(|| ActionHostError::InvalidRequest {
                reason: format!("{content} is not a tilo.content:<hash> reference"),
            })?;
        let store = self.0.lock_recover();
        let bytes = store
            .contents
            .get(hash)
            .ok_or_else(|| ActionHostError::InvalidRequest {
                reason: format!("this workspace's store holds no {content}"),
            })?;
        let size = bytes.len() as u64;
        if offset > size {
            return Err(ActionHostError::InvalidRequest {
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

#[test]
fn an_agent_downloads_what_it_uploaded() {
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

#[test]
fn a_large_download_pages_by_offset_and_says_where_the_next_piece_is() {
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

#[test]
fn content_the_store_does_not_hold_is_refused_and_nothing_names_anothers() {
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

#[test]
fn a_host_without_a_content_store_offers_no_get_tool() {
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
                .all(|tool| tool["name"] != GET),
            "{listed}"
        );
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

/// The answer type round-trips through the wire as the host builds it.
#[test]
fn a_piece_travels_as_hex() {
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
