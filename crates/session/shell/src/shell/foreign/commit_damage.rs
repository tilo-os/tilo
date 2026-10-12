//! Bounded damage history across capture coalescing and missed paints.
use std::collections::VecDeque;

use vello::kurbo::Rect;

/// None means the client or capture path did not certify bounded damage.
pub(super) type Damage = Option<Vec<Rect>>;

#[derive(Default)]
pub(super) struct History {
    generation: u64,
    captured: Option<(u64, (u32, u32))>,
    commits: VecDeque<(u64, Damage)>,
    frames: VecDeque<(u64, (u32, u32), Damage)>,
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::super::ForeignFrameStore;
    use super::*;

    #[test]
    fn captured_pixels_keep_damage_from_coalesced_commits_and_missed_paints() {
        let frames = ForeignFrameStore::default();
        let id = 810_731;
        let pixels = [255, 0, 0, 255].repeat(1024);
        frames.publish_rgba(id, &pixels, 32, 32);
        let previous = frames.frame_seq(id);
        let first = Rect::new(2.0, 3.0, 6.0, 7.0);
        let second = Rect::new(12.0, 13.0, 16.0, 17.0);
        let last_commit = frames.with_entry(id, |entry| {
            entry.damage_history.begin(Some(vec![first]));
            entry.damage_history.begin(Some(vec![second]))
        });
        // The worker coalesced the first pending buffer. Its damage still
        // belongs to the replacement buffer which contains both updates.
        frames.publish_captured_for_test(id, &pixels, (32, 32), last_commit);
        let middle = frames.frame_seq(id);
        assert_eq!(
            frames.frame_damage_between(id, previous, middle, (32, 32)),
            Some(vec![first, second])
        );
        let third = Rect::new(22.0, 23.0, 26.0, 27.0);
        let next = frames.with_entry(id, |entry| entry.damage_history.begin(Some(vec![third])));
        frames.publish_captured_for_test(id, &pixels, (32, 32), next);
        let current = frames.frame_seq(id);
        assert_eq!(
            frames.frame_damage_between(id, previous, current, (32, 32)),
            Some(vec![first, second, third])
        );
        assert_eq!(
            frames.frame_damage_between(id, middle, current, (32, 32)),
            Some(vec![third])
        );
    }

    #[test]
    fn capture_history_cannot_certify_unknown_resized_or_evicted_frames() {
        let frames = ForeignFrameStore::default();
        let id = 810_732;
        let pixels = [255, 0, 0, 255].repeat(1024);
        frames.publish_rgba(id, &pixels, 32, 32);
        let previous = frames.frame_seq(id);
        let known = frames.with_entry(id, |entry| {
            entry
                .damage_history
                .begin(Some(vec![Rect::new(2.0, 3.0, 6.0, 7.0)]))
        });
        frames.publish_captured_for_test(id, &pixels, (32, 32), known);
        let current = frames.frame_seq(id);
        assert!(
            frames
                .frame_damage_between(id, previous, current, (31, 32))
                .is_none()
        );
        frames.publish_rgba(id, &pixels, 32, 32);
        assert!(
            frames
                .frame_damage_between(id, previous, frames.frame_seq(id), (32, 32))
                .is_none()
        );
        let start = frames.frame_seq(id);
        let newest = frames.with_entry(id, |entry| {
            let mut newest = 0;
            for _ in 0..65 {
                newest = entry
                    .damage_history
                    .begin(Some(vec![Rect::new(2.0, 3.0, 6.0, 7.0)]));
            }
            newest
        });
        frames.publish_captured_for_test(id, &pixels, (32, 32), newest);
        assert!(
            frames
                .frame_damage_between(id, start, frames.frame_seq(id), (32, 32))
                .is_none(),
            "evicted intermediate commits require whole-frame damage"
        );
    }
}

impl History {
    pub(super) fn begin(&mut self, damage: Damage) -> u64 {
        let Some(next) = self.generation.checked_add(1) else {
            *self = Self::default();
            return self.begin(None);
        };
        self.generation = next;
        let bounded = damage.filter(|rects| {
            !rects.is_empty()
                && rects.len() <= 128
                && rects
                    .iter()
                    .all(|rect| rect.is_finite() && rect.width() > 0.0 && rect.height() > 0.0)
        });
        self.commits.push_back((next, bounded));
        if self.commits.len() > 64 {
            self.commits.pop_front();
        }
        next
    }

    pub(super) fn publish(&mut self, frame: u64, commit: u64, size: (u32, u32)) {
        let damage = self.captured.and_then(|(previous, previous_size)| {
            if previous_size != size || previous >= commit {
                return None;
            }
            let mut next = previous.checked_add(1)?;
            let mut combined = Vec::new();
            for (generation, damage) in &self.commits {
                if *generation < next || *generation > commit {
                    continue;
                }
                if *generation != next {
                    return None;
                }
                combined.extend(damage.as_ref()?.iter().copied());
                if combined.len() > 128 {
                    return None;
                }
                next = next.checked_add(1)?;
            }
            if next != commit.checked_add(1)? {
                return None;
            }
            let extent = Rect::new(0.0, 0.0, f64::from(size.0), f64::from(size.1));
            Some(
                combined
                    .into_iter()
                    .map(|rect| rect.intersect(extent))
                    .filter(|rect| !rect.is_zero_area())
                    .collect(),
            )
        });
        self.captured = Some((commit, size));
        self.frames.push_back((frame, size, damage));
        if self.frames.len() > 64 {
            self.frames.pop_front();
        }
    }

    pub(super) fn between(&self, previous: u64, current: u64, size: (u32, u32)) -> Damage {
        if previous >= current {
            return None;
        }
        let mut next = previous.checked_add(1)?;
        let mut combined = Vec::new();
        for (frame, extent, damage) in &self.frames {
            if *frame < next || *frame > current {
                continue;
            }
            if *frame != next || *extent != size {
                return None;
            }
            combined.extend(damage.as_ref()?.iter().copied());
            if combined.len() > 128 {
                return None;
            }
            next = next.checked_add(1)?;
        }
        (next == current.checked_add(1)?).then_some(combined)
    }
}
