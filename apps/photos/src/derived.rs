//! What is derived from the library and published as places people act
//! on: favorites, albums, the timeline and its months and days, shared
//! places, people, folders, tags, and the objects and pets analysis found.
//!
//! Each is computed from the photographs alone, so every replica publishes
//! the same places under the same ids, and offers the views that show it
//! and the verbs that change it with their arguments bound.

use std::collections::BTreeMap;

use ui::tilo_resource::Derived;

use crate::actions::{PhotosEffect, TimelineAnchor};
use crate::model::{PhotoLibrary, PhotoLocation};
use crate::resource::photo_library;

/// Labels of one kind with how many photographs carry each.
pub(crate) type LabelCounts = Vec<(String, usize)>;

/// One kind of label: its counts, the kind its places are published as, and
/// the view that shows a label's photographs.
type Labelled<'a> = (&'a LabelCounts, &'static str, fn(&str) -> PhotosEffect);

/// The places derived from `library`.
pub(crate) fn derived(library: &PhotoLibrary) -> Vec<Derived<PhotoLibrary>> {
    let (folders, people, tags, objects, pets) = metadata_label_counts(library);
    let mut places = vec![
        analysis(),
        favorites(library),
        albums(library),
        located(library),
        people_root(&people),
        timeline(library),
    ];
    let labelled: [Labelled<'_>; 4] = [
        (&folders, "folder", |label| PhotosEffect::ViewFolder {
            folder: label.to_owned(),
        }),
        (&tags, "tag", |label| PhotosEffect::ViewTag {
            tag: label.to_owned(),
        }),
        (&objects, "object", |label| PhotosEffect::ViewObject {
            object: label.to_owned(),
        }),
        (&pets, "pet", |label| PhotosEffect::ViewPet {
            pet: label.to_owned(),
        }),
    ];
    for (counts, kind, view) in labelled {
        places.extend(counts.iter().map(|(label, count)| {
            Derived::new(label.clone())
                .of_kind(kind)
                .identified(label_id(kind, label))
                .with_effect(&view(label))
                .with_child(counted(*count))
        }));
    }
    places
}

/// How many photographs a place holds, as a child of it.
fn counted(count: usize) -> Derived<PhotoLibrary> {
    Derived::new(crate::strings::tr_count("photos-count-photos", count))
        .of_kind("count")
        .identified("count")
}

/// The id a label of `kind` is published under: its hash, so a label of
/// any length or script is one portable segment.
pub(crate) fn label_id(kind: &str, label: &str) -> String {
    format!("{kind}:{}", tilo_content::hash_bytes(label.as_bytes()))
}

/// The analysis this device runs over the library.
fn analysis() -> Derived<PhotoLibrary> {
    Derived::new(crate::strings::tr("photos-act-photo-analysis"))
        .of_kind("analysis")
        .identified("analysis")
        .with_effect(&PhotosEffect::ViewLibraryAnalysis)
        .with_effect(&PhotosEffect::AnalyzeLibrary)
        .with_effect(&PhotosEffect::PauseLibraryAnalysis)
        .with_effect(&PhotosEffect::ResumeLibraryAnalysis)
        .with_effect(&PhotosEffect::RetryLibraryAnalysisFailures)
}

fn favorites(library: &PhotoLibrary) -> Derived<PhotoLibrary> {
    let count = library.all_photos().filter(|photo| photo.favorite).count();
    Derived::new(crate::strings::tr("photos-favorites"))
        .of_kind("favorites")
        .identified("favorites")
        .with_effect(&PhotosEffect::ViewFavorites)
        .with_child(counted(count))
}

/// The albums as one place to browse them; each album is an item of the
/// library itself.
fn albums(library: &PhotoLibrary) -> Derived<PhotoLibrary> {
    Derived::new(crate::strings::tr("photos-albums"))
        .of_kind("albums")
        .identified("albums")
        .with_effect(&PhotosEffect::ViewAlbums)
        .with_child(
            Derived::new(crate::strings::tr_count(
                "photos-count-albums",
                library.albums.len(),
            ))
            .of_kind("count")
            .identified("count"),
        )
}

