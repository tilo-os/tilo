//! A host's workspace served read only over a Unix socket, browsed and
//! read through the ordinary `FsClient`.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::sync::Arc;

use runtime_fs::workspace_server::{WorkspaceBackend, serve_workspace};
use runtime_fs::{DirEntry, EntryKind, Metadata, OpenMode};

/// Files by path; directories are the paths' prefixes.
struct Files(BTreeMap<&'static str, &'static [u8]>);

fn file(size: usize) -> Metadata {
    Metadata {
        kind: EntryKind::File,
        size: size as u64,
        modified_unix_seconds: Some(1_700_000_000),
        needs_hydration: false,
    }
}

const DIR: Metadata = Metadata {
    kind: EntryKind::Directory,
    size: 0,
    modified_unix_seconds: None,
    needs_hydration: false,
};

impl WorkspaceBackend for Files {
    fn list(&self, path: &str) -> Result<Vec<DirEntry>, String> {
        let prefix = if path.is_empty() {
            String::new()
        } else {
            format!("{path}/")
        };
        let mut entries: BTreeMap<String, Metadata> = BTreeMap::new();
        for (name, bytes) in &self.0 {
            if let Some(rest) = name.strip_prefix(&prefix) {
                match rest.split_once('/') {
                    Some((dir, _)) => entries.insert(dir.to_owned(), DIR),
                    None => entries.insert(rest.to_owned(), file(bytes.len())),
                };
            }
        }
        Ok(entries
            .into_iter()
            .map(|(name, metadata)| DirEntry { name, metadata })
            .collect())
    }

    fn stat(&self, path: &str) -> Result<Metadata, String> {
        if let Some(bytes) = self.0.get(path) {
            return Ok(file(bytes.len()));
        }
        let prefix = format!("{path}/");
        (path.is_empty() || self.0.keys().any(|name| name.starts_with(&prefix)))
            .then_some(DIR)
            .ok_or_else(|| format!("no {path}"))
    }

    fn read(&self, path: &str, offset: u64, max_len: u32) -> Result<Vec<u8>, String> {
        let bytes = self.0.get(path).ok_or_else(|| format!("no {path}"))?;
        let start = usize::try_from(offset).unwrap().min(bytes.len());
        let end = start.saturating_add(max_len as usize).min(bytes.len());
        Ok(bytes.get(start..end).unwrap_or_default().to_vec())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_served_workspace_is_browsed_and_read_but_not_written() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("files.sock");
    let files = Files(BTreeMap::from([
        ("notes/plan.md", b"# Plan".as_slice()),
        ("readme.txt", b"hello".as_slice()),
    ]));
    let server =
        serve_workspace(&socket, Arc::new(files), tokio::runtime::Handle::current()).unwrap();

    let client_socket = socket.clone();
    tokio::task::LocalSet::new()
        .run_until(async move {
            let fs = runtime_fs::connect(&client_socket).unwrap();
            let root = fs.list("").await.unwrap();
            let names: Vec<_> = root
                .iter()
                .map(|entry| (entry.name.as_str(), entry.metadata.kind))
                .collect();
            assert_eq!(
                names,
                [
                    ("notes", EntryKind::Directory),
                    ("readme.txt", EntryKind::File)
                ]
            );
            assert_eq!(fs.stat("/notes/plan.md").await.unwrap().size, 6);

            let plan = fs.open("notes/plan.md", OpenMode::Read).await.unwrap();
            assert_eq!(plan.read_all().await.unwrap(), b"# Plan");
            assert!(
                fs.open("notes", OpenMode::Read).await.is_err(),
                "a directory"
            );
            assert!(fs.open("readme.txt", OpenMode::Write).await.is_err());
            assert!(fs.mkdir("new").await.is_err());
            assert!(fs.remove("readme.txt").await.is_err());
        })
        .await;

    drop(server);
    assert!(!socket.exists(), "a stopped server removes its socket");
}
