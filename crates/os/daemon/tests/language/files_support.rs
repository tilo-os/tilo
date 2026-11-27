//! What the tests of a device's Files flows read of what its daemon keeps: the workspace's tree,
//! the bytes a node names, and the `tilo.content:` reference an upload answers. What a person or
//! an agent does goes through the paths production uses; what the tests read of the result is the
//! workspace's own tree and content.

use fs_protocol::files::FileNodeKind;
use fs_protocol::tree::{self, FILES_APP, ROOT_ID, Tree, TreeChunks};
use serde_json::Value;
use tilo_actions::remote::{ActionProvider as _, ReceiptStatus};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};

use crate::language::host::{self, Host};

/// An agent's MCP session, whatever provider it answers through: the device's own, or one the
/// workspace's person runs a turn as. The daemon already scopes each to its workspace.
pub trait Asking: Send + Sync {
    /// The MCP tool `name` with `arguments`.
    fn ask<'a>(
        &'a self,
        name: &'a str,
        arguments: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send + 'a>>;
}

impl<H: tilo_actions::remote::ActionProvider + Send + Sync + 'static> Asking for host::Agent<H> {
    fn ask<'a>(
        &'a self,
        name: &'a str,
        arguments: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send + 'a>> {
        Box::pin(async move { self.tool(name, arguments).await })
    }
}

/// An MCP session the tests act through: a key's, which names the workspace it acts in, and an
/// agent's.
#[derive(Clone, Copy)]
pub enum Session<'a> {
    Key(&'a host::Key),
    Agent(&'a dyn Asking),
}

impl Session<'_> {
    /// The MCP tool `name` with `arguments`.
    pub async fn tool(&self, workspace: &str, name: &str, arguments: &Value) -> Value {
        let mut arguments = arguments.clone();
        if matches!(self, Self::Key(_))
            && let Some(map) = arguments.as_object_mut()
        {
            map.insert("workspace".into(), workspace.into());
        }
        match self {
            Self::Key(key) => key.tool(name, &arguments).await,
            Self::Agent(agent) => agent.ask(name, &arguments).await,
        }
    }
}

/// The text of a tool's answer, and whether it failed.
pub fn said(result: &Value) -> (String, bool) {
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let failed = result["isError"]
        .as_bool()
        .unwrap_or_else(|| result.get("content").is_none());
    (text, failed)
}

/// The first JSON object in `text`.
pub fn json_in(text: &str) -> Value {
    let start = text
        .find('{')
        .unwrap_or_else(|| panic!("no object in {text}"));
    serde_json::Deserializer::from_str(&text[start..])
        .into_iter::<Value>()
        .next()
        .and_then(Result::ok)
        .unwrap_or_else(|| panic!("no object in {text}"))
}

/// The `tilo.content:<reference>` a tool's answer names, with the key a sealed reference carries.
pub fn content_in(text: &str) -> String {
    let start = text
        .find("tilo.content:")
        .unwrap_or_else(|| panic!("no content named in {text}"));
    let reference: String = text[start + "tilo.content:".len()..]
        .chars()
        .take_while(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
        .collect();
    format!("tilo.content:{}", reference.trim_end_matches('.'))
}

/// `bytes`, base64 as the put tools carry them.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        let group = (u32::from(chunk[0]) << 16) | (u32::from(second) << 8) | u32::from(third);
        out.push(char::from(ALPHABET[(group >> 18) as usize & 63]));
        out.push(char::from(ALPHABET[(group >> 12) as usize & 63]));
        out.push(if chunk.len() > 1 {
            char::from(ALPHABET[(group >> 6) as usize & 63])
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(ALPHABET[group as usize & 63])
        } else {
            '='
        });
    }
    out
}

/// The bytes of standard base64 `text`.
pub fn unbase64(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut group = 0_u32;
    let mut bits = 0_u32;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => u32::from(byte - b'A'),
            b'a'..=b'z' => u32::from(byte - b'a') + 26,
            b'0'..=b'9' => u32::from(byte - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => continue,
        };
        group = (group << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((group >> bits) & 0xff).expect("a byte"));
        }
    }
    out
}

