//! A phone signed in to the person's workspace offers its files to Android's own pickers, through
//! the module the provider calls: the person's workspaces are roots, a photo saved on the phone is
//! written into the workspace, another device of the person's reads it from there, and a change
//! that device makes is what the phone reads next — and what the watch the provider keeps of a
//! folder it shows hears as it lands. Nothing outside the workspace is reachable.
//!
//! The daemon reads its places from the process's environment, so this binary is one test: one
//! phone, in a home of its own, made before anything starts.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

// The server admits the phone's records through an Automerge worker: this binary is that
// worker, as the server host's own is.
tilo_automerge_isolate::test_worker!();

mod server;
mod support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_phone::workspace_files;
use tilo_workspace_service::content::{
    ContentScope, ContentShelf, RouteContentService, routes_content,
};
use tilo_workspace_service::files::Paths;

use server::{Joined, Server, WORKSPACE_NAME};

/// A picture the person's brother saves on the phone.
const PHOTO: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46];

/// The same picture, its last bytes changed, as the laptop's editor saves it.
const EDITED: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x47];

#[test]
fn a_phone_offers_its_workspace_files_and_another_device_reads_what_it_writes() {
    let (_home, server) = Server::for_phone();
    let kept = kept_workspace(&server);
    let Joined { laptop, phone, .. } = server.joined();
    let workspace = server.workspace.clone();

    // The phone offers the person's workspace as a root of the picker, by the name the person
    // gave it.
    let roots = support::until("the phone offers the person's workspaces", || {
        let roots = workspace_files::roots(&phone)?;
        if roots.iter().any(|root| root.key == workspace) {
            Ok(roots)
        } else {
            Err(format!("{roots:?}"))
        }
    });
    assert_eq!(
        roots
            .iter()
            .find(|root| root.key == workspace)
            .map(|root| root.name.as_str()),
        Some(WORKSPACE_NAME),
        "the phone names the workspace as the person does"
    );

    // A folder of the workspace is a listing of the picker, empty to begin with.
    let listed = support::until("the phone lists the workspace's own folder", || {
        workspace_files::list(&phone, &workspace, "")
    });
    assert!(
        listed.is_empty(),
        "a workspace nobody wrote to lists nothing: {listed:?}"
    );

    photos_written_and_read_back(&phone, &kept, &workspace);
    refusals_keep_the_phone_to_its_workspaces(&phone, &workspace);
    phone.stop().expect("the phone's daemon stops");
    drop(laptop);
    drop(kept);
}

