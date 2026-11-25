//! Files a `files#open` names, put into the tree the service holds: the person's own open, one
//! file and a folder of them, the list given as content of its own, and what the tree refuses.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture failing is its test failing"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fs_protocol::files::FileNodeKind;
use fs_protocol::tree::{self, FILES_APP, ROOT_ID, Tree, TreeChunks};
use serde_json::json;

use crate::workspaces::fixtures::{hosting_device, test_host};

use super::*;

/// A content store's shelf in memory, as a device's own is: content kept under the scope a
/// record's content lives under, addressed by its hash.
#[derive(Default)]
struct Shelf(Mutex<HashMap<ContentScope, HashMap<String, Vec<u8>>>>);

impl ContentShelf for Shelf {
    fn read(&self, scope: &ContentScope, hash: &str) -> Option<Vec<u8>> {
        self.0.lock().unwrap().get(scope)?.get(hash).cloned()
    }

    fn keep(&self, scope: &ContentScope, hash: &str, bytes: &[u8]) -> Result<(), String> {
        let address = hex::encode(blake3::hash(bytes).as_bytes());
        if address != hash {
            return Err(format!("these bytes are {address}, not {hash}"));
        }
        self.0
            .lock()
            .unwrap()
            .entry(scope.clone())
            .or_default()
            .insert(hash.to_owned(), bytes.to_vec());
        Ok(())
    }

    fn size(&self, scope: &ContentScope, hash: &str) -> Option<u64> {
        self.read(scope, hash)
            .map(|bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
    }
}

/// Put `bytes` into `workspace`'s content, as an upload does, and answer their reference.
fn put_content(shelf: &Shelf, workspace: WorkspaceId, bytes: &[u8]) -> String {
    let hash = hex::encode(blake3::hash(bytes).as_bytes());
    shelf
        .keep(
            &ContentScope {
                workspace: workspace.to_string(),
                app: CONTENT_PUT_SCOPE.to_owned(),
            },
            &hash,
            bytes,
        )
        .expect("the shelf keeps the content");
    format!("tilo.content:{hash}")
}

/// `record` of the workspace's tree as this device reads it, `None` while the workspace holds
/// none of it. The tree's records are sealed under the Files keys, outside the store.
async fn tree_record(workspaces: &Workspaces, workspace: &str, record: &str) -> Option<Vec<u8>> {
    use tilo_peer_state::workspace_documents::DocumentRead;
    match workspaces
        .own_read(workspace, (FILES_APP, record), &[], usize::MAX)
        .await
        .unwrap_or_else(|error| panic!("the tree's record {record} reads: {error}"))
    {
        DocumentRead::Whole { bytes, .. } => Some(bytes),
        DocumentRead::Absent => None,
        read => panic!("the tree's record {record} reads whole, not {read:?}"),
    }
}

/// The workspace's tree as its records read, `None` before anything was put into it.
async fn tree_if_kept(workspaces: &Workspaces, workspace: &str) -> Option<Tree> {
    let index = tree_record(workspaces, workspace, &tree::document_id()).await?;
    let index = automerge::AutoCommit::load(&index).expect("the tree's own record reads");
    let mut kept = TreeChunks::default();
    kept.read_index(&index);
    for chunk in tree::listed_chunks(&index) {
        let record = tree_record(workspaces, workspace, &tree::chunk_document_id(chunk))
            .await
            .unwrap_or_else(|| panic!("the workspace keeps chunk {chunk} of the tree"));
        kept.read_chunk(
            chunk,
            &automerge::AutoCommit::load(&record).expect("a chunk reads"),
        );
    }
    Some(kept.tree())
}

/// The workspace's tree as its records read.
async fn tree_of(workspaces: &Workspaces, workspace: &str) -> Tree {
    tree_if_kept(workspaces, workspace)
        .await
        .expect("the workspace keeps the tree's own record")
}

/// Whether nothing is in the workspace's tree yet.
async fn tree_is_empty(workspaces: &Workspaces, workspace: &str) -> bool {
    tree_if_kept(workspaces, workspace)
        .await
        .is_none_or(|tree| tree.children(ROOT_ID).is_empty())
}

/// The file at `dir/name` in `tree`, as its node.
fn file_at<'a>(tree: &'a Tree, dir: &str, name: &str) -> Option<&'a fs_protocol::tree::TreeEntry> {
    let mut parent = ROOT_ID;
    for folder in dir.split('/').filter(|folder| !folder.is_empty()) {
        let (id, _) = tree.children(parent).into_iter().find(|(_, node)| {
            node.kind == FileNodeKind::Directory && node.name.as_bytes() == folder.as_bytes()
        })?;
        parent = id;
    }
    tree.children(parent)
        .into_iter()
        .find(|(_, node)| {
            node.kind == FileNodeKind::File && node.name.as_bytes() == name.as_bytes()
        })
        .map(|(_, node)| node)
}

