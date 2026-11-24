//! The workspaces another workspace service of this same device governed, moved into this one.
//!
//! A desktop once kept the workspaces it founded on a service of its own, beside the device's.
//! Once the device's service keeps its person's workspaces, the desktop hands over, once, what
//! that service kept ([`moving_out`]), reading its files and never writing them: each
//! workspace's own files, the sealed writes and channel member lists its store kept, and the key
//! that service wrote under. This service takes them in ([`Workspaces::move_in`]): the writes
//! into its own store as the move of an older host's history does
//! ([`crate::history_move`]), the workspace as it was kept, and then, writing one last time as
//! the service that governed it, it admits this service and names it among the devices keeping
//! the workspace in that service's place.

use std::path::Path;
use std::sync::Arc;

use neighbour_wire::NeighbourMsg;
use tilo_peer_state::CopyRole;
use tilo_peer_state::store::{Appetite, Budget, Frame, Store, advance};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;
use tokio::sync::Mutex as AsyncMutex;

use super::delete::DELETED;
use super::{GovernedWorkspace, RELEASED, Workspaces};

/// What another workspace service of this device kept, as it moves into this one.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct MovingService {
    /// The key that service wrote under.
    pub key: [u8; 32],
    /// The certificate the device issued that service.
    pub cert: DeviceCert,
    /// Each workspace it governed.
    pub workspaces: Vec<MovingWorkspace>,
}

impl MovingService {
    /// As it is handed over: postcard, in hex.
    ///
    /// # Errors
    ///
    /// It does not encode.
    pub fn to_hex(&self) -> Result<String, String> {
        postcard::to_stdvec(self)
            .map(hex::encode)
            .map_err(|error| error.to_string())
    }

    /// What [`Self::to_hex`] handed over.
    ///
    /// # Errors
    ///
    /// `text` is not one.
    pub fn from_hex(text: &str) -> Result<Self, String> {
        super::decode_hex(text)
    }
}

/// One workspace a service governed, as it moves.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct MovingWorkspace {
    pub id: WorkspaceId,
    /// Its record, the files beside it and the documents it keeps, by path in the workspaces'
    /// directory, `/` between its parts.
    pub files: Vec<(String, Vec<u8>)>,
    /// Every sealed, signed write its store kept.
    pub frames: Vec<NeighbourMsg>,
    /// The member list of each of its channels its store kept, by author.
    pub channels: Vec<([u8; 32], NeighbourMsg)>,
}

/// What [`Workspaces::move_in`] did.
#[derive(Debug, Default)]
pub struct MovedIn {
    /// The workspaces this service keeps now.
    pub moved: Vec<WorkspaceId>,
    /// Those it did not take, and why: they stay where they were.
    pub refused: Vec<(WorkspaceId, String)>,
}

/// What the workspace service whose state is under `root` kept, its workspaces written as the
/// device its `cert` certifies and `key` signs for, read without writing anything there: `None`
/// when it kept nothing, or it was moved out already.
///
/// # Errors
///
/// A workspace's files or its store could not be read.
pub fn moving_out(
    root: &Path,
    cert: &DeviceCert,
    key: &ed25519_dalek::SigningKey,
) -> Result<Option<MovingService>, String> {
    if !tilo_paths::desktop_service::kept(root) {
        return Ok(None);
    }
    let read = |what: &Path, error: &dyn std::fmt::Display| format!("{}: {error}", what.display());
    let dir = root.join("workspaces");
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|error| read(&dir, &error))? {
        let path = entry.map_err(|error| read(&dir, &error))?.path();
        let Some(id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".workspace"))
            .and_then(WorkspaceId::parse)
        else {
            continue;
        };
        // A workspace taken part in is its host's to keep, and one let go or deleted is no
        // longer this service's.
        let beside = |what: &str| path.with_extension(what).exists();
        if !beside(super::member::MEMBER) && !beside(RELEASED) && !beside(DELETED) {
            ids.push(id);
        }
    }
    ids.sort();
    let store_root = root.join("store");
    let store = if store_root.exists() {
        Some(
            Store::open(store_root.clone(), Appetite::StoreAll)
                .map_err(|error| read(&store_root, &error))?,
        )
    } else {
        None
    };
    let mut workspaces = Vec::with_capacity(ids.len());
    for id in ids {
        let mut moving = MovingWorkspace {
            id,
            files: files_of(&dir, &id.to_string())?,
            frames: Vec::new(),
            channels: Vec::new(),
        };
        if let Some(store) = &store {
            let mut cursors = Vec::new();
            loop {
                let replay = store
                    .replay(id, &cursors, Budget::default())
                    .map_err(|error| format!("{id}: {error}"))?;
                advance(&mut cursors, &replay.frames);
                moving.frames.extend(replay.frames);
                if !replay.more {
                    break;
                }
            }
            moving.channels = store.channels(id);
        }
        workspaces.push(moving);
    }
    Ok(Some(MovingService {
        key: key.to_bytes(),
        cert: cert.clone(),
        workspaces,
    }))
}

/// Say the service whose state is under `root` was moved out, so it is not moved again.
///
/// # Errors
///
/// The mark could not be written.
pub fn moved_out(root: &Path) -> Result<(), String> {
    let mark = root.join(tilo_paths::desktop_service::MOVED_OUT);
    tilo_files::native::atomic::write(&mark, b"")
        .map_err(|error| format!("{}: {error}", mark.display()))
}

