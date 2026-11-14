//! A workspace's store as the 0.1 releases kept it, brought up once.
//!
//! Those releases kept a workspace's store under `workspaces/<number>`, the
//! number they derived from the workspace's id
//! ([`WorkspaceId::released_number`]), and every frame it retained named the
//! workspace by that number too. This build keeps it under
//! `workspaces/<id>` and names it by the id. Opening the store of a workspace
//! whose released directory is there and whose own is not brings it up
//! ([`bring_up`]): the directory is copied beside it with each retained or
//! pending frame written as this build writes it
//! ([`tilo_relay_store::frames::released_frame`]) and every other file as it
//! was, the copy is made durable and renamed into place, and the released
//! directory stays beside it as `workspaces/<number>.kept` ([`kept`]), never
//! read again. A crash before the rename leaves the released directory as it was
//! and the next open starts over; after it, the workspace's own directory is
//! there and the next open finds nothing to bring up.
//!
//! A generation's journal is not copied: its records are restored from it
//! first ([`super::retained::journal::recover`]), so every record it names is
//! whole in its own file, and the copy names them as this build writes them.
//! What a device's resume log recorded names the frames as they were
//! delivered then, so a resuming device is sent those frames again.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tilo_relay_protocol::WorkspaceId;
use tilo_relay_store::durable::JOURNAL;
use tilo_relay_store::frames::released_frame;

use super::{RETAINED_DIR, StoreError};

/// The directory the released directory of a workspace is kept as, once its
/// store is brought up.
const KEPT_SUFFIX: &str = ".kept";
/// The copy being made, before it is renamed into place.
const COPYING_SUFFIX: &str = ".bringing-up";
/// A store's queue of frames for devices that were away
/// ([`super::WorkspaceStore::enqueue_pending`]).
const PENDING_DIR: &str = "pending";

/// Where the directory the 0.1 releases kept for `workspace` under
/// `workspaces` stays once its store is brought up.
pub(crate) fn kept(workspaces: &Path, workspace: WorkspaceId) -> PathBuf {
    workspaces.join(format!("{}{KEPT_SUFFIX}", workspace.released_number()))
}

/// Bring the store the 0.1 releases kept for `workspace` under `workspaces`
/// up to `root`, its directory in this build, when it is there and `root`
/// is not. Whether there was one.
///
/// # Errors
///
/// A file of the released store cannot be read, or the copy cannot be
/// written; the released directory is unchanged then.
pub(super) fn bring_up(
    workspaces: &Path,
    root: &Path,
    workspace: WorkspaceId,
) -> Result<bool, StoreError> {
    let number = workspace.released_number().to_string();
    let released = workspaces.join(&number);
    if root.exists() || !released.is_dir() {
        return Ok(false);
    }
    super::retained::journal::recover(&released.join(RETAINED_DIR).join("v2"))?;
    let copying = workspaces.join(format!("{workspace}{COPYING_SUFFIX}"));
    match fs::remove_dir_all(&copying) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    copy(&released, &copying, workspace, Frames::None)?;
    fs::rename(&copying, root)?;
    fs::rename(&released, kept(workspaces, workspace))?;
    super::retained::sync_dir(workspaces)?;
    tracing::info!(
        %workspace,
        released = %released.display(),
        "the store a 0.1 release kept for this workspace is brought up"
    );
    Ok(true)
}

/// Which frames a directory holds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Frames {
    /// None: its files are copied as they are.
    None,
    /// Retained frames: each record is a frame.
    Retained,
    /// Pending frames, each named by its sequence and the digest of its
    /// bytes.
    Pending,
}

/// Copy the directory `from` to `to`, its frames written as this build
/// writes them, and make the copy durable.
fn copy(from: &Path, to: &Path, workspace: WorkspaceId, frames: Frames) -> Result<(), StoreError> {
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let source = entry.path();
        if entry.file_type()?.is_dir() {
            let inner = match (frames, name.to_str()) {
                (Frames::None, Some(RETAINED_DIR)) => Frames::Retained,
                (Frames::None, Some(PENDING_DIR)) => Frames::Pending,
                (frames, _) => frames,
            };
            copy(&source, &to.join(&name), workspace, inner)?;
            continue;
        }
        if name == JOURNAL && frames == Frames::Retained {
            continue;
        }
        let bytes = fs::read(&source)?;
        let is_frame = frames != Frames::None
            && Path::new(&name)
                .extension()
                .is_some_and(|extension| extension == "delta");
        let (target, bytes) = match is_frame.then(|| released_frame(&bytes, workspace)) {
            Some(Some(frame)) => (frame_name(to, &name, &frame, frames), frame),
            _ => (to.join(&name), bytes),
        };
        super::atomic_write(&target, &bytes)?;
    }
    super::retained::sync_dir(to)?;
    Ok(())
}

