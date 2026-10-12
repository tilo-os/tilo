//! The photographs a screenshot shows: generated test patterns, put in the
//! capture's content store by [`put_content`] and imported into the library
//! by the scenario's actions ([`scenario`]). Nothing here reaches a live
//! launch; the live app starts empty.

use std::sync::OnceLock;

use ui::tilo_resource::{Id, Invoke};

use crate::model::{
    Album, Edits, LibraryPhoto, LocationProvenance, OpenImage, PhotoLibrary, PhotoLocation, Rect01,
};
use crate::pixels;

/// A generated test pattern.
///
/// Generated in memory for deterministic screenshots. Wide (3:2) and
/// asymmetric to exercise letterboxing, orientation and crop.
///
/// # Panics
///
/// Panics if `w` or `h` is `0`; every call site passes a fixed, non-zero
/// scenario size, so this never fires in practice.
#[allow(
    clippy::many_single_char_names,
    reason = "x/y/r/g/b/t/dx/dy are the idiomatic names for pixel coordinates, \
              colour channels and an interpolation factor"
)]
#[allow(
    clippy::expect_used,
    reason = "the buffer this builds is always exactly w*h*4 bytes, and every \
              call site passes a fixed non-zero w/h"
)]
pub fn shots_pattern(w: u32, h: u32) -> pixels::Rgba {
    let mut bytes = Vec::with_capacity((w as usize) * (h as usize) * 4);
    for y in 0..h {
        for x in 0..w {
            let fx = x as f32 / w as f32;
            let fy = y as f32 / h as f32;
            // A sky-to-water gradient with a horizon and a sun, so the image
            // has an unambiguous up, left and right.
            let horizon = 0.55;
            let (r, g, b) = if fy < horizon {
                let t = fy / horizon;
                (
                    (40.0 + 150.0 * t) as u8,
                    (90.0 + 110.0 * t) as u8,
                    (160.0 + 70.0 * t) as u8,
                )
            } else {
                let t = (fy - horizon) / (1.0 - horizon);
                (
                    (20.0 + 40.0 * t) as u8,
                    (60.0 + 60.0 * t) as u8,
                    (110.0 - 40.0 * t) as u8,
                )
            };
            // The sun sits left of centre and above the horizon.
            let dx = fx - 0.28;
            let dy = fy - 0.3;
            let sun = (dx * dx + (dy * dy) * 2.25).sqrt();
            let (r, g, b) = if sun < 0.09 {
                (255, 226, 160)
            } else {
                (r, g, b)
            };
            bytes.extend_from_slice(&[r, g, b, 255]);
        }
    }
    pixels::Rgba::new(w, h, bytes).expect("generated pattern is well formed")
}

/// Deterministic, hermetic gallery fixtures. They are deliberately derived
/// from the same small procedural scene rather than bundled stock imagery,
/// but each has a distinct light and colour story so a library reads as a
/// collection instead of four copies of one placeholder.
#[allow(
    clippy::many_single_char_names,
    clippy::indexing_slicing,
    reason = "chunks_exact_mut(4) proves each RGBA pixel has the indexed channels"
)]
pub fn shots_pattern_variant(w: u32, h: u32, variant: u8) -> pixels::Rgba {
    let mut image = shots_pattern(w, h);
    for pixel in image.bytes.chunks_exact_mut(4) {
        let (r, g, b) = (pixel[0], pixel[1], pixel[2]);
        match variant % 5 {
            // Warm trail, cool gallery lights, and rain respectively.
            0 => {}
            1 => {
                pixel[0] = r.saturating_add(32);
                pixel[1] = g.saturating_add(14);
                pixel[2] = b.saturating_sub(30);
            }
            2 => {
                pixel[0] = b.saturating_sub(18);
                pixel[1] = r.saturating_add(12);
                pixel[2] = g.saturating_add(28);
            }
            3 => {
                pixel[0] = ((u16::from(r) + u16::from(g)) / 3) as u8;
                pixel[1] = ((u16::from(g) + u16::from(b)) / 3) as u8;
                pixel[2] = b.saturating_add(18);
            }
            _ => {
                pixel[0] = r.saturating_sub(20);
                pixel[1] = g.saturating_add(25);
                pixel[2] = b.saturating_add(35);
            }
        }
    }
    image
}

