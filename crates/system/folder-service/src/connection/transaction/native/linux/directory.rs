//! Directory owners never turn historical descriptor bytes into live authority.
use super::super::descriptor::directory as d;
use super::{
    CString, File, Instant, NativeDirectory, NativeName, ObjectId, OsStr, PinnedEndpoint,
    RelativePath, Stamp, StoreError, descriptor_endpoint, descriptor_object, descriptor_stamp,
    endpoint, io, object, open_at, planning_check,
};
use crate::connection::{
    model::Side,
    store::{ConnectionRecord, ConnectionStore, OperationIntent},
};
use std::{
    os::unix::ffi::OsStrExt,
    sync::{Weak, atomic::AtomicBool},
};

struct DirectoryPath {
    parent: NativeDirectory,
    name: CString,
    chain: Vec<super::d::Object>,
    present: Option<(File, ObjectId, Stamp)>,
    writable: bool,
}
impl DirectoryPath {
    fn capture(
        endpoint: &PinnedEndpoint,
        path: &RelativePath,
        present: bool,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<Self, StoreError> {
        let capture = || -> Result<Self, endpoint::EndpointError> {
            planning_check(deadline, cancel)?;
            let (leaf, ancestors) = path
                .components()
                .split_last()
                .ok_or(endpoint::EndpointError::Unsupported)?;
            let mut parent = endpoint.native_directory(!present)?;
            let mut chain = vec![descriptor_object(&object(parent.file())?)];
            for component in ancestors {
                planning_check(deadline, cancel)?;
                let NativeName::Unix(bytes) = component else {
                    return Err(endpoint::EndpointError::Unsupported);
                };
                parent = parent.child(OsStr::from_bytes(bytes), !present)?;
                chain.push(descriptor_object(&object(parent.file())?));
            }
            let NativeName::Unix(bytes) = leaf else {
                return Err(endpoint::EndpointError::Unsupported);
            };
            let name =
                CString::new(bytes.as_slice()).map_err(|_| endpoint::EndpointError::Unsupported)?;
            let present = if present {
                let file = open_at(parent.file(), &name, libc::O_RDONLY | libc::O_DIRECTORY)?;
                endpoint::native_same_mount(parent.file(), &file)?;
                endpoint::native_directory_metadata(&file)?;
                let identity = object(&file)?;
                let stamp = Stamp::new(&file.metadata()?);
                Some((file, identity, stamp))
            } else {
                None
            };
            let value = Self {
                parent,
                name,
                chain,
                writable: present.is_none(),
                present,
            };
            value.validate(deadline, cancel)?;
            Ok(value)
        };
        capture().map_err(|_| StoreError::RecoveryRequired)
    }
    fn validate(
        &self,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<(), endpoint::EndpointError> {
        planning_check(deadline, cancel)?;
        self.parent.validate(self.writable)?;
        match &self.present {
            Some((file, identity, stamp)) => {
                endpoint::native_directory_metadata(file)?;
                let named = open_at(self.parent.file(), &self.name, libc::O_PATH)?;
                endpoint::native_same_mount(self.parent.file(), file)?;
                if object(&named)? != *identity
                    || object(file)? != *identity
                    || Stamp::new(&file.metadata()?) != *stamp
                {
                    return Err(endpoint::EndpointError::Unstable);
                }
                drop(named);
            }
            None => match open_at(self.parent.file(), &self.name, libc::O_PATH) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Ok(_) => return Err(endpoint::EndpointError::Unstable),
                Err(error) => return Err(error.into()),
            },
        }
        planning_check(deadline, cancel)?;
        self.parent.validate(self.writable)
    }
    fn state(&self) -> Option<d::Leaf> {
        self.present.as_ref().map(|(_, object, stamp)| d::Leaf {
            object: descriptor_object(object),
            stamp: descriptor_stamp(stamp),
        })
    }
}

/// Pre-Intent metadata observation with retained source/parent identities. No step authority.
pub(in crate::connection) struct NativeDirectoryAdmission<'a> {
    record: ConnectionRecord,
    intent: OperationIntent,
    lifetime: Weak<()>,
    endpoints: [&'a PinnedEndpoint; 2],
    paths: [DirectoryPath; 2],
    direction: u8,
    deadline: Instant,
    cancel: &'a AtomicBool,
}
impl<'a> NativeDirectoryAdmission<'a> {
    pub(in crate::connection::transaction) fn capture(
        store: &ConnectionStore,
        intent: &OperationIntent,
        endpoints: [&'a PinnedEndpoint; 2],
        cancel: &'a AtomicBool,
    ) -> Result<Self, StoreError> {
        let side = crate::connection::transaction::directory::addition_side(intent)?;
        let (record, lifetime, _) = store.native_admission_basis(intent)?;
        if record.baseline_revision.is_none() {
            return Err(StoreError::RecoveryRequired);
        }
        let [left, right] = endpoints;
        if [left.identity(), right.identity()]
            != [record.source.identity, record.destination.identity]
        {
            return Err(StoreError::IdentityChanged);
        }
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        let paths = [
            DirectoryPath::capture(left, &intent.path, side == Side::Source, deadline, cancel)?,
            DirectoryPath::capture(
                right,
                &intent.path,
                side == Side::Destination,
                deadline,
                cancel,
            )?,
        ];
        Ok(Self {
            record,
            intent: intent.clone(),
            lifetime,
            endpoints,
            paths,
            direction: u8::from(side == Side::Destination),
            deadline,
            cancel,
        })
    }
    pub(in crate::connection) fn consume(
        self,
        store: &ConnectionStore,
        intent: &OperationIntent,
        serial: u64,
    ) -> Result<Vec<u8>, StoreError> {
        if self.intent != *intent
            || !store.same_native_lifetime(&self.lifetime)
            || self.lifetime.strong_count() == 0
            || store.load_connection(intent.connection)? != self.record
        {
            return Err(StoreError::StaleRevision);
        }
        for (endpoint, path) in self.endpoints.iter().zip(&self.paths) {
            if path.writable {
                endpoint.revalidate_for_write()
            } else {
                endpoint.revalidate()
            }
            .map_err(|_| StoreError::IdentityChanged)?;
            path.validate(self.deadline, self.cancel)
                .map_err(|_| StoreError::RecoveryRequired)?;
        }
        let [left, right] = &self.paths;
        let fact = d::Admission {
            connection: intent.connection.0,
            operation: serial,
            revision: self.record.revision,
            fence: self.record.fence_generation,
            baseline: self
                .record
                .baseline_revision
                .ok_or(StoreError::RecoveryRequired)?,
            endpoints: [
                descriptor_endpoint(self.record.source.identity),
                descriptor_endpoint(self.record.destination.identity),
            ],
            direction: self.direction,
            decision: self.direction + 1,
            path: intent.path.clone(),
            chains: [left.chain.clone(), right.chain.clone()],
            states: [left.state(), right.state()],
        };
        fact.encode().map_err(|_| StoreError::Corrupt)
    }
}

use super::{Boundary, EndpointError, Failure, NativeEpoch, ParentGuard};
use crate::connection::store::{LiveOperation, NativeOperationKind, RecoveryContext};
pub(super) struct DirectoryAncestors {
    pub(super) admission: d::Admission,
    parents: [ParentGuard; 2],
    stability: Option<[Vec<Stamp>; 2]>,
}
impl DirectoryAncestors {
    pub(super) fn parent(&self, side: super::Side) -> super::Result<ParentGuard> {
        let original = self
            .parents
            .get(usize::from(side == super::Side::Target))
            .ok_or(Failure {
                _cause: EndpointError::Unverified,
                _boundary: Boundary::Untouched,
                _os_error: None,
            })?;
        Ok(ParentGuard {
            directory: original.directory.clone(),
            name: original.name.clone(),
            epoch: original.epoch,
            side,
            chain: original.chain.clone(),
            path: original.path.clone(),
        })
    }
}
impl<'a> NativeEpoch<'a> {
    pub(super) fn begin_directory(
        live: &LiveOperation,
        source: &'a PinnedEndpoint,
        target: &'a PinnedEndpoint,
        cancel: &'a AtomicBool,
    ) -> super::Result<Self> {
        if live.native_kind() != Some(NativeOperationKind::DirectoryV1) {
            return Err(Failure {
                _cause: EndpointError::Unverified,
                _boundary: Boundary::Untouched,
                _os_error: None,
            });
        }
        Self::begin_connected_mode(live, source, target, cancel, false)
    }
    pub(super) fn begin_directory_recovery(
        context: &RecoveryContext,
        source: &'a PinnedEndpoint,
        target: &'a PinnedEndpoint,
        cancel: &'a AtomicBool,
    ) -> super::Result<Self> {
        if context.native_kind() != Some(NativeOperationKind::DirectoryV1) {
            return Err(Failure {
                _cause: EndpointError::Unverified,
                _boundary: Boundary::Untouched,
                _os_error: None,
            });
        }
        Self::begin_recovery_mode(context, source, target, cancel, false)
    }
    pub(super) fn admit_directory_descriptor(&mut self) -> super::Result<()> {
        if let Some(used) = &self.directory_requests {
            if charge_directory_request(used).is_err() {
                return Err(self.failure(EndpointError::LimitExceeded));
            }
        }
        Ok(())
    }
    pub(super) fn prove_directory_ancestors(&mut self, bytes: Option<&[u8]>) -> super::Result<()> {
        let admission = d::Admission::decode(bytes.ok_or(Failure {
            _cause: EndpointError::Unverified,
            _boundary: self.boundary,
            _os_error: None,
        })?)
        .map_err(|_| self.failure(EndpointError::Unverified))?;
        let binding = self.connected.as_ref().ok_or(Failure {
            _cause: EndpointError::Unverified,
            _boundary: self.boundary,
            _os_error: None,
        })?;
        if admission.connection != binding.id.connection.0
            || admission.operation != binding.id.serial
            || admission.revision.checked_add(1) != Some(binding.admission)
            || admission.fence != binding.fence
            || Some(admission.baseline) != binding.intent.expected_baseline
            || admission.path != binding.intent.path
            || usize::from(admission.direction) != binding.direction
            || admission.endpoints.get(binding.direction)
                != Some(&descriptor_endpoint(self.identities[0]))
            || admission.endpoints.get(1 - binding.direction)
                != Some(&descriptor_endpoint(self.identities[1]))
        {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        if admission.path.components().len() == 1 {
            return Ok(());
        }
        self.admit_ancestor_chain(admission.path.components().len())?;
        let (source, _) = self.pin_parent_sample(super::Side::Source, &admission.path)?;
        self.admit_ancestor_chain(admission.path.components().len())?;
        let (target, _) = self.pin_parent_sample(super::Side::Target, &admission.path)?;
        self.directory_ancestors = Some(DirectoryAncestors {
            admission,
            parents: [source, target],
            stability: None,
        });
        self.validate_directory_ancestors(false)
    }
    pub(super) fn validate_directory_ancestors(&mut self, start_window: bool) -> super::Result<()> {
        let Some(mut proof) = self.directory_ancestors.take() else {
            return Ok(());
        };
        let result = (|| {
            let mut stamps = [Vec::new(), Vec::new()];
            for (index, ((original, slot), side)) in proof
                .parents
                .iter()
                .zip(&mut stamps)
                .zip([super::Side::Source, super::Side::Target])
                .enumerate()
            {
                self.admit_ancestor_chain(proof.admission.path.components().len())?;
                self.parent_check(original)?;
                let (fresh, observed) = self.pin_parent_sample(side, &proof.admission.path)?;
                let logical = if index == 0 {
                    usize::from(proof.admission.direction)
                } else {
                    1 - usize::from(proof.admission.direction)
                };
                let expected = proof.admission.chains.get(logical).ok_or(Failure {
                    _cause: EndpointError::Unverified,
                    _boundary: self.boundary,
                    _os_error: None,
                })?;
                if original
                    .chain
                    .iter()
                    .map(descriptor_object)
                    .collect::<Vec<_>>()
                    != *expected
                    || fresh
                        .chain
                        .iter()
                        .map(descriptor_object)
                        .collect::<Vec<_>>()
                        != *expected
                    || proof
                        .stability
                        .as_ref()
                        .and_then(|expected| expected.get(index))
                        .is_some_and(|expected| *expected != observed)
                {
                    return Err(self.failure(EndpointError::IdentityChanged));
                }
                self.parent_check(original)?;
                self.parent_check(&fresh)?;
                *slot = observed;
            }
            if start_window && proof.stability.is_none() {
                proof.stability = Some(stamps);
            }
            self.check()
        })();
        self.directory_ancestors = Some(proof);
        result
    }
}

use super::{CStr, FileExt, NamespaceReservation, StepKind, destination_security, witness};
use sha2::Digest;
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DirectoryPhaseProof {
    PreparedStaged,
    OwnPublished,
}
struct DirectoryOwner<'a> {
    epoch: NativeEpoch<'a>,
    admission: d::Admission,
    source_parent: ParentGuard,
    target_parent: ParentGuard,
    source: File,
    namespace: NativeDirectory,
    namespace_identity: ObjectId,
    directory: Arc<File>,
    created: ObjectId,
    intent: Vec<u8>,
    stage: Vec<u8>,
    outcome: Option<Vec<u8>>,
    phase: DirectoryPhaseProof,
    reconstructed: bool,
}
/// Retains original objects and exclusive-created descriptor evidence until Prepared fsync.
pub(in crate::connection) struct DirectoryPrepared<'a> {
    owner: DirectoryOwner<'a>,
}
pub(in crate::connection) struct DirectoryOutcome<'a> {
    owner: DirectoryOwner<'a>,
}
pub(in crate::connection) struct DirectoryContinuation<'a> {
    owner: DirectoryOwner<'a>,
}

