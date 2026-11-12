//! A call's pictures on the workspace's live plane: the camera and screen this device's call
//! encodes for the layers the call's devices show, and the pictures they send for its app to draw.
//! A plane track is one layer of one source, and asking for it is the demand:
//! [`Plane::wanted_pictures`] is everything the call's devices draw of this device, and a layer it
//! does not list is never encoded. A layer that gains a reader is answered with a keyframe, so a
//! still screen sends it the picture the encoder stands on rather than a new encode.
//! What arrives is opened by the plane with the workspace's keys and queued in the call's inbox: a
//! page's pictures and a device's are the same objects, drawn the same way.

use std::sync::atomic::Ordering;

use super::{Shared, pictures};
use tilo_live::live::Track;
use tilo_lock::MutexRecover as _;

impl Shared {
    /// This device's pictures as the encoder of one source reads them.
    pub(super) fn sink(self: &std::sync::Arc<Self>) -> std::sync::Arc<dyn pictures::Sink> {
        std::sync::Arc::clone(self) as std::sync::Arc<dyn pictures::Sink>
    }

    /// Stop reading the call's pictures on the plane: a call that ended draws nothing, and the
    /// plane stops sending it the devices' layers.
    pub(super) fn unsubscribe_pictures(&self) {
        let Some(plane) = self.presence.plane(&self.workspace) else {
            return;
        };
        let mut shown = self.shown_devices.lock_recover();
        for device in std::mem::take(&mut *shown) {
            plane.show(device, &[]);
        }
    }

    /// Something a device of this call shows changed: every source encodes for it from now.
    pub(super) fn wake_pictures(&self) {
        let side = self.side.lock_recover();
        if let Some(camera) = &side.camera {
            camera.sending.ask_keyframe();
        }
        if let Some(screen) = &side.screen {
            screen.ask_keyframe();
        }
    }

    /// Move the pictures once: what the plane showed since the last pass is drawn here, and what a
    /// device started showing is encoded for at once.
    pub(super) fn pump_pictures(&self) {
        // The app changed what it shows, or the people of the call moved — a device joined or left:
        // the devices the app's layers go to are told again, and those that left are no longer
        // read.
        let people = self.presence.persons_revision(&self.call);
        if self.shows_at.swap(people, Ordering::Relaxed) != people
            || self.shows_dirty.swap(false, Ordering::Release)
        {
            self.send_showing(&self.presence.persons(&self.call));
        }
        let Some(plane) = self.presence.plane(&self.workspace) else {
            return;
        };
        // A layer gained a reader, or one left: the encoder answers with a keyframe even while a
        // still picture has no new frame.
        let revision = plane.demand_revision();
        if self.pictures_at.swap(revision, Ordering::Relaxed) != revision {
            self.wake_pictures();
        }
        let (heard, lost) = self.presence.took_pictures(&self.workspace, &self.call);
        for (device, track) in lost {
            if let Some(source) = track_source(track) {
                // The stream lost a picture on the way here: nothing of it is drawn until its next
                // keyframe.
                self.video.behind(device, source);
            }
        }
        if heard.is_empty() {
            return;
        }
        for heard in heard {
            if heard.sender == self.own {
                continue;
            }
            let Some(source) = track_source(heard.track) else {
                continue;
            };
            self.video.heard(heard.sender, source, heard.body);
        }
    }
}

impl pictures::Sink for Shared {
    fn asking(&self, source: pictures::Source) -> pictures::Asking {
        // What the plane's devices show of this source, and this person's own tile: one encode per
        // layer either way.
        let mut wanted = std::collections::BTreeSet::new();
        let mut restart = std::collections::BTreeSet::new();
        if let Some(plane) = self.presence.plane(&self.workspace) {
            for (of, layer) in plane.wanted_pictures() {
                if of == source {
                    wanted.insert(layer);
                }
            }
            if plane.wants_picture_keyframe(source) {
                let _ = plane.take_picture_keyframes(source);
                restart.insert(source);
            }
        }
        let mut asking = pictures::Asking {
            wanted: std::collections::BTreeMap::new(),
            restart,
            own: self.video.own_layer(),
        };
        if !wanted.is_empty() {
            asking.wanted.insert(source, wanted);
        }
        asking
    }

    fn sent(&self, source: pictures::Source, object: &pictures::VideoObject) {
        if let Some(plane) = self.presence.plane(&self.workspace)
            && plane.send_picture(source, object).is_err()
        {
            // Nobody shows this layer on the plane any more, or its group is past what one carries:
            // the next keyframe starts it over.
            tracing::debug!(
                ?source,
                layer = ?object.layer,
                "a call picture had no plane reader"
            );
        }
    }

    fn own(&self, object: &pictures::VideoObject) {
        self.video.own_picture(object);
    }
}

/// The source a plane picture track carries, if it is one a call draws.
pub(super) const fn track_source(track: Track) -> Option<pictures::Source> {
    match track {
        Track::Camera(_) => Some(pictures::Source::Camera),
        Track::Screen(_) => Some(pictures::Source::Screen),
        _ => None,
    }
}