/// Every screenshot scenario, in the order the gallery reads.
pub const SCENARIOS: &[&str] = &[
    "empty",
    "loading",
    "error",
    "opened",
    "library",
    "albums",
    "people",
    "people-detail",
    "places",
    "analysis",
    "import-progress",
    "import-errors",
    "selection",
    "selection-many-albums",
    "album-manage",
    "metadata-edit",
    "metadata-batch",
    "cropping",
    "edited",
];

/// The scenarios that show the library rather than one photograph.
pub(crate) fn shows_library(scenario: &str) -> bool {
    matches!(
        scenario,
        "library"
            | "albums"
            | "people"
            | "people-detail"
            | "places"
            | "analysis"
            | "import-progress"
            | "import-errors"
            | "selection"
            | "selection-many-albums"
            | "album-manage"
            | "metadata-edit"
            | "metadata-batch"
    )
}

/// One fixture photograph: its encoded bytes and how it is filed.
struct Sample {
    bytes: Vec<u8>,
    photo: LibraryPhoto,
}

/// What a scenario cannot stage with actions: the bytes of every photograph
/// it shows, put in the capture's content store.
pub fn put_content(store: &tilo_content::ContentStore) {
    for sample in harbour().into_iter().chain(gallery()) {
        if let Err(error) = store.put(&sample.bytes) {
            tracing::warn!(%error, "photos: could not stage a sample photograph");
        }
    }
    if let Err(error) = store.put(UNREADABLE) {
        tracing::warn!(%error, "photos: could not stage the unreadable sample");
    }
}

/// Bytes that name themselves an image and are not one.
const UNREADABLE: &[u8] = b"this is not an image";

/// The photograph every scenario but the empty, loading and error ones opens.
fn harbour() -> Option<&'static Sample> {
    static HARBOUR: OnceLock<Option<Sample>> = OnceLock::new();
    HARBOUR
        .get_or_init(|| {
            let mut sample = sample("harbour.png", &shots_pattern_variant(1200, 800, 4))?;
            // An honest capture day joins the newest timeline group instead of
            // an orphaned "Undated" tile ahead of the collection.
            sample.photo.captured_at = Some("2026-07-12T18:16".into());
            sample.photo.folder = Some("Travel 2026".into());
            sample.photo.people = ["Élodie".to_owned(), "Sam".to_owned()]
                .into_iter()
                .collect();
            sample.photo.tags = std::iter::once("golden hour".to_owned()).collect();
            Some(sample)
        })
        .as_ref()
}

/// A library worth browsing: the landscape, portrait, square and panoramic
/// shapes a photo library actually contains, each with a distinct light, so
/// the justified chronology, selection and metadata hierarchy are legible.
fn gallery() -> &'static [Sample] {
    static GALLERY: OnceLock<Vec<Sample>> = OnceLock::new();
    GALLERY.get_or_init(|| {
        [
            ("Harbour dusk", "2026-07-12T20:24"),
            ("Summer trail", "2026-06-15T09:18"),
            ("Museum light", "2026-06-15T14:42"),
            ("Rainy market", "2026-06-16T10:06"),
            ("Dog at the lake", "2026-07-12T17:30"),
            ("Window cat", "2026-07-12T08:11"),
            ("Sunset ferry", "2026-07-12T21:02"),
            ("Garden supper", "2026-07-12T19:14"),
            ("Paper lanterns", "2026-07-12T22:09"),
            ("Mountain lookout", "2026-07-12T11:28"),
            ("Studio morning", "2026-07-12T07:51"),
            ("First swim", "2026-07-12T15:45"),
        ]
        .into_iter()
        .enumerate()
        .filter_map(|(variant, (name, captured_at))| {
            let (width, height) = match variant % 5 {
                0 => (900, 600),
                1 => (600, 900),
                2 => (800, 800),
                3 => (1_200, 675),
                _ => (720, 960),
            };
            let mut pattern = shots_pattern_variant(width, height, variant as u8);
            // Two fixtures of one shape and light would be the same bytes,
            // and so one photograph: the first pixel tells them apart.
            if let Some(first) = pattern.bytes.first_mut() {
                *first = variant as u8;
            }
            let mut sample = sample(&format!("{name}.png"), &pattern)?;
            let photo = &mut sample.photo;
            photo.favorite = matches!(name, "Summer trail" | "Dog at the lake" | "Sunset ferry");
            photo.folder = Some(if name.contains("Museum") || name.contains("Studio") {
                "City walks".into()
            } else {
                "Travel 2026".into()
            });
            let people: &[&str] = if name.contains("Summer") || name.contains("Dog") {
                &["Élodie", "Sam"]
            } else if name.contains("cat") {
                &["Noor"]
            } else {
                &["Élodie"]
            };
            photo.people = people.iter().map(|&person| person.to_owned()).collect();
            let tag = if name.contains("Rainy") {
                "rain"
            } else if name.contains("Dog") || name.contains("cat") {
                "pets"
            } else {
                "golden hour"
            };
            photo.tags = std::iter::once(tag.to_owned()).collect();
            photo.captured_at = Some(captured_at.into());
            Some(sample)
        })
        .collect()
    })
}