impl NativeEpoch<'_> {
    fn source_directory(
        &mut self,
        parent: &ParentGuard,
        expected: &super::d::Object,
    ) -> super::Result<File> {
        self.parent_check(parent)?;
        let file = self.io(open_at(
            parent.directory.file(),
            &parent.name,
            libc::O_RDONLY | libc::O_DIRECTORY,
        ))?;
        self.ep(endpoint::native_same_mount(parent.directory.file(), &file))?;
        self.ep(endpoint::native_directory_metadata(&file))?;
        if descriptor_object(&self.ep(object(&file))?) != *expected {
            return Err(self.failure(EndpointError::IdentityChanged));
        }
        self.parent_check(parent)?;
        Ok(file)
    }
    fn directory_namespace_security(&mut self, directory: &NativeDirectory) -> super::Result<()> {
        // Pinned directory descriptors may be O_PATH. NativeDirectory validates
        // metadata through a fresh readable handle and closes it before returning.
        self.ep(directory.validate(true))?;
        self.io(destination_security::check_private_file(directory.file()))?;
        if Stamp::new(&self.io(directory.file().metadata())?).mode != (libc::S_IFDIR | 0o700) {
            return Err(self.failure(EndpointError::Unsupported));
        }
        Ok(())
    }
    fn directory_stage_security(&mut self, file: &File) -> super::Result<()> {
        self.ep(endpoint::native_directory_metadata(file))?;
        self.io(destination_security::check_private_file(file))?;
        let stamp = Stamp::new(&self.io(file.metadata())?);
        if stamp.mode != (libc::S_IFDIR | 0o700) {
            return Err(self.failure(EndpointError::Unsupported));
        }
        Ok(())
    }
}
impl<'a> DirectoryPrepared<'a> {
    #[expect(
        clippy::too_many_lines,
        reason = "One ordered namespace/stage creation cut retains the original native owner"
    )]
    pub(in crate::connection::transaction) fn create(
        mut epoch: NativeEpoch<'a>,
        store: &ConnectionStore,
        live: &LiveOperation,
        reservation: &NamespaceReservation,
        session: &crate::connection::transaction::TransactionSession,
    ) -> super::Result<Self> {
        let cancel = epoch.cancel_read.ok_or(Failure {
            _cause: EndpointError::Unverified,
            _boundary: epoch.boundary,
            _os_error: None,
        })?;
        let source_parent = epoch.pin_parent(super::Side::Source, &live.intent().path)?;
        let target_parent = epoch.pin_parent(super::Side::Target, &live.intent().path)?;
        let step = session
            .admit_step(store, live, StepKind::Namespace, cancel)
            .map_err(|_| epoch.failure(EndpointError::Unverified))?;
        epoch.admitted(live, &step, StepKind::Namespace)?;
        let bytes = live.native_identity().ok_or(Failure {
            _cause: EndpointError::Unverified,
            _boundary: epoch.boundary,
            _os_error: None,
        })?;
        let admission =
            d::Admission::decode(bytes).map_err(|_| epoch.failure(EndpointError::Unverified))?;
        let header = reservation
            .header(live, epoch.nonce)
            .map_err(|_| epoch.failure(EndpointError::Unverified))?;
        let expected = admission
            .states
            .get(usize::from(admission.direction))
            .and_then(Option::as_ref)
            .ok_or(Failure {
                _cause: EndpointError::Unverified,
                _boundary: epoch.boundary,
                _os_error: None,
            })?;
        let source = epoch.source_directory(&source_parent, &expected.object)?;
        for (parent, index) in [
            (&source_parent, usize::from(admission.direction)),
            (&target_parent, 1 - usize::from(admission.direction)),
        ] {
            if admission.chains.get(index)
                != Some(
                    &parent
                        .chain
                        .iter()
                        .map(descriptor_object)
                        .collect::<Vec<_>>(),
                )
            {
                return Err(epoch.failure(EndpointError::IdentityChanged));
            }
        }
        let root = epoch.target_root.clone();
        let name = CString::new(reservation.name())
            .map_err(|_| epoch.failure(EndpointError::Unsupported))?;
        epoch.witness = Some(epoch.io(witness::NamespaceWitness::new_directory(
            &root,
            &target_parent,
            name.clone(),
            true,
        ))?);
        epoch.check_absent(&target_parent)?;
        epoch.parent_check(&source_parent)?;
        epoch.parent_check(&target_parent)?;
        if epoch.namespace_created {
            return Err(epoch.failure(EndpointError::Unverified));
        }
        epoch.admitted(live, &step, StepKind::Namespace)?;
        epoch.namespace_created = true;
        epoch.io(destination_security::create_directory(
            root.file(),
            OsStr::from_bytes(name.to_bytes()),
        ))?;
        let namespace = epoch.ep(root.child(OsStr::from_bytes(name.to_bytes()), true))?;
        epoch.witness_watch_namespace(&namespace)?;
        #[cfg(test)]
        test_point("namespace");
        epoch.directory_namespace_security(&namespace)?;
        let namespace_identity = epoch.ep(object(namespace.file()))?;
        epoch.witness_checkpoint()?;
        let body = d::Body::Intent(d::Intent {
            endpoints: admission.endpoints,
            direction: admission.direction,
            decision: admission.decision,
            namespace: descriptor_object(&namespace_identity),
            admission: super::Sha256::digest(bytes).into(),
            path: admission.path.clone(),
            chains: admission.chains.clone(),
            states: admission.states.clone(),
        });
        let intent =
            d::encode(&header, &body).map_err(|_| epoch.failure(EndpointError::Unverified))?;
        epoch.write_descriptor(&namespace, c"intent", &intent)?;
        #[cfg(test)]
        test_point("intent-descriptor");
        epoch.barriers(&namespace, &root)?;
        drop(step);
        let step = session
            .admit_step(store, live, StepKind::Stage, cancel)
            .map_err(|_| epoch.failure(EndpointError::Unverified))?;
        epoch.admitted(live, &step, StepKind::Stage)?;
        epoch.witness_create(c"stage")?;
        epoch.io(destination_security::create_directory(
            namespace.file(),
            OsStr::new("stage"),
        ))?;
        let directory = Arc::new(epoch.io(open_at(
            namespace.file(),
            c"stage",
            libc::O_RDONLY | libc::O_DIRECTORY,
        ))?);
        let created = epoch.ep(object(&directory))?;
        let result = epoch
            .witness
            .as_mut()
            .ok_or(Failure {
                _cause: EndpointError::Unverified,
                _boundary: epoch.boundary,
                _os_error: None,
            })?
            .watch_directory(Arc::clone(&directory), false);
        epoch.io(result)?;
        epoch.ep(endpoint::native_same_mount(namespace.file(), &directory))?;
        #[cfg(test)]
        test_point("stage-created");
        epoch.directory_stage_security(&directory)?;
        epoch.io(directory.sync_all())?;
        epoch.io(super::sync_directory(namespace.file()))?;
        epoch.witness_checkpoint()?;
        let intent_fact =
            d::decode(&intent).map_err(|_| epoch.failure(EndpointError::Unverified))?;
        let stage = d::encode(
            &header,
            &d::Body::Stage(d::Stage {
                intent: intent_fact.digest,
                created: descriptor_object(&created),
                source: expected.object.clone(),
                target_parent: descriptor_object(
                    &epoch.ep(object(target_parent.directory.file()))?,
                ),
            }),
        )
        .map_err(|_| epoch.failure(EndpointError::Unverified))?;
        epoch.write_descriptor(&namespace, c"staged-intent", &stage)?;
        #[cfg(test)]
        test_point("stage-descriptor");
        epoch.check()?;
        drop(step);
        Ok(Self {
            owner: DirectoryOwner {
                epoch,
                admission,
                source_parent,
                target_parent,
                source,
                namespace,
                namespace_identity,
                directory,
                created,
                intent,
                stage,
                outcome: None,
                phase: DirectoryPhaseProof::PreparedStaged,
                reconstructed: false,
            },
        })
    }
}