/// The files of `workspace` in `dir`: its record, those beside it, and its own directory of
/// the documents it keeps, every directory read through.
fn files_of(dir: &Path, workspace: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
    let read = |what: &Path, error: &dyn std::fmt::Display| format!("{}: {error}", what.display());
    let mut files = Vec::new();
    let mut walking = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|error| read(dir, &error))? {
        let path = entry.map_err(|error| read(dir, &error))?.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned);
        if let Some(name) = name.filter(|name| ours(name, workspace)) {
            walking.push((path, name));
        }
    }
    while let Some((path, name)) = walking.pop() {
        if path.is_dir() {
            for inner in std::fs::read_dir(&path).map_err(|error| read(&path, &error))? {
                let inner = inner.map_err(|error| read(&path, &error))?.path();
                if let Some(file) = inner.file_name().and_then(|file| file.to_str()) {
                    let named = format!("{name}/{file}");
                    walking.push((inner, named));
                }
            }
        } else {
            let bytes = std::fs::read(&path).map_err(|error| read(&path, &error))?;
            files.push((name, bytes));
        }
    }
    files.sort();
    Ok(files)
}

/// Whether `name`, in a service's workspaces' directory, is one of `workspace`'s: its own
/// directory, or a file or directory beside its record.
fn ours(name: &str, workspace: &str) -> bool {
    name == workspace
        || name
            .split_once('.')
            .is_some_and(|(stem, _)| stem == workspace)
}

impl Workspaces {
    /// Take in what `moving`, this device's workspace service from before, kept: each workspace
    /// not kept here already, its writes and channel member lists into this service's store, its
    /// files beside this service's, served from here, with this service named among the devices
    /// keeping it in the other's place, written as the one device both are.
    ///
    /// # Errors
    ///
    /// `moving` is not this device's. A workspace that does not move is said in
    /// [`MovedIn::refused`] and the rest move.
    pub async fn move_in(&self, moving: MovingService) -> Result<MovedIn, String> {
        let key = ed25519_dalek::SigningKey::from_bytes(&moving.key);
        let own = self.device_cert().clone();
        if key.verifying_key().to_bytes() != moving.cert.device_public_key
            || moving.cert.device_public_key != own.device_public_key
        {
            return Err(
                "only this device's own workspace service moves its workspaces here".into(),
            );
        }
        let mut moved = MovedIn::default();
        for workspace in moving.workspaces {
            let id = workspace.id;
            match self.move_one(&key, &moving.cert, &own, workspace).await {
                Ok(true) => moved.moved.push(id),
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(workspace = %id, %error, "a workspace another service of this device governed did not move here");
                    moved.refused.push((id, error));
                }
            }
        }
        Ok(moved)
    }

    /// Move `workspace` in, as [`Self::move_in`] does: whether it moved, not when it was kept
    /// here already.
    async fn move_one(
        &self,
        key: &ed25519_dalek::SigningKey,
        from: &DeviceCert,
        own: &DeviceCert,
        workspace: MovingWorkspace,
    ) -> Result<bool, String> {
        let id = workspace.id;
        let name = id.to_string();
        if self.inner.workspaces.lock().await.contains_key(&name) {
            return Ok(false);
        }
        for message in &workspace.frames {
            if let Ok(frame) = Frame::of(message) {
                self.inner
                    .store
                    .retain(id, &frame)
                    .map_err(|error| error.to_string())?;
            }
        }
        for (author, message) in &workspace.channels {
            self.inner
                .store
                .keep_channel(id, *author, message)
                .map_err(|error| error.to_string())?;
        }
        for (file, bytes) in &workspace.files {
            // Only the workspace's own names: what is handed names nothing outside its files.
            let parts: Vec<&str> = file.split('/').collect();
            let plain = |part: &&str| !part.is_empty() && *part != "." && *part != "..";
            if !parts.first().is_some_and(|first| ours(first, &name)) || !parts.iter().all(plain) {
                return Err(format!("a workspace's file is named {file:?}"));
            }
            let path = self.inner.dir.join(file);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            tilo_files::native::atomic::write(&path, bytes)
                .map_err(|error| format!("{}: {error}", path.display()))?;
        }
        let path = self.inner.dir.join(format!("{name}.workspace"));
        let governed = GovernedWorkspace::open(path, self.device_keys().as_ref())?;
        if governed.workspace_id() != id {
            return Err("the workspace's record names another workspace".into());
        }
        self.serve_direct(&governed)?;
        let reading = governed.reading();
        let state = Arc::new(AsyncMutex::new(governed));
        self.inner.readings.hold(&name, &state, reading);
        self.inner
            .workspaces
            .lock()
            .await
            .insert(name.clone(), Arc::clone(&state));
        let mut governed = state.lock().await;
        let hint = self
            .transport_hint_as(&governed, CopyRole::KeyHolder)
            .map_err(|error| error.to_string())?;
        let now = tilo_clock::wall();
        governed.change(
            key,
            now,
            |_, candidate| {
                let admitted = candidate
                    .governance_mut()
                    .admit_member_device(own, key, now)?;
                let named = candidate
                    .governance_mut()
                    .author_keeping_device(&hint, key, now)?;
                let gone = from.device_public_key != own.device_public_key
                    && candidate.governance_mut().author_keeping_device_removed(
                        &from.device_public_key,
                        key,
                        now,
                    )?;
                Ok(((), admitted || named || gone))
            },
            |record, _| Ok(record.collaboration.clone()),
        )?;
        drop(governed);
        // Served from here as a workspace this service kept from its last run is.
        self.serve_kept(std::slice::from_ref(&state)).await?;
        let _ = self.inner.changes.send(name);
        tracing::info!(workspace = %id, "a workspace another service of this device governed is kept here");
        Ok(true)
    }
}