/// A picture the phone writes into the workspace is listed by the phone, read by another device of
/// the person's, watched as that device changes it, and renamed and deleted from the phone.
fn photos_written_and_read_back(
    phone: &tilo_runtime::in_process::Device,
    kept: &WorkspaceKept<'_>,
    workspace: &str,
) {
    // A picture saved on the phone is written into the workspace, under a folder it makes.
    workspace_files::write(phone, workspace, "Photos/beach.jpg", PHOTO)
        .expect("the phone writes the picture into the workspace");
    let photos = support::until("the phone lists the folder it wrote", || {
        let photos = workspace_files::list(phone, workspace, "Photos")?;
        if photos.iter().any(|entry| entry.name == "beach.jpg") {
            Ok(photos)
        } else {
            Err(format!("{photos:?}"))
        }
    });
    let photo = photos
        .iter()
        .find(|entry| entry.name == "beach.jpg")
        .expect("the picture the phone wrote is listed");
    assert_eq!(photo.mime, "image/jpeg", "the picker knows what a jpg is");
    assert_eq!(
        photo.size,
        u64::try_from(PHOTO.len()).expect("the picture's length"),
        "the listing counts its bytes"
    );

    // Another device of the person's reads the picture from the workspace.
    let read = support::until("the workspace hands the picture over", || {
        kept.files.read(workspace, "Photos/beach.jpg")
    });
    assert_eq!(read, PHOTO, "the bytes are the picture the phone saved");

    // The provider shows the folder it listed, so the phone watches its workspace: a change
    // another device makes reaches it as it lands, with nothing asking for a revision.
    let (told, changes) = std::sync::mpsc::channel();
    let _watching = workspace_files::watch(phone, workspace, move |folders| {
        let _ = told.send(folders);
    })
    .expect("the phone watches the workspace its provider shows");

    // That device changes the picture, and the phone reads the change.
    let written = std::time::Instant::now();
    kept.files
        .write(workspace, "Photos/beach.jpg", EDITED, true)
        .expect("the other device changes the picture");
    let deadline = written
        .checked_add(Duration::from_secs(30))
        .unwrap_or(written);
    let reached = loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let folders = changes
            .recv_timeout(left)
            .expect("the change reaches the phone's watch");
        if folders.iter().any(|folder| folder == "Photos") {
            break folders;
        }
    };
    let took = written.elapsed();
    assert!(
        took < Duration::from_secs(5),
        "the change reached the watch of {reached:?} only after {took:?}"
    );
    let read_back = support::until("the phone reads the change", || {
        let bytes = workspace_files::read(phone, workspace, "Photos/beach.jpg")?;
        if bytes == EDITED {
            Ok(bytes)
        } else {
            Err(format!("{} bytes, not the change", bytes.len()))
        }
    });
    assert_eq!(read_back, EDITED);

    // The phone renames the picture and deletes it, and the workspace follows.
    let renamed = workspace_files::rename(phone, workspace, "Photos/beach.jpg", "beach-2.jpg")
        .expect("the phone renames the picture");
    assert_eq!(
        renamed,
        workspace_files::document_id(workspace, "Photos/beach-2.jpg")
    );
    support::until("the workspace shows the renamed picture", || {
        let renamed = kept.files.stat(workspace, "Photos/beach-2.jpg")?;
        match renamed {
            Some(entry) => Ok(entry),
            None => Err("the workspace has not taken the new name yet".to_owned()),
        }
    });
    workspace_files::delete(phone, workspace, "Photos/beach-2.jpg")
        .expect("the phone deletes the picture");
    support::until("the workspace shows the picture gone", || {
        let gone = kept.files.stat(workspace, "Photos/beach-2.jpg")?;
        if gone.is_none() {
            Ok(())
        } else {
            Err(format!("{gone:?}"))
        }
    });
}

/// Nothing outside the workspace is reachable, and a workspace the person is not in is not this
/// phone's to read.
fn refusals_keep_the_phone_to_its_workspaces(
    phone: &tilo_runtime::in_process::Device,
    workspace: &str,
) {
    // Nothing outside the workspace is reachable, whatever path a caller names.
    for path in ["../secret.txt", "/etc/passwd", "Photos/../../../secret.txt"] {
        let refused = workspace_files::read(phone, workspace, path)
            .expect_err("a path outside the workspace is refused");
        assert!(
            refused.contains("workspace"),
            "{path:?} was refused as {refused:?}"
        );
    }

    // A workspace the person is not in is not this phone's to read.
    let other = uuid::Uuid::new_v4().to_string();
    let refused = workspace_files::list(phone, &other, "")
        .expect_err("a workspace this device does not hold is refused");
    assert!(refused.contains("does not hold"), "{refused}");
}

/// The workspace as the person's server keeps it, which is also what the laptop whose ticket
/// entered the phone reads and writes: the same files by path the phone's provider uses, over the
/// workspace's own copy, with the bytes on its route.
struct WorkspaceKept<'a> {
    files: Paths<'a>,
    /// The route the bytes travel on, held for as long as the workspace is read.
    _content: RouteContentService,
}

fn kept_workspace(server: &Server) -> WorkspaceKept<'_> {
    let workspaces = server.workspaces();
    let content = routes_content(&workspaces, Arc::new(MemoryShelf::default()));
    WorkspaceKept {
        files: Paths::new(
            workspaces,
            Some(Arc::clone(&content.content)),
            &server.runtime,
        ),
        _content: content,
    }
}

/// The bytes a device keeps, in this test's memory: a content store without a disk.
#[derive(Default)]
struct MemoryShelf(Mutex<HashMap<(String, String, String), Vec<u8>>>);

impl ContentShelf for MemoryShelf {
    fn read(&self, scope: &ContentScope, hash: &str) -> Option<Vec<u8>> {
        self.0
            .lock()
            .unwrap()
            .get(&(scope.workspace.clone(), scope.app.clone(), hash.to_owned()))
            .cloned()
    }

    fn keep(&self, scope: &ContentScope, hash: &str, bytes: &[u8]) -> Result<(), String> {
        self.0.lock().unwrap().insert(
            (scope.workspace.clone(), scope.app.clone(), hash.to_owned()),
            bytes.to_vec(),
        );
        Ok(())
    }
}