/// The bytes the tree's file named `hash` opens as: its manifest's chunks, opened through the
/// workspace's Files keys where it names sealed ones.
fn opened(
    shelf: &Shelf,
    workspace: WorkspaceId,
    keys: Option<&key_group::ContentKeys>,
    hash: &str,
) -> Vec<u8> {
    let files = ContentScope {
        workspace: workspace.to_string(),
        app: FILES_APP.to_owned(),
    };
    let encoded = shelf.read(&files, hash).expect("the manifest is kept");
    let manifest = Manifest::decode(&encoded).expect("the manifest decodes");
    let mut file = Vec::new();
    for part in manifest.chunks() {
        let stored = shelf
            .read(&files, &hex::encode(part.hash))
            .expect("a chunk is kept");
        let clear = match (manifest.sealed(), keys) {
            (Some(sealed), Some(keys)) => keys
                .open_content(
                    collab_types::file_content_artifact(workspace),
                    &collab_types::EncryptionMetadata::content_group(
                        workspace,
                        WorkspaceId::from_u128(u128::from_be_bytes(sealed.group)),
                        sealed.epoch,
                        collab_types::ContentRetention::Delta,
                    ),
                    &stored,
                )
                .expect("the chunk opens under the Files keys")
                .to_vec(),
            _ => stored,
        };
        assert_eq!(
            clear.len(),
            usize::try_from(part.len).unwrap_or(usize::MAX),
            "a chunk is its clear length"
        );
        file.extend_from_slice(&clear);
    }
    file
}

/// A host keeping a workspace, the shelf its content is kept on, and the puts a call makes into
/// its tree.
struct Put {
    workspaces: Workspaces,
    shelf: Arc<Shelf>,
    puts: OpenedFiles,
    workspace: WorkspaceId,
    name: String,
    // Last, so the host stops before the state it keeps on disk is deleted.
    _host: crate::workspaces::fixtures::TestHost,
    _state: tempfile::TempDir,
}

async fn fixture() -> Put {
    let state = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let host = test_host(state.path(), &key, &cert).await;
    let workspaces = host.workspaces().clone();
    let name = workspaces.create("Files put").await.unwrap().workspace;
    let workspace = WorkspaceId::parse(&name).unwrap();
    let shelf = Arc::new(Shelf::default());
    let puts = OpenedFiles::new(
        workspaces.clone(),
        Arc::clone(&shelf) as Arc<dyn ContentShelf>,
    );
    Put {
        workspaces,
        shelf,
        puts,
        workspace,
        name,
        _host: host,
        _state: state,
    }
}

impl Put {
    /// Put `arguments` as the dispatch does, on a thread of its own: the tree's sync then has
    /// this device's runtime to itself, as it does when a daemon serves a call.
    fn put(&self, arguments: &Value) -> Result<Value, String> {
        self.put_for(arguments, None)
    }

    /// [`Self::put`], made for the `files#open` of `arguments` the workspace's owner signed on
    /// their device.
    fn put_signed(&self, arguments: &Value) -> Result<Value, String> {
        let request = self.signed_open(arguments);
        let context = tilo_actions::ActionContext {
            source_invocation: Some(request.invocation),
            ..tilo_actions::ActionContext::default()
        }
        .with_invoker(tilo_actions::ActionInvoker::person(hex::encode(
            hosting_device().1.user_public_key,
        )));
        self.put_for(
            arguments,
            Some(crate::apps::services::FilesPutCall {
                context,
                request: &request,
            }),
        )
    }

