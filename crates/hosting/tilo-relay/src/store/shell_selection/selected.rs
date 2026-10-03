//! Validation of the immutable selected basis and recoverable exact-input GC.
use super::{
    Basis, CoeditPayload, CoverageDecision, DIRECTORY, File, PathBuf, RetentionAuthority, Seek,
    Step, StoreError, WorkspaceStore, frame, fs, invalid, read_meta, replay, retained, route_key,
    verify_room, verify_snapshot,
};
use crate::store::bounded::Invariant;

/// Reads one immutable selected reference per quantum, including prerequisites.
/// Rebinding local authority is mandatory after restart; the disk pin is not trust.
pub(crate) struct SelectedRead {
    root: PathBuf,
    authority: RetentionAuthority,
    workspace: u64,
    input: Option<File>,
    pub(super) basis: Basis,
    generation: Option<PathBuf>,
    gc: Option<SelectedGc>,
}
impl SelectedRead {
    pub(crate) fn new(store: &WorkspaceStore) -> Result<Option<Self>, StoreError> {
        let Some(authority) = store.shell_authority().cloned() else {
            return Ok(None);
        };
        let selected = retained::current(&store.root.join(DIRECTORY))?;
        if let Some(path) = &selected {
            retained::sync_file_once(&store.root.join(DIRECTORY).join("CURRENT"))?;
            retained::sync_dir_once(&store.root.join(DIRECTORY))?;
            retained::sync_file_once(&path.join("basis.manifest"))?;
            retained::sync_dir_once(path)?;
        }
        Ok(Some(Self {
            root: store.root.clone(),
            authority,
            workspace: store.workspace_id,
            input: selected
                .as_ref()
                .map(|p| File::open(p.join("basis.manifest")))
                .transpose()?,
            basis: Basis::default(),
            generation: selected,
            gc: None,
        }))
    }
    pub(crate) fn step(&mut self) -> Result<Step<()>, StoreError> {
        if let Some(gc) = &mut self.gc {
            if let Step::Done(()) = gc.step()? {
                self.gc = None;
                return Ok(Step::Done(()));
            }
            return Ok(Step::More);
        }
        let Some(input) = &mut self.input else {
            return Ok(Step::Done(()));
        };
        let Some(meta) = read_meta(input)? else {
            self.input = None;
            if self.basis.room.is_some() && !self.basis.complete() {
                return Err(invalid("incomplete selected basis"));
            }
            if let Some(generation) = self.generation.take() {
                self.gc = Some(SelectedGc::new(self.root.clone(), generation)?);
                return Ok(Step::More);
            }
            return Ok(Step::Done(()));
        };
        if meta.key.first().is_some_and(|v| v == "extra") {
            return Ok(Step::More);
        }
        let value = frame(&self.root.join(&meta.source), self.workspace)?
            .ok_or_else(|| invalid("invalid selected shell frame"))?;
        if meta.key.first().is_some_and(|v| v == "room") {
            let room = verify_room(
                &self.authority,
                value.source,
                value.channel,
                &value.body,
                value.kind,
                self.basis.room.as_ref(),
            )
            .map_err(|_| invalid("invalid selected room prerequisite"))?;
            self.basis.set_room(room)?;
        } else if meta.key.first().is_some_and(|v| v == "app") {
            let snapshot = verify_snapshot(
                self.basis
                    .room
                    .as_ref()
                    .ok_or_else(|| invalid("selected application without room"))?,
                value.source,
                value.channel,
                &value.body,
            )
            .map_err(|_| invalid("invalid selected application pair"))?;
            let route = self
                .basis
                .routes
                .get_mut(&route_key(
                    &snapshot.coverage().app,
                    &snapshot.coverage().record,
                ))
                .ok_or_else(|| invalid("selected route missing"))?;
            route.file = Some(meta);
            route.verified_input = Some(value.identity());
        }
        Ok(Step::More)
    }
    pub(crate) fn obsolete(&self, bytes: &[u8]) -> Result<bool, StoreError> {
        if !self.basis.complete() {
            return Ok(false);
        }
        let Some((source, payload)) = retained::payload(bytes, self.workspace) else {
            return Ok(false);
        };
        let (channel, body) = match payload {
            CoeditPayload::Checkpoint { channel, body }
            | CoeditPayload::RetainedDelta { channel, body } => (channel, body),
        };
        let room = self.basis.room.as_ref().invariant("room missing")?;
        if room.covers_frame(source, channel, &body) == CoverageDecision::Covered {
            return Ok(true);
        }
        for route in self.basis.routes.values() {
            if channel
                != *blake3::hash(
                    format!("app\0{}\0{}", route.coverage.app, route.coverage.record).as_bytes(),
                )
                .as_bytes()
            {
                continue;
            }
            let file = frame(
                &self
                    .root
                    .join(&route.file.as_ref().invariant("file missing")?.source),
                self.workspace,
            )?
            .ok_or_else(|| invalid("missing selected snapshot"))?;
            let snapshot = verify_snapshot(room, file.source, file.channel, &file.body)
                .map_err(|_| invalid("invalid selected snapshot"))?;
            return Ok(snapshot.covers_incoming_snapshot(source, channel, &body)
                || snapshot.classify_tail(source, channel, &body).coverage
                    == CoverageDecision::Covered);
        }
        Ok(false)
    }
}