/// Where a frame brought up is written: a pending frame's name carries the
/// digest of its bytes.
fn frame_name(to: &Path, name: &std::ffi::OsStr, frame: &[u8], frames: Frames) -> PathBuf {
    let sequence = name
        .to_str()
        .and_then(|name| name.split_once('-'))
        .map(|(sequence, _)| sequence);
    match (frames, sequence) {
        (Frames::Pending, Some(sequence)) => {
            to.join(format!("{sequence}-{}.delta", blake3::hash(frame).to_hex()))
        }
        _ => to.join(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::WorkspaceStore;

    fn workspace() -> WorkspaceId {
        WorkspaceId::parse("abee87a1-91ca-46b7-9233-8ab47f9ec3e7").unwrap()
    }

    /// A frame as a 0.1 release retained it: its envelope naming the
    /// workspace by its released number.
    fn released(number: u64, checkpoint: bool, body: &[u8]) -> Vec<u8> {
        let payload = if checkpoint {
            crate::coedit::CoeditPayload::Checkpoint {
                channel: [9; 32],
                body: body.to_vec(),
            }
        } else {
            crate::coedit::CoeditPayload::RetainedDelta {
                channel: [9; 32],
                body: body.to_vec(),
            }
        };
        let current = crate::transport::encode(&crate::transport::Envelope::Delta {
            workspace_id: WorkspaceId::nil(),
            author_device_public_key: [1; 32],
            authored_at_ns: 5,
            delta: payload.encode().unwrap(),
            signature: [0; 64],
        })
        .unwrap();
        // The tag, the id (a length and sixteen bytes), the rest.
        let rest = &current[4 + 1 + 17..];
        let mut envelope = vec![current[4]];
        envelope.extend(postcard::to_stdvec(&number).unwrap());
        envelope.extend_from_slice(rest);
        let mut frame = u32::try_from(envelope.len())
            .unwrap()
            .to_le_bytes()
            .to_vec();
        frame.extend(envelope);
        frame
    }

    /// A frame a 0.1 release retained comes up as this build writes the same
    /// frame, and a frame of another workspace does not.
    #[test]
    fn a_released_frame_reads_as_this_build_writes_it() {
        let number = workspace().released_number();
        let frame = released(number, true, b"body");
        let brought = released_frame(&frame, workspace()).unwrap();
        let (envelope, rest) = tilo_relay_store::frames::framed_envelope(&brought).unwrap();
        assert!(rest.is_empty());
        let crate::transport::Envelope::Delta {
            workspace_id,
            author_device_public_key,
            authored_at_ns,
            ..
        } = envelope
        else {
            panic!("a delta")
        };
        assert_eq!(
            (workspace_id, author_device_public_key, authored_at_ns),
            (workspace(), [1; 32], 5)
        );
        assert_eq!(
            tilo_relay_store::frames::body_of(&brought, workspace()).unwrap(),
            b"body"
        );
        assert!(released_frame(&released(number + 1, true, b"body"), workspace()).is_none());
    }

    /// The store a 0.1 release kept opens with what it retained, under the
    /// workspace's id, the released directory kept beside it, and a second
    /// open brings nothing up again.
    #[test]
    fn a_released_store_opens_with_what_it_retained() {
        let tmp = tempfile::tempdir().unwrap();
        let number = workspace().released_number();
        let released_root = tmp.path().join("workspaces").join(number.to_string());
        let lineage = released_root
            .join(RETAINED_DIR)
            .join("v2")
            .join("route")
            .join("0123456789abcdef0123456789abcdef");
        fs::create_dir_all(&lineage).unwrap();
        fs::write(
            lineage.join("checkpoint.delta"),
            released(number, true, b"one"),
        )
        .unwrap();
        fs::write(
            lineage.join("0000000000000001.delta"),
            released(number, false, b"two"),
        )
        .unwrap();
        fs::write(
            released_root
                .join(RETAINED_DIR)
                .join("v2")
                .join("route")
                .join("CURRENT"),
            "0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        fs::write(released_root.join("snapshot.automerge"), b"kept").unwrap();
        let pending = released_root.join(PENDING_DIR).join("peer");
        fs::create_dir_all(&pending).unwrap();
        let queued = released(number, false, b"queued");
        fs::write(
            pending.join(format!(
                "{:016x}-{}.delta",
                3,
                blake3::hash(&queued).to_hex()
            )),
            &queued,
        )
        .unwrap();

        let store = WorkspaceStore::open(tmp.path(), workspace(), false).unwrap();
        assert_eq!(
            store.root(),
            tmp.path().join("workspaces").join(workspace().to_string())
        );
        assert!(!released_root.exists());
        assert!(
            tmp.path()
                .join("workspaces")
                .join(format!("{number}{KEPT_SUFFIX}"))
                .is_dir()
        );
        assert_eq!(
            fs::read(store.root().join("snapshot.automerge")).unwrap(),
            b"kept"
        );
        let brought = store.root().join(RETAINED_DIR).join("v2").join("route");
        for (name, body) in [
            ("checkpoint.delta", b"one".as_slice()),
            ("0000000000000001.delta", b"two".as_slice()),
        ] {
            let frame =
                fs::read(brought.join("0123456789abcdef0123456789abcdef").join(name)).unwrap();
            assert_eq!(
                tilo_relay_store::frames::body_of(&frame, workspace()).unwrap(),
                body
            );
        }
        let queued: Vec<_> = fs::read_dir(store.root().join(PENDING_DIR).join("peer"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(queued.len(), 1);
        let frame = fs::read(&queued[0]).unwrap();
        assert_eq!(
            queued[0].file_name().unwrap().to_str().unwrap(),
            format!("{:016x}-{}.delta", 3, blake3::hash(&frame).to_hex())
        );
        assert_eq!(
            tilo_relay_store::frames::body_of(&frame, workspace()).unwrap(),
            b"queued"
        );

        drop(store);
        WorkspaceStore::open(tmp.path(), workspace(), false).unwrap();
        assert!(
            tmp.path()
                .join("workspaces")
                .join(format!("{number}{KEPT_SUFFIX}"))
                .is_dir()
        );
    }
}