    /// [`Self::put`], made for `call`.
    fn put_for(
        &self,
        arguments: &Value,
        call: Option<crate::apps::services::FilesPutCall<'_>>,
    ) -> Result<Value, String> {
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    self.puts.put(
                        self.workspace,
                        arguments.as_object().unwrap(),
                        call,
                        &|| true,
                    )
                })
                .join()
                .expect("the put's thread ends")
        })
    }

    /// Files' `verb` on the folder with `arguments`, as the workspace's owner signed it on their
    /// device and the dispatch carries it out: on a thread of its own, as a put is.
    fn change(&self, verb: &str, arguments: &Value) -> Result<Value, String> {
        let request = self.signed_call(verb, arguments);
        let context = tilo_actions::ActionContext {
            source_invocation: Some(request.invocation),
            ..tilo_actions::ActionContext::default()
        }
        .with_invoker(tilo_actions::ActionInvoker::person(hex::encode(
            hosting_device().1.user_public_key,
        )));
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    self.puts.change_folder(
                        self.workspace,
                        verb,
                        arguments.as_object().unwrap(),
                        Some(crate::apps::services::FilesPutCall {
                            context,
                            request: &request,
                        }),
                        &|| true,
                    )
                })
                .join()
                .expect("the change's thread ends")
        })
    }

    /// The `files#open` of `arguments`, signed by the workspace's owner on their device.
    fn signed_open(&self, arguments: &Value) -> tilo_actions::remote::InvokeRequest {
        self.signed_call(tilo_actions::remote::APPLICATION_OPEN_ACTION, arguments)
    }

    /// The `files#open` of `arguments`, signed by the key `cert` certifies.
    fn open_signed_by(
        &self,
        arguments: &Value,
        keys: (&ed25519_dalek::SigningKey, &tilo_user_identity::DeviceCert),
    ) -> tilo_actions::remote::InvokeRequest {
        self.call_signed_by(tilo_actions::remote::APPLICATION_OPEN_ACTION, arguments, keys)
    }

    /// Files' `action` of `arguments`, signed by the workspace's owner on their device.
    fn signed_call(&self, action: &str, arguments: &Value) -> tilo_actions::remote::InvokeRequest {
        let (key, cert) = hosting_device();
        self.call_signed_by(action, arguments, (&key, &cert))
    }

    /// Files' `action` of `arguments`, signed by the key `cert` certifies.
    fn call_signed_by(
        &self,
        action: &str,
        arguments: &Value,
        (key, cert): (&ed25519_dalek::SigningKey, &tilo_user_identity::DeviceCert),
    ) -> tilo_actions::remote::InvokeRequest {
        let mut request = tilo_actions::remote::InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: tilo_actions::remote::workspace_provider(&self.name),
                address: tilo_actions::ActionAddress::new(
                    format!(
                        "{}{FILES_APP}",
                        tilo_actions::remote::APPLICATION_RESOURCE_PREFIX
                    ),
                    action,
                ),
            },
            argument: tilo_actions::remote::InvokeArgument::Json(
                arguments.as_object().cloned().unwrap(),
            ),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        request.signed = Some(Box::new(
            tilo_server_protocol::invocation::sign_invocation_now(
                &self.name,
                &request,
                cert,
                key,
                tilo_clock::unix_now_ns(),
            )
            .unwrap(),
        ));
        request
    }

    /// Let the agent `id` of `person` write the workspace's files, as a person's Settings grants
    /// it.
    async fn allow_agent(&self, id: &str, person: &str) {
        crate::workspaces::fixtures::record_grants(
            &self.workspaces,
            &self.name,
            &[tilo_action_policy::Grant::new(
                format!("{id}-files"),
                tilo_action_policy::Grantee::agent(id, person),
                tilo_action_policy::TreePath::workspace(&self.name).app(FILES_APP),
                tilo_action_policy::KindSet::READ_UNDOABLE,
                tilo_action_policy::Answer::Allow,
                tilo_action_policy::Grantee::person(person),
            )],
        )
        .await;
    }

    /// What the workspace's Files keys are, as this device keeps them.
    async fn keys(&self) -> Option<key_group::ContentKeys> {
        Some(
            self.workspaces
                .own_route_keys(&tree_route(self.workspace), false)
                .await
                .expect("this device takes part in the workspace's key group")
                .expect("the workspace's Files keys open here"),
        )
    }
}

