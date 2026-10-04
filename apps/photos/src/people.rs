//! Device-local People recognition and review.
//!
//! This module is deliberately *not* part of [`crate::doc`]. Face embeddings,
//! geometry, crop pixels, and even the existence of an unconfirmed proposal
//! are sensitive device-local state. The only bridge to collaboration is
//! [`PeopleController::approve_membership`], which produces a compact,
//! user-approved [`crate::actions::PhotosAction::SetPersonMembership`].
//!
//! A generic image-description model is not a face-recognition model. Until a
//! signed local face-clustering capability is installed, this controller stays
//! honest: it can manage imported or manually created proposals, but it never
//! claims to have recognized a person from a photograph.

use std::collections::{BTreeMap, BTreeSet};

use crate::actions::PhotosAction;
use crate::model::PhotoLibrary;

/// One explicitly shared person as a presentation-ready collection. It is
/// derived from durable labels only, so it is safe to use for a resource rail,
/// global search, history, and collaboration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedPerson {
    pub label: String,
    pub photo_ids: Vec<String>,
}

impl SharedPerson {
    #[must_use]
    pub fn count(&self) -> usize {
        self.photo_ids.len()
    }
}

/// Build the approved People directory from collaborative labels. This is
/// intentionally distinct from `PeopleController::visible_clusters`: a local
/// proposal does not appear here until somebody has reviewed and approved it.
#[must_use]
pub fn approved_people(library: &PhotoLibrary) -> Vec<SharedPerson> {
    let mut people = BTreeMap::<String, Vec<String>>::new();
    for photo in &library.photos {
        for label in &photo.people {
            people
                .entry(label.clone())
                .or_default()
                .push(photo.id.clone());
        }
    }
    people
        .into_iter()
        .map(|(label, photo_ids)| SharedPerson { label, photo_ids })
        .collect()
}

/// Privacy state for the device-local face-cluster pipeline.
///
/// This is suitable for a local settings store, but must never be written to
/// the Photos CRDT: one collaborator's opt-in is not another's consent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct PeoplePrivacy {
    /// Whether the owner has explicitly allowed local face clustering on this
    /// device. It defaults to off, including after migration from older apps.
    pub recognition_enabled: bool,
}

/// What this device can honestly do with face clusters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FaceCapability {
    /// The owner has not opted in; no local scan should begin.
    #[default]
    DisabledByOwner,
    /// The owner opted in but no separately verified local face model exists.
    /// Generic OCR/vision capabilities never satisfy this case.
    NoVerifiedFaceModel,
    /// A signed, local-only model was explicitly discovered by a caller that
    /// owns the runtime integration. The controller records no endpoint or
    /// credential, only the auditable identity needed in the local UI.
    Ready { model_id: String, revision: String },
}

impl FaceCapability {
    /// Return a calm, non-technical explanation appropriate for an inline UI.
    #[must_use]
    pub fn summary(&self) -> String {
        crate::strings::tr(match self {
            Self::DisabledByOwner => "photos-people-state-off",
            Self::NoVerifiedFaceModel => "photos-people-state-no-model",
            Self::Ready { .. } => "photos-people-state-ready",
        })
    }

    #[must_use]
    pub fn can_cluster(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

/// Opaque output from a real, verified local face pipeline. The detection
/// rectangles and embeddings remain inside the pipeline's private store; the
/// Photos controller needs only stable photo ids to offer an on-device review.
///
/// This is intentionally not constructible from a generic vision response.
/// Callers must validate the signed face-model identity before passing it in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedFaceClusterBatch {
    pub model_id: String,
    pub revision: String,
    pub clusters: Vec<VerifiedFaceCluster>,
}

/// One opaque cluster from [`VerifiedFaceClusterBatch`]. No confidence, face
/// geometry, crop, or embedding appears in this application-level handoff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedFaceCluster {
    pub id: String,
    pub photo_ids: Vec<String>,
}

/// A non-biometric reference to one photograph in a private local cluster.
///
/// Deliberately absent: face rectangle, confidence, embedding, crop, image
/// bytes, path, and any model output. A runtime that needs those keeps them in
/// its own ephemeral/private cache keyed by this opaque reference.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, serde::Serialize)]
pub struct LocalClusterPhoto {
    pub photo_id: String,
}