impl DirectoryOwner<'_> {
    fn metadata(&mut self) -> super::Result<()> {
        self.epoch.directory_namespace_security(&self.namespace)?;
        if self.epoch.ep(object(self.namespace.file()))? != self.namespace_identity
            || self.epoch.ep(object(&self.directory))? != self.created
        {
            return Err(self.epoch.failure(EndpointError::IdentityChanged));
        }
        self.epoch.parent_check(&self.source_parent)?;
        self.epoch.parent_check(&self.target_parent)?;
        self.epoch
            .ep(endpoint::native_directory_metadata(&self.source))?;
        let expected = self
            .admission
            .states
            .get(usize::from(self.admission.direction))
            .and_then(Option::as_ref)
            .ok_or(Failure {
                _cause: EndpointError::Unverified,
                _boundary: self.epoch.boundary,
                _os_error: None,
            })?;
        let source_name = self.epoch.io(open_at(
            self.source_parent.directory.file(),
            &self.source_parent.name,
            libc::O_PATH,
        ))?;
        if descriptor_object(&self.epoch.ep(object(&source_name))?) != expected.object
            || descriptor_object(&self.epoch.ep(object(&self.source))?) != expected.object
        {
            return Err(self.epoch.failure(EndpointError::IdentityChanged));
        }
        drop(source_name);
        self.epoch.directory_stage_security(&self.directory)?;
        let (parent, name) = if self.phase == DirectoryPhaseProof::PreparedStaged {
            (self.namespace.file(), c"stage")
        } else {
            (
                self.target_parent.directory.file(),
                self.target_parent.name.as_c_str(),
            )
        };
        let named = self.epoch.io(open_at(parent, name, libc::O_PATH))?;
        self.epoch
            .ep(endpoint::native_same_mount(parent, &self.directory))?;
        if self.epoch.ep(object(&named))? != self.created {
            return Err(self.epoch.failure(EndpointError::IdentityChanged));
        }
        drop(named); // Close both leaf observations before any nested provider sample.
        if self.phase == DirectoryPhaseProof::PreparedStaged {
            self.epoch.check_absent(&self.target_parent)?;
        } else {
            match open_at(self.namespace.file(), c"stage", libc::O_PATH) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Ok(_) => return Err(self.epoch.failure(EndpointError::IdentityChanged)),
                Err(error) => return self.epoch.io(Err(error)),
            }
        }
        self.epoch.check()
    }
    fn prepared_validation(&mut self) -> super::Result<()> {
        self.metadata()?;
        let intent =
            d::decode(&self.intent).map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
        let stage =
            d::decode(&self.stage).map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
        stage
            .joined(&intent)
            .map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
        self.epoch
            .read_descriptor(&self.namespace, c"intent", &self.intent)?;
        self.epoch
            .read_descriptor(&self.namespace, c"staged-intent", &self.stage)?;
        if let Some(bytes) = &self.outcome {
            d::decode(bytes)
                .and_then(|outcome| outcome.joined_outcome(&intent, &stage))
                .map_err(|_| self.epoch.failure(EndpointError::Unverified))?;
            self.epoch
                .read_descriptor(&self.namespace, c"outcome", bytes)?;
        }
        self.epoch.witness_checkpoint()?;
        self.metadata()
    }
}
impl DirectoryPrepared<'_> {
    pub(in crate::connection) fn validate(
        &mut self,
        live: &LiveOperation,
    ) -> Result<(), StoreError> {
        let result = (|| {
            self.owner.epoch.connected_check(live)?;
            if !self
                .owner
                .epoch
                .witness
                .as_ref()
                .is_some_and(witness::NamespaceWitness::created_preparation)
            {
                return Err(self.owner.epoch.failure(EndpointError::Unverified));
            }
            self.owner.prepared_validation()
        })();
        result.map_err(|_| StoreError::RecoveryRequired)
    }
    pub(in crate::connection) fn descriptors(&self) -> (Vec<u8>, Vec<u8>) {
        (self.owner.intent.clone(), self.owner.stage.clone())
    }
}