/// A person puts one file into the workspace's folder: a node of its tree naming content the
/// workspace keeps, the folder `dir` made on the way, and the same name put again taking the
/// new content rather than a second node.
#[tokio::test]
async fn a_file_put_is_a_node_naming_content_the_workspace_keeps() {
    let put = fixture().await;
    let visa = b"%PDF-1.7\nApplication for Schengen Visa\n";
    let content = put_content(&put.shelf, put.workspace, visa);
    let answer = put
        .put(&json!({"content": content, "name": "visa.pdf"}))
        .unwrap();
    assert_eq!(answer["path"], "visa.pdf", "{answer}");
    assert_eq!(answer["content"], content.as_str(), "{answer}");
    assert_eq!(answer["size"], visa.len() as u64, "{answer}");
    assert_eq!(answer["replaced"], false, "{answer}");
    let tree = tree_of(&put.workspaces, &put.name).await;
    let node = file_at(&tree, "", "visa.pdf").expect("the file is in the workspace's tree");
    let hash = node.hash.clone().expect("the file names its content");
    assert_eq!(node.size, visa.len() as u64);
    let keys = put.keys().await;
    assert_eq!(
        opened(&put.shelf, put.workspace, keys.as_ref(), &hash),
        visa.as_slice()
    );

    // A folder path keeps the folder, and the file is in it, not at the root.
    let permit = b"%PDF-1.7\nApplication for Residence Permit\n";
    let permit_content = put_content(&put.shelf, put.workspace, permit);
    let answer = put
        .put(&json!({"content": permit_content, "name": "permit.pdf", "dir": "forms/2026"}))
        .unwrap();
    assert_eq!(answer["path"], "forms/2026/permit.pdf", "{answer}");
    let tree = tree_of(&put.workspaces, &put.name).await;
    let node = file_at(&tree, "forms/2026", "permit.pdf").expect("the file is in its folder");
    assert_eq!(
        opened(
            &put.shelf,
            put.workspace,
            keys.as_ref(),
            node.hash.as_deref().unwrap()
        ),
        permit.as_slice()
    );
    assert!(
        file_at(&tree, "", "permit.pdf").is_none(),
        "it is not at the root"
    );

    // The same name put again takes the new content rather than making a second node.
    let cover = b"%PDF-1.7\nCover letter\n";
    let cover_content = put_content(&put.shelf, put.workspace, cover);
    let answer = put
        .put(&json!({"content": cover_content, "name": "visa.pdf"}))
        .unwrap();
    assert_eq!(answer["replaced"], true, "{answer}");
    assert_ne!(answer["content"], content.as_str(), "{answer}");
    let tree = tree_of(&put.workspaces, &put.name).await;
    let node = file_at(&tree, "", "visa.pdf").expect("still one file of that name");
    assert_eq!(
        opened(
            &put.shelf,
            put.workspace,
            keys.as_ref(),
            node.hash.as_deref().unwrap()
        ),
        cover.as_slice()
    );
    let roots = tree
        .children(ROOT_ID)
        .into_iter()
        .filter(|(_, node)| node.name.as_bytes() == b"visa.pdf")
        .count();
    assert_eq!(roots, 1, "the file is not kept beside itself");
}

