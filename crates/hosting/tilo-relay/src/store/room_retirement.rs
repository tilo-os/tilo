//! Crash-recoverable, channel-narrow retirement, run before a workspace goes live.
//! A durable resource cut precedes deletion so recovery never reads retired manifests.
//! Startup finishes an interrupted scrub before opening any replay/fetch surface.
use super::{StoreError, WorkspaceStore, atomic_write, fsync};
use crate::{coedit::CoeditPayload, transport::Envelope};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use tilo_peer_state::{SanitizedRoomBaseline, SanitizedRoomBaselineAnchor};

const INTENT: &str = "room-retirement-pending-v1.json";
const CURRENT: &str = "room-baseline-v1.json";
const CUT: &str = "room-retirement-resources-v1.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct ResourceCut {
    replacement: Vec<u8>,
    frames: Vec<Vec<u8>>,
}

fn invalid(message: &str) -> StoreError {
    io::Error::new(io::ErrorKind::InvalidData, message).into()
}

fn files(path: &Path, output: &mut Vec<PathBuf>) -> Result<(), StoreError> {
    if !path.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(invalid("symlink in room retirement sources"));
        }
        if kind.is_dir() {
            files(&entry.path(), output)?;
        } else if kind.is_file() {
            output.push(entry.path());
        }
    }
    Ok(())
}
fn remove_dir(
    path: &Path,
    after_delete: &mut impl FnMut() -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let mut entries = entries.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(invalid("symlink in room retirement sources"));
        }
        if kind.is_dir() {
            remove_dir(&entry.path(), after_delete)?;
        } else {
            fs::remove_file(entry.path())?;
            after_delete()?;
        }
    }
    fs::remove_dir(path)?;
    after_delete()
}

impl WorkspaceStore {
    /// The replacement pin installed by an authenticated retirement transaction,
    /// never a pin derived by replay from an untrusted retained frame.
    pub(crate) fn sanitized_room_baseline(
        &self,
    ) -> Result<Option<SanitizedRoomBaseline>, StoreError> {
        let path = self.root.join(CURRENT);
        if !path.exists() {
            return Ok(None);
        }
        serde_json::from_slice(&fs::read(path)?)
            .map(Some)
            .map_err(|e| StoreError::Json(e.to_string()))
    }
    pub(crate) fn sanitized_room_anchor(
        &self,
    ) -> Result<Option<SanitizedRoomBaselineAnchor>, StoreError> {
        Ok(self
            .sanitized_room_baseline()?
            .map(|baseline| baseline.anchor()))
    }
    pub(crate) fn room_retirement_ready(&self) -> Result<(), StoreError> {
        if self.root.join(INTENT).exists() {
            return Err(invalid(
                "room retirement is unfinished; replay is unavailable",
            ));
        }
        Ok(())
    }
    /// A baseline is a destructive migration, never an ordinary checkpoint.
    /// Intake may replay an independently committed pin but must refuse a new
    /// generation until an authenticated retirement has completed.
    /// In particular a publisher's signature cannot serve as its own host pin.
    pub(crate) fn check_room_baseline_intake(
        &self,
        author: [u8; 32],
        delta: &[u8],
    ) -> Result<(), StoreError> {
        let Some(payload) = CoeditPayload::decode(delta) else {
            return Ok(());
        };
        let (channel, body) = match payload {
            CoeditPayload::Checkpoint { channel, body }
            | CoeditPayload::RetainedDelta { channel, body } => (channel, body),
        };
        if channel != *blake3::hash(b"shell-tree").as_bytes() {
            return Ok(());
        }
        let Ok((neighbour_wire::NeighbourMsg::SanitizedRoomBaseline { room, baseline }, tail)) =
            postcard::take_from_bytes::<neighbour_wire::NeighbourMsg>(&body)
        else {
            return Ok(());
        };
        if !tail.is_empty() {
            return Err(invalid("trailing baseline envelope bytes"));
        }
        let (incoming, tail) = postcard::take_from_bytes::<SanitizedRoomBaseline>(&baseline)
            .map_err(|e| invalid(&e.to_string()))?;
        if !tail.is_empty() {
            return Err(invalid("trailing baseline bytes"));
        }
        let current = self.sanitized_room_baseline()?.ok_or_else(|| {
            invalid("new room baseline requires inactive authenticated history retirement")
        })?;
        if author != current.governor
            || self
                .shell_binding()
                .is_some_and(|pin| pin.governor().device_public_key != author)
        {
            return Err(invalid("baseline intake does not match pinned governor"));
        }
        let anchor = self
            .sanitized_room_anchor()?
            .ok_or_else(|| invalid("missing room anchor"))?;
        incoming
            .verify_anchored_document(&room, author, &anchor)
            .map_err(|e| invalid(&e))?;
        Ok(())
    }