/// The places photographs were shared at, each a cluster on the offline
/// map.
fn located(library: &PhotoLibrary) -> Derived<PhotoLibrary> {
    let count = library
        .all_photos()
        .filter(|photo| photo.location.as_ref().is_some_and(PhotoLocation::is_valid))
        .count();
    let mut places = Derived::new(crate::strings::tr("photos-places"))
        .of_kind("places")
        .identified("places")
        .with_effect(&PhotosEffect::ViewPlaces)
        .with_child(counted(count));
    for (key, label, count) in place_counts(library) {
        places = places.with_child(
            Derived::new(label)
                .of_kind("place")
                .identified(label_id("place", &key))
                .with_effect(&PhotosEffect::ViewPlace { place_key: key })
                .with_child(counted(count)),
        );
    }
    places
}

/// The people named in the library: each offers its photographs, renaming
/// or merging, and coming off every photograph.
fn people_root(people: &LabelCounts) -> Derived<PhotoLibrary> {
    let mut root = Derived::new(crate::strings::tr("photos-people"))
        .of_kind("people")
        .identified("people")
        .with_effect(&PhotosEffect::ViewPeople)
        .with_effect(&PhotosEffect::ManagePeopleRecognition)
        .with_effect(&PhotosEffect::EnablePeopleRecognition)
        .with_effect(&PhotosEffect::AnalyzePeopleLibrary);
    for (person, count) in people {
        root = root.with_child(
            Derived::new(person.clone())
                .of_kind("person")
                .identified(label_id("person", person))
                .with_effect(&PhotosEffect::ViewPerson {
                    person: person.clone(),
                })
                .with(PhotoLibrary::SET_PERSON_MEMBERSHIP.bind(
                    photo_library::SetPersonMembership::paths().person,
                    person.clone(),
                ))
                .with(
                    PhotoLibrary::RENAME_PERSON
                        .bind(photo_library::RenamePerson::paths().person, person.clone()),
                )
                .with(
                    PhotoLibrary::REMOVE_PERSON
                        .bind(photo_library::RemovePerson::paths().person, person.clone()),
                )
                .with_child(counted(*count)),
        );
    }
    root
}

/// The timeline, its months and each month's days.
fn timeline(library: &PhotoLibrary) -> Derived<PhotoLibrary> {
    let boundaries = timeline_boundaries(library);
    let mut timeline = Derived::new(crate::strings::tr("photos-timeline"))
        .of_kind("timeline")
        .identified("timeline")
        .with_effect(&PhotosEffect::ViewTimeline { anchor: None })
        .with_child(counted(library.photos.len()));
    for (month, count) in &boundaries.months {
        let mut node = Derived::new(month.clone())
            .of_kind("month")
            .identified(format!("month:{month}"))
            .with_effect(&PhotosEffect::ViewTimeline {
                anchor: Some(TimelineAnchor::Month {
                    month: month.clone(),
                }),
            })
            .with_child(counted(*count));
        for (day, count) in boundaries
            .days
            .iter()
            .filter(|(day, _)| day.starts_with(month.as_str()))
        {
            node = node.with_child(
                Derived::new(day.clone())
                    .of_kind("day")
                    .identified(format!("day:{day}"))
                    .with_effect(&PhotosEffect::ViewTimeline {
                        anchor: Some(TimelineAnchor::Day { day: day.clone() }),
                    })
                    .with_child(counted(*count)),
            );
        }
        timeline = timeline.with_child(node);
    }
    timeline
}

/// The published id of the timeline month `month`.
pub(crate) fn timeline_month_resource_id(month: &str) -> String {
    crate::resource::derived_resource_id(&format!("timeline/month:{month}"))
}

/// The published id of the timeline day `day` (`YYYY-MM-DD`).
pub(crate) fn timeline_day_resource_id(day: &str) -> String {
    let month = day.get(..7).unwrap_or(day);
    crate::resource::derived_resource_id(&format!("timeline/month:{month}/day:{day}"))
}

/// The published id of the folder `folder`.
pub(crate) fn folder_resource_id(folder: &str) -> String {
    crate::resource::derived_resource_id(&label_id("folder", folder))
}

/// The published id of the person `person`.
pub(crate) fn person_resource_id(person: &str) -> String {
    crate::resource::derived_resource_id(&format!("people/{}", label_id("person", person)))
}

/// The published id of the tag `tag`.
pub(crate) fn tag_resource_id(tag: &str) -> String {
    crate::resource::derived_resource_id(&label_id("tag", tag))
}

/// The published id of the object label `object`.
pub(crate) fn object_resource_id(object: &str) -> String {
    crate::resource::derived_resource_id(&label_id("object", object))
}