/// Put `bytes` into the session's workspace through its uploads, in pieces as an agent sends
/// them; answers the content's reference.
pub async fn put(session: &Session<'_>, workspace: &str, bytes: &[u8]) -> String {
    let (text, failed) = said(
        &session
            .tool(
                workspace,
                "tilo_put_begin",
                &serde_json::json!({"size": bytes.len()}),
            )
            .await,
    );
    assert!(!failed, "an upload begins: {text}");
    let begun = json_in(&text);
    let upload = begun["upload"].as_u64().expect("an upload id");
    let mut offset = 0_usize;
    for chunk in bytes.chunks(2048) {
        let (text, failed) = said(
            &session
                .tool(
                    workspace,
                    "tilo_put_append",
                    &serde_json::json!({
                        "upload": upload,
                        "offset": offset,
                        "chunk": base64(chunk),
                    }),
                )
                .await,
        );
        assert!(!failed, "a piece of the upload was refused: {text}");
        offset += chunk.len();
    }
    content_in(
        &said(
            &session
                .tool(
                    workspace,
                    "tilo_put_finish",
                    &serde_json::json!({"upload": upload}),
                )
                .await,
        )
        .0,
    )
}

/// Run Files' `verb` by its path on the session, and its answer's text.
pub async fn files(session: &Session<'_>, workspace: &str, verb: &str, args: Value) -> String {
    let (text, failed) = said(
        &session
            .tool(
                workspace,
                "tilo_do",
                &serde_json::json!({"path": format!("files#{verb}"), "args": args}),
            )
            .await,
    );
    assert!(!failed, "files#{verb} failed: {text}");
    text
}

/// Run Files' `verb` by its path on the session, which must be refused: the words it is refused
/// with.
pub async fn files_refused(
    session: &Session<'_>,
    workspace: &str,
    verb: &str,
    args: Value,
) -> String {
    let (text, failed) = said(
        &session
            .tool(
                workspace,
                "tilo_do",
                &serde_json::json!({"path": format!("files#{verb}"), "args": args}),
            )
            .await,
    );
    assert!(failed, "files#{verb} was not refused: {text}");
    text
}

/// The bytes the workspace keeps at the content `reference`.
pub async fn content_bytes(session: &Session<'_>, workspace: &str, reference: &str) -> Vec<u8> {
    read_content(session, workspace, reference)
        .await
        .unwrap_or_else(|text| panic!("the content is kept: {text}"))
}

/// The bytes the workspace keeps under the content address `hash`.
pub async fn content_at_hash(
    session: &Session<'_>,
    workspace: &str,
    hash: &str,
) -> Option<Vec<u8>> {
    read_content(session, workspace, &format!("tilo.content:{hash}"))
        .await
        .ok()
}

/// Every byte at the content `reference`, read as `tilo_get` answers it: a piece a call, each
/// asked at the offset the last one ended at, until the piece reaches the content's size. A
/// refusal answers its words.
async fn read_content(
    session: &Session<'_>,
    workspace: &str,
    reference: &str,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    loop {
        let (text, failed) = said(
            &session
                .tool(
                    workspace,
                    "tilo_get",
                    &serde_json::json!({"content": reference, "offset": bytes.len()}),
                )
                .await,
        );
        if failed {
            return Err(text);
        }
        let piece = json_in(&text);
        assert_eq!(
            piece["offset"].as_u64(),
            Some(bytes.len() as u64),
            "the piece starts where it was asked: {text}",
        );
        let given = unbase64(piece["bytes"].as_str().expect("the bytes"));
        let size = piece["size"].as_u64().expect("the content's size");
        assert!(
            !given.is_empty() || bytes.len() as u64 == size,
            "a piece short of the content's end carries bytes: {text}",
        );
        bytes.extend_from_slice(&given);
        if bytes.len() as u64 >= size {
            return Ok(bytes);
        }
    }
}

/// The tree `workspace`'s host keeps, or `None` until it holds every record the tree names.
pub async fn tree_if_kept(host: &Host, workspace: &str) -> Option<Tree> {
    let daemon = host.daemon();
    let workspaces = daemon.workspaces();
    let index = workspaces
        .resource_document(workspace, FILES_APP, &tree::document_id())
        .await
        .ok()??;
    let index = automerge::AutoCommit::load(&index).ok()?;
    let mut kept = TreeChunks::default();
    kept.read_index(&index);
    for chunk in tree::listed_chunks(&index) {
        let record = workspaces
            .resource_document(workspace, FILES_APP, &tree::chunk_document_id(chunk))
            .await
            .ok()??;
        let document = automerge::AutoCommit::load(&record).ok()?;
        kept.read_chunk(chunk, &document);
    }
    Some(kept.tree())
}

