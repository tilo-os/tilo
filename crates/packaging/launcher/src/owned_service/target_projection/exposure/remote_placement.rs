//! Exact managed consuming namespace placement; wire facts never grant source access.
use super::{
    Arc, BASE, ExecutingPeer, ExposurePreparation, File, NamespaceWitness, OwnedFd, Read, identity,
    io, kernel_stopped, mount_id, namespace, refused,
};
use crate::ordinary_child::{ManagedConsumingBootstrap, exposure_control::PlacementRecord};
use std::sync::Mutex;

pub(super) struct RemotePlacement {
    managed: Arc<Mutex<ManagedConsumingBootstrap>>,
    record: PlacementRecord,
    namespace: File,
    namespace_identity: (u64, u64),
    launcher: OwnedFd,
    view: Option<File>,
    attempted: bool,
    retired: bool,
}
impl RemotePlacement {
    pub(super) fn prepare(
        preparation: &mut ExposurePreparation,
        managed: Arc<Mutex<ManagedConsumingBootstrap>>,
    ) -> io::Result<Self> {
        preparation.check()?;
        let owner = managed
            .lock()
            .map_err(|_| refused("Managed exposure custody poisoned"))?;
        owner.verify_launcher(preparation.target_peer())?;
        let namespace = owner.clone_namespace()?;
        let namespace_identity = identity(&namespace)?;
        if namespace_identity == identity(&self::namespace()?)? {
            return Err(refused("Consuming helper namespace is not private"));
        }
        let launcher = preparation.peer.pidfd().try_clone_to_owned()?;
        drop(owner);
        Ok(Self {
            managed,
            record: preparation.record.clone(),
            namespace,
            namespace_identity,
            launcher,
            view: preparation.clone.take(),
            attempted: false,
            retired: false,
        })
    }
    pub(super) fn place(&mut self) -> io::Result<()> {
        if self.attempted {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        self.attempted = true;
        let mut owner = self
            .managed
            .lock()
            .map_err(|_| refused("Managed exposure custody poisoned"))?;
        let (named, ns) = owner.place_exposure(
            &self.record,
            self.view.as_ref().ok_or(io::ErrorKind::NotConnected)?,
        )?;
        // Hold returned aliases immediately, before subsequent validation.
        self.view = Some(named);
        if identity(&ns)? != self.namespace_identity
            || identity(&self.namespace)? != self.namespace_identity
        {
            return Err(refused("Remote placement namespace differs"));
        }
        drop(owner);
        self.check()
    }
    pub(super) fn check(&self) -> io::Result<()> {
        if self.retired || !self.attempted {
            return Err(io::ErrorKind::NotConnected.into());
        }
        let owner = self
            .managed
            .lock()
            .map_err(|_| refused("Managed exposure custody poisoned"))?;
        owner.check_current()?;
        if identity(&owner.clone_namespace()?)? != self.namespace_identity
            || identity(&self.namespace)? != self.namespace_identity
        {
            return Err(refused("Remote exposure namespace changed"));
        }
        let rows = mounts(owner.mountinfo()?)?;
        drop(owner);
        let clone = rows
            .iter()
            .find(|row| row.id == self.record.clone_mount_id)
            .ok_or_else(|| refused("Remote clone absent"))?;
        let stage_path = format!("{BASE}/{}", hex::encode(self.record.nonce));
        let stage = rows
            .iter()
            .find(|row| row.path == stage_path.as_bytes())
            .ok_or_else(|| refused("Remote carrier absent"))?;
        if !clone.private
            || !clone.restricted
            || !stage.private
            || !stage.restricted
            || clone.parent != stage.id
            || clone.path != format!("{stage_path}/view").as_bytes()
            || (self.record.readonly && !clone.readonly)
        {
            return Err(refused("Remote placement topology or flags differ"));
        }
        if mount_id(self.view.as_ref().ok_or(io::ErrorKind::NotConnected)?)? != clone.id {
            return Err(refused("Remote held clone differs"));
        }
        Ok(())
    }
    pub(super) fn file(&self) -> io::Result<&File> {
        self.check()?;
        self.view
            .as_ref()
            .ok_or_else(|| io::ErrorKind::NotConnected.into())
    }
    pub(super) fn adopt_consumer(
        &self,
        witness: &NamespaceWitness,
        peer: &ExecutingPeer,
    ) -> io::Result<()> {
        let mut owner = self
            .managed
            .lock()
            .map_err(|_| refused("Managed exposure custody poisoned"))?;
        owner.verify_launcher(peer)?;
        owner.adopt_exposure_consumer(&self.record, witness, peer)
    }
    pub(super) fn retire(&mut self) -> io::Result<()> {
        if self.retired {
            return Ok(());
        }
        let mut owner = self
            .managed
            .lock()
            .map_err(|_| refused("Managed exposure custody poisoned"))?;
        owner.check_current()?;
        if identity(&owner.clone_namespace()?)? != self.namespace_identity
            || identity(&self.namespace)? != self.namespace_identity
        {
            return Err(refused("Remote retirement namespace differs"));
        }
        // Drop all parent aliases BEFORE child closes its aliases and unmounts.
        self.view.take();
        let result = owner.retire_exposure(&self.record);
        // R reaps the actual owned ordinary outer child before detach. An ACK
        // alone is insufficient: independently observe its actual kernel task.
        if !kernel_stopped(&self.launcher)? {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let rows = mounts(owner.mountinfo()?)?;
        drop(owner);
        let stage = format!("{BASE}/{}", hex::encode(self.record.nonce));
        if rows.iter().any(|row| {
            row.id == self.record.clone_mount_id
                || row.path == stage.as_bytes()
                || row.path == format!("{stage}/view").as_bytes()
        }) {
            return Err(refused("Remote clone/carrier absence not observed"));
        }
        self.retired = true;
        result
    }
}
struct Row {
    id: u64,
    parent: u64,
    path: Vec<u8>,
    private: bool,
    restricted: bool,
    readonly: bool,
}
fn mounts(file: File) -> io::Result<Vec<Row>> {
    let mut bytes = Vec::new();
    file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(refused("Remote mountinfo bound exceeded"));
    }
    let mut result = Vec::new();
    let number = |v: &[u8]| -> io::Result<u64> {
        if v.is_empty() || !v.iter().all(u8::is_ascii_digit) {
            return Err(refused("Remote mount ID malformed"));
        }
        std::str::from_utf8(v)
            .map_err(io::Error::other)?
            .parse()
            .map_err(io::Error::other)
    };
    for row in bytes.split(|b| *b == b'\n').filter(|v| !v.is_empty()) {
        let f: Vec<_> = row.split(|b| *b == b' ').collect();
        let split = f
            .iter()
            .position(|v| *v == b"-")
            .ok_or_else(|| refused("Remote mount row separator missing"))?;
        if split < 6 || split.checked_add(4) != Some(f.len()) {
            return Err(refused("Remote mount row malformed"));
        }
        let [mount, parent, _, _, path, options, ..] = f.as_slice() else {
            return Err(refused("Remote mount row malformed"));
        };
        let id = number(mount)?;
        let parent = number(parent)?;
        if id == 0 || result.iter().any(|r: &Row| r.id == id) {
            return Err(refused("Remote mount ID duplicate"));
        }
        let options: Vec<_> = options.split(|b| *b == b',').collect();
        result.push(Row {
            id,
            parent,
            path: path.to_vec(),
            private: !f
                .get(6..split)
                .ok_or_else(|| refused("Remote mount row malformed"))?
                .iter()
                .any(|v| v.starts_with(b"shared:") || v.starts_with(b"master:")),
            restricted: [b"nodev".as_slice(), b"nosuid", b"noexec"]
                .iter()
                .all(|v| options.contains(v)),
            readonly: options.contains(&b"ro".as_slice()),
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Seek, Write},
        os::fd::FromRawFd,
    };
    fn table(bytes: &[u8]) -> File {
        let raw =
            unsafe { libc::memfd_create(c"exposure-mount-table-test".as_ptr(), libc::MFD_CLOEXEC) };
        assert!(raw >= 0);
        let mut file = unsafe { File::from_raw_fd(raw) };
        file.write_all(bytes).unwrap();
        file.rewind().unwrap();
        file
    }
    #[test]
    fn exact_kernel_table_parser_preserves_paths_and_restrictions() {
        let rows=mounts(table(b"42 2 0:1 / /run/stage rw,nosuid,nodev,noexec - tmpfs stage rw\n43 42 0:2 / /run/stage/view ro,nosuid,nodev,noexec - fuse view rw\n")).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].parent, 42);
        assert!(rows[0].private && rows[0].restricted && rows[1].readonly);
        let shared = mounts(table(
            b"42 2 0:1 / /raw\xff rw,nosuid,nodev,noexec shared:7 - tmpfs stage rw\n",
        ))
        .unwrap();
        assert!(!shared[0].private);
        assert_eq!(shared[0].path, b"/raw\xff");
    }
    #[test]
    fn exact_kernel_table_parser_refuses_duplicate_malformed_and_unbounded() {
        let row = b"42 2 0:1 / /run/stage rw - tmpfs stage rw\n";
        let mut duplicate = row.to_vec();
        duplicate.extend_from_slice(row);
        assert!(mounts(table(&duplicate)).is_err());
        assert!(mounts(table(b"x 2 0:1 / /run/stage rw - tmpfs stage rw\n")).is_err());
        assert!(mounts(table(b"42 2 0:1 / /run/stage rw tmpfs stage rw\n")).is_err());
        assert!(mounts(table(&vec![b'x'; 4 * 1024 * 1024 + 1])).is_err());
    }
}