/// The tree's change of a put keeps the request it was made for, and the workspace takes it in:
/// the person's as they signed it, decided by their own grants on the tree's verbs, and an
/// agent's as it signed it with the key its person's root certified for it. An agent's call
/// nobody signed for it puts nothing: this device signs no agent's put for it.
#[tokio::test]
async fn a_put_the_person_signed_is_taken_in_keeping_their_request() {
    let put = fixture().await;
    let bytes = b"signed for\n";
    let content = put_content(&put.shelf, put.workspace, bytes);
    let arguments = json!({"content": content, "name": "asked.txt", "dir": "signed"});
    let answer = put.put_signed(&arguments).unwrap();
    assert_eq!(answer["path"], "signed/asked.txt", "{answer}");
    let tree = tree_of(&put.workspaces, &put.name).await;
    assert!(
        file_at(&tree, "signed", "asked.txt").is_some(),
        "the workspace took the change in"
    );

    // An agent of the person's signed its open with the key their root certified for it: the
    // workspace takes the change in as the agent's.
    let agents = put_content(&put.shelf, put.workspace, b"an agent's\n");
    let arguments = json!({"content": agents, "name": "agent.txt", "dir": "signed"});
    let owner = hex::encode(hosting_device().1.user_public_key);
    put.allow_agent("porter", &owner).await;
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    let key = ed25519_dalek::SigningKey::from_bytes(&[51; 32]);
    let now = tilo_clock::unix_now_ns();
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Porter".to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::agent_grant(
            tilo_user_identity::AgentScope::agent_in(*put.workspace.as_uuid().as_bytes(), "porter"),
        ),
        issued_at_ns: now - 1,
        expires_at_ns: Some(now + 3_600_000_000_000),
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
    let porter = |request: &tilo_actions::remote::InvokeRequest| {
        tilo_actions::ActionContext {
            source_invocation: Some(request.invocation),
            ..tilo_actions::ActionContext::default()
        }
        .with_invoker(tilo_actions::ActionInvoker::admitted(
            &tilo_action_policy::Acting::agent("porter", owner.as_str())
                .acting_for(tilo_action_policy::Acting::person(owner.as_str())),
        ))
    };
    let request = put.open_signed_by(&arguments, (&key, &cert));
    let answer = put
        .put_for(
            &arguments,
            Some(crate::apps::services::FilesPutCall {
                context: porter(&request),
                request: &request,
            }),
        )
        .unwrap();
    assert_eq!(answer["path"], "signed/agent.txt", "{answer}");
    let tree = tree_of(&put.workspaces, &put.name).await;
    assert!(file_at(&tree, "signed", "agent.txt").is_some());

    // The same agent's call nobody signed: the workspace takes no change this device signed for
    // it, and nothing is put.
    let unsigned = json!({"content": agents, "name": "unsigned.txt", "dir": "signed"});
    let mut request = put.signed_open(&unsigned);
    request.signed = None;
    let refused = put
        .put_for(
            &unsigned,
            Some(crate::apps::services::FilesPutCall {
                context: porter(&request),
                request: &request,
            }),
        )
        .unwrap_err();
    assert_eq!(refused, crate::apps::services::KEYLESS_AGENT);
    let tree = tree_of(&put.workspaces, &put.name).await;
    assert!(file_at(&tree, "signed", "unsigned.txt").is_none());
}

/// A folder of files goes in as one change, whether the list is given inline or as content of
/// its own: one answer with the count, and every file lands in its folder.
#[tokio::test]
async fn a_folder_of_files_goes_in_inline_or_by_its_content_reference() {
    let put = fixture().await;
    let mut listed = Vec::new();
    let mut names = Vec::new();
    for index in 0..3 {
        let bytes = format!("file {index}\n").into_bytes();
        let content = put_content(&put.shelf, put.workspace, &bytes);
        let name = format!("f{index}.txt");
        names.push(name.clone());
        listed.push(json!({"content": content, "name": name, "dir": "repo/src"}));
    }
    let answer = put.put(&json!({"files": listed})).unwrap();
    assert_eq!(answer["count"], 3, "{answer}");
    let keys = put.keys().await;
    let tree = tree_of(&put.workspaces, &put.name).await;
    for (index, name) in names.iter().enumerate() {
        let node =
            file_at(&tree, "repo/src", name).unwrap_or_else(|| panic!("{name} is in the folder"));
        assert_eq!(
            opened(
                &put.shelf,
                put.workspace,
                keys.as_ref(),
                node.hash.as_deref().unwrap()
            ),
            format!("file {index}\n").into_bytes(),
            "{name} opens as what was put"
        );
    }

    // The same kind of list, given as content the workspace keeps: one reference, however many
    // files it names.
    let mut second = Vec::new();
    for index in 3..5 {
        let bytes = format!("file {index}\n").into_bytes();
        let content = put_content(&put.shelf, put.workspace, &bytes);
        second.push(json!({
            "content": content,
            "name": format!("f{index}.txt"),
            "dir": "repo/src",
        }));
    }
    let list = put_content(
        &put.shelf,
        put.workspace,
        &serde_json::to_vec(&second).unwrap(),
    );
    let answer = put.put(&json!({"files": list})).unwrap();
    assert_eq!(answer["count"], 2, "{answer}");
    let tree = tree_of(&put.workspaces, &put.name).await;
    for name in ["f3.txt", "f4.txt"] {
        assert!(
            file_at(&tree, "repo/src", name).is_some(),
            "{name} is in the folder the list named"
        );
    }
}

