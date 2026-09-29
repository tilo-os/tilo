use super::*;
use runtime_fs::workspace_server::{WorkspaceBackend, WorkspaceServer, serve_workspace};
use runtime_fs::{DirEntry, EntryKind, Metadata};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

/// One folder holding one file.
struct Workspace;

const PLAN: &[u8] = b"# Plan\n\nShip the file provider.\n";

fn metadata(kind: EntryKind, size: usize) -> Metadata {
    Metadata {
        kind,
        size: size as u64,
        modified_unix_seconds: Some(1_700_000_000),
        needs_hydration: kind == EntryKind::File,
    }
}

impl WorkspaceBackend for Workspace {
    fn list(&self, path: &str) -> Result<Vec<DirEntry>, String> {
        match path {
            "" => Ok(vec![DirEntry {
                name: "notes".into(),
                metadata: metadata(EntryKind::Directory, 0),
            }]),
            "notes" => Ok(vec![DirEntry {
                name: "plan.md".into(),
                metadata: metadata(EntryKind::File, PLAN.len()),
            }]),
            _ => Err(format!("{path}: not a directory")),
        }
    }

    fn stat(&self, path: &str) -> Result<Metadata, String> {
        match path {
            "" | "notes" => Ok(metadata(EntryKind::Directory, 0)),
            "notes/plan.md" => Ok(metadata(EntryKind::File, PLAN.len())),
            _ => Err(format!("{path}: no such item")),
        }
    }

    fn read(&self, path: &str, offset: u64, max_len: u32) -> Result<Vec<u8>, String> {
        if path != "notes/plan.md" {
            return Err(format!("{path}: no such file"));
        }
        let start = usize::try_from(offset).unwrap().min(PLAN.len());
        let end = start.saturating_add(max_len as usize).min(PLAN.len());
        Ok(PLAN[start..end].to_vec())
    }
}

fn serve(runtime: &tokio::runtime::Runtime, socket: &Path) -> WorkspaceServer {
    serve_workspace(socket, Arc::new(Workspace), runtime.handle().clone()).unwrap()
}

fn c(text: &str) -> CString {
    CString::new(text).unwrap()
}

/// Takes and parses one answer the library returned.
fn take(answer: *mut c_char) -> Value {
    assert!(!answer.is_null());
    // SAFETY: the library returned it and it is freed right after.
    let parsed = serde_json::from_str(unsafe { CStr::from_ptr(answer) }.to_str().unwrap());
    // SAFETY: returned by the library, used no more.
    unsafe { tilo_fp_string_free(answer) };
    parsed.unwrap()
}

struct Opened(*mut Session);

impl Opened {
    fn new(socket: &Path) -> Self {
        let socket = c(socket.to_str().unwrap());
        // SAFETY: a valid string.
        let session = unsafe { tilo_fp_open(socket.as_ptr()) };
        assert!(!session.is_null());
        Self(session)
    }

    fn list(&self, path: &str) -> Value {
        // SAFETY: a live session and a valid string.
        take(unsafe { tilo_fp_list(self.0, c(path).as_ptr()) })
    }

    fn stat(&self, path: &str) -> Value {
        // SAFETY: a live session and a valid string.
        take(unsafe { tilo_fp_stat(self.0, c(path).as_ptr()) })
    }

    fn fetch(&self, path: &str, destination: &Path) -> Value {
        let destination = c(destination.to_str().unwrap());
        // SAFETY: a live session and valid strings.
        take(unsafe { tilo_fp_fetch(self.0, c(path).as_ptr(), destination.as_ptr()) })
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        // SAFETY: opened by `new`, closed once.
        unsafe { tilo_fp_close(self.0) };
    }
}

#[test]
fn the_extension_enumerates_and_hydrates_the_served_workspace() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("files.sock");
    let _server = serve(&runtime, &socket);
    let session = Opened::new(&socket);

    assert_eq!(
        session.list(""),
        json!({"ok": [{"name": "notes", "kind": "directory", "size": 0, "modified": 1_700_000_000}]})
    );
    assert_eq!(
        session.stat("notes/plan.md"),
        json!({"ok": {"name": "plan.md", "kind": "file", "size": PLAN.len(), "modified": 1_700_000_000}})
    );
    let hydrated = dir.path().join("plan.md");
    assert_eq!(
        session.fetch("notes/plan.md", &hydrated),
        json!({"ok": {"size": PLAN.len()}})
    );
    assert_eq!(std::fs::read(&hydrated).unwrap(), PLAN);

    assert!(session.stat("missing").get("error").is_some());
    assert!(
        session
            .fetch("notes/plan.md", &hydrated)
            .get("error")
            .is_some(),
        "a hydration never overwrites an existing file"
    );
}

#[test]
fn a_session_outlives_a_runtime_restart() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("files.sock");
    let session = Opened::new(&socket);
    assert!(
        session.list("").get("error").is_some(),
        "nothing serves the workspace yet"
    );

    let server = serve(&runtime, &socket);
    assert!(session.list("notes").get("ok").is_some());
    drop(server);
    assert!(session.list("notes").get("error").is_some());

    let _server = serve(&runtime, &socket);
    assert!(session.list("notes").get("ok").is_some());
}

#[test]
fn null_arguments_are_refused_not_followed() {
    // SAFETY: null is an accepted session and path.
    let answer = take(unsafe { tilo_fp_list(std::ptr::null(), std::ptr::null()) });
    assert!(answer.get("error").is_some());
}
