//! Waves on the room's live presence, as this desktop sends and hears them.
//!
//! A browser tab and this desktop wave the same way, through the same code
//! ([`tilo_desktop::here::WaveOut`], [`tilo_desktop::here::WaveEar`]): the
//! waver's live presence frame carries `{seq, target}`, the target being
//! the waved-at device's key in hex (its live publisher), and only that
//! person's devices show it, once per new `seq`. Here a person is known by
//! their roster id, `peer@{handle}` with the handle of their device key, so
//! a wave is aimed at the device behind a roster id and heard back as the
//! roster id of the device that waved.

use std::collections::HashMap;

use tilo_desktop::here::{Wave, WaveEar, WaveOut};
use tilo_relay_protocol::live::LiveWave;

/// The waves this desktop sent and heard on the live plane.
#[derive(Debug, Default)]
pub struct LiveWaves {
    ear: WaveEar,
    out: WaveOut,
    /// The device key behind each handle a live frame came from.
    devices: HashMap<u64, [u8; 32]>,
    /// Who waved at this person and was not taken yet, by roster id.
    heard: Vec<String>,
}

#[must_use]
pub fn roster_id(device: &[u8; 32]) -> String {
    crate::shell::peer_presence::presence_id(neighbour_wire::peer_handle(device))
}

impl LiveWaves {
    /// A live frame from `from` carrying `wave`; `own` is this device's
    /// key. Keeps the waver, and says so, when it is a new wave at this
    /// person.
    pub fn hear(&mut self, from: [u8; 32], wave: Option<&LiveWave>, own: &[u8; 32]) -> bool {
        self.devices
            .insert(neighbour_wire::peer_handle(&from), from);
        let wave = wave.map(|wave| Wave {
            seq: wave.seq,
            target: wave.target.clone(),
        });
        let own = hex::encode(own);
        let heard = self
            .ear
            .hear(&hex::encode(from), wave.as_ref(), |target| target == own);
        if heard {
            self.heard.push(roster_id(&from));
        }
        heard
    }

    /// The wave to put on this device's frame for a wave at `target` (a
    /// roster id) at `now_ms`: aimed at the device behind it, among those a
    /// live frame came from and `others` (the room's other devices). `None`
    /// for nobody known by that id, or someone waved at within the minute.
    pub fn aim(&mut self, target: &str, others: &[[u8; 32]], now_ms: u64) -> Option<LiveWave> {
        let handle: u64 = target.strip_prefix("peer@")?.parse().ok()?;
        let device = self.devices.get(&handle).copied().or_else(|| {
            others
                .iter()
                .find(|device| neighbour_wire::peer_handle(device) == handle)
                .copied()
        })?;
        let wave = self.out.wave_at(&hex::encode(device), now_ms)?;
        Some(LiveWave {
            seq: wave.seq,
            target: wave.target,
        })
    }