/// The durable plan pins each exactly proven retirement input before CURRENT.
/// Restart may finish it after the original historical proof has disappeared.
/// A reused path is never a target: require the captured inode and exact bytes.
pub(super) struct SelectedGc {
    root: PathBuf,
    generation: PathBuf,
    input: File,
    selection_synced: bool,
}
impl SelectedGc {
    pub(super) fn new(root: PathBuf, generation: PathBuf) -> Result<Self, StoreError> {
        Ok(Self {
            input: File::open(generation.join("gc.manifest"))?,
            root,
            generation,
            selection_synced: false,
        })
    }
    pub(super) fn step(&mut self) -> Result<Step<()>, StoreError> {
        if retained::current(&self.root.join(DIRECTORY))?.as_ref() != Some(&self.generation) {
            return Err(invalid("retirement selection changed"));
        }
        // This cursor holds one immutable plan under the serialized owner.
        // Confirm its selection once, not once per historical record. A failed
        // barrier leaves the flag clear, so retries and newly opened cursors
        // must still establish durability before retiring anything.
        if !self.selection_synced {
            retained::sync_file_once(&self.root.join(DIRECTORY).join("CURRENT"))?;
            retained::sync_dir_once(&self.root.join(DIRECTORY))?;
            self.selection_synced = true;
        }
        let offset = self.input.stream_position()?;
        let Some(meta) = read_meta(&mut self.input)? else {
            return Ok(Step::Done(()));
        };
        let result = (|| {
            if !meta.source.starts_with("retained/v2") {
                return Err(invalid("invalid shell retirement target"));
            }
            let pin = self.generation.join(format!("gc-{:016x}.delta", meta.pin));
            let captured = match fs::symlink_metadata(&pin) {
                Ok(m) if m.is_file() => m,
                Ok(_) => return Err(invalid("invalid retirement pin")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(e.into()),
            };
            let source = self.root.join(&meta.source);
            match fs::symlink_metadata(&source) {
                Ok(current) => {
                    #[cfg(unix)]
                    let same = {
                        use std::os::unix::fs::MetadataExt;
                        current.is_file()
                            && current.dev() == captured.dev()
                            && current.ino() == captured.ino()
                    };
                    #[cfg(not(unix))]
                    let same = false;
                    if same && replay::equal_files(&source, &pin)? {
                        fs::remove_file(&source)?;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
            // A prior quantum/process may have unlinked the source but failed
            // its directory barrier. Absence alone is not durable retirement.
            retained::sync_dir(source.parent().invariant("source missing")?)?;
            fs::remove_file(&pin)?;
            retained::sync_dir(&self.generation)?;
            Ok(())
        })();
        if let Err(error) = result {
            self.input.rewind()?;
            self.input.seek(std::io::SeekFrom::Start(offset))?;
            return Err(error);
        }
        Ok(Step::More)
    }
}
