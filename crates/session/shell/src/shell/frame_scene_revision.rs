//! One scene identity space for desktop and lock producers sharing a presenter.

use super::FrameBuilder;

impl FrameBuilder {
    /// Zero means an unversioned full frame. Exhaustion stays at zero rather
    /// than reusing an identity that a presenter might still hold.
    pub(crate) fn reserve_scene_revision(&mut self) -> u64 {
        let revision = self.next_scene_revision;
        if revision != 0 {
            self.next_scene_revision = revision.checked_add(1).unwrap_or(0);
        }
        revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_revision_exhaustion_remains_an_unversioned_full_boundary() {
        let mut builder = FrameBuilder::new((1280, 800));
        assert_eq!(builder.reserve_scene_revision(), 1);
        assert_eq!(builder.reserve_scene_revision(), 2);
        builder.next_scene_revision = u64::MAX;
        assert_eq!(builder.reserve_scene_revision(), u64::MAX);
        assert_eq!(builder.reserve_scene_revision(), 0);
        assert_eq!(builder.reserve_scene_revision(), 0);
        let frame = builder.paint_with_damage();
        assert_eq!(frame.revision, 0);
        assert!(frame.damage_regions.is_none());
        let next = builder.paint_with_damage();
        drop(builder);
        assert_eq!(next.revision, 0);
        assert!(next.damage_regions.is_none());
    }
}