/// How a proposal entered the local People controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum ProposalOrigin {
    /// A separately capability-gated local face pipeline supplied this opaque
    /// grouping. The controller never receives its embedding or geometry.
    VerifiedLocalPipeline,
    /// A person grouped the photographs manually or imported a proposal.
    ImportedOrManual,
}

impl ProposalOrigin {
    #[must_use]
    pub fn label(self) -> String {
        crate::strings::tr(match self {
            Self::VerifiedLocalPipeline => "photos-people-local-suggestion",
            Self::ImportedOrManual => "photos-people-imported-suggestion",
        })
    }
}

/// A locally held candidate person. This is intentionally a management record
/// rather than a remote identity: it is free to be renamed, split, hidden,
/// rejected, or deleted without producing document history.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct LocalPersonCluster {
    pub id: String,
    pub display_name: Option<String>,
    pub photos: BTreeSet<LocalClusterPhoto>,
    pub origin: ProposalOrigin,
    pub hidden: bool,
    pub rejected: bool,
}

impl LocalPersonCluster {
    #[must_use]
    pub fn is_proposal(&self) -> bool {
        self.display_name.is_none() && !self.rejected
    }

    #[must_use]
    pub fn visible_photo_ids(&self) -> Vec<String> {
        self.photos
            .iter()
            .map(|photo| photo.photo_id.clone())
            .collect()
    }
}

/// Result of an interaction with a local proposal. These values are intended
/// for accessible status announcements and never name a remote person.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeopleChange {
    Updated,
    Deleted,
    Unchanged,
}

/// Local People state. Its owner is the running app/local settings store; it
/// must never be serialized by `PhotosDoc` or offered as a Resource.
#[derive(Clone, Debug, Default)]
pub struct PeopleController {
    privacy: PeoplePrivacy,
    capability: FaceCapability,
    clusters: BTreeMap<String, LocalPersonCluster>,
    storage_path: Option<crate::files::StateFile>,
}

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
struct PersistedPeople {
    privacy: PeoplePrivacy,
    clusters: BTreeMap<String, LocalPersonCluster>,
}

impl PeopleController {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Load the private review queue for this device. The file contains only
    /// consent, opaque cluster ids, and photo resource ids. Model identity,
    /// embeddings, face geometry, crops, and confidence are never persisted.
    #[must_use]
    pub fn persistent() -> Self {
        Self::load_from(crate::files::StateFile::in_state(
            "photos/people-review.json",
        ))
    }

    fn load_from(storage_path: Option<crate::files::StateFile>) -> Self {
        let persisted = storage_path
            .as_ref()
            .and_then(crate::files::StateFile::read)
            .and_then(|bytes| serde_json::from_slice::<PersistedPeople>(&bytes).ok())
            .unwrap_or_default();
        let capability = if persisted.privacy.recognition_enabled {
            FaceCapability::NoVerifiedFaceModel
        } else {
            FaceCapability::DisabledByOwner
        };
        Self {
            privacy: persisted.privacy,
            capability,
            clusters: persisted.clusters,
            storage_path,
        }
    }

    fn save(&self) {
        let Some(file) = &self.storage_path else {
            return;
        };
        let state = PersistedPeople {
            privacy: self.privacy,
            clusters: self.clusters.clone(),
        };
        if let Ok(bytes) = serde_json::to_vec(&state) {
            let _ = file.write(&bytes);
        }
    }

    #[must_use]
    pub fn privacy(&self) -> PeoplePrivacy {
        self.privacy
    }

    #[must_use]
    pub fn capability(&self) -> &FaceCapability {
        &self.capability
    }

    /// Update the explicit per-device consent switch. Turning it off does not
    /// delete local clusters by surprise; callers should offer the separate,
    /// clearly destructive `delete_all_local_biometric_data` control.
    pub fn set_recognition_enabled(&mut self, enabled: bool) {
        self.privacy.recognition_enabled = enabled;
        if !enabled {
            self.capability = FaceCapability::DisabledByOwner;
        } else if matches!(self.capability, FaceCapability::DisabledByOwner) {
            self.capability = FaceCapability::NoVerifiedFaceModel;
        }
        self.save();
    }

