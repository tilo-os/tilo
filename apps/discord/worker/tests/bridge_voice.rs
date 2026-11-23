//! A Discord voice channel and the Tilo voice channel it is mapped to, carried
//! as one call by the workspace's host: the `discord-worker` binary in the
//! fake server's voice channel, the host on the workspace's live session, and
//! a member's device on it too. What a Discord person says is heard in Tilo,
//! what the member says is heard in Discord, nothing comes back, and every
//! call view shows the call bridged to Discord while the bridge carries it.

use std::time::Duration;

use collab_types::presence::CallPresence;
use fake_discord::{CHANNEL_VOICE, User, tone};
use tilo_live::live::Track;
use tilo_live::live_session::{Join, LiveSession};
use tilo_live::presence::{self, Board, Call, LiveCaller, Own};
use tilo_live::roster::Roster;
use tilo_live::scope::Scope;
use tilo_live::voice::{FRAME_SAMPLES, VoiceObject};
use tilo_server::workspace_bots::discord_voice::{SERVICE, VoiceBridge};

use super::*;

/// A member's device on the workspace's live session: what it says of
/// itself, and the voices it heard, by sender.
struct Member {
    live: LiveSession,
    board: Board,
    voices: Vec<([u8; 32], Vec<u8>)>,
    sequence: u32,
}

#[expect(
    clippy::cast_precision_loss,
    reason = "milliseconds since the epoch are exact in an f64"
)]
fn now_ms() -> f64 {
    tilo_clock::unix_now_ms() as f64
}

impl Member {
    /// Read what the live session heard and say what is due.
    fn pump(&mut self) {
        let now = now_ms();
        while let Some(heard) = self.live.try_recv() {
            match heard.track {
                Track::Presence => {
                    self.board.heard(heard.sender, &heard.body, now);
                }
                Track::Audio => {
                    let object = VoiceObject::decode(&heard.body).unwrap();
                    self.voices.push((heard.sender, object.payload));
                }
                _ => {}
            }
        }
        if let Some(frames) = self.board.due(now) {
            presence::say(&self.live, &frames).unwrap();
        }
    }

    /// Be in `channel`'s call, or in none.
    fn in_call(&mut self, channel: Option<&str>) {
        self.board.say(&Own {
            label: "Grace".into(),
            device: "laptop".into(),
            call: channel.map(|channel| Call {
                call: CallPresence {
                    channel: channel.into(),
                    name: "voice".into(),
                    bridge: None,
                },
                camera: false,
                screen: false,
                muted: false,
                deafened: false,
            }),
            ..Own::default()
        });
    }

    /// Say one 20 ms frame of `opus` in the call.
    fn speak(&mut self, opus: &[u8]) {
        let object = VoiceObject {
            sequence: u64::from(self.sequence),
            timestamp: u64::from(self.sequence) * u64::from(FRAME_SAMPLES),
            duration: FRAME_SAMPLES,
            payload: opus.to_vec(),
        };
        self.live
            .publish(Track::Audio, self.sequence, &object.encode().unwrap())
            .unwrap();
        self.sequence = self.sequence.wrapping_add(1);
    }

    /// The bridge in `channel`'s call, as this device's call views see it.
    fn bridge(&self, channel: &str) -> Option<LiveCaller> {
        self.board
            .calls()
            .into_iter()
            .filter(|call| call.channel == channel)
            .flat_map(|call| call.people)
            .find(|caller| caller.bridge.is_some())
    }

