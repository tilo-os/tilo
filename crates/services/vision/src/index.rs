//! The system index's side of the image model: the pictures it holds that
//! this device can read, and where one result is written, beside the entry
//! the app that keeps the picture published.
//!
//! Search's index is what every app publishes what it holds into. A picture's
//! app publishes one entry that names the picture's bytes (`IndexEntry::image`);
//! the device's image model reads it and adds the words it found to that same
//! entry ([`VisionIndexSink`]), so one picture is one row, found by its name,
//! the text in it and what it shows, whatever app it is in, with no app
//! running.

use std::sync::Arc;

use crate::analysis::{AnalysisSink, Found, ImageRef, ImageSource};
use tilo_desktop_search::content_index::{AppContentIndex, IndexKey};
use tilo_resource::search_index::{IndexDelta, IndexEntry};

/// Every picture this device may read that the system index holds.
pub struct VisionIndexSource(pub Arc<AppContentIndex>);

impl ImageSource for VisionIndexSource {
    fn images(&self) -> Result<Vec<ImageRef>, String> {
        let mut images = Vec::new();
        for indexed in self.0.contents().visible {
            if indexed.entry.secret {
                continue;
            }
            let Some(image) = indexed.entry.image.clone() else {
                continue;
            };
            images.push(ImageRef {
                resource: indexed.entry.resource.clone(),
                app: indexed.key.app.clone(),
                workspace: indexed.key.workspace.clone(),
                // The slot the record's entries are kept under, so what is
                // written back lands beside the app's own entry.
                record: indexed.key.record.clone(),
                image,
            });
        }
        Ok(images)
    }
}

/// Where one analyzed picture's words are written: the system index, under
/// the picture's own record.
pub struct VisionIndexSink(pub Arc<AppContentIndex>);

impl AnalysisSink for VisionIndexSink {
    fn enriched(&self, image: &ImageRef, found: &Found) -> Result<(), String> {
        let finding = crate::finding(image, found);
        let key = IndexKey {
            app: image.app.clone(),
            workspace: image.workspace.clone(),
            record: image.record.clone(),
        };
        // The words join the entry the app already publishes for the picture,
        // so one picture is one row: its own name and glyph stay, and its
        // text, objects and pets are found as well.
        let base = self.0.contents().visible.into_iter().find(|indexed| {
            indexed.key == key && indexed.entry.resource == image.resource && !indexed.entry.secret
        });
        let entry = match base {
            Some(indexed) => {
                let mut entry = indexed.entry;
                for (text, weight) in finding.fields {
                    if !entry.fields.iter().any(|(held, _)| held == &text) {
                        entry.fields.push((text, weight));
                    }
                }
                entry.image = Some(image.image.clone());
                entry
            }
            None => IndexEntry {
                key: finding.key,
                resource: finding.resource,
                title: finding.title,
                fields: finding.fields,
                image: Some(image.image.clone()),
                icon: Some("image".to_owned()),
                ..IndexEntry::default()
            },
        };
        self.0.apply(
            &key,
            IndexDelta {
                upsert: vec![entry],
                ..IndexDelta::default()
            },
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The app's entry for a picture, as its own publication writes it.
    fn picture_entry() -> IndexEntry {
        IndexEntry {
            key: "photo-1".to_owned(),
            resource: "tl:so.tilo.photos:photo:1".to_owned(),
            title: "At the beach".to_owned(),
            image: Some("tilo.content:abc".to_owned()),
            ..IndexEntry::default()
        }
    }

    #[test]
    fn an_analyzed_picture_is_found_by_its_words_in_the_apps_own_entry() {
        let index = AppContentIndex::in_memory();
        let key = IndexKey::new("so.tilo.photos", "workspace", "library");
        index.apply(
            &key,
            IndexDelta {
                upsert: vec![picture_entry()],
                ..IndexDelta::default()
            },
        );
        let images = VisionIndexSource(std::sync::Arc::new(index))
            .images()
            .unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].resource, "tl:so.tilo.photos:photo:1");
        assert_eq!(images[0].app, "so.tilo.photos");
        assert_eq!(images[0].image, "tilo.content:abc");
        assert_eq!(images[0].record, key.record);

        let index = std::sync::Arc::new(AppContentIndex::in_memory());
        index.apply(
            &key,
            IndexDelta {
                upsert: vec![picture_entry()],
                ..IndexDelta::default()
            },
        );
        let images = VisionIndexSource(std::sync::Arc::clone(&index))
            .images()
            .unwrap();
        VisionIndexSink(std::sync::Arc::clone(&index))
            .enriched(
                &images[0],
                &Found {
                    description: "A dog on a beach".to_owned(),
                    ocr_text: "42 North Street".to_owned(),
                    objects: vec!["dog".to_owned()],
                    pets: Vec::new(),
                    model: "vision-1".to_owned(),
                    revision: "a".repeat(40),
                },
            )
            .expect("the index takes it");
        let contents = index.contents().visible;
        assert_eq!(
            contents.len(),
            1,
            "one picture is one row: the app's entry takes the words"
        );
        let picture = &contents[0].entry;
        assert_eq!(picture.key, "photo-1");
        assert_eq!(picture.title, "At the beach", "the app's own name stays");
        assert!(picture.fields.iter().any(|(word, _)| word == "dog"));
        assert!(
            picture
                .fields
                .iter()
                .any(|(word, _)| word == "42 North Street")
        );
    }
}