/// Content the workspace does not hold refuses the whole open, nothing is put, and what the tree
/// and a file's name may not be refuses it too.
#[tokio::test]
async fn content_the_workspace_lacks_and_names_the_tree_refuses_put_nothing() {
    let put = fixture().await;
    let missing = format!("tilo.content:{}", "0".repeat(64));
    let refused = |answer: Result<Value, String>| answer.expect_err("the open is refused");
    let said = refused(put.put(&json!({"content": missing, "name": "gone.bin"})));
    assert!(said.contains("holds no"), "{said}");
    assert!(
        tree_is_empty(&put.workspaces, &put.name).await,
        "nothing was put"
    );

    let content = put_content(&put.shelf, put.workspace, b"words\n");
    for (arguments, why) in [
        (
            json!({"content": content, "name": "a/b.txt"}),
            "no file's name",
        ),
        (json!({"content": content, "name": ".."}), "no file's name"),
        (
            json!({"content": content, "name": "a.txt", "dir": "../out"}),
            "no folder's name",
        ),
        (
            json!({"content": "not-content", "name": "a.txt"}),
            "names no content",
        ),
        (json!({"files": []}), "1 to"),
    ] {
        let said = refused(put.put(&arguments));
        assert!(said.contains(why), "{arguments}: {said}");
    }
    assert!(
        tree_is_empty(&put.workspaces, &put.name).await,
        "a refused open put nothing"
    );
}

/// What a put names of its files: a content reference and a name a file may have, a folder path
/// kept as the tree keeps one, and the shapes the tree and the workspace refuse.
#[test]
fn a_put_names_its_files_and_refuses_what_the_tree_may_not_hold() {
    let content = format!("tilo.content:{}", "ab".repeat(32));
    let named = |arguments: Value| Putting::named(arguments.as_object().unwrap());
    let arguments = json!({"content": content, "name": "plan.md", "dir": " / Tasks // 2026 / "});
    let putting = named(arguments).expect("a file named by its content");
    assert_eq!(
        putting.dir, "Tasks/2026",
        "the folder path as the tree keeps one"
    );
    assert_eq!(putting.name, "plan.md");
    assert_eq!(putting.hash, "ab".repeat(32));

    for (arguments, why) in [
        (
            json!({"content": "notes.md", "name": "plan.md"}),
            "names no content",
        ),
        (json!({"content": content, "name": ""}), "no file's name"),
        (
            json!({"content": content, "name": "a/b.md"}),
            "no file's name",
        ),
        (
            json!({"content": content, "name": "a.md", "dir": "../out"}),
            "no folder's name",
        ),
        (
            json!({"content": content, "name": "a.md", "dir": "a/./b"}),
            "no folder's name",
        ),
    ] {
        let said = named(arguments.clone())
            .err()
            .unwrap_or_else(|| panic!("{arguments} is refused"));
        assert!(said.contains(why), "{arguments}: {said}");
    }

    // A list is each file it names, given inline or as a content reference to one, and a list
    // of none or too many is refused as a whole.
    let listed = json!([
        {"content": content, "name": "a.md"},
        {"content": content, "name": "b.md", "dir": "Tasks"},
    ]);
    let files = Putting::all(json!({"files": listed}).as_object().unwrap(), &|_| None)
        .expect("a list is read");
    assert_eq!(files.len(), 2);
    assert_eq!(files[1].dir, "Tasks");
    let bytes = serde_json::to_vec(&listed).unwrap();
    let files = Putting::all(
        json!({"files": format!("tilo.content:{}", "cd".repeat(32))})
            .as_object()
            .unwrap(),
        &|_| Some(bytes.clone()),
    )
    .expect("a list given as content is read");
    assert_eq!(files.len(), 2);
    assert!(Putting::all(json!({"files": []}).as_object().unwrap(), &|_| None).is_err());
    let too_many: Vec<Value> = (0..=MAX_FILES)
        .map(|_| json!({"content": content, "name": "a.md"}))
        .collect();
    let said = Putting::all(json!({"files": too_many}).as_object().unwrap(), &|_| None)
        .err()
        .expect("more files than one open puts");
    assert!(said.contains("1 to"), "{said}");
}

