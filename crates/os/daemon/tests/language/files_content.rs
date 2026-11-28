//! Files put into a device's workspace land in the workspace's own file tree: `files#open
//! content=… name=… dir=…`, the one way the API, `tilo files put`, an agent and a person's page
//! put a file there.
//!
//! The workspace has one folder — its Files app record, the tree its members mount — and a put
//! writes a node into it: a folder `dir` that is not there yet is made on the way, a file of the
//! same name takes the new content, and no app's record is made of the file. What opens it
//! afterwards is the person's choice, from the file.

use serde_json::{Value, json};

use crate::language::files_support::{
    Session, assert_put_file, content_at, content_bytes, content_in, files, form_pdf, kept_when,
    put, said,
};
use crate::language::host;
use fs_protocol::files::FileNodeKind;
use fs_protocol::tree::{self, FILES_APP, ROOT_ID};

/// A PDF at the workspace's root is a node of its tree naming content it holds, the same name put
/// again takes the new content, a folder path keeps the folder, and no app's record is made of
/// any of them.
#[test]
fn a_put_file_is_a_node_of_the_workspace_tree_and_no_app_record() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let agent = host.agent();
        let session = Session::Agent(&agent);

        // A PDF at the workspace's root.
        let form = form_pdf("Application for Schengen Visa");
        let content = put(&session, &workspace, &form).await;
        files(
            &session,
            &workspace,
            "open",
            json!({"content": content, "name": "visa.pdf"}),
        )
        .await;
        let tree = kept_when(&host, &workspace, |tree| {
            crate::language::files_support::file_at(tree, "", "visa.pdf").is_some()
        })
        .await;
        let node = crate::language::files_support::file_at(&tree, "", "visa.pdf")
            .expect("the file is in the workspace's tree");
        assert_eq!(node.kind, FileNodeKind::File);
        let first = node
            .hash
            .as_deref()
            .expect("the file names its content")
            .to_owned();
        assert_put_file(&session, &workspace, &content, &first, &form).await;
        assert_eq!(node.size, form.len() as u64);
        assert_eq!(
            content_bytes(&session, &workspace, &content).await,
            form,
            "the member reads the file back",
        );

        // A folder path keeps the folder.
        let permit = form_pdf("Application for Residence Permit");
        let permit_content = put(&session, &workspace, &permit).await;
        files(
            &session,
            &workspace,
            "open",
            json!({"content": permit_content, "name": "permit.pdf", "dir": "forms/2026"}),
        )
        .await;
        let tree = kept_when(&host, &workspace, |tree| {
            crate::language::files_support::file_at(tree, "forms/2026", "permit.pdf").is_some()
        })
        .await;
        let node = crate::language::files_support::file_at(&tree, "forms/2026", "permit.pdf")
            .expect("the file is in its folder");
        assert_put_file(
            &session,
            &workspace,
            &permit_content,
            node.hash.as_deref().unwrap(),
            &permit,
        )
        .await;
        assert!(
            crate::language::files_support::file_at(&tree, "", "permit.pdf").is_none(),
            "it is not at the root"
        );

        // The same name put again takes the new content rather than making a second node.
        let cover = form_pdf("Cover letter");
        let cover_content = put(&session, &workspace, &cover).await;
        files(
            &session,
            &workspace,
            "open",
            json!({"content": cover_content, "name": "visa.pdf"}),
        )
        .await;
        let tree = kept_when(&host, &workspace, |tree| {
            content_at(tree, "", "visa.pdf").is_some_and(|hash| hash != first)
        })
        .await;
        let node = crate::language::files_support::file_at(&tree, "", "visa.pdf")
            .expect("still one file of that name");
        assert_put_file(
            &session,
            &workspace,
            &cover_content,
            node.hash.as_deref().unwrap(),
            &cover,
        )
        .await;
        let roots = tree
            .children(ROOT_ID)
            .into_iter()
            .filter(|(_, node)| node.name.as_bytes() == b"visa.pdf")
            .count();
        assert_eq!(roots, 1, "the file is not kept beside itself");

        // No app's record was made of any of them.
        let records = host
            .daemon()
            .workspaces()
            .records(&workspace)
            .await
            .unwrap_or_default();
        let made: Vec<&(String, String)> = records
            .iter()
            .filter(|(app, _)| app.as_str() != FILES_APP)
            .collect();
        assert!(
            made.iter().all(|(app, _)| !app.contains("pdf-viewer")),
            "no PDF Viewer record holds a put file: {made:?}"
        );
    });
}