    /// Device intake may introduce a generation only from the exact selected
    /// predecessor and the authenticated, pinned governor. Verification alone
    /// does not install a replay anchor; the ordered owner must retire it first.
    pub(crate) fn authenticated_room_retirement(
        &self,
        publisher: [u8; 32],
        author: [u8; 32],
        delta: &[u8],
    ) -> Result<Option<SanitizedRoomBaseline>, StoreError> {
        if self.check_room_baseline_intake(author, delta).is_ok() {
            return Ok(None);
        }
        if publisher != author {
            return Err(invalid("retirement publisher differs from envelope author"));
        }
        let Some(CoeditPayload::Checkpoint { channel, body }) = CoeditPayload::decode(delta) else {
            return Err(invalid("retirement requires a room checkpoint"));
        };
        if channel != *blake3::hash(b"shell-tree").as_bytes() {
            return Err(invalid("retirement requires the room channel"));
        }
        let binding = self
            .shell_binding()
            .ok_or_else(|| invalid("retirement requires a pinned governor"))?;
        binding.verify_room_retirement(self, publisher, &body)?;
        let (neighbour_wire::NeighbourMsg::SanitizedRoomBaseline { room, baseline }, tail) =
            postcard::take_from_bytes::<neighbour_wire::NeighbourMsg>(&body)
                .map_err(|e| invalid(&e.to_string()))?
        else {
            return Err(invalid("retirement requires a sanitized baseline"));
        };
        let (baseline, rest) = postcard::take_from_bytes::<SanitizedRoomBaseline>(&baseline)
            .map_err(|e| invalid(&e.to_string()))?;
        if !tail.is_empty()
            || !rest.is_empty()
            || crate::room_workspace_id(&room).map_err(|e| invalid(&e.to_string()))?
                != self.workspace_id
        {
            return Err(invalid("retirement belongs to another workspace"));
        }
        Ok(Some(baseline))
    }

