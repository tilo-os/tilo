//! What capture and playout need of one workspace's live session, wherever it runs: the
//! daemon holds the session, a desktop's runs over its link
//! ([`crate::link_call::Call`]), and a test runs one locally
//! ([`crate::live_session::LiveSession`]). The capture loops, the shared-window
//! pictures and the drawing above them ask this and nothing more.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::live::Track;
use crate::live_session::{Heard, LiveError, LiveSession};
use crate::page::PageObject;
use crate::share::Said;
use crate::video::{Layer, Source, VideoObject};

/// The live-session surface one workspace's capture, playout and drawing use.
pub trait Session: Send + Sync {
    /// Who this device is there.
    fn device(&self) -> [u8; 32];

    /// Show `tracks` of `device`: what this device's drawing reads of it.
    fn show(&self, device: [u8; 32], tracks: &[Track]);

    /// Moves whenever the session asks for a track it did not before.
    fn demand_revision(&self) -> u64;

    /// The pictures anybody shows: each source's layers.
    fn wanted_pictures(&self) -> Vec<(Source, Layer)>;

    /// Whether `source` has a layer nobody asked for since the last take.
    fn wants_picture_keyframe(&self, source: Source) -> bool;

    /// Take the layers of `source` that became wanted since the last take: each starts with a
    /// keyframe.
    fn take_picture_keyframes(&self, source: Source) -> Vec<Layer>;

    /// Send one picture of `source`.
    fn send_picture(&self, source: Source, object: &VideoObject) -> Result<(), LiveError>;

    /// The shared windows anybody shows.
    fn wanted_windows(&self) -> Vec<u64>;

    /// Take the windows that became wanted since the last take: each starts with a keyframe.
    fn take_window_keyframes(&self) -> Vec<u64>;

    /// Send one picture of the shared window `tab`.
    fn send_window(&self, tab: u64, object: &VideoObject) -> Result<(), LiveError>;

    /// The pages anybody shows.
    fn wanted_pages(&self) -> Vec<u64>;

    /// Take the pages that became wanted since the last take: each starts with a whole frame.
    fn take_page_keyframes(&self) -> Vec<u64>;

    /// Send one object of the shared window `tab`'s page.
    fn send_page(&self, tab: u64, object: &PageObject) -> Result<(), LiveError>;

    /// Publish `body` as object `sequence` of `track`: voice and presence travel this way.
    fn publish(&self, track: Track, sequence: u32, body: &[u8]) -> Result<(), LiveError>;

    /// Say `said` in the session's shared windows.
    fn say(&self, said: &[Said]) -> Result<(), LiveError>;

    /// The next object the session heard, opened.
    fn try_recv(&self) -> Option<Heard>;

    /// How many objects the session opened for this device.
    fn heard_total(&self) -> u64;

    /// Devices the session reads but has no room to read.
    fn unheard(&self) -> usize;

    /// Why the session could not open what it read, by reason and count.
    fn refusals(&self) -> BTreeMap<String, usize>;

    /// Per device, the presence objects opened from it and how often it left.
    fn presence_from(&self) -> BTreeMap<[u8; 32], (u64, u64)>;

    /// Have `wake` woken wherever this device next reads the session.
    fn set_wake(&self, wake: Arc<dyn Fn() + Send + Sync>);

    /// Have `ended` run when the session ends.
    fn set_ended(&self, ended: Arc<dyn Fn() + Send + Sync>);

    /// Have `moved` run wherever what was not heard moves.
    fn set_unheard_changed(&self, moved: Arc<dyn Fn() + Send + Sync>);

    /// Whether this device reads the session's presence at all.
    fn hear_presence(&self, hear: bool);

    /// Whether the session ended.
    fn ended(&self) -> bool;
}

impl Session for LiveSession {
    fn device(&self) -> [u8; 32] {
        LiveSession::device(self)
    }

    fn show(&self, device: [u8; 32], tracks: &[Track]) {
        LiveSession::show(self, device, tracks);
    }

    fn demand_revision(&self) -> u64 {
        LiveSession::demand_revision(self)
    }

    fn wanted_pictures(&self) -> Vec<(Source, Layer)> {
        LiveSession::wanted_pictures(self)
    }

    fn wants_picture_keyframe(&self, source: Source) -> bool {
        LiveSession::wants_picture_keyframe(self, source)
    }

    fn take_picture_keyframes(&self, source: Source) -> Vec<Layer> {
        LiveSession::take_picture_keyframes(self, source)
    }

    fn send_picture(&self, source: Source, object: &VideoObject) -> Result<(), LiveError> {
        LiveSession::send_picture(self, source, object)
    }

    fn wanted_windows(&self) -> Vec<u64> {
        LiveSession::wanted_windows(self)
    }

    fn take_window_keyframes(&self) -> Vec<u64> {
        LiveSession::take_window_keyframes(self)
    }

    fn send_window(&self, tab: u64, object: &VideoObject) -> Result<(), LiveError> {
        LiveSession::send_window(self, tab, object)
    }

    fn wanted_pages(&self) -> Vec<u64> {
        LiveSession::wanted_pages(self)
    }

    fn take_page_keyframes(&self) -> Vec<u64> {
        LiveSession::take_page_keyframes(self)
    }