    /// Discover the dedicated signed loopback face capability. Generic vision
    /// models are deliberately ignored; no model means a truthful unavailable
    /// state rather than synthetic people suggestions.
    pub fn discover_face_capability(&mut self, config: &agent_provider_config::ProviderConfig) {
        if !self.privacy.recognition_enabled {
            self.capability = FaceCapability::DisabledByOwner;
            return;
        }
        self.capability = tilo_vision::discover_local_face_capability(config).map_or(
            FaceCapability::NoVerifiedFaceModel,
            |capability| FaceCapability::Ready {
                model_id: capability.model_id,
                revision: capability.revision,
            },
        );
    }

    /// Admit a signed local face capability after the runtime independently
    /// validates it. Empty identities are refused, avoiding an accidental
    /// upgrade from generic local vision/OCR.
    pub fn set_verified_local_face_model(
        &mut self,
        model_id: impl Into<String>,
        revision: impl Into<String>,
    ) -> bool {
        let model_id = model_id.into();
        let revision = revision.into();
        if !self.privacy.recognition_enabled
            || model_id.trim().is_empty()
            || revision.trim().is_empty()
            || model_id.chars().any(char::is_control)
            || revision.chars().any(char::is_control)
        {
            return false;
        }
        self.capability = FaceCapability::Ready { model_id, revision };
        true
    }

    /// Register an opaque local cluster. Only a verified pipeline can label
    /// its origin as recognition; manually supplied/imported collections are
    /// useful and stay honestly marked as such.
    pub fn add_cluster(
        &mut self,
        id: impl Into<String>,
        photo_ids: impl IntoIterator<Item = String>,
        origin: ProposalOrigin,
    ) -> bool {
        let id = id.into();
        let photos: BTreeSet<_> = photo_ids
            .into_iter()
            .filter(|id| valid_photo_id(id))
            .map(|photo_id| LocalClusterPhoto { photo_id })
            .collect();
        if !valid_cluster_id(&id)
            || photos.is_empty()
            || self.clusters.contains_key(&id)
            || (origin == ProposalOrigin::VerifiedLocalPipeline && !self.capability.can_cluster())
        {
            return false;
        }
        self.clusters.insert(
            id.clone(),
            LocalPersonCluster {
                id,
                display_name: None,
                photos,
                origin,
                hidden: false,
                rejected: false,
            },
        );
        self.save();
        true
    }

    /// Accept results from a separately verified, local-only face pipeline.
    /// A generic vision/OCR result cannot call this route: the signed model id
    /// and revision must exactly match the currently admitted face capability.
    /// Invalid clusters are skipped individually, so an untrusted batch can
    /// never erase or partially alter an existing local review collection.
    pub fn ingest_verified_face_clusters(&mut self, batch: VerifiedFaceClusterBatch) -> usize {
        let FaceCapability::Ready { model_id, revision } = &self.capability else {
            return 0;
        };
        if model_id != &batch.model_id || revision != &batch.revision {
            return 0;
        }
        let mut changed = 0;
        for cluster in batch.clusters {
            let photos: BTreeSet<_> = cluster
                .photo_ids
                .into_iter()
                .filter(|id| valid_photo_id(id))
                .map(|photo_id| LocalClusterPhoto { photo_id })
                .collect();
            if !valid_cluster_id(&cluster.id) || photos.is_empty() {
                continue;
            }

            // A face pipeline scopes its opaque cluster ids to each request.
            // Reconcile only by a stable photo id that occurs in both bounded
            // batches; no embedding, crop, geometry, or confidence crosses
            // the private runtime/controller boundary.
            let mut matches: Vec<_> = self
                .clusters
                .values()
                .filter(|known| {
                    known.origin == ProposalOrigin::VerifiedLocalPipeline
                        && !known.rejected
                        && !known.photos.is_disjoint(&photos)
                })
                .map(|known| known.id.clone())
                .collect();
            matches.sort();
            matches.dedup();

            if matches.is_empty() {
                if self.add_cluster(
                    cluster.id,
                    photos.into_iter().map(|photo| photo.photo_id),
                    ProposalOrigin::VerifiedLocalPipeline,
                ) {
                    changed += 1;
                }
                continue;
            }

            let destination = matches.remove(0);
            let Some(mut destination_cluster) = self.clusters.remove(&destination) else {
                continue;
            };
            let before = destination_cluster.photos.len();
            destination_cluster.photos.extend(photos);
            let mut merged = false;
            for source in matches {
                if source == destination {
                    continue;
                }
                if let Some(source_cluster) = self.clusters.remove(&source) {
                    destination_cluster.photos.extend(source_cluster.photos);
                    if destination_cluster.display_name.is_none() {
                        destination_cluster.display_name = source_cluster.display_name;
                    }
                    merged = true;
                }
            }
            let changed_cluster = destination_cluster.photos.len() != before || merged;
            self.clusters.insert(destination, destination_cluster);
            if changed_cluster {
                changed += 1;
                self.save();
            }
        }
        changed
    }

