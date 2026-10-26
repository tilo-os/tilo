//! An image attachment whose bytes are not on this device yet, as a read
//! ([`ui::arriving`]): bytes on this device decode within a frame or two and
//! show with no stand-in; bytes on their way from the room keep the card's
//! ground, then its skeleton, then say where they come from; bytes no
//! device has say so.

use ui::arriving::Step;

use crate::app::ChatApp;

impl ChatApp {
    /// The placeholder step of the image `blob` while its bytes are on
    /// their way, adding what its card says of them to `meta`; `None` once
    /// they are here (or never will be).
    pub(crate) fn image_arrival(&self, blob: &str, meta: &mut Vec<String>) -> Option<Step> {
        let read = self.content_reads.read(blob);
        let Some(clock) = read.clock() else {
            if let Some(missing) = read.with(|read| read.failure().copied()) {
                meta.push(missing.to_string());
            }
            return None;
        };
        let now = ui::motion::frame_now();
        if let Some(next) = clock.next_step_at(now) {
            let wake = self.arriving_wake.get().map_or(next, |at| at.min(next));
            self.arriving_wake.set(Some(wake));
        }
        let face =
            ui::widget::ArrivalFace::new(ui::widget::SkeletonShape::Text { lines: 1 }, &clock, now);
        meta.extend(face.caption());
        Some(face.step())
    }
}