impl NativeEpoch<'_> {
    fn load_directory_descriptor(
        &mut self,
        directory: &NativeDirectory,
        name: &CStr,
    ) -> super::Result<Vec<u8>> {
        self.check()?;
        self.admit_directory_descriptor()?;
        let file = self.io(open_at(directory.file(), name, libc::O_RDONLY))?;
        self.ep(endpoint::native_same_mount(directory.file(), &file))?;
        self.io(destination_security::check_private_file(&file))?;
        self.ep(endpoint::native_regular_metadata(&file))?;
        let before = Stamp::new(&self.io(file.metadata())?);
        if before.length > super::d::MAX_BYTES as u64 {
            return Err(self.failure(EndpointError::LimitExceeded));
        }
        let mut bytes = vec![
            0;
            usize::try_from(before.length)
                .map_err(|_| self.failure(EndpointError::LimitExceeded))?
                + 1
        ];
        self.charge(bytes.len() as u64)?;
        let count = self.io(file.read_at(&mut bytes, 0))?;
        if count as u64 != before.length || Stamp::new(&self.io(file.metadata())?) != before {
            return Err(self.failure(EndpointError::Unstable));
        }
        drop(file); // The fresh snapshot may itself use two temporary fds.
        bytes.truncate(count);
        d::decode(&bytes).map_err(|_| self.failure(EndpointError::Unverified))?;
        self.read_descriptor(directory, name, &bytes)?;
        Ok(bytes)
    }
}
impl<'a> DirectoryContinuation<'a> {
    #[expect(
        clippy::too_many_lines,
        reason = "Restart reconstructs all descriptor and retained-object joins before exposing one owner"
    )]
    pub(in crate::connection::transaction) fn inspect(
        mut epoch: NativeEpoch<'a>,
        context: &RecoveryContext,
        record: &crate::connection::store::NativeNamespaceRecord,
    ) -> super::Result<Self> {
        epoch.recovery_check(context)?;
        record
            .validate_recovery(context)
            .map_err(|_| epoch.failure(EndpointError::Unverified))?;
        if record.operation() != context.id()
            || record.native_kind() != Some(NativeOperationKind::DirectoryV1)
        {
            return Err(epoch.failure(EndpointError::Unverified));
        }
        let (intent, stage) = record
            .descriptors()
            .map_err(|_| epoch.failure(EndpointError::Unverified))?;
        let stage = stage.ok_or(Failure {
            _cause: EndpointError::Unverified,
            _boundary: epoch.boundary,
            _os_error: None,
        })?;
        let admission_bytes = context.native_identity().ok_or(Failure {
            _cause: EndpointError::Unverified,
            _boundary: epoch.boundary,
            _os_error: None,
        })?;
        let admission = d::Admission::decode(admission_bytes)
            .map_err(|_| epoch.failure(EndpointError::Unverified))?;
        let before = d::decode(intent).map_err(|_| epoch.failure(EndpointError::Unverified))?;
        let prepared = d::decode(stage).map_err(|_| epoch.failure(EndpointError::Unverified))?;
        admission
            .joined(admission_bytes, &before)
            .map_err(|_| epoch.failure(EndpointError::Unverified))?;
        prepared
            .joined(&before)
            .map_err(|_| epoch.failure(EndpointError::Unverified))?;
        let d::Body::Intent(body) = &before.body else {
            return Err(epoch.failure(EndpointError::Unverified));
        };
        let d::Body::Stage(staged) = &prepared.body else {
            return Err(epoch.failure(EndpointError::Unverified));
        };
        let source_parent = epoch.pin_parent(super::Side::Source, &admission.path)?;
        let target_parent = epoch.pin_parent(super::Side::Target, &admission.path)?;
        for (parent, index) in [
            (&source_parent, usize::from(admission.direction)),
            (&target_parent, 1 - usize::from(admission.direction)),
        ] {
            if admission.chains.get(index)
                != Some(
                    &parent
                        .chain
                        .iter()
                        .map(descriptor_object)
                        .collect::<Vec<_>>(),
                )
            {
                return Err(epoch.failure(EndpointError::IdentityChanged));
            }
        }
        let root = epoch.target_root.clone();
        let name =
            CString::new(record.name()).map_err(|_| epoch.failure(EndpointError::Unverified))?;
        if before.header.namespace_name().as_bytes() != name.to_bytes() {
            return Err(epoch.failure(EndpointError::Unverified));
        }
        epoch.witness = Some(epoch.io(witness::NamespaceWitness::new_directory(
            &root,
            &target_parent,
            name.clone(),
            false,
        ))?);
        let namespace = epoch.ep(root.child(OsStr::from_bytes(name.to_bytes()), true))?;
        epoch.witness_watch_namespace(&namespace)?;
        let namespace_identity = epoch.ep(object(namespace.file()))?;
        if descriptor_object(&namespace_identity) != body.namespace {
            return Err(epoch.failure(EndpointError::IdentityChanged));
        }
        epoch.read_descriptor(&namespace, c"intent", intent)?;
        epoch.read_descriptor(&namespace, c"staged-intent", stage)?;
        let (directory, phase) = match open_at(
            namespace.file(),
            c"stage",
            libc::O_RDONLY | libc::O_DIRECTORY,
        ) {
            Ok(file) => (Arc::new(file), DirectoryPhaseProof::PreparedStaged),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (
                Arc::new(epoch.io(open_at(
                    target_parent.directory.file(),
                    &target_parent.name,
                    libc::O_RDONLY | libc::O_DIRECTORY,
                ))?),
                DirectoryPhaseProof::OwnPublished,
            ),
            Err(error) => return epoch.io(Err(error)),
        };
        let created = epoch.ep(object(&directory))?;
        if descriptor_object(&created) != staged.created {
            return Err(epoch.failure(EndpointError::IdentityChanged));
        }
        let watch = epoch
            .witness
            .as_mut()
            .ok_or(Failure {
                _cause: EndpointError::Unverified,
                _boundary: epoch.boundary,
                _os_error: None,
            })?
            .watch_directory(
                Arc::clone(&directory),
                phase == DirectoryPhaseProof::OwnPublished,
            );
        epoch.io(watch)?;
        let source = epoch.source_directory(&source_parent, &staged.source)?;
        let has_outcome = match open_at(namespace.file(), c"outcome", libc::O_PATH) {
            Ok(file) => {
                drop(file);
                true
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return epoch.io(Err(error)),
        };
        if has_outcome && phase == DirectoryPhaseProof::PreparedStaged
            || record.outcome().is_some() && !has_outcome
        {
            return Err(epoch.failure(EndpointError::Unverified));
        }
        let mut names = vec![b"intent".as_slice(), b"staged-intent"];
        if phase == DirectoryPhaseProof::PreparedStaged {
            names.push(b"stage");
        }
        if has_outcome {
            names.push(b"outcome");
        }
        let set = epoch
            .witness
            .as_mut()
            .ok_or(Failure {
                _cause: EndpointError::Unverified,
                _boundary: epoch.boundary,
                _os_error: None,
            })?
            .set_inventory(&names);
        epoch.io(set)?;
        let outcome = if has_outcome {
            let bytes = epoch.load_directory_descriptor(&namespace, c"outcome")?;
            d::decode(&bytes)
                .and_then(|outcome| outcome.joined_outcome(&before, &prepared))
                .map_err(|_| epoch.failure(EndpointError::Unverified))?;
            if record
                .outcome()
                .is_some_and(|registered| registered != bytes)
            {
                return Err(epoch.failure(EndpointError::IdentityChanged));
            }
            Some(bytes)
        } else {
            None
        };
        epoch.namespace_created = true;
        let mut value = Self {
            owner: DirectoryOwner {
                epoch,
                admission,
                source_parent,
                target_parent,
                source,
                namespace,
                namespace_identity,
                directory,
                created,
                intent: intent.to_vec(),
                stage: stage.to_vec(),
                outcome,
                phase,
                reconstructed: has_outcome,
            },
        };
        value
            .validate(context)
            .map_err(|_| value.owner.epoch.failure(EndpointError::Unverified))?;
        Ok(value)
    }
    pub(in crate::connection) fn validate(
        &mut self,
        context: &RecoveryContext,
    ) -> Result<(), StoreError> {
        let result = (|| {
            self.owner.epoch.recovery_check(context)?;
            self.owner.prepared_validation()?;
            Ok(())
        })();
        result.map_err(|_: Failure| StoreError::RecoveryRequired)
    }
    fn validate_live(&mut self, live: &LiveOperation) -> super::Result<()> {
        self.owner.epoch.connected_check(live)?;
        self.owner.prepared_validation()
    }
    pub(in crate::connection::transaction) fn complete(
        mut self,
        store: &ConnectionStore,
        live: &LiveOperation,
        session: &crate::connection::transaction::TransactionSession,
        cancel: &AtomicBool,
    ) -> Result<DirectoryOutcome<'a>, StoreError> {
        self.validate_live(live)
            .map_err(|_| StoreError::RecoveryRequired)?;
        // These branches cannot exchange permits: only staged work can request Publish.
        match self.owner.phase {
            DirectoryPhaseProof::PreparedStaged => {
                let step = session.admit_step(store, live, StepKind::Publish, cancel)?;
                let result = (|| {
                    self.owner.epoch.admitted(live, &step, StepKind::Publish)?;
                    self.validate_live(live)?; // New full prepublication cut after admission.
                    self.owner.epoch.admitted(live, &step, StepKind::Publish)?;
                    self.owner.epoch.witness_move(true)?;
                    self.owner.epoch.io(super::rename_noreplace(
                        self.owner.namespace.file(),
                        c"stage",
                        self.owner.target_parent.directory.file(),
                        &self.owner.target_parent.name,
                    ))?;
                    self.owner.phase = DirectoryPhaseProof::OwnPublished;
                    self.owner.metadata()?;
                    self.owner.epoch.io(self.owner.directory.sync_all())?;
                    self.owner
                        .epoch
                        .barriers(&self.owner.namespace, &self.owner.target_parent.directory)?;
                    self.owner.epoch.witness_checkpoint()?;
                    self.owner.metadata()?;
                    self.owner.epoch.boundary = Boundary::Published;
                    #[cfg(test)]
                    test_point("published-without-outcome");
                    self.make_outcome(live)
                })();
                drop(step);
                result.map_err(|_| StoreError::RecoveryRequired)?;
            }
            DirectoryPhaseProof::OwnPublished => {
                let permit = session.admit_directory_completion(store, live, cancel)?;
                permit.validate(live)?;
                self.make_outcome(live)
                    .map_err(|_| StoreError::RecoveryRequired)?;
                permit.validate(live)?;
                drop(permit);
            }
        }
        let mut outcome = DirectoryOutcome { owner: self.owner };
        outcome.validate(live)?;
        Ok(outcome)
    }
    fn make_outcome(&mut self, live: &LiveOperation) -> super::Result<()> {
        self.owner.metadata()?;
        self.owner.epoch.io(self.owner.directory.sync_all())?;
        self.owner
            .epoch
            .barriers(&self.owner.namespace, &self.owner.target_parent.directory)?;
        if self.owner.outcome.is_none() {
            let before = d::decode(&self.owner.intent)
                .map_err(|_| self.owner.epoch.failure(EndpointError::Unverified))?;
            let stage = d::decode(&self.owner.stage)
                .map_err(|_| self.owner.epoch.failure(EndpointError::Unverified))?;
            let mut header = before.header.clone();
            header.epoch = self.owner.epoch.nonce;
            header.session = live.native_session();
            header.progress = live.revision();
            let source = descriptor_object(&self.owner.epoch.ep(object(&self.owner.source))?);
            let target = descriptor_object(&self.owner.created);
            let states = if self.owner.admission.direction == 0 {
                [source, target]
            } else {
                [target, source]
            };
            let bytes = d::encode(
                &header,
                &d::Body::Outcome(d::Outcome {
                    intent: before.digest,
                    stage: stage.digest,
                    states,
                    sequence: 1,
                }),
            )
            .map_err(|_| self.owner.epoch.failure(EndpointError::Unverified))?;
            self.owner
                .epoch
                .write_descriptor(&self.owner.namespace, c"outcome", &bytes)?;
            self.owner.outcome = Some(bytes);
            #[cfg(test)]
            test_point("outcome-descriptor");
        }
        self.owner.metadata()
    }
}