    #[must_use]
    pub fn cluster(&self, id: &str) -> Option<&LocalPersonCluster> {
        self.clusters.get(id)
    }

    /// A bounded listing for the visible People rail. Hidden/rejected records
    /// are intentionally omitted; the management dialog uses `all_clusters`.
    #[must_use]
    pub fn visible_clusters(&self) -> Vec<&LocalPersonCluster> {
        self.clusters
            .values()
            .filter(|cluster| !cluster.hidden && !cluster.rejected)
            .collect()
    }

    pub fn all_clusters(&self) -> impl Iterator<Item = &LocalPersonCluster> {
        self.clusters.values()
    }

    /// A tiny local-only representative set for the next bounded recognition
    /// request. Sending one already-local photo beside a fresh disjoint slice
    /// lets the signed local runtime connect batch-local cluster ids; only the
    /// resulting photo-id grouping returns here. These anchors are never put
    /// in a Photos action or resource.
    #[must_use]
    pub fn reconciliation_anchor_photo_ids(&self, limit: usize) -> Vec<String> {
        self.clusters
            .values()
            .filter(|cluster| {
                cluster.origin == ProposalOrigin::VerifiedLocalPipeline && !cluster.rejected
            })
            .filter_map(|cluster| cluster.photos.iter().next())
            .map(|photo| photo.photo_id.clone())
            .take(limit)
            .collect()
    }

    /// Name/confirm a local proposal. This does not share it: call
    /// `approve_membership` only after the user has inspected the photos.
    pub fn confirm_suggestion(&mut self, id: &str, name: &str) -> PeopleChange {
        let Some(cluster) = self.clusters.get_mut(id) else {
            return PeopleChange::Unchanged;
        };
        let name = name.trim();
        if !valid_person_name(name) || cluster.rejected {
            return PeopleChange::Unchanged;
        }
        cluster.display_name = Some(name.to_owned());
        cluster.hidden = false;
        self.save();
        PeopleChange::Updated
    }

    /// Reject a suggestion locally. It keeps no collaborative history and can
    /// later be deleted completely with the private-data control.
    pub fn reject_suggestion(&mut self, id: &str) -> PeopleChange {
        let Some(cluster) = self.clusters.get_mut(id) else {
            return PeopleChange::Unchanged;
        };
        cluster.rejected = true;
        cluster.hidden = true;
        cluster.display_name = None;
        self.save();
        PeopleChange::Updated
    }

    /// Return a hidden or rejected local proposal to the review queue. This is
    /// deliberately local and reversible; it never recreates a shared label.
    pub fn restore_suggestion(&mut self, id: &str) -> PeopleChange {
        let Some(cluster) = self.clusters.get_mut(id) else {
            return PeopleChange::Unchanged;
        };
        if !cluster.hidden && !cluster.rejected {
            return PeopleChange::Unchanged;
        }
        cluster.hidden = false;
        cluster.rejected = false;
        self.save();
        PeopleChange::Updated
    }

    pub fn rename_cluster(&mut self, id: &str, name: &str) -> PeopleChange {
        self.confirm_suggestion(id, name)
    }