/// The tree `workspace`'s host keeps, waiting until it reads as `ready` says, or until the
/// process's hang bound.
pub async fn kept_when(host: &Host, workspace: &str, ready: impl Fn(&Tree) -> bool) -> Tree {
    let started = std::time::Instant::now();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(tree) = tree_if_kept(host, workspace).await
            && ready(&tree)
        {
            return tree;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the workspace's tree never read as the test expects after {started:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// The file at `dir/name` in `tree`, as its node; `None` when it is not there.
pub fn file_at<'a>(
    tree: &'a Tree,
    dir: &str,
    name: &str,
) -> Option<&'a fs_protocol::tree::TreeEntry> {
    entry_at(tree, dir, (name, FileNodeKind::File))
}

/// The folder at `dir/name` in `tree`, as its node; `None` when it is not there.
pub fn folder_node<'a>(
    tree: &'a Tree,
    dir: &str,
    name: &str,
) -> Option<&'a fs_protocol::tree::TreeEntry> {
    entry_at(tree, dir, (name, FileNodeKind::Directory))
}

/// Whether `dir/name` is a folder of `tree`.
pub fn folder_at(tree: &Tree, dir: &str, name: &str) -> bool {
    folder_node(tree, dir, name).is_some()
}

/// The entry of `kind` named `name` in the folder `dir` of `tree`; `None` when it is not there.
fn entry_at<'a>(
    tree: &'a Tree,
    dir: &str,
    (name, kind): (&str, FileNodeKind),
) -> Option<&'a fs_protocol::tree::TreeEntry> {
    let mut parent = ROOT_ID;
    for folder in dir.split('/').filter(|folder| !folder.is_empty()) {
        let (id, _) = tree.children(parent).into_iter().find(|(_, node)| {
            node.kind == FileNodeKind::Directory && node.name.as_bytes() == folder.as_bytes()
        })?;
        parent = id;
    }
    tree.children(parent)
        .into_iter()
        .find(|(_, node)| node.kind == kind && node.name.as_bytes() == name.as_bytes())
        .map(|(_, node)| node)
}

/// What the file at `dir/name` of `tree` names its content by.
pub fn content_at(tree: &Tree, dir: &str, name: &str) -> Option<String> {
    file_at(tree, dir, name).and_then(|node| node.hash.clone())
}

/// How many chunks a file of `bytes` is kept as.
pub fn chunks_of(bytes: &[u8]) -> usize {
    tilo_fs::content::build(bytes, None).0.chunks().len()
}