/// An agent's folder put answers each file's sealed reference, says which of those addresses the
/// workspace holds, and lands 1,200 files as one change per record per open, not one per file.
#[test]
fn a_folder_put_is_one_run_and_one_write_per_chunk() {
    const FILES: usize = 1200;
    const INLINE: usize = 600;
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let agent = host.agent();
        let session = Session::Agent(&agent);

        // Many small files are kept whole in one call, each answered by the reference that opens
        // it, and the workspace says which of those addresses it holds.
        let first = b"first small file".to_vec();
        let second = b"second small file".to_vec();
        let (text, failed) = said(
            &session
                .tool(
                    &workspace,
                    "tilo_put_many",
                    &json!({"files": [
                        crate::language::files_support::base64(&first),
                        crate::language::files_support::base64(&second),
                    ]}),
                )
                .await,
        );
        assert!(!failed, "the files are kept whole: {text}");
        let kept: Value = crate::language::files_support::json_in(&text);
        let contents = kept["contents"].as_array().expect("contents");
        assert_eq!(contents.len(), 2, "one answer for each file: {text}");
        let mut addresses = Vec::with_capacity(contents.len());
        for (bytes, content) in [&first, &second].iter().zip(contents) {
            let reference = content
                .as_str()
                .and_then(|content| content.strip_prefix("tilo.content:"))
                .unwrap_or_else(|| panic!("no content reference for the file: {text}"));
            assert_eq!(
                content_bytes(&session, &workspace, &format!("tilo.content:{reference}")).await,
                **bytes,
                "the reference opens the file: {text}",
            );
            let address = reference.split('.').next().unwrap_or(reference);
            addresses.push(address.to_owned());
        }
        let known = addresses.first().cloned().unwrap_or_default();
        let unknown = "0".repeat(64);
        let (text, failed) = said(
            &session
                .tool(
                    &workspace,
                    "tilo_put_have",
                    &json!({"hashes": [known, unknown]}),
                )
                .await,
        );
        assert!(!failed, "{text}");
        let have: Value = crate::language::files_support::json_in(&text);
        assert_eq!(have["have"], json!([true, false]), "{text}");

        // A folder large enough to fill more than one chunk of the tree, put in an open whose
        // list a small part of it carries beside the file's own path, and then the rest as
        // content of its own: every file goes in before anything is written.
        let mut entries = Vec::with_capacity(FILES);
        let mut names = Vec::with_capacity(FILES);
        for index in 0..FILES {
            let bytes = format!("file {index}\n").into_bytes();
            let content = put(&session, &workspace, &bytes).await;
            let name = format!("f{index}.txt");
            names.push(name.clone());
            entries.push(json!({"content": content, "name": name, "dir": "repo/src"}));
        }
        let inline = entries.split_off(INLINE);
        files(&session, &workspace, "open", json!({"files": entries})).await;
        let list = put(&session, &workspace, &serde_json::to_vec(&inline).unwrap()).await;
        files(&session, &workspace, "open", json!({"files": list})).await;

        // Each open lands its chunks one after another, so the folder is whole only once every
        // file is in it, not once the last one listed is.
        let tree = kept_when(&host, &workspace, |tree| {
            names.iter().all(|name| {
                crate::language::files_support::file_at(tree, "repo/src", name).is_some()
            })
        })
        .await;
        for name in &names {
            assert!(
                crate::language::files_support::file_at(&tree, "repo/src", name).is_some(),
                "{name} is in the folder"
            );
        }

        // The records the folder changed each carry one change, not one per file.
        let daemon = host.daemon();
        let workspaces = daemon.workspaces();
        let index = workspaces
            .resource_document(&workspace, FILES_APP, &tree::document_id())
            .await
            .unwrap()
            .expect("the workspace keeps the tree's own record");
        let mut index = automerge::AutoCommit::load(&index).expect("the tree's own record reads");
        let chunks = tree::listed_chunks(&index);
        assert!(chunks.len() > 1, "the folder fills more than one chunk");
        let mut changes = index.get_changes(&[]).len();
        for chunk in &chunks {
            let record = workspaces
                .resource_document(&workspace, FILES_APP, &tree::chunk_document_id(*chunk))
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("the workspace keeps chunk {chunk} of the tree"));
            changes += automerge::AutoCommit::load(&record)
                .expect("a chunk of the tree reads")
                .get_changes(&[])
                .len();
        }
        assert!(
            changes.saturating_mul(10) < FILES,
            "the folder is a change per record per open ({changes} changes over {} records), not \
             one per file",
            chunks.len() + 1,
        );
    });
}

