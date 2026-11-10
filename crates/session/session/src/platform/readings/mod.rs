//! What a host reads of the machine it runs on, in the bar's terms: a Linux desktop reads its
//! services, the page what a browser knows and nothing for the rest.

use std::rc::Rc;

use tilo_desktop::chrome::{ActiveMediaUse, LinkStatus, VolumeState};
use tilo_desktop_surface::now_playing::NowPlaying;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(all(target_os = "linux", test))]
pub(crate) use linux::reading;

/// The machine a host reads.
pub trait Machine {
    /// Moves whenever anything it reads changed: a pass where it stands still reads nothing.
    fn revision(&self) -> u64;
    /// What it reads now.
    fn read(&self) -> Reading;
}

/// The machine as a host read it, each part only when it could read it.
#[derive(Clone, Default)]
pub struct Reading {
    pub link: Option<LinkStatus>,
    pub volume: Option<VolumeState>,
    /// The apps using the camera or the microphone.
    pub uses: Vec<ActiveMediaUse>,
    /// The pack's charge, when this machine has one.
    pub battery: Option<Charge>,
    pub media: Option<Rc<dyn Media>>,
}

/// How full a machine's pack is, and which way it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Charge {
    pub percent: u8,
    pub charging: bool,
    /// It runs off the pack, which empties.
    pub draining: bool,
}

/// What the machine's players hold, read when the clock asks: a playhead moves on between readings.
pub trait Media {
    /// What plays now; `None` while no player holds anything and none stands paused.
    fn playing(&self) -> Option<NowPlaying>;
    /// What the front player names of itself, when another of the person's devices may continue it.
    fn handover(&self) -> Option<Handover>;
}

/// What the front player names, when another of the person's devices may continue what plays here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handover {
    pub session_id: String,
    pub app_id: String,
    pub title: String,
    /// The stream it plays from.
    pub url: String,
    /// Where the playhead stands now, in milliseconds.
    pub position_ms: u64,
    /// It plays rather than stands paused.
    pub playing: bool,
}