/// Wait until `workspace`'s tree names `dir/name` and its content holds `bytes` whole: the
/// node's content address.
pub async fn keeps(
    session: &Session<'_>,
    host: &Host,
    workspace: &str,
    (dir, name): (&str, &str),
    bytes: &[u8],
) -> String {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let tree = tree_if_kept(host, workspace).await;
        if let Some(hash) = tree.as_ref().and_then(|tree| content_at(tree, dir, name))
            && content_at_hash(session, workspace, &hash).await.as_deref() == Some(bytes)
        {
            return hash;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{workspace} never kept {dir}/{name} whole"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// The file `workspace`'s tree names at `hash` is the one the upload `reference` answered, as
/// Files keeps a file: the upload names the bytes it took by their own address, kept where
/// content put into the workspace waits for whatever refers to it, while the tree names the file
/// by its chunk manifest, kept and sealed where the workspace keeps its files. The two names
/// differ, and both open, for a member of the workspace, as exactly the uploaded bytes.
///
/// The store-side manifest and its chunks are not read here: a device's dispatch answers content
/// only as the workspace opens it for its member. That the manifest is sealed under the Files keys
/// is the files service's own test.
pub async fn assert_put_file(
    session: &Session<'_>,
    workspace: &str,
    reference: &str,
    hash: &str,
    expected: &[u8],
) {
    let uploaded = tilo_actions::content_hash(reference)
        .unwrap_or_else(|| panic!("the upload answers a content reference: {reference}"));
    assert_eq!(
        uploaded,
        blake3::hash(expected).to_hex().as_str(),
        "the upload names the bytes it took",
    );
    assert_ne!(
        hash, uploaded,
        "the tree names the file by its own manifest, not by the upload's address",
    );
    assert_eq!(
        content_at_hash(session, workspace, hash).await.as_deref(),
        Some(expected),
        "the serving device opens exactly the uploaded bytes under the Files keys",
    );
    assert_eq!(
        content_bytes(session, workspace, reference).await,
        expected,
        "the upload's reference reads the same bytes",
    );
}

/// A one-page PDF headed `title`, as a person files it.
pub fn form_pdf(title: &str) -> Vec<u8> {
    let labels = format!("BT /Helv 12 Tf 40 800 Td ({title}) Tj 0 -36 Td (Surname) Tj ET");
    let objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R /Resources << /Font << /Helv 5 0 R >> >> >>".into(),
        format!("<< /Length {} >>\nstream\n{labels}\nendstream", labels.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// `bytes` no two runs of which repeat, so a content-defined chunker cuts them where it would cut
/// a real file's.
pub fn varied(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state.to_be_bytes()[0]
        })
        .collect()
}

/// What the person's own dispatch answers a call of `target` with `arguments`, followed to its
/// end: its output, or why it was refused.
pub async fn person_calls(
    host: &Host,
    target: tilo_actions::remote::RemoteActionTarget,
    arguments: Value,
    revision: Option<&str>,
) -> Result<Value, tilo_actions::remote::ActionCallError> {
    use tilo_actions::remote::ActionCallError;
    let caller = host.caller();
    let provider = host.dispatch();
    let request = tilo_actions::remote::InvokeRequest {
        asks: false,
        invocation: InvocationId::new(),
        target,
        argument: match &arguments {
            Value::Object(map) => tilo_actions::remote::InvokeArgument::Json(map.clone()),
            _ => tilo_actions::remote::InvokeArgument::None,
        },
        context: None,
        confirmation: None,
        revision: revision.map(str::to_owned),
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let invocation = request.invocation;
    let receipt = provider
        .invoke(&caller, request)
        .await
        .expect("the dispatch answers")?;
    let status = receipt.status;
    let deadline = tilo_test_env::hang_deadline();
    while matches!(status, ReceiptStatus::Accepted) {
        assert!(
            std::time::Instant::now() < deadline,
            "the call never settled"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let Some(state) = provider.observe(&caller, &invocation) else {
            continue;
        };
        match state.update {
            InvocationUpdate::Succeeded { output, .. } => {
                return Ok(output.value.unwrap_or_default());
            }
            InvocationUpdate::Failed { error, .. } => {
                return Err(ActionCallError::Refused {
                    reason: error.message,
                });
            }
            InvocationUpdate::Cancelled { .. } => {
                return Err(ActionCallError::Refused {
                    reason: "the call was cancelled".into(),
                });
            }
            _ => {}
        }
    }
    match status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Failed { error } => Err(ActionCallError::Refused {
            reason: error.message,
        }),
        ReceiptStatus::Rejected { error } => Err(error),
        ReceiptStatus::Cancelled { .. } => Err(ActionCallError::Refused {
            reason: "the call was cancelled".into(),
        }),
        ReceiptStatus::Accepted => unreachable!("waited on above"),
    }
}

/// Files' `verb` by its path, asked of the person's dispatch as the person acts in
/// `workspace`: the folder reads are the workspace's listing of its own tree, and every verb
/// that changes it is the device's own action on the folder it keeps (`tilo.device-files`).
/// What it answered, or the words it was refused with.
pub async fn person_files(
    host: &Host,
    workspace: &str,
    verb: &str,
    arguments: Value,
) -> Result<Value, String> {
    person_calls(host, folder_target(workspace, verb), arguments, None)
        .await
        .map_err(|error| error.to_string())
}

/// [`person_files`]' target: what answers Files' `verb` by a path, where the product publishes
/// it.
fn folder_target(workspace: &str, verb: &str) -> tilo_actions::remote::RemoteActionTarget {
    if verb == fs_protocol::tree::LIST || verb == fs_protocol::tree::READ_TEXT {
        return tilo_actions::remote::RemoteActionTarget {
            provider: tilo_actions::remote::workspace_provider(workspace),
            address: ActionAddress {
                resource: format!("app.{FILES_APP}"),
                action: verb.to_owned(),
            },
        };
    }
    tilo_actions::remote::RemoteActionTarget {
        provider: tilo_action_services::device_files::PROVIDER.to_owned(),
        address: ActionAddress {
            resource: tilo_action_services::device_files::FOLDER.to_owned(),
            action: verb.to_owned(),
        },
    }
}

/// Files' `verb` by its path asked of the person's dispatch, which must be refused: the words it
/// is refused with.
pub async fn person_files_refused(
    host: &Host,
    workspace: &str,
    verb: &str,
    arguments: Value,
) -> String {
    person_files(host, workspace, verb, arguments)
        .await
        .expect_err(&format!("files#{verb} was not refused"))
}