/// A large file uploaded in pieces, its upload finished where the host says it stands, lands once
/// and whole.
#[test]
fn a_resumed_upload_of_a_large_file_lands_once_and_whole() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let agent = host.agent();
        let session = Session::Agent(&agent);

        // A file of many pieces, its upload interrupted after the first ones and picked up where
        // the host says it stands.
        let mut bytes = b"%PDF-1.7\n".to_vec();
        bytes.extend((0..300_000_u32).map(|n| u8::try_from(n % 251).unwrap_or_default()));
        let begun = crate::language::files_support::json_in(
            &said(
                &session
                    .tool(&workspace, "tilo_put_begin", &json!({"size": bytes.len()}))
                    .await,
            )
            .0,
        );
        let upload = begun["upload"].as_u64().expect("an upload id");
        let half = bytes.len() / 2;
        for (offset, chunk) in [(0, &bytes[..half]), (half, &bytes[half..])] {
            let (text, failed) = said(
                &session
                    .tool(
                        &workspace,
                        "tilo_put_append",
                        &json!({
                            "upload": upload,
                            "offset": offset,
                            "chunk": crate::language::files_support::base64(chunk),
                        }),
                    )
                    .await,
            );
            assert!(!failed, "a piece was refused: {text}");
        }
        let content = content_in(
            &said(
                &session
                    .tool(&workspace, "tilo_put_finish", &json!({"upload": upload}))
                    .await,
            )
            .0,
        );
        files(
            &session,
            &workspace,
            "open",
            json!({"content": content, "name": "scan.pdf", "dir": "scans"}),
        )
        .await;

        let tree = kept_when(&host, &workspace, |tree| {
            crate::language::files_support::file_at(tree, "scans", "scan.pdf").is_some()
        })
        .await;
        let node = crate::language::files_support::file_at(&tree, "scans", "scan.pdf")
            .expect("the large file is in the tree");
        assert_put_file(
            &session,
            &workspace,
            &content,
            node.hash.as_deref().unwrap(),
            &bytes,
        )
        .await;
        assert_eq!(node.size, bytes.len() as u64);
        assert_eq!(
            content_bytes(&session, &workspace, &content).await,
            bytes,
            "every piece arrived, in order",
        );
    });
}