/// What `files` folder's read `action` answers with `arguments`, read as the dispatch reads it:
/// on a thread of its own, the tree's sync having this device's runtime to itself.
fn read(put: &Put, action: &str, arguments: &Value) -> Result<Value, String> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                put.puts
                    .read_folder(put.workspace, action, arguments.as_object().unwrap())
            })
            .join()
            .expect("the read's thread ends")
    })
}

/// Files put into the workspace's folder are read back as a caller with no folder mounted
/// reads them: the workspace's folder and a folder in it listed by name, a file's text by its
/// path, put together from the chunks the workspace seals it in, and content a message names by
/// its address. A file that is no text says what reads it, and a path that is not there says so.
#[tokio::test]
async fn files_put_are_listed_and_read_back_as_text() {
    let put = fixture().await;
    let notes = "Visa appointment: Tuesday, 10:30.\nBring the passport.\n".repeat(1500);
    let notes_content = put_content(&put.shelf, put.workspace, notes.as_bytes());
    put.put(&json!({"content": notes_content, "name": "notes.md", "dir": "trip"}))
        .unwrap();
    let scan = b"%PDF-1.7\n\xff\xfe binary\n";
    let scan_content = put_content(&put.shelf, put.workspace, scan);
    put.put(&json!({"content": scan_content, "name": "scan.pdf"}))
        .unwrap();
    assert!(
        put.keys().await.is_some(),
        "the workspace seals its files, so the read opens them"
    );

    let top = read(&put, tree::LIST, &json!({})).unwrap();
    let names: Vec<_> = top["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| (entry["name"].clone(), entry["kind"].clone()))
        .collect();
    assert_eq!(
        names,
        [
            (json!("scan.pdf"), json!("file")),
            (json!("trip"), json!("folder"))
        ],
        "{top}"
    );
    let trip = read(&put, tree::LIST, &json!({"folder": "trip"})).unwrap();
    assert_eq!(trip["entries"][0]["path"], "trip/notes.md", "{trip}");
    assert_eq!(trip["entries"][0]["size"], notes.len(), "{trip}");
    assert_eq!(trip["total"], 1, "{trip}");

    // The text a page at a time, the next page where the first ends.
    let first = read(&put, tree::READ_TEXT, &json!({"file": "trip/notes.md"})).unwrap();
    assert_eq!(first["total"], notes.chars().count(), "{first}");
    let next = first["next"]
        .as_u64()
        .expect("the notes are longer than a page");
    let second = read(
        &put,
        tree::READ_TEXT,
        &json!({"file": "trip/notes.md", "from": next}),
    )
    .unwrap();
    assert_eq!(second["next"], Value::Null, "{second}");
    let whole = format!(
        "{}{}",
        first["text"].as_str().unwrap(),
        second["text"].as_str().unwrap()
    );
    assert_eq!(whole, notes, "the file reads as it was put");

    // Content a message names is read by its address, as put.
    let by_address = read(&put, tree::READ_TEXT, &json!({"content": notes_content})).unwrap();
    assert_eq!(by_address["total"], notes.chars().count(), "{by_address}");

    let error = read(&put, tree::READ_TEXT, &json!({"file": "scan.pdf"})).unwrap_err();
    assert!(
        error.contains("is not text") && error.contains("PDF"),
        "{error}"
    );
    let error = read(&put, tree::READ_TEXT, &json!({"file": "trip/missing.md"})).unwrap_err();
    assert!(error.contains("trip/missing.md"), "{error}");
    let error = read(&put, tree::READ_TEXT, &json!({"file": "trip"})).unwrap_err();
    assert!(error.contains("folder"), "{error}");
}

/// The names `listed` answers, in order.
fn names(listed: &Value) -> Vec<String> {
    listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap().to_owned())
        .collect()
}