    fn send_page(&self, tab: u64, object: &PageObject) -> Result<(), LiveError> {
        LiveSession::send_page(self, tab, object)
    }

    fn publish(&self, track: Track, sequence: u32, body: &[u8]) -> Result<(), LiveError> {
        LiveSession::publish(self, track, sequence, body)
    }

    fn say(&self, said: &[Said]) -> Result<(), LiveError> {
        LiveSession::say(self, said)
    }

    fn try_recv(&self) -> Option<Heard> {
        LiveSession::try_recv(self)
    }

    fn heard_total(&self) -> u64 {
        LiveSession::heard_total(self)
    }

    fn unheard(&self) -> usize {
        LiveSession::unheard(self)
    }

    fn refusals(&self) -> BTreeMap<String, usize> {
        LiveSession::refusals(self)
    }

    fn presence_from(&self) -> BTreeMap<[u8; 32], (u64, u64)> {
        LiveSession::presence_from(self)
    }

    fn set_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        LiveSession::set_wake(self, wake);
    }

    fn set_ended(&self, ended: Arc<dyn Fn() + Send + Sync>) {
        LiveSession::set_ended(self, ended);
    }

    fn set_unheard_changed(&self, moved: Arc<dyn Fn() + Send + Sync>) {
        LiveSession::set_unheard_changed(self, moved);
    }

    fn hear_presence(&self, hear: bool) {
        LiveSession::hear_presence(self, hear);
    }

    fn ended(&self) -> bool {
        LiveSession::ended(self)
    }
}

#[cfg(all(feature = "relay", feature = "iroh", not(target_arch = "wasm32")))]
mod linked {
    use super::{BTreeMap, Layer, PageObject, Said, Source, Track, VideoObject};

    impl super::Session for crate::link_call::Call {
        fn device(&self) -> [u8; 32] {
            crate::link_call::Call::device(self)
        }

        fn show(&self, device: [u8; 32], tracks: &[Track]) {
            crate::link_call::Call::show(self, device, tracks);
        }

        fn demand_revision(&self) -> u64 {
            crate::link_call::Call::demand_revision(self)
        }

        fn wanted_pictures(&self) -> Vec<(Source, Layer)> {
            crate::link_call::Call::wanted_pictures(self)
        }

        fn wants_picture_keyframe(&self, source: Source) -> bool {
            crate::link_call::Call::wants_picture_keyframe(self, source)
        }

        fn take_picture_keyframes(&self, source: Source) -> Vec<Layer> {
            crate::link_call::Call::take_picture_keyframes(self, source)
        }

        fn send_picture(
            &self,
            source: Source,
            object: &VideoObject,
        ) -> Result<(), super::LiveError> {
            crate::link_call::Call::send_picture(self, source, object)
        }

        fn wanted_windows(&self) -> Vec<u64> {
            crate::link_call::Call::wanted_windows(self)
        }

        fn take_window_keyframes(&self) -> Vec<u64> {
            crate::link_call::Call::take_window_keyframes(self)
        }

        fn send_window(&self, tab: u64, object: &VideoObject) -> Result<(), super::LiveError> {
            crate::link_call::Call::send_window(self, tab, object)
        }

        fn wanted_pages(&self) -> Vec<u64> {
            crate::link_call::Call::wanted_pages(self)
        }

        fn take_page_keyframes(&self) -> Vec<u64> {
            crate::link_call::Call::take_page_keyframes(self)
        }

        fn send_page(&self, tab: u64, object: &PageObject) -> Result<(), super::LiveError> {
            crate::link_call::Call::send_page(self, tab, object)
        }

        fn publish(
            &self,
            track: Track,
            sequence: u32,
            body: &[u8],
        ) -> Result<(), super::LiveError> {
            crate::link_call::Call::publish(self, track, sequence, body)
        }

        fn say(&self, said: &[Said]) -> Result<(), super::LiveError> {
            crate::link_call::Call::say(self, said)
        }

        fn try_recv(&self) -> Option<super::Heard> {
            crate::link_call::Call::try_recv(self)
        }

        fn heard_total(&self) -> u64 {
            crate::link_call::Call::heard_total(self)
        }

        fn unheard(&self) -> usize {
            crate::link_call::Call::unheard(self)
        }

        fn refusals(&self) -> BTreeMap<String, usize> {
            crate::link_call::Call::refusals(self)
        }

        fn presence_from(&self) -> BTreeMap<[u8; 32], (u64, u64)> {
            crate::link_call::Call::presence_from(self)
        }

        fn set_wake(&self, wake: std::sync::Arc<dyn Fn() + Send + Sync>) {
            crate::link_call::Call::set_wake(self, wake);
        }

        fn set_ended(&self, ended: std::sync::Arc<dyn Fn() + Send + Sync>) {
            crate::link_call::Call::set_ended(self, ended);
        }

        fn set_unheard_changed(&self, moved: std::sync::Arc<dyn Fn() + Send + Sync>) {
            crate::link_call::Call::set_unheard_changed(self, moved);
        }

        fn hear_presence(&self, hear: bool) {
            crate::link_call::Call::hear_presence(self, hear);
        }

        fn ended(&self) -> bool {
            crate::link_call::Call::ended(self)
        }
    }
}