/// A member who edits a public workspace founds its file tree from their computer's mounted
/// folder; a viewer's computer founds nothing and writes nothing.
#[test]
#[ignore = "mounts a FUSE tree: the real-fuse check runs it"]
fn an_editor_founds_a_public_workspace_tree_and_a_viewer_does_not() {
    host::run(|host| async move {
        let lounge = host
            .daemon()
            .workspaces()
            .create("Lounge")
            .await
            .unwrap()
            .workspace;
        host.daemon()
            .workspaces()
            .found_entry(&lounge, host.daemon().workspaces().device_cert(), |entry| {
                entry.listing = Some(tilo_action_policy::ListingPolicy {
                    description: "Everyone".into(),
                    default_role: tilo_peer_state::join::Role::Editor,
                });
            })
            .await
            .unwrap();
        assert!(
            crate::language::files_support::tree_if_kept(&host, &lounge)
                .await
                .is_none(),
            "nobody founded the lounge's tree yet",
        );

        // Another person's computer joins as an editor and mounts the workspace: what it writes
        // into the folder is its record write, and the first change founds the tree.
        let editor = host::Second::joined(&host, "Ada", "editor", &[], &[]).await;
        let folder = editor.mount("Lounge");
        std::fs::write(folder.join("plan.md"), b"Launch on Friday.\n")
            .expect("the editor's mount takes the write");
        let tree = crate::language::files_support::kept_when(&host, &lounge, |tree| {
            crate::language::files_support::file_at(tree, "", "plan.md").is_some()
        })
        .await;
        assert!(
            crate::language::files_support::file_at(&tree, "", "plan.md").is_some(),
            "the editor's folder founded the workspace's tree",
        );

        // A viewer's computer mounts it too, and writes nothing: the folder is read only, and the
        // workspace's tree gains nothing.
        let viewer = host::Second::joined(&host, "Vic", "viewer", &[], &[]).await;
        let read_only = viewer.mount("Lounge");
        assert!(
            std::fs::write(read_only.join("other.md"), b"Nothing of mine.\n").is_err(),
            "a viewer's folder takes no write",
        );
        let tree = crate::language::files_support::kept_when(&host, &lounge, |_| true).await;
        assert!(
            crate::language::files_support::file_at(&tree, "", "other.md").is_none(),
            "the viewer's folder wrote nothing",
        );
    });
}

/// A computer's mounted folder founds a workspace's tree, and the page of the person whose
/// computer it is opens every document the mount wrote.
#[test]
#[ignore = "mounts a FUSE tree: the real-fuse check runs it"]
fn a_tree_a_computers_mount_founds_is_one_a_page_opens() {
    host::run(|host| async move {
        let workspace = host
            .daemon()
            .workspaces()
            .create("Lounge")
            .await
            .unwrap()
            .workspace;
        host.daemon()
            .workspaces()
            .found_entry(
                &workspace,
                host.daemon().workspaces().device_cert(),
                |entry| {
                    entry.listing = Some(tilo_action_policy::ListingPolicy {
                        description: "Everyone".into(),
                        default_role: tilo_peer_state::join::Role::Editor,
                    });
                },
            )
            .await
            .unwrap();

        // The laptop's folder mounts the workspace and writes the tree's records, as `tilo
        // login` mounts it: the write is the member's document write over the device link.
        let laptop = host::Second::joined(&host, "Ada's laptop", "editor", &[], &[]).await;
        let folder = laptop.mount("Lounge");
        std::fs::write(folder.join("plan.md"), b"Launch on Friday.\n")
            .expect("the laptop's mount takes the write");

        // The page of the person whose computer it is opens every document the mount wrote: the
        // tree names the file and the workspace's content holds its bytes.
        let tree = crate::language::files_support::kept_when(&host, &workspace, |tree| {
            crate::language::files_support::file_at(tree, "", "plan.md").is_some()
        })
        .await;
        let node = crate::language::files_support::file_at(&tree, "", "plan.md")
            .expect("the mount's file is in the tree");
        let hash = node.hash.clone().expect("the file names its content");
        let key = host.key("the page");
        let read = crate::language::files_support::content_at_hash(
            &crate::language::files_support::Session::Key(&key),
            &workspace,
            &hash,
        )
        .await;
        assert_eq!(
            read.as_deref(),
            Some(b"Launch on Friday.\n".as_slice()),
            "the page opens what the mount wrote",
        );
    });
}
