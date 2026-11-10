//! The machine as a Linux desktop reads it: off the resources its services publish — `network`
//! from the connectivity service, `sound` and `media` from the audio service, `power:battery`
//! from the power service.

use std::rc::Rc;
use std::time::Instant;

use tilo_audio_service::media::Media as Players;
use tilo_audio_service::sound::{Sound, Use, UseKind};
use tilo_connectivity_service::model::{LinkKind, Network};
use tilo_desktop::chrome::{
    ActiveMediaUse, LinkKind as BarLinkKind, LinkStatus, MediaKind, VolumeState,
};
use tilo_desktop_surface::now_playing::NowPlaying;
use tilo_power_mgmt::BatteryState;
use tilo_service::Services;
use tilo_session_power::{BATTERY, Battery};

use super::{Charge, Handover, Machine, Media, Reading};
use crate::platform::keys::{MEDIA, NETWORK, SOUND};

impl Machine for Services {
    fn revision(&self) -> u64 {
        Services::revision(self)
    }

    fn read(&self) -> Reading {
        reading(
            read(self, NETWORK),
            read(self, SOUND),
            read(self, BATTERY),
            read(self, MEDIA),
            Instant::now(),
        )
    }
}

/// The machine in the bar's terms, from what its services published; `at` is when what plays was
/// read.
pub(crate) fn reading(
    network: Option<Network>,
    sound: Option<Sound>,
    battery: Option<Battery>,
    media: Option<Players>,
    at: Instant,
) -> Reading {
    Reading {
        link: network
            .and_then(|network| network.link)
            .map(|link| LinkStatus {
                kind: match link.kind {
                    LinkKind::Offline => BarLinkKind::Offline,
                    LinkKind::Wired => BarLinkKind::Wired,
                    LinkKind::Wifi => BarLinkKind::Wifi,
                },
                name: link.name,
                signal: link.signal,
            }),
        volume: sound.as_ref().and_then(|sound| {
            Some(VolumeState {
                level: sound.volume?,
                muted: sound.muted,
            })
        }),
        uses: sound.map_or_else(Vec::new, |sound| {
            sound.uses.iter().map(active_use).collect()
        }),
        battery: battery
            .filter(|battery| battery.present)
            .map(|battery| Charge {
                percent: battery.percentage,
                charging: battery.state == BatteryState::Charging,
                draining: battery.state.is_discharging(),
            }),
        media: media.map(|players| Rc::new(Playing { players, at }) as Rc<dyn Media>),
    }
}

/// What the audio service published of the players, and when: it publishes the playhead as it
/// stood at its last event, so a reading advances it from here.
struct Playing {
    players: Players,
    at: Instant,
}

impl Media for Playing {
    fn playing(&self) -> Option<NowPlaying> {
        tilo_desktop_surface::now_playing::now_playing(
            &self.players,
            Some(self.at),
            Instant::now(),
            None,
            None,
        )
    }

    fn handover(&self) -> Option<Handover> {
        let front = self.players.front()?;
        Some(Handover {
            session_id: front.session_id.clone()?,
            app_id: front.app_id.clone()?,
            title: front.headline(),
            url: front.url.clone()?,
            position_ms: tilo_desktop_surface::now_playing::playhead_at(
                front,
                Some(self.at),
                Instant::now(),
            ),
            playing: front.playing,
        })
    }
}

/// One application using the camera or the microphone, as the bar names it.
fn active_use(published: &Use) -> ActiveMediaUse {
    ActiveMediaUse {
        client_id: published.client_id,
        label: published.label.clone(),
        kind: match published.kind {
            UseKind::Microphone => MediaKind::Microphone,
            UseKind::Camera => MediaKind::Camera,
        },
    }
}

/// The reading of `resource` , parsed into what its service publishes.
fn read<T: serde::de::DeserializeOwned>(services: &Services, resource: &str) -> Option<T> {
    let published = services.read(resource)?;
    match serde_json::from_value(published) {
        Ok(reading) => Some(reading),
        Err(error) => {
            tracing::warn!(%error, resource, "a reading the session shows did not parse");
            None
        }
    }
}