    pub fn set_hidden(&mut self, id: &str, hidden: bool) -> PeopleChange {
        let Some(cluster) = self.clusters.get_mut(id) else {
            return PeopleChange::Unchanged;
        };
        if cluster.hidden == hidden {
            return PeopleChange::Unchanged;
        }
        cluster.hidden = hidden;
        self.save();
        PeopleChange::Updated
    }

    /// Remove a wrongly grouped photograph from a local proposal. It is not a
    /// shared action; any already-approved membership must be edited through
    /// the corresponding canonical resource action.
    pub fn remove_photo(&mut self, id: &str, photo_id: &str) -> PeopleChange {
        let Some(cluster) = self.clusters.get_mut(id) else {
            return PeopleChange::Unchanged;
        };
        let removed = cluster.photos.remove(&LocalClusterPhoto {
            photo_id: photo_id.to_owned(),
        });
        if !removed {
            return PeopleChange::Unchanged;
        }
        if cluster.photos.is_empty() {
            self.clusters.remove(id);
            self.save();
            PeopleChange::Deleted
        } else {
            self.save();
            PeopleChange::Updated
        }
    }

    /// Split explicitly chosen mistaken members into a fresh private review
    /// suggestion. The new id is supplied by the local runtime/UI so this
    /// operation has no global identity and cannot accidentally become a
    /// shared person. At least one photo remains in each side of the split.
    pub fn split_cluster(
        &mut self,
        id: &str,
        new_id: impl Into<String>,
        photo_ids: impl IntoIterator<Item = String>,
    ) -> PeopleChange {
        let new_id = new_id.into();
        if !valid_cluster_id(&new_id) || self.clusters.contains_key(&new_id) {
            return PeopleChange::Unchanged;
        }
        let chosen: BTreeSet<_> = photo_ids
            .into_iter()
            .map(|photo_id| LocalClusterPhoto { photo_id })
            .collect();
        let Some(cluster) = self.clusters.get_mut(id) else {
            return PeopleChange::Unchanged;
        };
        if chosen.is_empty() || !chosen.is_subset(&cluster.photos) || chosen == cluster.photos {
            return PeopleChange::Unchanged;
        }
        let origin = cluster.origin;
        for photo in &chosen {
            cluster.photos.remove(photo);
        }
        self.clusters.insert(
            new_id.clone(),
            LocalPersonCluster {
                id: new_id,
                display_name: None,
                photos: chosen,
                origin,
                hidden: false,
                rejected: false,
            },
        );
        self.save();
        PeopleChange::Updated
    }

    /// Merge two local clusters. Names remain local until the caller has
    /// explicitly chosen a final shared membership action.
    pub fn merge_clusters(&mut self, destination: &str, source: &str) -> PeopleChange {
        if destination == source {
            return PeopleChange::Unchanged;
        }
        let Some(source_cluster) = self.clusters.remove(source) else {
            return PeopleChange::Unchanged;
        };
        let Some(destination_cluster) = self.clusters.get_mut(destination) else {
            self.clusters.insert(source.to_owned(), source_cluster);
            return PeopleChange::Unchanged;
        };
        destination_cluster.photos.extend(source_cluster.photos);
        if destination_cluster.display_name.is_none() {
            destination_cluster.display_name = source_cluster.display_name;
        }
        self.save();
        PeopleChange::Updated
    }

    /// Build the one permissible bridge into collaborative metadata. This
    /// refuses unnamed/rejected clusters and intentionally carries only photo
    /// ids and the chosen label — never the local clustering evidence.
    #[must_use]
    pub fn approve_membership(&self, id: &str) -> Option<PhotosAction> {
        let cluster = self.clusters.get(id)?;
        let person = cluster.display_name.as_ref()?.trim();
        (!cluster.rejected && valid_person_name(person)).then(|| {
            PhotosAction::SetPersonMembership {
                person: person.to_owned(),
                photo_ids: cluster.visible_photo_ids(),
            }
        })
    }