/// With no folder mounted, a caller keeps a folder for a task by paths in the workspace: a folder
/// made with the one on the way, a file written from text that writes over nothing unless asked,
/// a folder renamed, moved and copied, the trash taking one, and each call taken back by the
/// invocation it answered. A device or a path out of the workspace changes nothing.
#[tokio::test]
async fn a_folder_is_kept_by_paths_and_each_call_taken_back() {
    let put = fixture().await;
    let made = put
        .change("new-folder", &json!({"parent": "Tasks", "name": "12 Plan"}))
        .unwrap();
    assert_eq!(made["entries"][0]["to"], "Tasks/12 Plan", "{made}");
    assert_eq!(made["made"], json!(["Tasks"]), "{made}");
    assert!(made["invocation"].is_string(), "{made}");

    let notes = "Launch on Friday.\n";
    let written = put
        .change(
            "write-file",
            &json!({"folder": "Tasks/12 Plan", "name": "report.md", "text": notes}),
        )
        .unwrap();
    assert_eq!(written["path"], "Tasks/12 Plan/report.md", "{written}");
    assert_eq!(written["size"], notes.len(), "{written}");
    assert_eq!(written["replaced"], false, "{written}");
    let text = |put: &Put| {
        read(
            put,
            tree::READ_TEXT,
            &json!({"file": "Tasks/12 Plan/report.md"}),
        )
        .unwrap()["text"]
            .clone()
    };
    assert_eq!(text(&put), notes);
    let said = put
        .change(
            "write-file",
            &json!({"folder": "Tasks/12 Plan", "name": "report.md", "text": "moved"}),
        )
        .unwrap_err();
    assert!(said.contains("is there already"), "{said}");
    let over = put
        .change(
            "write-file",
            &json!({"folder": "Tasks/12 Plan", "name": "report.md", "text": "Monday.\n", "replace": true}),
        )
        .unwrap();
    assert_eq!(over["replaced"], true, "{over}");
    assert_eq!(text(&put), "Monday.\n");
    let back = put
        .change("undo", &json!({"invocation": over["invocation"]}))
        .unwrap();
    assert_eq!(back["taken"], 1, "{back}");
    assert_eq!(text(&put), notes, "the undo put back what the file said");

    for name in ["drafts", "sources"] {
        put.change("new-folder", &json!({"parent": "Inbox", "name": name}))
            .unwrap();
    }
    put.change(
        "move",
        &json!({"from": "Inbox", "entries": ["drafts", "sources"], "to": "Tasks/12 Plan"}),
    )
    .unwrap();
    let renamed = put
        .change(
            "rename",
            &json!({"folder": "Tasks/12 Plan", "entry": "drafts", "name": "Drafts"}),
        )
        .unwrap();
    assert_eq!(
        renamed["entries"][0]["to"], "Tasks/12 Plan/Drafts",
        "{renamed}"
    );
    put.change(
        "copy-into",
        &json!({"from": "Tasks", "entries": ["12 Plan"], "to": "Archive"}),
    )
    .unwrap();
    let listed = |folder: &str| names(&read(&put, tree::LIST, &json!({"folder": folder})).unwrap());
    assert_eq!(
        listed("Archive/12 Plan"),
        ["Drafts", "report.md", "sources"]
    );
    assert!(listed("Inbox").is_empty());

    let gone = put
        .change(
            "trash",
            &json!({"folder": "Tasks/12 Plan", "entries": ["sources"]}),
        )
        .unwrap();
    assert_eq!(listed("Tasks/12 Plan"), ["Drafts", "report.md"]);
    put.change("undo", &json!({"invocation": gone["invocation"]}))
        .unwrap();
    assert_eq!(listed("Tasks/12 Plan"), ["Drafts", "report.md", "sources"]);

    for (verb, arguments, why) in [
        (
            "new-folder",
            json!({"device": "laptop", "parent": "/home/ada", "name": "x"}),
            "names no device",
        ),
        (
            "move",
            json!({"from": "Tasks", "entries": ["12 Plan"], "to": "../outside"}),
            "no name in the workspace",
        ),
        (
            "write-file",
            json!({"folder": "Tasks", "name": "x.md"}),
            "one of the two",
        ),
        (
            "undo",
            json!({"invocation": tilo_actions::InvocationId::new().0.to_string()}),
            "no change of that call",
        ),
    ] {
        let said = put.change(verb, &arguments).unwrap_err();
        assert!(said.contains(why), "{verb} {arguments}: {said}");
    }
    assert_eq!(
        listed(""),
        ["Archive", "Inbox", "Tasks"],
        "a refused call changed nothing"
    );
}