    /// Who waved at this person since the last call, by roster id.
    pub fn take(&mut self) -> Vec<String> {
        std::mem::take(&mut self.heard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::live_presence::{Outbound, inbound};
    use tilo_desktop::here::{HereState, LiveMember};
    use tilo_relay_protocol::live::{LiveFrame, LiveParticipant, ParticipantKind};

    fn participant(label: &str) -> LiveParticipant {
        LiveParticipant {
            kind: ParticipantKind::Person,
            label: label.into(),
            color: [1, 2, 3],
            activity: None,
        }
    }

    /// A browser tab's member as its page hands it to the desktop: the
    /// publisher and what its live presence frame carries.
    fn member(publisher: &[u8; 32], frame: &[u8]) -> LiveMember {
        let Ok(LiveFrame::Presence(presence)) = LiveFrame::decode(frame) else {
            panic!("a presence frame");
        };
        LiveMember {
            publisher: hex::encode(publisher),
            kind: "person".into(),
            label: presence.participant.map(|p| p.label).unwrap_or_default(),
            wave: presence.wave.map(|wave| Wave {
                seq: wave.seq,
                target: wave.target,
            }),
            ..LiveMember::default()
        }
    }

    /// The installed desktop waves at a browser tab and the tab waves back,
    /// both on the room's live presence: each frame the one sends is the
    /// frame the other reads, and each side shows a wave once.
    #[test]
    fn waves_cross_between_this_desktop_and_a_browser_tab() {
        let desk = [7u8; 32];
        let tab = [9u8; 32];
        let ws = 42;
        let desktop_focus = tilo_relay_protocol::live::LiveRecord {
            app: tilo_desktop_model::ownership::DESKTOP_APP.into(),
            record: tilo_desktop_model::ownership::DESKTOP_RECORD.into(),
        };
        let now = std::time::Instant::now();

        // The tab sees the desktop's people; the desktop has seen the tab.
        let mut browser = HereState::default();
        let own = LiveMember {
            publisher: hex::encode(tab),
            kind: "person".into(),
            label: "Eager Dolphin".into(),
            ..LiveMember::default()
        };
        let mut desktop = LiveWaves::default();
        desktop.hear(tab, None, &desk);

        let mut outbound = Outbound::default();
        let wave = desktop
            .aim(&roster_id(&tab), &[], 1_000)
            .expect("the tab's device is known from its frames");
        assert_eq!(wave.target, hex::encode(tab));
        assert!(
            desktop.aim(&roster_id(&tab), &[], 2_000).is_none(),
            "not again within the minute"
        );
        let frames = outbound.wave(ws, desktop_focus.clone(), participant("Kim"), wave, now);
        assert_eq!(frames.len(), 2, "first it says it is here, then it waves");
        for frame in &frames {
            browser.set_members(Some(own.clone()), Some(vec![member(&desk, frame)]));
        }
        let heard = browser.take_waves();
        assert_eq!(heard.len(), 1, "the tab hears the wave once");
        assert_eq!(heard[0].id, hex::encode(desk));
        assert_eq!(heard[0].name, "Kim");
        // Restated with the desktop's next frame, it is not news again.
        let restated = outbound.wave(
            ws,
            desktop_focus,
            participant("Kim"),
            LiveWave {
                seq: 1,
                target: hex::encode(tab),
            },
            now,
        );
        browser.set_members(Some(own), Some(vec![member(&desk, &restated[0])]));
        assert!(browser.take_waves().is_empty());

        assert!(browser.wave_at(&hex::encode(desk), 3_000));
        assert_eq!(
            browser.waved(3_000),
            [hex::encode(desk)],
            "its row shows it sent"
        );
        let sent = browser.sent_wave().cloned().expect("the page's wave");
        let frame = LiveFrame::Presence(tilo_relay_protocol::live::PresenceFrame {
            seq: 2,
            participant: Some(participant("Eager Dolphin")),
            wave: Some(LiveWave {
                seq: sent.seq,
                target: sent.target,
            }),
            ..tilo_relay_protocol::live::PresenceFrame::default()
        })
        .encode()
        .expect("the page's frame");
        let read = inbound(&frame).expect("the desktop reads it");
        desktop.hear(tab, read.wave.as_ref(), &desk);
        assert_eq!(desktop.take(), [roster_id(&tab)], "the desktop hears who");
        desktop.hear(tab, read.wave.as_ref(), &desk);
        assert!(desktop.take().is_empty(), "once");
    }

    /// A wave at someone else, or from a device first heard with it, shows
    /// nothing here.
    #[test]
    fn only_new_waves_at_this_person_are_heard() {
        let (desk, ada) = ([7u8; 32], [8u8; 32]);
        let mut waves = LiveWaves::default();
        let at = |seq, target: &[u8; 32]| LiveWave {
            seq,
            target: hex::encode(target),
        };
        waves.hear(ada, Some(&at(4, &desk)), &desk);
        assert!(waves.take().is_empty(), "already there when first heard");
        waves.hear(ada, Some(&at(5, &[1; 32])), &desk);
        assert!(waves.take().is_empty(), "for someone else");
        waves.hear(ada, Some(&at(6, &desk)), &desk);
        assert_eq!(waves.take(), [roster_id(&ada)]);
        assert!(
            waves.aim("peer@1", &[], 0).is_none(),
            "nobody known by that id"
        );
        assert!(waves.aim(&roster_id(&[3; 32]), &[[3; 32]], 0).is_some());
    }
}