impl DirectoryOutcome<'_> {
    pub(in crate::connection) fn validate(
        &mut self,
        live: &LiveOperation,
    ) -> Result<(), StoreError> {
        let result = (|| {
            self.owner.epoch.connected_check(live)?;
            if self.owner.phase != DirectoryPhaseProof::OwnPublished {
                return Err(self.owner.epoch.failure(EndpointError::Unverified));
            }
            self.owner.metadata()?;
            let intent = d::decode(&self.owner.intent)
                .map_err(|_| self.owner.epoch.failure(EndpointError::Unverified))?;
            let stage = d::decode(&self.owner.stage)
                .map_err(|_| self.owner.epoch.failure(EndpointError::Unverified))?;
            let bytes = self.owner.outcome.as_ref().ok_or(Failure {
                _cause: EndpointError::Unverified,
                _boundary: self.owner.epoch.boundary,
                _os_error: None,
            })?;
            let outcome = d::decode(bytes)
                .map_err(|_| self.owner.epoch.failure(EndpointError::Unverified))?;
            outcome
                .joined_outcome(&intent, &stage)
                .map_err(|_| self.owner.epoch.failure(EndpointError::Unverified))?;
            if outcome.header.progress > live.revision() {
                return Err(self.owner.epoch.failure(EndpointError::Unverified));
            }
            self.owner
                .epoch
                .read_descriptor(&self.owner.namespace, c"outcome", bytes)?;
            self.owner.epoch.witness_checkpoint()?;
            self.owner.metadata()
        })();
        result.map_err(|_| StoreError::RecoveryRequired)
    }
    pub(in crate::connection) fn facts(&self) -> Result<(u32, Vec<u8>), StoreError> {
        let intent = d::decode(&self.owner.intent).map_err(|_| StoreError::Corrupt)?;
        Ok((
            u32::try_from(intent.header.attempt).map_err(|_| StoreError::Corrupt)?,
            self.owner
                .outcome
                .clone()
                .ok_or(StoreError::RecoveryRequired)?,
        ))
    }
    pub(in crate::connection) fn reconstructed(&self) -> bool {
        self.owner.reconstructed
    }
    pub(in crate::connection) fn deadline(&self) -> Instant {
        self.owner.epoch.deadline
    }
    pub(in crate::connection::transaction::native) fn observe_leaf(
        &mut self,
        store: &ConnectionStore,
        live: &LiveOperation,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        store.check_native(live)?;
        planning_check(self.owner.epoch.deadline, cancel).map_err(|_| StoreError::Cancelled)?;
        let result = (|| {
            for side in [super::Side::Source, super::Side::Target] {
                let path = &self.owner.admission.path;
                if path.components().len() > 1 {
                    self.owner
                        .epoch
                        .admit_ancestor_chain(path.components().len())?;
                }
                let (parent, _) = self.owner.epoch.pin_parent_sample(side, path)?;
                let logical = if side == super::Side::Source {
                    usize::from(self.owner.admission.direction)
                } else {
                    1 - usize::from(self.owner.admission.direction)
                };
                if self.owner.admission.chains.get(logical)
                    != Some(
                        &parent
                            .chain
                            .iter()
                            .map(descriptor_object)
                            .collect::<Vec<_>>(),
                    )
                {
                    return Err(self.owner.epoch.failure(EndpointError::IdentityChanged));
                }
                let expected = if side == super::Side::Source {
                    self.owner
                        .admission
                        .states
                        .get(logical)
                        .and_then(Option::as_ref)
                        .ok_or(Failure {
                            _cause: EndpointError::Unverified,
                            _boundary: self.owner.epoch.boundary,
                            _os_error: None,
                        })?
                        .object
                        .clone()
                } else {
                    descriptor_object(&self.owner.created)
                };
                let file = self.owner.epoch.source_directory(&parent, &expected)?;
                let before = Stamp::new(&self.owner.epoch.io(file.metadata())?);
                self.owner
                    .epoch
                    .ep(endpoint::native_directory_metadata(&file))?;
                if Stamp::new(&self.owner.epoch.io(file.metadata())?) != before {
                    return Err(self.owner.epoch.failure(EndpointError::Unstable));
                }
                drop(file);
                self.owner.epoch.parent_check(&parent)?;
            }
            self.owner.epoch.connected_check(live)
        })();
        result.map_err(|_| StoreError::RecoveryRequired)
    }
    pub(in crate::connection::transaction::native) fn comparison_ancestors(
        &mut self,
        store: &ConnectionStore,
        live: &LiveOperation,
        cancel: &AtomicBool,
    ) -> Result<Option<crate::connection::store::DirectoryAncestorPatchV1>, StoreError> {
        self.owner
            .epoch
            .connected_check(live)
            .map_err(|_| StoreError::RecoveryRequired)?;
        self.owner
            .epoch
            .validate_directory_ancestors(true)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let receipt = store.native_directory_ancestor_patch(live, self.deadline(), cancel)?;
        if let Some(receipt) = &receipt {
            let proof = self
                .owner
                .epoch
                .directory_ancestors
                .as_ref()
                .ok_or(StoreError::RecoveryRequired)?;
            let bytes = proof.admission.encode().map_err(|_| StoreError::Corrupt)?;
            if receipt.admission != <[u8; 32]>::from(super::Sha256::digest(bytes)) {
                return Err(StoreError::RecoveryRequired);
            }
        }
        self.owner
            .epoch
            .connected_check(live)
            .map_err(|_| StoreError::RecoveryRequired)?;
        Ok(receipt)
    }
    #[cfg(test)]
    pub(in crate::connection::transaction::native) fn revoke_for_test(&mut self) {
        self.owner.epoch.leases.revoke();
    }
}

