//! A private session bus and a fake player, for the client's tests: the
//! client under test never sees the real bus of whoever runs them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use zbus::message::Type as MessageType;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;

pub(super) use tilo_test_env::PrivateBus;

use super::MPRIS_PATH;

pub(super) const PROBE: &str = "org.mpris.MediaPlayer2.probe";

/// A connection of the test's own to `bus`, which never sees the caller's
/// real one.
pub(super) async fn connect(bus: &PrivateBus) -> zbus::Connection {
    zbus::connection::Builder::address(bus.address())
        .unwrap()
        .build()
        .await
        .unwrap()
}

pub(super) struct FakeRoot {
    pub(super) identity: String,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl FakeRoot {
    #[zbus(property)]
    fn identity(&self) -> String {
        self.identity.clone()
    }
}

/// A player that reports what its fields say, and signals only when the
/// test makes it.
#[derive(Default)]
pub(super) struct FakePlayer {
    pub(super) status: String,
    pub(super) title: String,
    pub(super) url: String,
    pub(super) position_us: i64,
    pub(super) controllable: bool,
    /// Every method called on it, in order.
    pub(super) called: Vec<String>,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl FakePlayer {
    async fn play_pause(&mut self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        let status = if self.status == "Playing" {
            "Paused"
        } else {
            "Playing"
        };
        self.transport("PlayPause", status, &emitter).await;
    }

    async fn play(&mut self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        self.transport("Play", "Playing", &emitter).await;
    }

    async fn pause(&mut self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        self.transport("Pause", "Paused", &emitter).await;
    }

    fn next(&mut self) {
        self.called.push("Next".into());
    }

    fn seek(&mut self, offset: i64) {
        self.called.push("Seek".into());
        self.position_us = self.position_us.saturating_add(offset);
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.controllable
    }

    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.status.clone()
    }

    #[zbus(property)]
    fn position(&self) -> i64 {
        self.position_us
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.controllable
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.controllable
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let text = |value: &str| {
            OwnedValue::try_from(zbus::zvariant::Value::from(value.to_string())).unwrap()
        };
        HashMap::from([
            ("xesam:title".to_string(), text(&self.title)),
            ("xesam:url".to_string(), text(&self.url)),
        ])
    }
}

impl FakePlayer {
    /// Take a transport call and announce the new status the way MPRIS
    /// players do.
    async fn transport(&mut self, method: &str, status: &str, emitter: &SignalEmitter<'_>) {
        self.called.push(method.into());
        self.status = status.into();
        let _ = self.playback_status_changed(emitter).await;
    }
}

/// Every method call `sender` makes on the bus, in order, as
/// `dbus-monitor` would see them.
pub(super) async fn record_calls(bus: &PrivateBus, sender: &str) -> Arc<Mutex<Vec<String>>> {
    let monitor = connect(bus).await;
    let mut messages = zbus::MessageStream::from(&monitor);
    let rule = zbus::MatchRule::builder()
        .msg_type(MessageType::MethodCall)
        .sender(sender.to_owned())
        .unwrap()
        .build();
    zbus::fdo::MonitoringProxy::new(&monitor)
        .await
        .unwrap()
        .become_monitor(&[rule], 0)
        .await
        .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&calls);
    drop(tokio::spawn(async move {
        let _monitor = monitor;
        while let Some(Ok(message)) = messages.next().await {
            if message.message_type() == MessageType::MethodCall
                && let Some(member) = message.header().member()
            {
                recorded.lock().unwrap().push(member.to_string());
            }
        }
    }));
    calls
}

pub(super) async fn eventually(what: &str, mut condition: impl FnMut() -> bool + Send) {
    let deadline = Instant::now()
        .checked_add(tilo_test_env::hang_bound())
        .unwrap();
    while !condition() {
        assert!(Instant::now() < deadline, "{what} never happened");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The probe player on `bus`, playing "Song" at 12 s.
pub(super) async fn serve_playing_probe(bus: &PrivateBus) -> zbus::Connection {
    zbus::connection::Builder::address(bus.address())
        .unwrap()
        .name(PROBE)
        .unwrap()
        .serve_at(
            MPRIS_PATH,
            FakeRoot {
                identity: "Probe".into(),
            },
        )
        .unwrap()
        .serve_at(
            MPRIS_PATH,
            FakePlayer {
                status: "Playing".into(),
                title: "Song".into(),
                position_us: 12_000_000,
                controllable: true,
                ..FakePlayer::default()
            },
        )
        .unwrap()
        .build()
        .await
        .unwrap()
}

/// Pause the probe player and announce it the way MPRIS players do: one
/// `PropertiesChanged`, no `Position`.
pub(super) async fn pause_and_signal(player: &zbus::Connection) {
    let player_iface = player
        .object_server()
        .interface::<_, FakePlayer>(MPRIS_PATH)
        .await
        .unwrap();
    player_iface.get_mut().await.status = "Paused".into();
    player_iface
        .get()
        .await
        .playback_status_changed(player_iface.signal_emitter())
        .await
        .unwrap();
}