fn sample(name: &str, pattern: &pixels::Rgba) -> Option<Sample> {
    let bytes = pixels::encode_png(pattern)?;
    let hash = tilo_content::hash_bytes(&bytes);
    let image = OpenImage::from_path(name, pattern.w, pattern.h)
        .with_content(hash)
        .portable();
    Some(Sample {
        bytes,
        photo: LibraryPhoto::from_image(image),
    })
}

fn not_here_yet() -> LibraryPhoto {
    placeholder("not-here-yet.png", "0".repeat(64))
}

fn unreadable() -> LibraryPhoto {
    placeholder("unreadable-image.png", tilo_content::hash_bytes(UNREADABLE))
}

/// The photograph `scenario` shows open in its window.
pub(crate) fn opened(scenario: &str) -> Option<String> {
    match scenario {
        "empty" => None,
        "loading" => Some(not_here_yet().id),
        "error" => Some(unreadable().id),
        _ => harbour().map(|sample| sample.photo.id.clone()),
    }
}

/// A photograph whose bytes have not arrived here (`loading`) or are not an
/// image (`error`): what a collaborator's import looks like before or
/// without its content.
fn placeholder(name: &str, content: String) -> LibraryPhoto {
    LibraryPhoto::from_image(
        OpenImage::from_path(name, 1200, 800)
            .with_content(content)
            .portable(),
    )
}

/// The library edits that stage `scenario` (`None` for a plain `--shots`,
/// which shows the opened photograph).
pub fn scenario(scenario: Option<&str>) -> Vec<Invoke<PhotoLibrary>> {
    let scenario = scenario.unwrap_or("opened");
    match scenario {
        "empty" => return Vec::new(),
        "loading" => return vec![crate::resource::import(&not_here_yet())],
        "error" => return vec![crate::resource::import(&unreadable())],
        _ => {}
    }
    let Some(harbour) = harbour() else {
        return Vec::new();
    };
    let mut actions = Vec::new();
    if shows_library(scenario) {
        actions.extend(library(scenario));
    }
    actions.extend(Invoke::edit(&crate::resource::insert(&harbour.photo)));
    let harbour_id = Id::from_raw(harbour.photo.id.clone());
    match scenario {
        // A completed crop, rendered fit-to-canvas. A screenshot must show a
        // truthful edit, not a deliberately zoomed inspection state that can
        // be mistaken for a broken initial fit on a narrow viewport.
        "cropping" => actions.push(PhotoLibrary::CROP.with((
            harbour_id,
            Rect01 {
                x: 0.12,
                y: 0.08,
                w: 0.76,
                h: 0.84,
            },
        ))),
        // A crop and a turn together, which is the pair worth showing: the
        // crop is stored against the *original*, so it keeps naming the same
        // region of the photograph however the view is later turned.
        "edited" => {
            let edits = Edits::default()
                .cropped_from_view(Rect01 {
                    x: 0.06,
                    y: 0.06,
                    w: 0.46,
                    h: 0.5,
                })
                .rotated_right();
            let paths = crate::model::photo_paths(&harbour.photo.id).edits;
            actions.extend(
                [
                    paths.crop.set(edits.crop),
                    paths.rotate_quarters.set(edits.rotate_quarters),
                ]
                .iter()
                .filter_map(|edit| Invoke::edit(edit).ok()),
            );
        }
        _ => {}
    }
    actions
}