impl super::super::NativeOwnedNames {
    fn read_directory_clue(
        &self,
        record: &crate::connection::store::NativeNamespaceRecord,
        directory: &NativeDirectory,
        name: &std::ffi::CStr,
    ) -> Result<Vec<u8>, EndpointError> {
        if let Some((operation, requests)) = &self.directory_requests {
            if requests.load(std::sync::atomic::Ordering::Acquire) > 128 {
                return Err(EndpointError::LimitExceeded);
            }
            if *operation == record.operation() {
                charge_directory_request(requests)?;
            }
        }
        let bytes = self.read_clue_bytes(directory, name)?;
        d::decode(&bytes).map_err(|_| EndpointError::Unverified)?;
        Ok(bytes)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Exact Directory namespace and descriptor inventory is one observer cut"
    )]
    pub(super) fn validate_directory_record(
        &self,
        root: &NativeDirectory,
        record: &crate::connection::store::NativeNamespaceRecord,
    ) -> Result<super::OwnedSnapshot, EndpointError> {
        self.charge(0)?;
        root.validate(false)?;
        if object(root.file())? != self.root {
            return Err(EndpointError::IdentityChanged);
        }
        let name = record.name();
        let directory = root.child(OsStr::new(&name), false)?;
        let snapshot = super::OwnedSnapshot::capture(directory.clone())?;
        destination_security::check_private_file(directory.file())?;
        if Stamp::new(&directory.file().metadata()?).mode != (libc::S_IFDIR | 0o700) {
            return Err(EndpointError::Unsupported);
        }
        let (intent_bytes, stage_bytes) = record
            .descriptors()
            .map_err(|_| EndpointError::Unverified)?;
        let intent = d::decode(intent_bytes).map_err(|_| EndpointError::Unverified)?;
        let d::Body::Intent(body) = &intent.body else {
            return Err(EndpointError::Unverified);
        };
        if intent.header.namespace_name() != name
            || body.endpoints.get(usize::from(record.side())) != Some(&self.endpoint)
            || body
                .chains
                .get(usize::from(record.side()))
                .and_then(|v| v.first())
                != Some(&descriptor_object(&self.root))
            || descriptor_object(&object(directory.file())?) != body.namespace
            || self.read_directory_clue(record, &directory, c"intent")? != intent_bytes
        {
            return Err(EndpointError::IdentityChanged);
        }
        let stage_bytes = stage_bytes.ok_or(EndpointError::Unverified)?;
        let stage = d::decode(stage_bytes).map_err(|_| EndpointError::Unverified)?;
        stage
            .joined(&intent)
            .map_err(|_| EndpointError::Unverified)?;
        if self.read_directory_clue(record, &directory, c"staged-intent")? != stage_bytes {
            return Err(EndpointError::IdentityChanged);
        }
        let d::Body::Stage(staged) = &stage.body else {
            return Err(EndpointError::Unverified);
        };
        let readable = open_at(directory.file(), c".", libc::O_RDONLY | libc::O_DIRECTORY)?;
        if object(&readable)? != object(directory.file())? {
            return Err(EndpointError::IdentityChanged);
        }
        let mut names = std::collections::BTreeSet::new();
        for child in cap_std::fs::Dir::from_std_file(readable).entries()? {
            self.charge(0)?;
            let name = child?.file_name();
            if names.len() >= 3
                || !matches!(
                    name.as_bytes(),
                    b"intent" | b"staged-intent" | b"stage" | b"outcome"
                )
            {
                return Err(EndpointError::Unverified);
            }
            names.insert(name);
        }
        let staged_names = ["intent", "staged-intent", "stage"]
            .map(std::ffi::OsString::from)
            .into_iter()
            .collect();
        let published_names = ["intent", "staged-intent", "outcome"]
            .map(std::ffi::OsString::from)
            .into_iter()
            .collect();
        if names == staged_names {
            if record.outcome().is_some() {
                return Err(EndpointError::Unverified);
            }
            let file = open_at(
                directory.file(),
                c"stage",
                libc::O_RDONLY | libc::O_DIRECTORY,
            )?;
            endpoint::native_directory_metadata(&file)?;
            endpoint::native_same_mount(directory.file(), &file)?;
            destination_security::check_private_file(&file)?;
            let before = Stamp::new(&file.metadata()?);
            if descriptor_object(&object(&file)?) != staged.created
                || before.mode != (libc::S_IFDIR | 0o700)
            {
                return Err(EndpointError::IdentityChanged);
            }
            // Observation requires a fresh exact empty stage; no published children are traversed.
            let readable = open_at(&file, c".", libc::O_RDONLY | libc::O_DIRECTORY)?;
            if cap_std::fs::Dir::from_std_file(readable)
                .entries()?
                .next()
                .transpose()?
                .is_some()
                || Stamp::new(&file.metadata()?) != before
            {
                return Err(EndpointError::Unstable);
            }
        } else if names == published_names {
            let bytes = self.read_directory_clue(record, &directory, c"outcome")?;
            d::decode(&bytes)
                .and_then(|outcome| outcome.joined_outcome(&intent, &stage))
                .map_err(|_| EndpointError::Unverified)?;
            if record.outcome().is_some_and(|saved| saved != bytes) {
                return Err(EndpointError::IdentityChanged);
            }
        } else {
            return Err(EndpointError::Unverified);
        }
        #[cfg(test)]
        super::after_owned_reads();
        snapshot.validate()?;
        root.validate(false)?;
        self.charge(0)?;
        Ok(snapshot)
    }
}

