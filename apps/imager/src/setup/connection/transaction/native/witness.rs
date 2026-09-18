//! Bounded Linux namespace transcript and inventory rules shared with its native driver.
use std::collections::BTreeSet;

pub(super) const CREATE: u32 = 0x100;
pub(super) const MOVED_FROM: u32 = 0x40;
pub(super) const MOVED_TO: u32 = 0x80;
pub(super) const ISDIR: u32 = 0x4000_0000;
pub(super) const ATTRIB: u32 = 0x4;
pub(super) const DELETE: u32 = 0x200;
pub(super) const DELETE_SELF: u32 = 0x400;
pub(super) const MOVE_SELF: u32 = 0x800;
pub(super) const UNMOUNT: u32 = 0x2000;
pub(super) const OVERFLOW: u32 = 0x4000;
pub(super) const IGNORED: u32 = 0x8000;
pub(super) const MASK: u32 =
    CREATE | MOVED_FROM | MOVED_TO | ATTRIB | DELETE | DELETE_SELF | MOVE_SELF;
pub(super) const EVENT_BUFFER: usize = 65536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Lost;
type Result<T> = std::result::Result<T, Lost>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    Root,
    Namespace,
    Target,
    Directory,
}
#[derive(Clone, Debug)]
pub(super) struct Expected {
    pub role: Role,
    pub name: Vec<u8>,
    pub mask: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DirectoryPhase {
    Unpublished,
    Publishing,
    Published,
}
pub(super) struct Transcript {
    directory_phase: Option<DirectoryPhase>,
    directory_live: bool,
    bindings: Vec<(i32, Role)>,
    namespace: Vec<u8>,
    target: Vec<u8>,
    expected: Vec<Expected>,
    seen: Vec<bool>,
    cookie: Option<u32>,
    bytes: usize,
    records: usize,
    failed: bool,
}
fn require(value: bool) -> Result<()> {
    if value { Ok(()) } else { Err(Lost) }
}
fn component(name: &[u8]) -> bool {
    !name.is_empty() && name.len() <= 255 && !name.contains(&0) && !name.contains(&b'/')
}
impl Transcript {
    pub fn directory(
        bindings: &[(i32, Role)],
        namespace: &[u8],
        target: &[u8],
        expected: &[Expected],
        phase: DirectoryPhase,
    ) -> Result<Self> {
        require(
            phase == DirectoryPhase::Unpublished
                || bindings.iter().any(|(_, role)| *role == Role::Directory),
        )?;
        if phase == DirectoryPhase::Publishing {
            require(
                expected.len() == 3
                    && expected.iter().any(|e| {
                        e.role == Role::Namespace
                            && e.name == b"stage"
                            && e.mask == MOVED_FROM | ISDIR
                    })
                    && expected.iter().any(|e| {
                        e.role == Role::Target && e.name == target && e.mask == MOVED_TO | ISDIR
                    })
                    && expected.iter().any(|e| {
                        e.role == Role::Directory && e.name.is_empty() && e.mask == MOVE_SELF
                    }),
            )?;
        }
        Self::new_inner(bindings, namespace, target, expected, Some(phase))
    }
    pub fn new(
        bindings: &[(i32, Role)],
        namespace: &[u8],
        target: &[u8],
        expected: &[Expected],
    ) -> Result<Self> {
        require(bindings.iter().all(|(_, role)| *role != Role::Directory))?;
        Self::new_inner(bindings, namespace, target, expected, None)
    }
    fn new_inner(
        bindings: &[(i32, Role)],
        namespace: &[u8],
        target: &[u8],
        expected: &[Expected],
        directory_phase: Option<DirectoryPhase>,
    ) -> Result<Self> {
        require(
            bindings.len() <= 4 && expected.len() <= 3 && component(namespace) && component(target),
        )?;
        for (index, (wd, role)) in bindings.iter().enumerate() {
            require(
                *wd >= 0
                    && !bindings
                        .get(..index)
                        .ok_or(Lost)?
                        .iter()
                        .any(|(_, previous)| previous == role),
            )?;
        }
        for value in expected {
            require(
                bindings.iter().any(|(_, role)| *role == value.role)
                    && if value.role == Role::Directory {
                        directory_phase == Some(DirectoryPhase::Publishing)
                            && value.name.is_empty()
                            && value.mask == MOVE_SELF
                    } else {
                        component(&value.name)
                    },
            )?;
        }
        Ok(Self {
            directory_phase,
            directory_live: directory_phase == Some(DirectoryPhase::Published),
            bindings: bindings.to_vec(),
            namespace: namespace.to_vec(),
            target: target.to_vec(),
            expected: expected.to_vec(),
            seen: vec![false; expected.len()],
            cookie: None,
            bytes: 0,
            records: 0,
            failed: false,
        })
    }
    pub fn feed(&mut self, bytes: &[u8]) -> Result<()> {
        let result = self.feed_inner(bytes);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn feed_inner(&mut self, mut bytes: &[u8]) -> Result<()> {
        require(!self.failed && !bytes.is_empty() && bytes.len() <= EVENT_BUFFER)?;
        self.bytes = self.bytes.checked_add(bytes.len()).ok_or(Lost)?;
        require(self.bytes <= 256 * 1024)?;
        while !bytes.is_empty() {
            self.records += 1;
            require(self.records <= 1024)?;
            let header = bytes.get(..16).ok_or(Lost)?;
            let wd = i32::from_ne_bytes(header.get(..4).ok_or(Lost)?.try_into().map_err(|_| Lost)?);
            let mask =
                u32::from_ne_bytes(header.get(4..8).ok_or(Lost)?.try_into().map_err(|_| Lost)?);
            let cookie = u32::from_ne_bytes(
                header
                    .get(8..12)
                    .ok_or(Lost)?
                    .try_into()
                    .map_err(|_| Lost)?,
            );
            let length = u32::from_ne_bytes(
                header
                    .get(12..16)
                    .ok_or(Lost)?
                    .try_into()
                    .map_err(|_| Lost)?,
            ) as usize;
            require(
                length <= 256
                    && length % 4 == 0
                    && mask & (OVERFLOW | IGNORED | UNMOUNT) == 0
                    && mask & !(MASK | ISDIR) == 0,
            )?;
            let record = bytes.get(..16 + length).ok_or(Lost)?;
            let padded = record.get(16..).ok_or(Lost)?;
            let name = if padded.is_empty() {
                padded
            } else {
                let end = padded.iter().position(|value| *value == 0).ok_or(Lost)?;
                require(
                    padded
                        .get(end..)
                        .ok_or(Lost)?
                        .iter()
                        .all(|value| *value == 0),
                )?;
                let name = padded.get(..end).ok_or(Lost)?;
                require(component(name) && name != b"." && name != b"..")?;
                name
            };
            let kind = mask & !ISDIR;
            require(matches!(
                kind,
                CREATE | DELETE | MOVED_FROM | MOVED_TO | ATTRIB | DELETE_SELF | MOVE_SELF
            ))?;
            require(if matches!(kind, MOVE_SELF | DELETE_SELF) {
                name.is_empty() && mask & ISDIR == 0
            } else {
                !name.is_empty() || kind == ATTRIB
            })?;
            require(if matches!(kind, MOVED_FROM | MOVED_TO) {
                cookie != 0
            } else {
                cookie == 0
            })?;
            self.observe_roles(wd, mask, cookie, name)?;
            bytes = bytes.get(16 + length..).ok_or(Lost)?;
        }
        Ok(())
    }
    fn observe_roles(&mut self, wd: i32, mask: u32, cookie: u32, name: &[u8]) -> Result<()> {
        let kind = mask & !ISDIR;
        let roles: Vec<_> = self
            .bindings
            .iter()
            .filter(|(watch, _)| *watch == wd)
            .map(|(_, role)| *role)
            .collect();
        require(!roles.is_empty())?;
        for role in roles {
            if role == Role::Directory && !name.is_empty() {
                require(self.directory_live)?;
                // Still evaluate this same event against every aliased role.
                continue;
            }
            let relevant = name.is_empty()
                || role == Role::Namespace
                || (role == Role::Root && name == self.namespace)
                || (role == Role::Target && name == self.target);
            if !relevant {
                continue;
            }
            let index = self
                .expected
                .iter()
                .position(|expected| {
                    expected.role == role && expected.name == name && expected.mask == mask
                })
                .ok_or(Lost)?;
            require(!self.seen.get(index).copied().ok_or(Lost)?)?;
            *self.seen.get_mut(index).ok_or(Lost)? = true;
            if role == Role::Directory {
                require(
                    self.directory_phase == Some(DirectoryPhase::Publishing)
                        && !self.directory_live
                        && kind == MOVE_SELF,
                )?;
                self.directory_live = true;
            }
            if matches!(kind, MOVED_FROM | MOVED_TO) {
                require(self.cookie.is_none_or(|before| before == cookie))?;
                self.cookie = Some(cookie);
            }
        }
        Ok(())
    }
    pub fn finish(self) -> Result<()> {
        require(
            !self.failed
                && self.seen.iter().all(|seen| *seen)
                && (self.directory_phase != Some(DirectoryPhase::Publishing)
                    || self.directory_live),
        )
    }
}
pub(super) struct Inventory {
    expected: Option<BTreeSet<Vec<u8>>>,
    seen: BTreeSet<Vec<u8>>,
    requests: usize,
    records: usize,
    failed: bool,
    done: bool,
}
impl Inventory {
    pub fn exact(names: &[&[u8]]) -> Self {
        let expected: BTreeSet<_> = [b".".as_slice(), b".."]
            .into_iter()
            .chain(names.iter().copied())
            .map(<[u8]>::to_vec)
            .collect();
        let failed = names.len() > 6
            || expected.len() != names.len() + 2
            || names.iter().any(|name| !component(name));
        Self {
            expected: Some(expected),
            seen: BTreeSet::new(),
            requests: 0,
            records: 0,
            failed,
            done: false,
        }
    }
    pub fn barrier() -> Self {
        Self {
            expected: None,
            seen: BTreeSet::new(),
            requests: 0,
            records: 0,
            failed: false,
            done: false,
        }
    }
    /// Called once for every actual positive-size request, including failed and EOF requests.
    pub fn response(&mut self, result: Result<&[u8]>) -> Result<bool> {
        self.requests += 1;
        let value = self.response_inner(result);
        if value.is_err() {
            self.failed = true;
        }
        value
    }
    fn response_inner(&mut self, result: Result<&[u8]>) -> Result<bool> {
        require(!self.failed && !self.done && self.requests <= 4)?;
        let mut bytes = result?;
        require(bytes.len() <= 4096)?;
        let eof = bytes.is_empty();
        while !bytes.is_empty() {
            self.records += 1;
            require(self.records <= if self.expected.is_some() { 8 } else { 170 })?;
            let header = bytes.get(..19).ok_or(Lost)?;
            let length = usize::from(u16::from_ne_bytes(
                header
                    .get(16..18)
                    .ok_or(Lost)?
                    .try_into()
                    .map_err(|_| Lost)?,
            ));
            require(length >= 24 && length % 8 == 0)?;
            let record = bytes.get(..length).ok_or(Lost)?;
            let padded = record.get(19..).ok_or(Lost)?;
            let end = padded.iter().position(|value| *value == 0).ok_or(Lost)?;
            let name = padded.get(..end).ok_or(Lost)?;
            require(component(name) && self.seen.insert(name.to_vec()))?;
            if let Some(expected) = &self.expected {
                require(expected.contains(name))?;
            }
            bytes = bytes.get(length..).ok_or(Lost)?;
        }
        if self.expected.is_none() || eof {
            if let Some(expected) = &self.expected {
                require(*expected == self.seen)?;
            }
            self.done = true;
        }
        Ok(self.done)
    }
}

#[derive(Default)]
pub(super) struct Passes {
    directory: bool,
    used: u8,
    failed: bool,
}
impl Passes {
    pub fn directory() -> Self {
        Self {
            directory: true,
            ..Self::default()
        }
    }
    #[cfg(test)]
    pub fn used(&self) -> u8 {
        self.used
    }
    pub fn enter(&mut self) -> Result<()> {
        if self.failed || self.used >= if self.directory { 32 } else { 38 } {
            self.failed = true;
            return Err(Lost);
        }
        self.used += 1;
        Ok(())
    }
}
pub(super) fn created_regular_join(
    created: &super::descriptor::Object,
    stamp: &super::descriptor::Stamp,
    actual: &super::descriptor::Object,
    reopened: &super::descriptor::Stamp,
    executable: bool,
) -> Result<()> {
    require(
        created == actual
            && stamp == reopened
            && created.kind == 0o100_000
            && created.generation != [0; 32]
            && stamp.links == 1
            && stamp.mode == if executable { 0o100_700 } else { 0o100_600 },
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RepairBinding {
    pub connection: [u8; 16],
    pub operation: u64,
    pub attempt: u64,
    pub revision: u64,
    pub fence: u64,
    pub namespace: super::descriptor::Object,
    pub displaced: super::descriptor::Object,
    pub stamp: super::descriptor::Stamp,
    pub bytes: Vec<u8>,
}
impl RepairBinding {
    pub fn validate(&self, current: &Self, closed_missing_k: bool) -> Result<()> {
        require(closed_missing_k && self == current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(wd: i32, mask: u32, cookie: u32, name: &[u8]) -> Vec<u8> {
        let size = if name.is_empty() {
            0
        } else {
            (name.len() + 1).next_multiple_of(16)
        };
        let mut bytes = Vec::new();
        bytes.extend(wd.to_ne_bytes());
        bytes.extend(mask.to_ne_bytes());
        bytes.extend(cookie.to_ne_bytes());
        bytes.extend((size as u32).to_ne_bytes());
        if size != 0 {
            bytes.extend(name);
            bytes.resize(16 + size, 0);
        }
        bytes
    }
    fn record(name: &[u8]) -> Vec<u8> {
        let size = (19 + name.len() + 1).next_multiple_of(8);
        let mut bytes = vec![0; size];
        bytes[..8].copy_from_slice(&u64::MAX.to_ne_bytes()); // deliberately not identity
        bytes[8..16].copy_from_slice(&i64::MIN.to_ne_bytes()); // deliberately not a cursor proof
        bytes[16..18].copy_from_slice(&(size as u16).to_ne_bytes());
        bytes[18] = 255; // d_type is not authority either
        bytes[19..19 + name.len()].copy_from_slice(name);
        bytes
    }
    #[test]
    fn directory_witness_requires_exact_move_self_and_allows_only_later_children() {
        let bindings = [
            (1, Role::Root),
            (2, Role::Namespace),
            (3, Role::Target),
            (4, Role::Directory),
        ];
        let expected = [
            Expected {
                role: Role::Namespace,
                name: b"stage".to_vec(),
                mask: MOVED_FROM | ISDIR,
            },
            Expected {
                role: Role::Target,
                name: b"dir".to_vec(),
                mask: MOVED_TO | ISDIR,
            },
            Expected {
                role: Role::Directory,
                name: vec![],
                mask: MOVE_SELF,
            },
        ];
        let make = || {
            Transcript::directory(
                &bindings,
                b"private",
                b"dir",
                &expected,
                DirectoryPhase::Publishing,
            )
        };
        let mut proof = make().expect(
            "Directory publication must accept its explicit nameless MOVE_SELF expectation",
        );
        proof
            .feed(&event(2, MOVED_FROM | ISDIR, 9, b"stage"))
            .unwrap();
        proof.feed(&event(3, MOVED_TO | ISDIR, 9, b"dir")).unwrap();
        proof.feed(&event(4, MOVE_SELF, 0, b"")).unwrap();
        proof.feed(&event(4, CREATE, 0, b"future-child")).unwrap();
        proof.finish().unwrap();
        for phase in [DirectoryPhase::Unpublished, DirectoryPhase::Publishing] {
            let mut proof = if phase == DirectoryPhase::Publishing {
                make().unwrap()
            } else {
                Transcript::directory(&bindings, b"private", b"dir", &[], phase).unwrap()
            };
            assert!(proof.feed(&event(4, CREATE, 0, b"early-child")).is_err());
        }
        for bad in [
            event(4, ATTRIB, 0, b""),
            event(4, MOVE_SELF, 0, b""),
            event(4, DELETE_SELF, 0, b""),
        ] {
            let mut proof = Transcript::directory(
                &bindings,
                b"private",
                b"dir",
                &[],
                DirectoryPhase::Published,
            )
            .unwrap();
            assert!(proof.feed(&bad).is_err());
        }
        let mut later = Transcript::directory(
            &bindings,
            b"private",
            b"dir",
            &[],
            DirectoryPhase::Published,
        )
        .unwrap();
        later.feed(&event(4, CREATE | ISDIR, 0, b"child")).unwrap();
        later.feed(&event(4, ATTRIB, 0, b"child")).unwrap();
        later.finish().unwrap();
        // A child event cannot escape a second role on the same authenticated watch.
        let aliases = [
            (1, Role::Root),
            (2, Role::Namespace),
            (4, Role::Target),
            (4, Role::Directory),
        ];
        let mut alias =
            Transcript::directory(&aliases, b"private", b"dir", &[], DirectoryPhase::Published)
                .unwrap();
        assert!(alias.feed(&event(4, CREATE | ISDIR, 0, b"dir")).is_err());
    }
    #[test]
    fn witness_full_pass_exhaustion_is_sticky_without_payload_chunk_charges() {
        let mut passes = Passes::default();
        for _ in 0..38 {
            passes.enter().unwrap();
        }
        assert!(passes.enter().is_err());
        assert!(passes.enter().is_err());
    }
    #[test]
    fn witness_created_handoff_rejects_equal_content_on_another_identity_and_wrong_mode() {
        use super::super::descriptor::{Object, Stamp};
        let original = Object {
            dev: 1,
            ino: 2,
            kind: 0o100_000,
            generation: [3; 32],
        };
        for executable in [false, true] {
            let stamp = Stamp {
                length: 0,
                modified: (0, 0),
                changed: (1, 0),
                mode: if executable { 0o100_700 } else { 0o100_600 },
                uid: 1,
                gid: 1,
                links: 1,
            };
            created_regular_join(&original, &stamp, &original, &stamp, executable).unwrap();
            let mut other = original.clone();
            other.generation = [4; 32];
            assert!(created_regular_join(&original, &stamp, &other, &stamp, executable).is_err());
            let mut changed = stamp.clone();
            changed.changed = (2, 0);
            assert!(
                created_regular_join(&original, &stamp, &original, &changed, executable).is_err()
            );
            let mut stripped = stamp.clone();
            stripped.mode &= !0o200;
            assert!(
                created_regular_join(&original, &stripped, &original, &stripped, executable)
                    .is_err()
            );
            assert!(
                created_regular_join(&original, &stamp, &original, &stamp, !executable).is_err()
            );
        }
    }
    #[test]
    fn witness_repair_binding_rejects_wrong_context_bytes_object_or_phase() {
        use super::super::descriptor::{Object, Stamp};
        let facts = RepairBinding {
            connection: [1; 16],
            operation: 2,
            attempt: 1,
            revision: 8,
            fence: 3,
            namespace: Object {
                dev: 1,
                ino: 2,
                kind: 0o040_000,
                generation: [3; 32],
            },
            displaced: Object {
                dev: 1,
                ino: 4,
                kind: 0o100_000,
                generation: [5; 32],
            },
            stamp: Stamp {
                length: 0,
                modified: (0, 0),
                changed: (1, 0),
                mode: 0o100_600,
                uid: 1,
                gid: 1,
                links: 1,
            },
            bytes: b"canonical".to_vec(),
        };
        facts.validate(&facts, true).unwrap();
        assert!(facts.validate(&facts, false).is_err());
        for index in 0..9 {
            let mut changed = facts.clone();
            match index {
                0 => changed.connection = [8; 16],
                1 => changed.operation += 1,
                2 => changed.attempt += 1,
                3 => changed.revision += 1,
                4 => changed.fence += 1,
                5 => changed.namespace.generation = [9; 32],
                6 => changed.displaced.generation = [9; 32],
                7 => changed.stamp.changed = (2, 0),
                _ => changed.bytes.push(0),
            }
            assert!(facts.validate(&changed, true).is_err());
        }
    }
    fn create() -> Expected {
        Expected {
            role: Role::Namespace,
            name: b"intent".to_vec(),
            mask: CREATE,
        }
    }
    fn transcript(expected: &[Expected]) -> Transcript {
        Transcript::new(
            &[(1, Role::Root), (2, Role::Namespace), (1, Role::Target)],
            b"private",
            b"file",
            expected,
        )
        .unwrap()
    }
    #[test]
    fn witness_accepts_exact_regular_create_and_ignores_only_unrelated_all_role_events() {
        let mut proof = transcript(&[create()]);
        proof.feed(&event(1, CREATE, 0, b"unrelated")).unwrap();
        proof.feed(&event(2, CREATE, 0, b"intent")).unwrap();
        proof.finish().unwrap();
    }
    #[test]
    fn witness_requires_exact_events_cookie_pair_and_all_alias_roles() {
        assert!(transcript(&[create()]).finish().is_err());
        let pair = [
            Expected {
                role: Role::Namespace,
                name: b"stage".to_vec(),
                mask: MOVED_FROM,
            },
            Expected {
                role: Role::Target,
                name: b"file".to_vec(),
                mask: MOVED_TO,
            },
        ];
        for cookie in [12, 13] {
            let mut proof = transcript(&pair);
            proof.feed(&event(2, MOVED_FROM, 12, b"stage")).unwrap();
            let fed = proof.feed(&event(1, MOVED_TO, cookie, b"file"));
            assert_eq!(fed.and_then(|()| proof.finish()).is_ok(), cookie == 12);
        }
        let mut alias = Transcript::new(
            &[(1, Role::Root), (1, Role::Target)],
            b"file",
            b"file",
            &[Expected {
                role: Role::Root,
                name: b"file".to_vec(),
                mask: CREATE | ISDIR,
            }],
        )
        .unwrap();
        assert!(alias.feed(&event(1, CREATE | ISDIR, 0, b"file")).is_err());
    }
    #[test]
    fn witness_rejects_extra_coalesced_masks_loss_unknown_watch_and_stays_failed() {
        for bad in [
            event(2, CREATE | ATTRIB, 0, b"intent"),
            event(2, CREATE | ISDIR, 0, b"intent"),
            event(-1, OVERFLOW, 0, b""),
            event(99, CREATE, 0, b"x"),
            event(1, IGNORED, 0, b""),
            event(1, ATTRIB | ISDIR, 0, b""),
        ] {
            let mut proof = transcript(&[create()]);
            assert!(proof.feed(&bad).is_err());
            assert!(proof.feed(&event(2, CREATE, 0, b"intent")).is_err());
            assert!(proof.finish().is_err());
        }
        let mut proof = transcript(&[create()]);
        proof.feed(&event(2, CREATE, 0, b"intent")).unwrap();
        assert!(proof.feed(&event(2, CREATE, 0, b"intent")).is_err());
    }
    #[test]
    fn witness_charges_unrelated_event_work_and_rejects_malformed_buffers() {
        let mut proof = transcript(&[]);
        for _ in 0..1024 {
            proof.feed(&event(1, CREATE, 0, b"x")).unwrap();
        }
        assert!(proof.feed(&event(1, CREATE, 0, b"x")).is_err());
        for bytes in [vec![0; 15], event(1, CREATE, 0, b"x/y"), {
            let mut e = event(1, CREATE, 0, b"x");
            e[16..].fill(1);
            e
        }] {
            assert!(transcript(&[]).feed(&bytes).is_err());
        }
    }
    #[test]
    fn witness_inventory_requires_actual_eof_and_ignores_inode_type_offset_fields() {
        let mut inventory = Inventory::exact(&[b"intent"]);
        assert!(!inventory.response(Ok(&record(b"."))).unwrap());
        assert!(
            !inventory
                .response(Ok(&[record(b".."), record(b"intent")].concat()))
                .unwrap()
        );
        assert!(inventory.response(Ok(&[])).unwrap());
        let mut barrier = Inventory::barrier();
        assert!(barrier.response(Ok(&record(b"ordinary-child"))).unwrap());
    }
    #[test]
    fn witness_inventory_rejects_partial_error_duplicates_unknown_and_exhaustion() {
        for records in [
            vec![record(b"."), record(b"..")],
            vec![record(b"."), record(b".."), record(b"extra")],
            vec![record(b"."), record(b"."), record(b"intent")],
        ] {
            let mut inventory = Inventory::exact(&[b"intent"]);
            let result = inventory.response(Ok(&records.concat()));
            assert!(result.and_then(|_| inventory.response(Ok(&[]))).is_err());
        }
        let mut inventory = Inventory::exact(&[]);
        inventory.response(Ok(&record(b"."))).unwrap();
        assert!(inventory.response(Err(Lost)).is_err());
        assert!(inventory.response(Ok(&[])).is_err());
        let mut inventory = Inventory::exact(&[b"a", b"b", b"c"]);
        for name in [b".".as_slice(), b"..", b"a", b"b"] {
            assert!(!inventory.response(Ok(&record(name))).unwrap());
        }
        assert!(inventory.response(Ok(&record(b"c"))).is_err());
        let mut malformed = record(b".");
        malformed[16..18].copy_from_slice(&0_u16.to_ne_bytes());
        assert!(Inventory::barrier().response(Ok(&malformed)).is_err());
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum RestartPhase {
    Staged,
    Quarantined,
    Published,
}
pub(super) fn regular_restart(shape: [bool; 4], replacement: bool) -> Result<RestartPhase> {
    let [stage, displaced, descriptor, outcome] = shape;
    require(!descriptor || displaced)?;
    require(!displaced || replacement)?;
    require(!stage || !outcome)?;
    if stage {
        Ok(if displaced {
            RestartPhase::Quarantined
        } else {
            RestartPhase::Staged
        })
    } else {
        require(displaced == replacement)?;
        Ok(RestartPhase::Published)
    }
}
#[cfg(test)]
mod restart_tests {
    use super::*;
    #[test]
    fn regular_restart_requires_exact_phase_and_original_target_kind() {
        for replacement in [false, true] {
            assert_eq!(
                regular_restart([true, false, false, false], replacement),
                Ok(RestartPhase::Staged)
            );
            assert_eq!(
                regular_restart([false, replacement, replacement, false], replacement),
                Ok(RestartPhase::Published)
            );
            assert_eq!(
                regular_restart([false, replacement, replacement, true], replacement),
                Ok(RestartPhase::Published)
            );
            assert!(regular_restart([true, false, false, true], replacement).is_err());
            assert!(regular_restart([true, false, true, false], replacement).is_err());
        }
        for descriptor in [false, true] {
            assert_eq!(
                regular_restart([true, true, descriptor, false], true),
                Ok(RestartPhase::Quarantined)
            );
            assert_eq!(
                regular_restart([false, true, descriptor, false], true),
                Ok(RestartPhase::Published)
            );
            assert!(regular_restart([true, true, descriptor, false], false).is_err());
        }
        assert!(regular_restart([false, false, false, false], true).is_err());
    }
}

fn inventory_is(names: &BTreeSet<Vec<u8>>, expected: &[&[u8]]) -> bool {
    names.len() == expected.len() && expected.iter().all(|name| names.contains(*name))
}
pub(super) fn creation_phase(names: &BTreeSet<Vec<u8>>, new: &[u8]) -> Result<()> {
    require(match new {
        b"intent" => names.is_empty(),
        b"stage" => inventory_is(names, &[b"intent"]),
        b"staged-intent" => inventory_is(names, &[b"intent", b"stage"]),
        b"displaced-intent" => [
            &[b"intent".as_slice(), b"staged-intent", b"displaced"][..],
            &[
                b"intent".as_slice(),
                b"staged-intent",
                b"displaced",
                b"stage",
            ][..],
            &[
                b"intent".as_slice(),
                b"staged-intent",
                b"displaced",
                b"outcome",
            ][..],
        ]
        .iter()
        .any(|expected| inventory_is(names, expected)),
        b"outcome" => {
            inventory_is(names, &[b"intent", b"staged-intent"])
                || inventory_is(
                    names,
                    &[
                        b"intent",
                        b"staged-intent",
                        b"displaced",
                        b"displaced-intent",
                    ],
                )
        }
        _ => false,
    })
}
pub(super) fn movement_phase(names: &BTreeSet<Vec<u8>>, publish: bool) -> Result<()> {
    require(
        inventory_is(names, &[b"intent", b"staged-intent", b"stage"])
            || (publish
                && inventory_is(
                    names,
                    &[
                        b"intent",
                        b"staged-intent",
                        b"stage",
                        b"displaced",
                        b"displaced-intent",
                    ],
                )),
    )
}
#[cfg(test)]
mod phase_tests {
    use super::*;
    fn names(values: &[&[u8]]) -> BTreeSet<Vec<u8>> {
        values.iter().map(|name| name.to_vec()).collect()
    }
    #[test]
    fn ordinary_creation_and_mutation_table_is_exact() {
        let mut inventory = names(&[]);
        for next in [b"intent".as_slice(), b"stage", b"staged-intent"] {
            assert_eq!(creation_phase(&inventory, next), Ok(()));
            inventory.insert(next.to_vec());
            assert!(creation_phase(&inventory, next).is_err());
        }
        assert!(creation_phase(&inventory, b"outcome").is_err());
        assert!(creation_phase(&inventory, b"displaced-intent").is_err());
        assert_eq!(movement_phase(&inventory, false), Ok(()));
        assert_eq!(movement_phase(&inventory, true), Ok(()));
        inventory.insert(b"displaced".to_vec());
        assert!(movement_phase(&inventory, true).is_err());
        assert_eq!(creation_phase(&inventory, b"displaced-intent"), Ok(()));
        inventory.insert(b"displaced-intent".to_vec());
        assert_eq!(movement_phase(&inventory, true), Ok(()));
        assert!(movement_phase(&inventory, false).is_err());
        inventory.remove(b"stage".as_slice());
        assert_eq!(creation_phase(&inventory, b"outcome"), Ok(()));
        inventory.insert(b"outcome".to_vec());
        assert!(movement_phase(&inventory, true).is_err());
        inventory.remove(b"displaced-intent".as_slice());
        assert_eq!(creation_phase(&inventory, b"displaced-intent"), Ok(()));
        assert!(creation_phase(&names(&[b"intruder"]), b"intent").is_err());
        assert!(creation_phase(&names(&[]), b"stage").is_err());
        assert_eq!(
            creation_phase(&names(&[b"intent", b"staged-intent"]), b"outcome"),
            Ok(())
        );
    }
}
