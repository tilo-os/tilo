//! Retained geometry for the Photos chronology.
//!
//! Justified rows are variable-height and never cross a capture day.  This
//! index therefore lays out one day at a time, retains its exact pixel extent,
//! and exposes only the rows intersecting a viewport.  Scrollbars, date
//! markers, painting and thumbnail hydration consequently share one coordinate
//! system instead of approximating the layout with a column count.

use std::collections::HashMap;

const DAY_HEADER_HEIGHT: f64 = 34.0;
const MAX_ROW_ITEMS: usize = 6;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TimelinePhoto {
    pub id: String,
    pub name: String,
    pub favorite: bool,
    pub day: String,
    /// Portable capture value retained for newest-first ordering inside a
    /// day. ISO timestamps sort lexically; malformed/legacy values remain
    /// deterministic through the id tie-break rather than being invented.
    pub captured_at: String,
    pub aspect_ratio: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TimelineGeometry {
    pub width: f64,
    pub target_height: f64,
    pub gap: f64,
    pub compact: bool,
}

impl TimelineGeometry {
    fn normalized(self) -> Self {
        Self {
            width: self.width.max(44.0),
            target_height: self.target_height.clamp(76.0, 420.0),
            gap: self.gap.max(0.0),
            compact: self.compact,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct TimelineRow {
    ids: Vec<String>,
    widths: Vec<f64>,
    height: f64,
    top: f64,
}

#[derive(Clone, Debug, PartialEq)]
struct TimelineDay {
    key: String,
    ids: Vec<String>,
    rows: Vec<TimelineRow>,
    top: f64,
    height: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TimelineAnchor {
    photo_id: String,
    screen_y: f64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct VisibleTimelinePhoto<'a> {
    pub photo: &'a TimelinePhoto,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VisibleTimelineDay {
    pub key: String,
    pub y: f64,
}

#[derive(Default)]
pub(crate) struct TimelineProjection {
    photos: HashMap<String, TimelinePhoto>,
    days: Vec<TimelineDay>,
    geometry: Option<TimelineGeometry>,
    total_height: f64,
}

impl TimelineProjection {
    pub fn replace(&mut self, photos: impl IntoIterator<Item = TimelinePhoto>) {
        self.photos = photos
            .into_iter()
            .map(|photo| (photo.id.clone(), photo))
            .collect();
        self.rebuild_days();
    }

    /// Reconcile a new filtered metadata generation without throwing away
    /// every retained day. Sparse collaborative edits consequently repack
    /// only the day buckets they actually change.
    pub fn reconcile(&mut self, photos: impl IntoIterator<Item = TimelinePhoto>) {
        let incoming: HashMap<_, _> = photos
            .into_iter()
            .map(|photo| (photo.id.clone(), photo))
            .collect();
        if self.photos.is_empty() {
            self.replace(incoming.into_values());
            return;
        }
        let removed: Vec<_> = self
            .photos
            .keys()
            .filter(|id| !incoming.contains_key(*id))
            .cloned()
            .collect();
        for id in removed {
            self.remove(&id);
        }
        for photo in incoming.into_values() {
            if self.photos.get(&photo.id) != Some(&photo) {
                self.upsert(photo);
            }
        }
    }

    /// Apply one durable-record delta. Only the old and new capture days are
    /// repacked; the prefix pass is proportional to the number of distinct
    /// days rather than the number of photographs.
    pub fn upsert(&mut self, photo: TimelinePhoto) {
        let old_day = self.photos.get(&photo.id).map(|old| old.day.clone());
        let new_day = photo.day.clone();
        self.photos.insert(photo.id.clone(), photo);
        self.reconcile_days(old_day.as_deref(), Some(&new_day));
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let Some(photo) = self.photos.remove(id) else {
            return false;
        };
        self.reconcile_days(Some(&photo.day), None);
        true
    }

    pub fn set_geometry(&mut self, geometry: TimelineGeometry) -> bool {
        let geometry = geometry.normalized();
        if self.geometry == Some(geometry) {
            return false;
        }
        self.geometry = Some(geometry);
        for day in &mut self.days {
            Self::layout_day(day, &self.photos, geometry);
        }
        self.rebuild_prefix();
        true
    }

    pub fn len(&self) -> usize {
        self.photos.len()
    }

    pub fn total_height(&self) -> f64 {
        self.total_height
    }

    pub fn max_scroll(&self, viewport: f64) -> f64 {
        (self.total_height - viewport.max(0.0)).max(0.0)
    }

    pub fn clamp_scroll(&self, scroll_y: f64, viewport: f64) -> f64 {
        scroll_y.clamp(0.0, self.max_scroll(viewport))
    }

    pub fn day_offset(&self, day: &str, viewport: f64) -> Option<f64> {
        // Synced metadata has day buckets before its first layout. Their zero
        // offsets are placeholders, so let semantic navigation build geometry.
        self.geometry?;
        self.days
            .iter()
            .find(|entry| entry.key == day)
            .map(|entry| entry.top.min(self.max_scroll(viewport)))
    }

    pub fn date_markers(&self, viewport: f64) -> Vec<(String, f64, bool)> {
        let denominator = self.max_scroll(viewport);
        let mut previous_year = "";
        self.days
            .iter()
            .map(|day| {
                let year = day.key.get(..4).unwrap_or_default();
                let major = year.len() == 4 && year != previous_year;
                if major {
                    previous_year = year;
                }
                let position = if denominator > 0.0 {
                    day.top.min(denominator) / denominator
                } else {
                    0.0
                };
                (day.key.clone(), position, major)
            })
            .collect()
    }

    /// Preserve the first row intersecting the viewport at the same screen
    /// coordinate through a density or width change.
    pub fn anchor_at(&self, scroll_y: f64) -> Option<TimelineAnchor> {
        self.days.iter().find_map(|day| {
            day.rows.iter().find_map(|row| {
                let absolute = day.top + DAY_HEADER_HEIGHT + row.top;
                (absolute + row.height >= scroll_y).then(|| TimelineAnchor {
                    photo_id: row.ids.first().cloned().unwrap_or_default(),
                    screen_y: absolute - scroll_y,
                })
            })
        })
    }

    pub fn restore_anchor(&self, anchor: &TimelineAnchor, viewport: f64) -> Option<f64> {
        for day in &self.days {
            for row in &day.rows {
                if row.ids.iter().any(|id| id == &anchor.photo_id) {
                    let absolute = day.top + DAY_HEADER_HEIGHT + row.top;
                    return Some(self.clamp_scroll(absolute - anchor.screen_y, viewport));
                }
            }
        }
        None
    }

    pub fn visible_days(&self, scroll_y: f64, viewport: f64) -> Vec<VisibleTimelineDay> {
        let top = (scroll_y - self.overscan()).max(0.0);
        let bottom = scroll_y + viewport + self.overscan();
        self.days
            .iter()
            .filter(|day| day.top + DAY_HEADER_HEIGHT >= top && day.top <= bottom)
            .map(|day| VisibleTimelineDay {
                key: day.key.clone(),
                y: day.top - scroll_y,
            })
            .collect()
    }

    pub fn visible_photos(&self, scroll_y: f64, viewport: f64) -> Vec<VisibleTimelinePhoto<'_>> {
        let top = (scroll_y - self.overscan()).max(0.0);
        let bottom = scroll_y + viewport + self.overscan();
        let Some(geometry) = self.geometry else {
            return Vec::new();
        };
        let mut visible = Vec::new();
        for day in &self.days {
            if day.top > bottom || day.top + day.height < top {
                continue;
            }
            for row in &day.rows {
                let y = day.top + DAY_HEADER_HEIGHT + row.top;
                if y > bottom || y + row.height < top {
                    continue;
                }
                let mut x = 0.0;
                for (id, width) in row.ids.iter().zip(&row.widths) {
                    if let Some(photo) = self.photos.get(id) {
                        visible.push(VisibleTimelinePhoto {
                            photo,
                            x,
                            y: y - scroll_y,
                            width: *width,
                            height: row.height,
                        });
                    }
                    x += *width + geometry.gap;
                }
            }
        }
        visible
    }

    fn overscan(&self) -> f64 {
        self.geometry.map_or(0.0, |geometry| {
            (geometry.target_height + geometry.gap) * 2.0 + DAY_HEADER_HEIGHT
        })
    }

    fn rebuild_days(&mut self) {
        let mut groups = HashMap::<String, Vec<String>>::new();
        for photo in self.photos.values() {
            groups
                .entry(photo.day.clone())
                .or_default()
                .push(photo.id.clone());
        }
        let mut keys: Vec<_> = groups.keys().cloned().collect();
        keys.sort_by(|left, right| day_cmp(left, right));
        self.days = keys
            .into_iter()
            .map(|key| {
                let mut ids = groups.remove(&key).unwrap_or_default();
                sort_day_ids(&mut ids, &self.photos);
                TimelineDay {
                    key,
                    ids,
                    rows: Vec::new(),
                    top: 0.0,
                    height: 0.0,
                }
            })
            .collect();
        if let Some(geometry) = self.geometry {
            for day in &mut self.days {
                Self::layout_day(day, &self.photos, geometry);
            }
        }
        self.rebuild_prefix();
    }

    fn reconcile_days(&mut self, old_day: Option<&str>, new_day: Option<&str>) {
        for key in [old_day, new_day].into_iter().flatten() {
            let mut ids: Vec<_> = self
                .photos
                .values()
                .filter(|photo| photo.day == key)
                .map(|photo| photo.id.clone())
                .collect();
            sort_day_ids(&mut ids, &self.photos);
            if let Some(index) = self.days.iter().position(|day| day.key == key) {
                if ids.is_empty() {
                    self.days.remove(index);
                } else if let Some(day) = self.days.get_mut(index) {
                    day.ids = ids;
                    if let Some(geometry) = self.geometry {
                        Self::layout_day(day, &self.photos, geometry);
                    }
                }
            } else if !ids.is_empty() {
                let mut day = TimelineDay {
                    key: key.to_owned(),
                    ids,
                    rows: Vec::new(),
                    top: 0.0,
                    height: 0.0,
                };
                if let Some(geometry) = self.geometry {
                    Self::layout_day(&mut day, &self.photos, geometry);
                }
                self.days.push(day);
            }
        }
        self.days
            .sort_by(|left, right| day_cmp(&left.key, &right.key));
        self.rebuild_prefix();
    }

    fn layout_day(
        day: &mut TimelineDay,
        photos: &HashMap<String, TimelinePhoto>,
        geometry: TimelineGeometry,
    ) {
        day.rows.clear();
        if geometry.compact {
            for id in &day.ids {
                let aspect = photos
                    .get(id)
                    .map_or(1.5, |photo| photo.aspect_ratio)
                    .clamp(0.35, 3.0);
                day.rows.push(TimelineRow {
                    ids: vec![id.clone()],
                    widths: vec![geometry.width],
                    height: (geometry.width / aspect).clamp(140.0, 420.0),
                    top: 0.0,
                });
            }
        } else {
            let mut start = 0;
            while start < day.ids.len() {
                let mut end = start;
                let mut aspect_sum = 0.0;
                let row_item_limit = if geometry.target_height > 320.0 {
                    2
                } else {
                    MAX_ROW_ITEMS
                };
                for id in day.ids.iter().skip(start) {
                    aspect_sum += photos
                        .get(id)
                        .map_or(1.5, |photo| photo.aspect_ratio)
                        .clamp(0.35, 3.0);
                    end += 1;
                    let count = end - start;
                    let natural = aspect_sum * geometry.target_height
                        + geometry.gap * count.saturating_sub(1) as f64;
                    if natural >= geometry.width || count >= row_item_limit {
                        break;
                    }
                }
                let fills =
                    end < day.ids.len() || aspect_sum * geometry.target_height >= geometry.width;
                let height = if fills {
                    ((geometry.width - geometry.gap * (end - start - 1) as f64) / aspect_sum)
                        // Ordinary timelines stay compact, while an explicit
                        // enlarged-text target may grow to the geometry's
                        // bounded 420px ceiling. Keeping the real justified
                        // height here avoids dumping all clamped leftover
                        // width into the final photograph in a row.
                        .clamp(76.0, geometry.target_height.clamp(320.0, 420.0))
                } else {
                    geometry.target_height.clamp(76.0, 320.0)
                };
                let mut widths = Vec::with_capacity(end - start);
                let mut used = 0.0;
                for (index, id) in day.ids.iter().enumerate().take(end).skip(start) {
                    let last = index + 1 == end;
                    let width = if fills && last {
                        (geometry.width - used).max(44.0)
                    } else {
                        let width = (photos
                            .get(id)
                            .map_or(1.5, |photo| photo.aspect_ratio)
                            .clamp(0.35, 3.0)
                            * height)
                            .round();
                        used += width + geometry.gap;
                        width
                    };
                    widths.push(width);
                }
                day.rows.push(TimelineRow {
                    ids: day.ids.iter().take(end).skip(start).cloned().collect(),
                    widths,
                    height,
                    top: 0.0,
                });
                start = end;
            }
        }
        let mut top = 0.0;
        for row in &mut day.rows {
            row.top = top;
            top += row.height + geometry.gap;
        }
        day.height = DAY_HEADER_HEIGHT + top;
    }

    fn rebuild_prefix(&mut self) {
        let mut top = 0.0;
        for day in &mut self.days {
            day.top = top;
            top += day.height;
        }
        self.total_height = top;
    }
}

fn day_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    let left_undated = left == "Undated";
    let right_undated = right == "Undated";
    left_undated
        .cmp(&right_undated)
        .then_with(|| right.cmp(left))
}

fn sort_day_ids(ids: &mut [String], photos: &HashMap<String, TimelinePhoto>) {
    ids.sort_by(|left, right| {
        let left_capture = photos
            .get(left)
            .map_or("", |photo| photo.captured_at.as_str());
        let right_capture = photos
            .get(right)
            .map_or("", |photo| photo.captured_at.as_str());
        right_capture
            .cmp(left_capture)
            .then_with(|| left.cmp(right))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(id: &str, day: &str, aspect_ratio: f64) -> TimelinePhoto {
        TimelinePhoto {
            id: id.into(),
            name: id.into(),
            favorite: false,
            day: day.into(),
            captured_at: format!("{day} 12:00"),
            aspect_ratio,
        }
    }

    fn geometry(width: f64) -> TimelineGeometry {
        TimelineGeometry {
            width,
            target_height: 160.0,
            gap: 12.0,
            compact: false,
        }
    }

    #[test]
    fn timeline_rows_have_exact_bounded_geometry() {
        eprintln!("phase date_markers_use_exact_variable_height_day_offsets");
        {
            let mut projection = TimelineProjection::default();
            projection.set_geometry(geometry(600.0));
            projection.replace([
                photo("wide", "2026-06-18", 3.0),
                photo("portrait", "2026-06-18", 0.4),
                photo("old", "2025-01-02", 1.0),
            ]);
            let viewport = 200.0;
            let old_top = projection.day_offset("2025-01-02", viewport).unwrap();
            let old_marker = projection
                .date_markers(viewport)
                .into_iter()
                .find(|(day, _, _)| day == "2025-01-02")
                .unwrap();
            assert!((old_marker.1 * projection.max_scroll(viewport) - old_top).abs() < 0.001);
        }

        eprintln!("phase virtual_window_is_bounded_and_keeps_canonical_row_geometry");
        {
            let mut projection = TimelineProjection::default();
            projection.set_geometry(geometry(900.0));
            projection.replace((0..2_000).map(|index| {
                photo(
                    &format!("photo-{index:04}"),
                    &format!("2026-{:02}-{:02}", 12 - (index / 600), 28 - (index % 27)),
                    0.5 + f64::from((index % 6) as u32) * 0.45,
                )
            }));
            let visible = projection.visible_photos(12_000.0, 800.0);
            assert!(!visible.is_empty());
            assert!(
                visible.len() <= 48,
                "overscan decoded {} photos",
                visible.len()
            );
            assert!(visible.iter().all(|entry| entry.width >= 44.0));
        }
    }

    #[test]
    fn timeline_updates_repack_and_restore_the_anchor_photo() {
        eprintln!("phase density_change_restores_the_same_photo_at_the_same_screen_y");
        {
            let mut projection = TimelineProjection::default();
            projection.set_geometry(geometry(720.0));
            projection.replace((0..80).map(|index| {
                photo(
                    &format!("p-{index:02}"),
                    if index < 40 {
                        "2026-06-18"
                    } else {
                        "2026-06-17"
                    },
                    0.6 + f64::from((index % 5) as u32) * 0.5,
                )
            }));
            let before = 900.0;
            let anchor = projection.anchor_at(before).unwrap();
            projection.set_geometry(TimelineGeometry {
                target_height: 110.0,
                ..geometry(720.0)
            });
            let restored = projection.restore_anchor(&anchor, 600.0).unwrap();
            let after = projection.anchor_at(restored).unwrap();
            assert_eq!(after.photo_id, anchor.photo_id);
            assert!((after.screen_y - anchor.screen_y).abs() < 0.001);
        }

        eprintln!("phase sparse_updates_repack_only_the_affected_day_semantically");
        {
            let mut projection = TimelineProjection::default();
            projection.set_geometry(geometry(600.0));
            projection.replace([
                photo("a", "2026-06-18", 1.0),
                photo("c", "2026-06-18", 3.0),
                photo("b", "2026-06-17", 1.0),
            ]);
            let first_old = projection.day_offset("2026-06-17", 100.0).unwrap();
            projection.upsert(photo("a", "2026-06-18", 2.0));
            let second_old = projection.day_offset("2026-06-17", 100.0).unwrap();
            assert!(
                (first_old - second_old).abs() > f64::EPSILON,
                "changed aspect updates following prefixes"
            );
            projection.upsert(photo("a", "2026-06-16", 2.0));
            assert!(projection.day_offset("2026-06-18", 100.0).is_some());
            assert!(projection.day_offset("2026-06-16", 100.0).is_some());
            assert!(projection.remove("b"));
            assert_eq!(projection.len(), 2);
        }
    }
}