    /// Delete every device-local face cluster and capability identity. This
    /// has no effect on already-approved shared person labels, making the
    /// boundary reversible and understandable: remove private biometric data
    /// here, manage shared labels through their canonical resources there.
    pub fn delete_all_local_biometric_data(&mut self) {
        self.clusters.clear();
        self.capability = if self.privacy.recognition_enabled {
            FaceCapability::NoVerifiedFaceModel
        } else {
            FaceCapability::DisabledByOwner
        };
        self.save();
    }
}

fn valid_cluster_id(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn valid_photo_id(value: &str) -> bool {
    value.starts_with("photos.photo:")
        && value.len() <= 4096
        && !value.chars().any(char::is_control)
}

fn valid_person_name(value: &str) -> bool {
    !value.is_empty() && value.chars().count() <= 128 && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "photos.photo:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "photos.photo:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn verified_local_recognition_is_opt_in_converges_and_survives_restart() {
        let mut people = PeopleController::new();
        assert_eq!(people.capability(), &FaceCapability::DisabledByOwner);
        assert!(!people.add_cluster(
            "recognition",
            [A.to_owned()],
            ProposalOrigin::VerifiedLocalPipeline
        ));
        people.set_recognition_enabled(true);
        assert_eq!(people.capability(), &FaceCapability::NoVerifiedFaceModel);
        assert!(!people.add_cluster(
            "recognition",
            [A.to_owned()],
            ProposalOrigin::VerifiedLocalPipeline
        ));
        assert!(people.set_verified_local_face_model("face-local", "r42"));
        assert!(people.add_cluster(
            "recognition",
            [A.to_owned()],
            ProposalOrigin::VerifiedLocalPipeline
        ));
        assert_eq!(
            people.ingest_verified_face_clusters(VerifiedFaceClusterBatch {
                model_id: "generic-vision".to_owned(),
                revision: "r42".to_owned(),
                clusters: vec![VerifiedFaceCluster {
                    id: "wrong-model".to_owned(),
                    photo_ids: vec![B.to_owned()],
                }],
            }),
            0
        );
        assert_eq!(
            people.ingest_verified_face_clusters(VerifiedFaceClusterBatch {
                model_id: "face-local".to_owned(),
                revision: "r42".to_owned(),
                clusters: vec![VerifiedFaceCluster {
                    id: "verified-output".to_owned(),
                    photo_ids: vec![B.to_owned()],
                }],
            }),
            1
        );
        assert_eq!(
            people.cluster("verified-output").unwrap().origin,
            ProposalOrigin::VerifiedLocalPipeline
        );

        {
            let mut people = PeopleController::new();
            people.set_recognition_enabled(true);
            assert!(people.set_verified_local_face_model("face-local", "r42"));
            let batch = |id: &str, photos: Vec<String>| VerifiedFaceClusterBatch {
                model_id: "face-local".to_owned(),
                revision: "r42".to_owned(),
                clusters: vec![VerifiedFaceCluster {
                    id: id.to_owned(),
                    photo_ids: photos,
                }],
            };
            let c = "photos.photo:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
            // These are disjoint fresh slices, so their opaque model-local ids do
            // not reconcile by themselves.
            assert_eq!(
                people.ingest_verified_face_clusters(batch("first", vec![A.to_owned()])),
                1
            );
            assert_eq!(
                people.ingest_verified_face_clusters(batch("later", vec![c.to_owned()])),
                1
            );
            assert_eq!(people.all_clusters().count(), 2);
            // The next fresh slice carries `A` only as a local representative
            // anchor. Its returned grouping bridges the two private proposals.
            assert_eq!(
                people.ingest_verified_face_clusters(batch(
                    "anchored-bridge",
                    vec![A.to_owned(), c.to_owned()]
                )),
                1
            );
            assert_eq!(people.all_clusters().count(), 1);
            let cluster = people.cluster("first").unwrap();
            assert_eq!(cluster.visible_photo_ids().len(), 2);
            assert_eq!(cluster.origin, ProposalOrigin::VerifiedLocalPipeline);
            // The persisted review record has stable ids only, never model output.
            assert!(
                cluster
                    .photos
                    .iter()
                    .all(|photo| photo.photo_id.starts_with("photos.photo:"))
            );
        }

        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("people-review.json");
            let mut people = PeopleController::load_from(crate::files::StateFile::at(&path));
            people.set_recognition_enabled(true);
            assert!(people.set_verified_local_face_model("face-model", "revision-7"));
            assert!(people.add_cluster(
                "local-cluster",
                [A.to_owned()],
                ProposalOrigin::VerifiedLocalPipeline,
            ));
            assert_eq!(
                people.confirm_suggestion("local-cluster", "Sam"),
                PeopleChange::Updated
            );

            let restored = PeopleController::load_from(crate::files::StateFile::at(&path));
            assert!(restored.privacy().recognition_enabled);
            assert_eq!(
                restored.capability(),
                &FaceCapability::NoVerifiedFaceModel,
                "a signed model must be rediscovered rather than trusted from disk"
            );
            let cluster = restored.cluster("local-cluster").unwrap();
            assert_eq!(cluster.display_name.as_deref(), Some("Sam"));
            assert_eq!(cluster.visible_photo_ids(), [A]);
        }
    }

    #[test]
    fn only_approved_people_are_exported_as_labels_and_photo_ids() {
        let mut people = PeopleController::new();
        assert!(people.add_cluster(
            "manual",
            [A.to_owned(), B.to_owned()],
            ProposalOrigin::ImportedOrManual
        ));
        assert_eq!(
            people.confirm_suggestion("manual", "Sam"),
            PeopleChange::Updated
        );
        assert_eq!(
            people.approve_membership("manual"),
            Some(PhotosAction::SetPersonMembership {
                person: "Sam".to_owned(),
                photo_ids: vec![A.to_owned(), B.to_owned()],
            })
        );

        let mut library = PhotoLibrary::default();
        let mut photo = crate::model::LibraryPhoto::from_image(crate::model::OpenImage::from_path(
            "one.png", 20, 10,
        ));
        photo.people = vec!["Sam".to_owned(), "Élodie".to_owned()];
        let photo_id = photo.id.clone();
        library.photos.push(photo);
        assert_eq!(
            approved_people(&library),
            vec![
                SharedPerson {
                    label: "Sam".to_owned(),
                    photo_ids: vec![photo_id.clone()],
                },
                SharedPerson {
                    label: "Élodie".to_owned(),
                    photo_ids: vec![photo_id],
                },
            ]
        );
    }

    #[test]
    fn local_review_edits_stay_private_and_reversible() {
        let mut people = PeopleController::new();
        assert!(people.add_cluster(
            "one",
            [A.to_owned(), B.to_owned()],
            ProposalOrigin::ImportedOrManual,
        ));
        assert!(people.add_cluster("two", [B.to_owned()], ProposalOrigin::ImportedOrManual,));
        assert_eq!(people.remove_photo("one", A), PeopleChange::Updated);
        assert_eq!(people.merge_clusters("one", "two"), PeopleChange::Updated);
        assert_eq!(people.set_hidden("one", true), PeopleChange::Updated);
        assert!(people.visible_clusters().is_empty());
        assert_eq!(people.reject_suggestion("one"), PeopleChange::Updated);
        assert_eq!(people.restore_suggestion("one"), PeopleChange::Updated);
        assert!(!people.cluster("one").unwrap().hidden);
        assert!(!people.cluster("one").unwrap().rejected);
        people.delete_all_local_biometric_data();
        assert_eq!(people.all_clusters().count(), 0);
        assert_eq!(people.capability(), &FaceCapability::DisabledByOwner);

        let mut people = PeopleController::new();
        assert!(people.add_cluster(
            "mixed",
            [A.to_owned(), B.to_owned()],
            ProposalOrigin::ImportedOrManual
        ));
        assert_eq!(
            people.split_cluster("mixed", "separate", [B.to_owned()]),
            PeopleChange::Updated
        );
        assert_eq!(people.cluster("mixed").unwrap().visible_photo_ids(), [A]);
        assert_eq!(people.cluster("separate").unwrap().visible_photo_ids(), [B]);
        assert_eq!(people.remove_photo("separate", B), PeopleChange::Deleted);
        assert_eq!(people.restore_suggestion("mixed"), PeopleChange::Unchanged);
    }
}
