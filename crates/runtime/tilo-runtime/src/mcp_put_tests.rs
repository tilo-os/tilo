//! The put tools against a host whose content store records what reached it.
#![allow(
    clippy::significant_drop_tightening,
    reason = "fixtures hold the fake store's log while asserting on it"
)]
use super::*;
use tilo_actions::remote::{
    Caller, CataloguePage, CatalogueQuery, ContentPuts, InvocationReceipt, InvocationState,
    InvokeRequest, Pending,
};

#[derive(Default)]
struct Log {
    /// The room of each upload begun.
    begun: Vec<Option<String>>,
    stored: Vec<Vec<u8>>,
    aborted: usize,
    /// The host has no content store.
    no_store: bool,
}

#[derive(Clone, Default)]
struct Shelf(Arc<Mutex<Log>>);

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
}

impl ContentPuts for Shelf {
    fn begin(&self, caller: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionHostError> {
        self.0
            .lock_recover()
            .begun
            .push(caller.room().map(str::to_owned));
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

    fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionHostError> {
        if offset != self.written() {
            return Err(ActionHostError::InvalidRequest {
                reason: format!("append at offset {}", self.written()),
            });
        }
        self.bytes.extend_from_slice(chunk);
        Ok(self.written())
    }

    fn finish(mut self: Box<Self>) -> Result<String, ActionHostError> {
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

#[test]
fn an_agent_uploads_in_chunks_into_its_room_and_gets_a_reference() {
    let host = Shelf::default();
    run(async {
        let server = McpServer::for_agent(host.clone(), "room".into(), "claude-code");
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
        assert_eq!(again, "append at offset 10000");
        let finished = put(&server, FINISH, json!({"upload": upload}))
            .await
            .unwrap();
        assert_eq!(
            finished["content"],
            format!("tilo.content:{}", blake3::hash(&bytes).to_hex())
        );
        {
            let log = host.0.lock_recover();
            assert_eq!(log.begun, [Some("room".to_owned())]);
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

#[test]
fn a_session_holds_few_uploads_takes_bounded_chunks_and_drops_what_it_left_open() {
    let host = Shelf::default();
    run(async {
        let server = McpServer::for_agent(host.clone(), "room".into(), "claude-code");
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

#[test]
fn a_host_without_a_content_store_offers_no_put_tools() {
    let host = Shelf::default();
    host.0.lock_recover().no_store = true;
    run(async {
        let server = McpServer::for_agent(host.clone(), "room".into(), "claude-code");
        let listed = server
            .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
            .await
            .unwrap();
        assert!(
            listed["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .all(|tool| !is_put_tool(tool["name"].as_str().unwrap())),
            "{listed}"
        );
        let refused = put(&server, BEGIN, json!({"size": 4})).await.unwrap_err();
        assert_eq!(refused, "content uploads are not available here");
    });
}
