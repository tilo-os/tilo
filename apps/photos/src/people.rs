//! Device-local People recognition and review. This module is deliberately *not* part of
//! `crate::doc`.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::PhotoLibrary;

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

#[must_use]
pub fn approved_people(library: &PhotoLibrary) -> Vec<SharedPerson> {
    let mut people = BTreeMap::<String, Vec<String>>::new();
    for photo in library.all_photos() {
        for label in photo.people.iter() {
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct PeoplePrivacy {
    pub recognition_enabled: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FaceCapability {
    #[default]
    DisabledByOwner,
    NoVerifiedFaceModel,
    Ready {
        model_id: String,
        revision: String,
    },
}

impl FaceCapability {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedFaceClusterBatch {
    pub model_id: String,
    pub revision: String,
    pub clusters: Vec<VerifiedFaceCluster>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedFaceCluster {
    pub id: String,
    pub photo_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, serde::Serialize)]
pub struct LocalClusterPhoto {
    pub photo_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum ProposalOrigin {
    VerifiedLocalPipeline,
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
    pub fn visible_photo_ids(&self) -> Vec<String> {
        self.photos
            .iter()
            .map(|photo| photo.photo_id.clone())
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeopleChange {
    Updated,
    Deleted,
    Unchanged,
}

#[derive(Clone, Debug, Default)]
pub struct PeopleController {
    privacy: PeoplePrivacy,
    capability: FaceCapability,
    clusters: BTreeMap<String, LocalPersonCluster>,
    storage_path: Option<crate::files::StateFile>,
    save_failure: Option<String>,
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

    #[must_use]
    pub fn persistent() -> Self {
        Self::load_from(crate::files::StateFile::in_state("people-review.json"))
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
            save_failure: None,
        }
    }

    fn save(&mut self) {
        let Some(file) = &self.storage_path else {
            return;
        };
        let state = PersistedPeople {
            privacy: self.privacy,
            clusters: self.clusters.clone(),
        };
        if let Err(error) = file.write_json(&state) {
            self.save_failure = Some(error);
        }
    }

    pub fn take_save_failure(&mut self) -> Option<String> {
        self.save_failure.take()
    }

    #[must_use]
    pub fn privacy(&self) -> PeoplePrivacy {
        self.privacy
    }

    #[must_use]
    pub fn capability(&self) -> &FaceCapability {
        &self.capability
    }

    pub fn set_recognition_enabled(&mut self, enabled: bool) {
        self.privacy.recognition_enabled = enabled;
        if !enabled {
            self.capability = FaceCapability::DisabledByOwner;
        } else if matches!(self.capability, FaceCapability::DisabledByOwner) {
            self.capability = FaceCapability::NoVerifiedFaceModel;
        }
        self.save();
    }

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

    #[cfg(test)]
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

    #[must_use]
    pub fn approve_membership(
        &self,
        id: &str,
    ) -> Option<(ui::tilo_resource::NonBlank, Vec<String>)> {
        let cluster = self.clusters.get(id)?;
        let person = ui::tilo_resource::NonBlank::parse(cluster.display_name.as_ref()?)?;
        (!cluster.rejected && valid_person_name(&person))
            .then(|| (person, cluster.visible_photo_ids()))
    }

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
            assert_eq!(
                people.ingest_verified_face_clusters(batch("first", vec![A.to_owned()])),
                1
            );
            assert_eq!(
                people.ingest_verified_face_clusters(batch("later", vec![c.to_owned()])),
                1
            );
            assert_eq!(people.all_clusters().count(), 2);
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
            assert_eq!(people.take_save_failure(), None);
        }

        {
            let dir = tempfile::tempdir().unwrap();
            let taken = dir.path().join("people-review.json");
            std::fs::create_dir(&taken).unwrap();
            std::fs::write(taken.join("held"), b"").unwrap();
            let file = crate::files::StateFile::at(&taken);
            let mut people = PeopleController::load_from(file);
            people.set_recognition_enabled(true);
            assert!(people.take_save_failure().is_some());
            assert_eq!(people.take_save_failure(), None);
            assert!(people.privacy().recognition_enabled);
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
            Some(("Sam".parse().unwrap(), vec![A.to_owned(), B.to_owned()]))
        );

        let mut library = PhotoLibrary::default();
        let mut photo = crate::model::LibraryPhoto::from_image(crate::model::OpenImage::fixture(
            "one.png", 20, 10,
        ));
        photo.people = ["Sam".to_owned(), "Élodie".to_owned()]
            .into_iter()
            .collect();
        let photo_id = photo.id.clone();
        library.add_photo(photo);
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
        assert!(people.add_cluster("single", [B.to_owned()], ProposalOrigin::ImportedOrManual));
        assert_eq!(people.remove_photo("single", B), PeopleChange::Deleted);
        assert!(people.cluster("single").is_none());
    }
}