    /// Pump until `done` holds, doing `each` every 20 ms.
    async fn until(
        &mut self,
        what: &str,
        mut each: impl FnMut(&mut Self),
        mut done: impl FnMut(&Self) -> bool,
    ) {
        let deadline = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
        loop {
            self.pump();
            if done(self) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{what} did not happen"
            );
            each(self);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_discord_voice_channel_and_its_tilo_call_hear_each_other_through_the_bridge() {
    let (bridge, mut runner) = Bridge::start().await;
    let voice = settle(&mut runner, "the voice channel is mapped", || async {
        bridge
            .workspace
            .guild()
            .await
            .by_discord(MappedKind::Channel, CHANNEL_VOICE)
            .map(|mapping| mapping.tilo.clone())
    })
    .await;
    let workspaces = &bridge.workspace.workspaces;
    let workspace = &bridge.workspace.id;
    let host = &bridge.workspace.cert;

    // Grace's laptop is a device of the workspace, on its live session.
    let (laptop_key, laptop) = tilo_workspace_service::workspaces::fixtures::device_cert(
        &SigningKey::from_bytes(&[9; 32]),
        "laptop",
        DeviceCapabilities::member(),
    );
    tilo_workspace_service::workspaces::fixtures::entry_join_as(
        workspaces,
        host,
        workspace,
        (&laptop_key, &laptop),
        9,
        tilo_server_protocol::workspaces::Role::Editor,
    )
    .await;
    let relay = bridge.server.live_relay().expect("the host's live session");
    let keys = workspaces
        .device_keys()
        .unwrap()
        .group_keys(workspace, collab_types::EVERY_MEMBER, true)
        .await
        .unwrap()
        .unwrap();
    let mut grace = Member {
        live: LiveSession::local(
            &relay,
            &Join {
                scope: Scope::workspace(workspace.clone()),
                device: laptop.device_public_key,
                keys: keys.into(),
                roster: Roster::new([laptop.device_public_key, host.device_public_key]),
                publish: true,
                presence: true,
            },
        )
        .unwrap(),
        board: Board::new(workspace.clone(), laptop.device_public_key),
        voices: Vec::new(),
        sequence: 0,
    };
    grace.in_call(None);

    let carrier = tokio::spawn(
        VoiceBridge::new(
            workspaces.clone(),
            workspace.clone(),
            host.clone(),
            relay,
            Arc::clone(&bridge.process),
        )
        .run(),
    );

    // Dana is in the Discord channel: the worker goes there, and the host
    // joins the Tilo call as its bridge to Discord, which Grace's call views
    // show as no one's face.
    bridge
        .fake
        .voice_join(&User::new("7001", "Dana"), CHANNEL_VOICE);
    grace
        .until(
            "the call is bridged",
            |_| {},
            |grace| grace.bridge(&voice).is_some(),
        )
        .await;
    let caller = grace.bridge(&voice).unwrap();
    assert_eq!(caller.bridge.as_deref(), Some(SERVICE));
    assert_eq!(caller.id, hex::encode(host.device_public_key));
    assert_eq!(
        bridge.fake.voice_bot_channel().as_deref(),
        Some(CHANNEL_VOICE)
    );
    assert!(
        bridge
            .fake
            .eventually(tilo_test_env::hang_bound(), |fake| fake
                .voice_dave_ready()
                .then_some(()))
            .await
            .is_some(),
        "the call is end-to-end encrypted on Discord's side"
    );

    // Dana says 660 Hz: Grace hears it from the bridge.
    let host_device = host.device_public_key;
    let from_dana = |grace: &Member| -> Vec<Vec<u8>> {
        grace
            .voices
            .iter()
            .filter(|(from, _)| *from == host_device)
            .map(|(_, opus)| opus.clone())
            .collect()
    };
    let dana_says = tone::sine(660.0, 50);
    let ((), ()) = tokio::join!(
        bridge.fake.voice_say("7001", &dana_says),
        grace.until(
            "Dana is heard in Tilo",
            |_| {},
            |grace| { from_dana(grace).len() >= 40 }
        ),
    );
    let heard = from_dana(&grace);
    assert!(
        crate::voice::tone_share(&heard, 660.0) > 0.5,
        "660 Hz arrived in Tilo: {}",
        crate::voice::tone_share(&heard, 660.0)
    );

    // Grace joins and says 440 Hz: Discord hears it, and sees her on the
    // Tilo side.
    grace.in_call(Some(&voice));
    grace
        .until(
            "Discord sees Grace on the Tilo side",
            |_| {},
            |_| {
                bridge
                    .fake
                    .voice_status(CHANNEL_VOICE)
                    .is_some_and(|status| status.contains("Grace"))
            },
        )
        .await;
    let mut said = tone::sine(440.0, 50).into_iter();
    grace
        .until(
            "Grace is heard in Discord",
            |grace| {
                if let Some(frame) = said.next() {
                    grace.speak(&frame);
                }
            },
            |_| bridge.fake.voice_heard_frames().len() >= 40,
        )
        .await;
    let heard = bridge.fake.voice_heard_frames();
    assert!(
        heard.iter().all(|frame| frame.e2ee),
        "every frame came through DAVE"
    );
    assert!(
        crate::voice::tone_share_received(&heard, 440.0) > 0.5,
        "440 Hz arrived in Discord: {}",
        crate::voice::tone_share_received(&heard, 440.0)
    );
    assert!(
        crate::voice::tone_share_received(&heard, 660.0) < 0.05,
        "nothing Dana said comes back to Discord"
    );

    // Both sides empty: the bridge leaves both, and no call view shows the
    // call bridged any more.
    bridge.fake.voice_leave("7001");
    grace.in_call(None);
    grace
        .until(
            "the bridge leaves the Tilo call",
            |_| {},
            |grace| grace.bridge(&voice).is_none(),
        )
        .await;
    assert!(
        bridge
            .fake
            .eventually(tilo_test_env::hang_bound(), |fake| fake
                .voice_bot_channel()
                .is_none()
                .then_some(()))
            .await
            .is_some(),
        "the bridge left Discord"
    );
    carrier.abort();
}