/// The gallery imported, with the albums `scenario` browses.
fn library(scenario: &str) -> Vec<Invoke<PhotoLibrary>> {
    let mut actions = Vec::new();
    let mut travel = Vec::new();
    for (index, sample) in gallery().iter().enumerate() {
        let mut photo = sample.photo.clone();
        if scenario == "places" {
            let locations = [
                (37.7749, -122.4194, "San Francisco"),
                (48.8566, 2.3522, "Paris"),
                (35.6762, 139.6503, "Tokyo"),
            ];
            #[expect(
                clippy::indexing_slicing,
                reason = "the index is reduced modulo the array length"
            )]
            let (latitude, longitude, label) = locations[index % locations.len()];
            photo.location = Some(PhotoLocation {
                latitude,
                longitude,
                label: Some(label.into()),
                provenance: LocationProvenance {
                    source: "user".into(),
                    scope: "shared".into(),
                },
            });
        }
        travel.push(photo.id.clone());
        actions.extend(Invoke::edit(&crate::resource::insert(&photo)));
    }
    let mut albums = vec![Album {
        id: "photos.album:travel".into(),
        name: "Travel".into(),
        photo_ids: travel.iter().cloned().collect(),
    }];
    if scenario == "albums" {
        albums.extend([
            Album {
                id: "photos.album:summer-weekends".into(),
                name: "Summer weekends".into(),
                photo_ids: travel.iter().skip(1).take(6).cloned().collect(),
            },
            Album {
                id: "photos.album:family-favourites".into(),
                name: "Family favourites".into(),
                photo_ids: travel.iter().step_by(2).take(5).cloned().collect(),
            },
            Album {
                id: "photos.album:ideas".into(),
                name: "Ideas for next time".into(),
                photo_ids: travel.iter().rev().take(3).cloned().collect(),
            },
            Album {
                id: "photos.album:new-home".into(),
                name: "New home".into(),
                photo_ids: ui::tilo_resource::Set::new(),
            },
        ]);
    }
    if scenario == "selection-many-albums" {
        albums.extend((1..=17).map(|number| Album {
            id: format!("photos.album:destination-{number}"),
            name: format!("Destination album {number}"),
            photo_ids: ui::tilo_resource::Set::new(),
        }));
    }
    actions.extend(albums.iter().filter_map(|album| {
        Invoke::edit(
            &PhotoLibrary::paths()
                .albums
                .insert_as(&album.item(), usize::MAX, album),
        )
        .ok()
    }));
    actions
}

/// The first four gallery photographs: a manual, explicitly labelled People
/// suggestion exercises the review layout without pretending that a face
/// model recognized anyone.
pub(crate) fn people_suggestion() -> Vec<String> {
    gallery()
        .iter()
        .take(4)
        .map(|sample| sample.photo.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scenario_photograph_is_its_own_content_and_stages_through_the_door() {
        let gallery = gallery();
        let ids: std::collections::HashSet<_> = gallery
            .iter()
            .map(|sample| sample.photo.id.as_str())
            .collect();
        assert_eq!(
            ids.len(),
            gallery.len(),
            "every fixture is a photograph of its own"
        );
        assert_ne!(
            shots_pattern_variant(32, 20, 0),
            shots_pattern_variant(32, 20, 1)
        );
        assert_eq!(
            shots_pattern_variant(32, 20, 1),
            shots_pattern_variant(32, 20, 1)
        );

        let store = tilo_content::ContentStore::memory();
        put_content(&store);
        for &name in SCENARIOS {
            let mut staged = ui::tilo_resource::Replica::<PhotoLibrary>::new();
            for action in scenario(Some(name)) {
                let id = action.id;
                staged
                    .apply(action)
                    .unwrap_or_else(|error| panic!("{name}: {id} refused: {error}"));
            }
            let library = staged.state().unwrap();
            for photo in library.all_photos() {
                let content = photo.image.content.as_deref().unwrap();
                if !matches!(name, "loading") {
                    assert!(
                        store.read(content).is_some(),
                        "{name}: {} has its bytes",
                        photo.id
                    );
                }
            }
        }
        let mut library = ui::tilo_resource::Replica::<PhotoLibrary>::new();
        for action in scenario(Some("library")) {
            library.apply(action).unwrap();
        }
        assert_eq!(library.state().unwrap().photos.len(), gallery.len() + 1);
    }
}