/// Absence of every reserved namespace plus the original source generation.
pub(in crate::connection) struct DirectoryUntouched<'a> {
    epoch: NativeEpoch<'a>,
    paths: [DirectoryPath; 2],
    admission: d::Admission,
    records: Vec<crate::connection::store::NativeNamespaceRecord>,
}
impl<'a> DirectoryUntouched<'a> {
    fn inspect(
        mut epoch: NativeEpoch<'a>,
        context: &RecoveryContext,
        records: Vec<crate::connection::store::NativeNamespaceRecord>,
        cancel: &AtomicBool,
    ) -> Result<Self, StoreError> {
        epoch
            .recovery_check(context)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let admission = d::Admission::decode(
            context
                .native_identity()
                .ok_or(StoreError::RecoveryRequired)?,
        )
        .map_err(|_| StoreError::Corrupt)?;
        if records.len() > 8 {
            return Err(StoreError::LimitExceeded);
        }
        let paths = [
            DirectoryPath::capture(epoch.source, &admission.path, true, epoch.deadline, cancel)?,
            DirectoryPath::capture(epoch.target, &admission.path, false, epoch.deadline, cancel)?,
        ];
        let mut value = Self {
            epoch,
            paths,
            admission,
            records,
        };
        #[cfg(test)]
        test_point("untouched-captured");
        value.validate(context)?;
        Ok(value)
    }
    pub(in crate::connection) fn validate(
        &mut self,
        context: &RecoveryContext,
    ) -> Result<(), StoreError> {
        self.epoch
            .recovery_check(context)
            .map_err(|_| StoreError::RecoveryRequired)?;
        let cancel = self.epoch.cancel_read.ok_or(StoreError::RecoveryRequired)?;
        for (index, path) in self.paths.iter().enumerate() {
            path.validate(self.epoch.deadline, cancel)
                .map_err(|_| StoreError::RecoveryRequired)?;
            let logical = if index == 0 {
                usize::from(self.admission.direction)
            } else {
                1 - usize::from(self.admission.direction)
            };
            // History binds the source generation, not its old child-change timestamps.
            // The fresh path above retains and validates its own full stamp without refresh.
            let original = self
                .admission
                .states
                .get(logical)
                .ok_or(StoreError::Corrupt)?;
            if self.admission.chains.get(logical) != Some(&path.chain)
                || original.as_ref().map(|leaf| &leaf.object)
                    != path.state().as_ref().map(|leaf| &leaf.object)
            {
                return Err(StoreError::IdentityChanged);
            }
        }
        for record in &self.records {
            if record.native_kind() != Some(NativeOperationKind::DirectoryV1)
                || !record.uncreated_for(context)?
            {
                return Err(StoreError::RecoveryRequired);
            }
            let name = CString::new(record.name()).map_err(|_| StoreError::Corrupt)?;
            match open_at(self.epoch.target_root.file(), &name, libc::O_PATH) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                _ => return Err(StoreError::RecoveryRequired),
            }
        }
        self.epoch
            .recovery_check(context)
            .map_err(|_| StoreError::RecoveryRequired)
    }
}
enum DirectoryBoundaryOwner<'a> {
    Untouched(DirectoryUntouched<'a>),
    Prepared(DirectoryContinuation<'a>),
}
pub(in crate::connection) struct DirectoryBoundary<'a> {
    owner: DirectoryBoundaryOwner<'a>,
}
impl DirectoryBoundary<'_> {
    pub(in crate::connection) fn validate(
        &mut self,
        context: &RecoveryContext,
    ) -> Result<crate::connection::store::DirectoryBoundaryFact, StoreError> {
        use crate::connection::store::DirectoryBoundaryFact as Fact;
        match &mut self.owner {
            DirectoryBoundaryOwner::Untouched(proof) => {
                proof.validate(context)?;
                Ok(Fact::Untouched)
            }
            DirectoryBoundaryOwner::Prepared(proof) => {
                proof.validate(context)?;
                let owner = &mut proof.owner;
                owner
                    .epoch
                    .io(owner.directory.sync_all())
                    .map_err(|_| StoreError::RecoveryRequired)?;
                owner
                    .epoch
                    .barriers(&owner.namespace, &owner.target_parent.directory)
                    .map_err(|_| StoreError::RecoveryRequired)?;
                // Accounting never creates an absent outcome or obtains a publication permit.
                let intent = d::decode(&owner.intent).map_err(|_| StoreError::Corrupt)?;
                let attempt =
                    u32::try_from(intent.header.attempt).map_err(|_| StoreError::Corrupt)?;
                let fact = match owner.phase {
                    DirectoryPhaseProof::PreparedStaged => Fact::Staged { attempt },
                    DirectoryPhaseProof::OwnPublished => Fact::Published {
                        attempt,
                        outcome: owner.outcome.clone(),
                    },
                };
                proof.validate(context)?;
                Ok(fact)
            }
        }
    }
}