    /// Refuse retired roots before they can re-enter durable retention. The
    /// hash-only admitted dependency set is reset by each retirement; resources
    /// do not consult or mutate this room-specific fence.
    pub(super) fn fence_room_input(&self, raw: &[u8]) -> Result<(), StoreError> {
        let Some((author, payload)) = super::retained::payload(raw, self.workspace_id) else {
            return Err(invalid("invalid retained room envelope"));
        };
        self.check_room_baseline_intake(
            author,
            &payload.encode().map_err(|e| invalid(&e.to_string()))?,
        )?;
        let Some(current) = self.sanitized_room_baseline()? else {
            return Ok(());
        };
        let (channel, body) = match payload {
            CoeditPayload::Checkpoint { channel, body }
            | CoeditPayload::RetainedDelta { channel, body } => (channel, body),
        };
        if channel != *blake3::hash(b"shell-tree").as_bytes() {
            return Ok(());
        }
        if author != current.governor {
            return Err(invalid("room history is governor-authored"));
        }
        let (message, tail) = postcard::take_from_bytes::<neighbour_wire::NeighbourMsg>(&body)
            .map_err(|e| invalid(&e.to_string()))?;
        if !tail.is_empty() {
            return Err(invalid("trailing room history bytes"));
        }
        let changes = match message {
            neighbour_wire::NeighbourMsg::SanitizedRoomBaseline { room, baseline } => {
                let (baseline, tail) =
                    postcard::take_from_bytes::<SanitizedRoomBaseline>(&baseline)
                        .map_err(|e| invalid(&e.to_string()))?;
                if !tail.is_empty() || room != current.room {
                    return Err(invalid("foreign baseline"));
                }
                baseline
                    .verify_anchored_document(&room, author, &current.anchor())
                    .map_err(|e| invalid(&e))?;
                return Ok(());
            }
            neighbour_wire::NeighbourMsg::SyncResp { room, changes } if room == current.room => {
                changes
            }
            _ => return Err(invalid("unknown room history family")),
        };
        let ledger = self.root.join("room-chain-hashes-v1.json");
        let mut known: std::collections::BTreeSet<String> =
            serde_json::from_slice(&fs::read(&ledger)?)
                .map_err(|e| StoreError::Json(e.to_string()))?;
        let mut incoming = Vec::new();
        for bytes in changes {
            let mut doc = automerge::AutoCommit::new();
            tilo_automerge_isolate::apply(
                &mut doc,
                &bytes,
                tilo_automerge_isolate::Dependencies::RetainOrphans,
            )
            .map_err(|e| invalid(&e.to_string()))?;
            // Governed publication emits individual raw changes. Require the
            // exact representation: a loader skipping an appended document or
            // suffix must never smuggle old payloads through this hash fence.
            let change = automerge::Change::from_bytes(bytes.clone())
                .map_err(|e| invalid(&e.to_string()))?;
            if change.raw_bytes() != bytes.as_slice() {
                return Err(invalid("room input is not one canonical raw change"));
            }
            incoming.push(change);
        }
        while !incoming.is_empty() {
            let before = incoming.len();
            incoming.retain(|change| {
                let hash = change.hash().to_string();
                if known.contains(&hash) {
                    return false;
                }
                if !change.deps().is_empty()
                    && change
                        .deps()
                        .iter()
                        .all(|dep| known.contains(&dep.to_string()))
                {
                    known.insert(hash);
                    return false;
                }
                true
            });
            if incoming.len() == before {
                return Err(invalid("room input resurrects retired history"));
            }
        }
        atomic_write(
            &ledger,
            &serde_json::to_vec(&known).map_err(|e| StoreError::Json(e.to_string()))?,
        )?;
        Ok(())
    }
    /// Caller must exclusively own workspace persistence (startup or its ordered
    /// owner) and authenticate this baseline against the admitted predecessor
    /// before invoking disk mutation. Captured old replays fail closed if their
    /// retired files disappear; they cannot reconstruct an anchor from input.
    pub(crate) fn retire_room_history(
        &self,
        baseline: &SanitizedRoomBaseline,
    ) -> Result<(), StoreError> {
        if self.root.join(INTENT).exists() {
            self.finish_room_retirement()?;
        }
        if let Some(current) = self.sanitized_room_baseline()? {
            if current.anchor() == baseline.anchor() {
                return Ok(());
            }
            if baseline.generation
                != current
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| invalid("baseline generation exhausted"))?
            {
                return Err(invalid("retirement generation mismatch"));
            }
        }
        let bytes = serde_json::to_vec(baseline).map_err(|e| StoreError::Json(e.to_string()))?;
        // A completed transaction may have crashed before removing its cut.
        // Clear that stale cut before making a new intent durable.
        let cut = self.root.join(CUT);
        if cut.exists() {
            fs::remove_file(cut)?;
            fsync(&fs::File::open(&self.root)?)?;
        }
        atomic_write(&self.root.join(INTENT), &bytes)?;
        self.finish_room_retirement()
    }
    /// Called at open, before any store reader or persistence owner exists.
    pub(super) fn finish_room_retirement(&self) -> Result<(), StoreError> {
        self.finish_room_retirement_with(|| Ok(()))
    }

    /// The hook runs after each destructive operation, allowing tests to stop
    /// at exactly the crash boundaries that startup must recover from.
    #[expect(
        clippy::too_many_lines,
        reason = "one startup recovery transaction orders preservation, scrub and durable publication"
    )]
    pub(crate) fn finish_room_retirement_with(
        &self,
        mut after_delete: impl FnMut() -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let intent = self.root.join(INTENT);
        if !intent.exists() {
            return Ok(());
        }
        let encoded = fs::read(&intent)?;
        let baseline: SanitizedRoomBaseline =
            serde_json::from_slice(&encoded).map_err(|e| StoreError::Json(e.to_string()))?;
        let channel = *blake3::hash(b"shell-tree").as_bytes();
        let mut sources = Vec::new();
        for directory in [
            "retained",
            "pending",
            super::shell_selection::DIRECTORY,
            super::replay::WORK_DIR,
        ] {
            files(&self.root.join(directory), &mut sources)?;
        }
        // Selected bases and captured replay pins may be the last durable links
        // to independent resource frames. Re-home them before removing manifests.
        // Disable selection only on this private maintenance handle; no live IO
        // may coexist with this startup transaction.
        let mut plain = self.clone();
        plain.shell_binding = None;
        let cut_path = self.root.join(CUT);
        let recovering = cut_path.exists();
        let mut cut = if recovering {
            let cut: ResourceCut = serde_json::from_slice(&fs::read(&cut_path)?)
                .map_err(|e| StoreError::Json(e.to_string()))?;
            if cut.replacement != encoded {
                return Err(invalid("resource cut belongs to a different retirement"));
            }
            cut
        } else {
            let mut frames = Vec::new();
            // Only this pre-deletion phase may follow selected manifests.
            // Retries use the durable cut even if those manifests now dangle.
            for raw in plain.load_shared_for_retirement()? {
                if let Some((_, payload)) = super::retained::payload(&raw, self.workspace_id) {
                    let route = match payload {
                        CoeditPayload::Checkpoint { channel, .. }
                        | CoeditPayload::RetainedDelta { channel, .. } => channel,
                    };
                    if route != channel {
                        frames.push(raw);
                    }
                }
            }
            ResourceCut {
                replacement: encoded.clone(),
                frames,
            }
        };
        sources.sort();
        let mut retired = Vec::new();
        for path in &sources {
            let raw = match fs::read(path) {
                Ok(raw) => raw,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            let Some((_, payload)) = super::retained::payload(&raw, self.workspace_id) else {
                // Unknown framed history cannot be proved independent of the
                // retired route. Metadata manifests are not payload frames.
                let framed = raw
                    .get(..4)
                    .and_then(|prefix| <[u8; 4]>::try_from(prefix).ok())
                    .is_some_and(|prefix| {
                        u32::from_le_bytes(prefix) as usize == raw.len().saturating_sub(4)
                    });
                if path.extension().is_some_and(|ext| ext == "delta") || framed {
                    return Err(invalid(
                        "unclassifiable retained payload blocks room retirement",
                    ));
                }
                continue;
            };
            let route = match payload {
                CoeditPayload::Checkpoint { channel, .. }
                | CoeditPayload::RetainedDelta { channel, .. } => channel,
            };
            if route == channel {
                retired.push(path.clone());
            } else if !recovering
                && (path.starts_with(self.root.join(super::shell_selection::DIRECTORY))
                    || path.starts_with(self.root.join(super::replay::WORK_DIR)))
            {
                cut.frames.push(raw);
            }
        }
        if !recovering {
            atomic_write(
                &cut_path,
                &serde_json::to_vec(&cut).map_err(|e| StoreError::Json(e.to_string()))?,
            )?;
        }
        // Re-home the durable ordered cut before deleting any predecessor.
        // Exact-frame deduplication makes interrupted preservation retryable.
        for raw in &cut.frames {
            match super::retained::payload(raw, self.workspace_id) {
                Some((_, CoeditPayload::Checkpoint { channel: route, .. })) if route != channel => {
                    plain.retain_checkpoint(&route, raw)?;
                }
                Some((_, CoeditPayload::RetainedDelta { channel: route, .. }))
                    if route != channel =>
                {
                    plain.retain_delta(&route, 0, raw)?;
                }
                _ => return Err(invalid("invalid preserved resource frame")),
            }
        }
        let mut parents = std::collections::BTreeSet::new();
        for path in retired {
            if let Some(parent) = path.parent() {
                parents.insert(parent.to_owned());
            }
            fs::remove_file(path)?;
            after_delete()?;
        }
        // Remove all generations, including unselected/corrupt predecessor
        // copies on the room route. Author identity is deliberately irrelevant.
        let v2 = self.root.join("retained/v2");
        if v2.exists() {
            let suffix = format!("-{}", hex::encode(channel));
            for entry in fs::read_dir(&v2)? {
                let entry = entry?;
                if entry.file_name().to_string_lossy().ends_with(&suffix) {
                    remove_dir(&entry.path(), &mut after_delete)?;
                }
            }
        }
        remove_dir(
            &self.root.join(super::shell_selection::DIRECTORY),
            &mut after_delete,
        )?;
        remove_dir(&self.root.join(super::replay::WORK_DIR), &mut after_delete)?;
        // Resume files contain only digests; old skip tokens must not hide the
        // replacement. Dropping delivery cursors does not drop resource payloads.
        remove_dir(&self.root.join("resume"), &mut after_delete)?;
        let generation = self.root.join("generation");
        if generation.exists() {
            fs::remove_file(generation)?;
            after_delete()?;
        }
        self.tail_cache.invalidate();
        if self.pinned {
            atomic_write(&self.root.join(super::SNAPSHOT_FILE), &baseline.document)?;
        }
        let message = neighbour_wire::NeighbourMsg::SanitizedRoomBaseline {
            room: baseline.room.clone(),
            baseline: postcard::to_stdvec(&baseline).map_err(|e| invalid(&e.to_string()))?,
        };
        let payload = CoeditPayload::Checkpoint {
            channel,
            body: postcard::to_stdvec(&message).map_err(|e| invalid(&e.to_string()))?,
        }
        .encode()
        .map_err(|e| invalid(&e.to_string()))?;
        let frame = crate::frame::encode(&Envelope::Delta {
            workspace_id: self.workspace_id,
            author_device_public_key: baseline.governor,
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: payload,
            signature: [0; 64],
        })
        .map_err(|e| invalid(&e.to_string()))?;
        let mut genesis = tilo_automerge_isolate::load(&baseline.document)
            .map_err(|e| invalid(&e.to_string()))?;
        let hashes = genesis
            .get_changes(&[])
            .into_iter()
            .map(|change| change.hash().to_string())
            .collect::<std::collections::BTreeSet<_>>();
        atomic_write(
            &self.root.join("room-chain-hashes-v1.json"),
            &serde_json::to_vec(&hashes).map_err(|e| StoreError::Json(e.to_string()))?,
        )?;
        atomic_write(&self.root.join(CURRENT), &encoded)?;
        plain.retain_checkpoint(&channel, &frame)?;
        // Sync deletion directory entries before clearing the recovery intent.
        for parent in parents {
            if parent.exists() {
                fsync(&fs::File::open(parent)?)?;
            }
        }
        for directory in ["retained/v2", "retained", "pending", ""] {
            fsync(&fs::File::open(self.root.join(directory))?)?;
        }
        fs::remove_file(intent)?;
        fsync(&fs::File::open(&self.root)?)?;
        after_delete()?;
        fs::remove_file(cut_path)?;
        fsync(&fs::File::open(&self.root)?)?;
        after_delete()?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn retirement_selected_fixture(&self, frames: &[Vec<u8>]) {
        let relative =
            Path::new(super::shell_selection::DIRECTORY).join("00000000000000000000000000000001");
        let selected = self.root.join(&relative);
        fs::create_dir_all(&selected).unwrap();
        let mut manifest = fs::File::create(selected.join("basis.manifest")).unwrap();
        for (index, raw) in frames.iter().enumerate() {
            let source = relative.join(format!("{index}.delta"));
            fs::write(self.root.join(&source), raw).unwrap();
            super::bounded::write_meta(
                &mut manifest,
                &super::bounded::Meta {
                    key: vec!["room".into()],
                    source,
                    pin: index as u64,
                    len: raw.len() as u64,
                    pending: false,
                },
            )
            .unwrap();
        }
        fs::write(
            self.root
                .join(super::shell_selection::DIRECTORY)
                .join("CURRENT"),
            b"00000000000000000000000000000001",
        )
        .unwrap();
    }
}