/// The published id of the pet label `pet`.
pub(crate) fn pet_resource_id(pet: &str) -> String {
    crate::resource::derived_resource_id(&label_id("pet", pet))
}

/// The published id of the place cluster `place_key`.
pub(crate) fn place_resource_id(place_key: &str) -> String {
    crate::resource::derived_resource_id(&format!("places/{}", label_id("place", place_key)))
}

/// Compact, shared-metadata-only counts for timeline navigation, newest
/// first to match the rendered timeline.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TimelineBoundaries {
    pub months: Vec<(String, usize)>,
    pub days: Vec<(String, usize)>,
}

pub(crate) fn timeline_boundaries(library: &PhotoLibrary) -> TimelineBoundaries {
    let mut months = BTreeMap::<String, usize>::new();
    let mut days = BTreeMap::<String, usize>::new();
    for photo in library.all_photos() {
        let Some(day) = photo
            .captured_at
            .as_deref()
            .and_then(timeline_day_from_capture)
        else {
            continue;
        };
        let month = day.get(..7).unwrap_or_default().to_owned();
        *months.entry(month).or_default() += 1;
        *days.entry(day).or_default() += 1;
    }
    TimelineBoundaries {
        months: months.into_iter().rev().collect(),
        days: days.into_iter().rev().collect(),
    }
}

/// The calendar day (`YYYY-MM-DD`) a capture value starts with, when it
/// starts with a real one.
pub(crate) fn timeline_day_from_capture(captured_at: &str) -> Option<String> {
    let day = captured_at.get(..10)?;
    let bytes = day.as_bytes();
    let month = day.get(5..7)?.parse::<u8>().ok()?;
    let day_of_month = day.get(8..10)?.parse::<u8>().ok()?;
    let year = day.get(..4)?.parse::<u16>().ok()?;
    let days_in_month = ui::date::days_in_month(i64::from(year), u32::from(month));
    (bytes.len() == 10
        && bytes.get(4) == Some(&b'-')
        && bytes.get(7) == Some(&b'-')
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
        && (1..=12).contains(&month)
        && (1..=days_in_month).contains(&u32::from(day_of_month)))
    .then(|| day.to_owned())
}

/// Deterministic, bounded place clusters based only on intentional location
/// metadata: each place's key, its label and how many photographs it holds.
pub(crate) fn place_counts(library: &PhotoLibrary) -> Vec<(String, String, usize)> {
    let mut counts = BTreeMap::<String, (String, usize)>::new();
    for photo in library.all_photos() {
        let Some(location) = &photo.location else {
            continue;
        };
        if !location.is_valid() {
            continue;
        }
        let entry = counts
            .entry(location.place_key())
            .or_insert_with(|| (location.display_name(), 0));
        entry.1 += 1;
    }
    counts
        .into_iter()
        .map(|(key, (label, count))| (key, label, count))
        .collect()
}

/// The folders, people, tags, objects and pets of the library, each with
/// how many photographs carry it.
pub(crate) fn metadata_label_counts(
    library: &PhotoLibrary,
) -> (
    LabelCounts,
    LabelCounts,
    LabelCounts,
    LabelCounts,
    LabelCounts,
) {
    let mut folders = BTreeMap::<String, usize>::new();
    let mut people = BTreeMap::<String, usize>::new();
    let mut tags = BTreeMap::<String, usize>::new();
    let mut objects = BTreeMap::<String, usize>::new();
    let mut pets = BTreeMap::<String, usize>::new();
    for photo in library.all_photos() {
        if let Some(folder) = &photo.folder {
            *folders.entry(folder.clone()).or_default() += 1;
        }
        for person in photo.people.iter() {
            *people.entry(person.clone()).or_default() += 1;
        }
        for tag in photo.tags.iter() {
            *tags.entry(tag.clone()).or_default() += 1;
        }
        if let Some(vision) = &photo.vision {
            for object in &vision.objects {
                *objects.entry(object.clone()).or_default() += 1;
            }
            for pet in &vision.pets {
                *pets.entry(pet.clone()).or_default() += 1;
            }
        }
    }
    (
        folders.into_iter().collect(),
        people.into_iter().collect(),
        tags.into_iter().collect(),
        objects.into_iter().collect(),
        pets.into_iter().collect(),
    )
}