#[cfg(test)]
pub(in crate::connection) fn execute_next_directory(
    store: &mut ConnectionStore,
    id: crate::connection::model::ConnectionId,
    session: &crate::connection::transaction::TransactionSession,
    left: &PinnedEndpoint,
    right: &PinnedEndpoint,
    cancel: &AtomicBool,
) -> crate::connection::transaction::TransactionOutcome {
    let result = super::execute_next_inner_mode(store, id, session, left, right, cancel, true);
    result.unwrap_or_else(|error| super::executor_error(store, id, error))
}
pub(super) fn execute_admitted(
    store: &mut ConnectionStore,
    mut live: LiveOperation,
    session: &crate::connection::transaction::TransactionSession,
    left: &PinnedEndpoint,
    right: &PinnedEndpoint,
    cancel: &AtomicBool,
) -> super::ExecutorResult {
    let reverse = crate::connection::transaction::directory::addition_side(live.intent())?
        == Side::Destination;
    let (source, target) = if reverse {
        (right, left)
    } else {
        (left, right)
    };
    let epoch = NativeEpoch::begin_directory(&live, source, target, cancel)
        .map_err(|_| StoreError::RecoveryRequired)?;
    let reservation = store.reserve_executor_namespace(
        &mut live,
        if reverse {
            Side::Source
        } else {
            Side::Destination
        },
    )?;
    store.preflight_directory_effects(&live)?;
    #[cfg(test)]
    test_point("namespace-reserved");
    let proof = DirectoryPrepared::create(epoch, store, &live, &reservation, session)
        .map_err(|_| StoreError::RecoveryRequired)?;
    store.prepare_native_directory(&mut live, reservation, proof)?;
    #[cfg(test)]
    test_point("prepared");
    // Preparation's owner has been consumed/dropped. Publication requires a fresh epoch.
    super::execute_pending(store, live.id(), session, left, right, cancel, false)
}
pub(super) fn execute_pending(
    store: &mut ConnectionStore,
    context: &RecoveryContext,
    session: &crate::connection::transaction::TransactionSession,
    left: &PinnedEndpoint,
    right: &PinnedEndpoint,
    cancel: &AtomicBool,
) -> super::ExecutorResult {
    use crate::connection::{store::ReceiptStage, transaction::TransactionOutcome};
    let id = context.id();
    let reverse = crate::connection::transaction::directory::addition_side(context.intent())?
        == Side::Destination;
    let (source, target) = if reverse {
        (right, left)
    } else {
        (left, right)
    };
    let records: Vec<_> = store
        .recovery_namespace_records(context)?
        .into_iter()
        .filter(|record| record.operation() == id)
        .collect();
    let prepared = records
        .iter()
        .filter(|record| record.descriptors().is_ok())
        .count();
    let epoch = NativeEpoch::begin_directory_recovery(context, source, target, cancel)
        .map_err(|_| StoreError::RecoveryRequired)?;
    if prepared == 0 {
        let mut proof = DirectoryUntouched::inspect(epoch, context, records, cancel)?;
        if context.recovery_only() {
            let mut boundary = DirectoryBoundary {
                owner: DirectoryBoundaryOwner::Untouched(proof),
            };
            store.record_directory_boundary(context, &mut boundary, cancel)?;
            return Ok(TransactionOutcome::AccountedPending {
                operation: Some(store.executor_receipt(id)?),
            });
        }
        let live = store.resume_directory_untouched(context, &mut proof, session, cancel)?;
        drop(proof);
        return execute_admitted(store, live, session, left, right, cancel);
    }
    if prepared != 1 || records.len() != 1 {
        return Err(StoreError::RecoveryRequired);
    }
    let record = records.first().ok_or(StoreError::RecoveryRequired)?;
    let mut proof = DirectoryContinuation::inspect(epoch, context, record)
        .map_err(|_| StoreError::RecoveryRequired)?;
    if context.recovery_only() {
        let mut boundary = DirectoryBoundary {
            owner: DirectoryBoundaryOwner::Prepared(proof),
        };
        store.record_directory_boundary(context, &mut boundary, cancel)?;
        return Ok(TransactionOutcome::AccountedPending {
            operation: Some(store.executor_receipt(id)?),
        });
    }
    let mut live = store.resume_native_directory(context, &mut proof, session, cancel)?;
    let mut outcome = proof.complete(store, &live, session, cancel)?;
    if !matches!(
        store.executor_receipt(id)?.stage,
        ReceiptStage::Published | ReceiptStage::Verified
    ) {
        store.record_directory_outcome(&mut live, &mut outcome, false, cancel)?;
        #[cfg(test)]
        test_point("published-record");
    }
    if store.executor_receipt(id)?.stage != ReceiptStage::Verified {
        store.record_directory_outcome(&mut live, &mut outcome, true, cancel)?;
        #[cfg(test)]
        test_point("verified-record");
    }
    let mut comparison = outcome.comparison(store, &live, cancel)?;
    let candidate = store.prepare_directory_baseline(&live, &mut comparison, cancel)?;
    store.commit_directory_baseline(&mut live, candidate, &mut comparison, cancel)?;
    drop(comparison);
    drop(outcome);
    super::executor_verified(store, id, left, right, cancel)
}

#[cfg(test)]
type DirectoryTestHook = Box<dyn FnMut(&str)>;
#[cfg(test)]
thread_local! {static DIRECTORY_TEST_HOOK:std::cell::RefCell<Option<DirectoryTestHook>>=const{std::cell::RefCell::new(None)};}
#[cfg(test)]
pub(super) fn test_point(point: &str) {
    DIRECTORY_TEST_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().as_mut() {
            hook(point);
        }
    });
}
#[cfg(test)]
mod tests;

fn charge_directory_request(used: &std::sync::atomic::AtomicU16) -> Result<(), EndpointError> {
    use std::sync::atomic::Ordering;
    let previous = used
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            Some(count.saturating_add(1).min(129))
        })
        .map_err(|_| EndpointError::LimitExceeded)?;
    if previous >= 128 {
        Err(EndpointError::LimitExceeded)
    } else {
        Ok(())
    }
}
