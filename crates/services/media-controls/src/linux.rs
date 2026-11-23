//! MPRIS on the Linux session bus: one `org.mpris.MediaPlayer2` player per active Playback.
//!
//! Each player owns a connection of its own, so its well-known name and its properties answer
//! independently and dropping the connection takes the player off every desktop at once.
//! Every method the interface exposes becomes the Playback's own action and travels back on one
//! channel to the host; nothing here changes a player itself.

// zbus's `#[interface]` macro generates dispatch code that re-binds underscore parameters by
// their literal names, outside the annotated impl where an allow cannot reach it.
#![allow(
    clippy::used_underscore_binding,
    clippy::needless_pass_by_ref_mut,
    reason = "the generated D-Bus dispatch re-binds underscore parameters and keeps &mut setters"
)]

use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;

use tokio::runtime::Handle;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::board::{Board, Change, ChangeKind, Command, PlayerView};

/// The one object path every MPRIS player serves.
const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";

/// Every player the service is showing, by the Playback each stands for.
pub struct Server {
    runtime: Handle,
    /// Where to serve: a test's own bus, else the session's.
    address: Option<String>,
    /// One update channel per active player; dropping it takes the player down.
    players: HashMap<String, UnboundedSender<PlayerView>>,
    /// Every player's controls come back here, to be run as the Playback's actions.
    commands: UnboundedSender<(String, Command)>,
}

impl Server {
    #[must_use]
    pub fn start(address: Option<String>, runtime: &Handle, on_command: crate::OnCommand) -> Self {
        let (commands, mut incoming) = unbounded_channel::<(String, Command)>();
        runtime.spawn(async move {
            while let Some((id, command)) = incoming.recv().await {
                on_command(&id, command);
            }
        });
        Self {
            runtime: runtime.clone(),
            address,
            players: HashMap::new(),
            commands,
        }
    }

    /// Show the players in `board`, start the ones that began, update the ones that moved, and
    /// take down the ones that stopped.
    pub fn apply(&mut self, board: &Board, changes: &[Change]) {
        let views = board.players();
        for change in changes {
            match change.kind {
                ChangeKind::Stopped => {
                    // Dropping the update channel ends the player's task, which drops its
                    // connection and releases its name.
                    self.players.remove(&change.id);
                }
                ChangeKind::Started => {
                    let Some(view) = views.iter().find(|view| view.id == change.id) else {
                        continue;
                    };
                    let (updates, receiver) = unbounded_channel::<PlayerView>();
                    self.players.insert(change.id.clone(), updates);
                    let address = self.address.clone();
                    let commands = self.commands.clone();
                    let initial = view.clone();
                    self.runtime.spawn(async move {
                        if let Err(error) = serve_player(address, initial, receiver, commands).await
                        {
                            tracing::warn!(
                                %error,
                                "media controls: a Playback's player did not start"
                            );
                        }
                    });
                }
                ChangeKind::Updated => {
                    let (Some(view), Some(updates)) = (
                        views.iter().find(|view| view.id == change.id),
                        self.players.get(&change.id),
                    ) else {
                        continue;
                    };
                    let _ = updates.send(view.clone());
                }
            }
        }
    }
}

/// Serve one Playback until its update channel closes or its connection fails.
async fn serve_player(
    address: Option<String>,
    initial: PlayerView,
    mut updates: UnboundedReceiver<PlayerView>,
    commands: UnboundedSender<(String, Command)>,
) -> zbus::Result<()> {
    let name = bus_name(&initial.id);
    let identity = if initial.identity.is_empty() {
        "Tilo".to_owned()
    } else {
        initial.identity.clone()
    };
    let player = PlayerIface {
        view: initial.clone(),
        id: initial.id.clone(),
        commands: commands.clone(),
    };
    let root = RootIface {
        identity,
        id: initial.id.clone(),
    };
    // Both interfaces are served before the name is claimed: a desktop reads a player's
    // properties the moment its name appears, and a player that cannot answer then is not shown.
    let builder = match &address {
        Some(address) => zbus::connection::Builder::address(address.as_str())?,
        None => zbus::connection::Builder::session()?,
    };
    let connection = builder
        .serve_at(OBJECT_PATH, player)?
        .serve_at(OBJECT_PATH, root)?
        .name(name)?
        .build()
        .await?;
    let mut current = initial;
    while let Some(next) = updates.recv().await {
        apply_update(&connection, &mut current, &next).await?;
    }
    Ok(())
}

/// Emit `PropertiesChanged` for whichever served properties moved, then keep `next` as the
/// served view.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the guard stays held across every property-changed signal, which reports the state \
              it has just written"
)]
async fn apply_update(
    connection: &zbus::Connection,
    current: &mut PlayerView,
    next: &PlayerView,
) -> zbus::Result<()> {
    let iref = connection
        .object_server()
        .interface::<_, PlayerIface>(OBJECT_PATH)
        .await?;
    let ctxt = iref.signal_emitter().clone();
    let old = current.clone();
    {
        let mut guard = iref.get_mut().await;
        guard.view = next.clone();
        if old.status != next.status {
            guard.playback_status_changed(&ctxt).await?;
        }
        if old.metadata_differs(next) {
            guard.metadata_changed(&ctxt).await?;
        }
        if (old.volume - next.volume).abs() > f64::EPSILON {
            guard.volume_changed(&ctxt).await?;
        }
        if old.can_next != next.can_next {
            guard.can_go_next_changed(&ctxt).await?;
        }
        if old.can_prev != next.can_prev {
            guard.can_go_previous_changed(&ctxt).await?;
        }
        if old.can_play != next.can_play {
            guard.can_play_changed(&ctxt).await?;
        }
        if old.can_pause != next.can_pause {
            guard.can_pause_changed(&ctxt).await?;
        }
        if old.can_seek != next.can_seek {
            guard.can_seek_changed(&ctxt).await?;
        }
    }
    // A playhead that moved a long way without the track changing is a seek, which MPRIS
    // reports as its own signal rather than a property change.
    if old.id == next.id && next.position_us.saturating_sub(old.position_us).abs() > 2_000_000 {
        let _ = PlayerIface::seeked(&ctxt, next.position_us).await;
    }
    *current = next.clone();
    Ok(())
}

/// The well-known name one Playback's player owns. A bus name element takes only letters,
/// digits, `_`, `-` and `.`, so anything else a Playback's id holds becomes `_`.
fn bus_name(id: &str) -> String {
    let mut clean = String::with_capacity(id.len());
    for character in id.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.') {
            clean.push(character);
        } else {
            clean.push('_');
        }
    }
    if clean.is_empty() {
        clean.push_str("player");
    }
    format!("org.mpris.MediaPlayer2.tilo.{clean}")
}

/// The `mpris:trackid` object path for a Playback's id: an object path element takes only
/// letters, digits and `_`.
fn track_path(id: &str) -> String {
    let mut clean = String::with_capacity(id.len());
    for character in id.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            clean.push(character);
        } else {
            clean.push('_');
        }
    }
    format!("/org/mpris/MediaPlayer2/tilo/{clean}")
}

/// The MPRIS `Metadata` map (`a{sv}`) for a view.
fn build_metadata(view: &PlayerView) -> HashMap<String, OwnedValue> {
    let mut metadata: HashMap<String, OwnedValue> = HashMap::new();
    if !view.id.is_empty()
        && let Ok(path) = ObjectPath::try_from(track_path(&view.id))
        && let Ok(value) = OwnedValue::try_from(Value::from(path))
    {
        metadata.insert("mpris:trackid".into(), value);
    }
    if view.length_us > 0
        && let Ok(value) = OwnedValue::try_from(Value::from(view.length_us))
    {
        metadata.insert("mpris:length".into(), value);
    }
    if !view.art_url.is_empty()
        && let Ok(value) = OwnedValue::try_from(Value::from(view.art_url.clone()))
    {
        metadata.insert("mpris:artUrl".into(), value);
    }
    if !view.title.is_empty()
        && let Ok(value) = OwnedValue::try_from(Value::from(view.title.clone()))
    {
        metadata.insert("xesam:title".into(), value);
    }
    if !view.artist.is_empty() {
        let artists = vec![view.artist.clone()];
        if let Ok(value) = OwnedValue::try_from(Value::from(artists)) {
            metadata.insert("xesam:artist".into(), value);
        }
    }
    if !view.album.is_empty()
        && let Ok(value) = OwnedValue::try_from(Value::from(view.album.clone()))
    {
        metadata.insert("xesam:album".into(), value);
    }
    metadata
}

/// `org.mpris.MediaPlayer2.Player` — the transport and metadata of one Playback.
pub struct PlayerIface {
    view: PlayerView,
    id: String,
    commands: UnboundedSender<(String, Command)>,
}

impl PlayerIface {
    fn send(&self, command: Command) {
        let _ = self.commands.send((self.id.clone(), command));
    }
}

// The interface dispatches by exact method signature, so a fixed property keeps its receiver and
// a setter the spec requires keeps `&mut` even when it changes nothing here.
#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
#[allow(
    clippy::unused_self,
    clippy::needless_pass_by_ref_mut,
    clippy::used_underscore_binding,
    reason = "zbus's #[interface] macro dispatches by exact method signature"
)]
impl PlayerIface {
    fn play(&self) {
        self.send(Command::Play);
    }
    fn pause(&self) {
        self.send(Command::Pause);
    }
    fn play_pause(&self) {
        self.send(Command::Toggle);
    }
    fn stop(&self) {
        self.send(Command::Stop);
    }
    fn next(&self) {
        self.send(Command::Next);
    }
    fn previous(&self) {
        self.send(Command::Previous);
    }
    fn seek(&self, offset: i64) {
        self.send(Command::SeekBy(offset));
    }
    fn set_position(&self, _track_id: ObjectPath<'_>, position: i64) {
        self.send(Command::SeekTo(position));
    }
    fn open_uri(&self, _uri: String) {}

    /// Emitted after a seek not caused by normal playback.
    #[zbus(signal)]
    async fn seeked(ctxt: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.view.status.as_mpris().into()
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        build_metadata(&self.view)
    }
    #[zbus(property)]
    fn position(&self) -> i64 {
        self.view.position_us
    }
    #[zbus(property)]
    fn volume(&self) -> f64 {
        self.view.volume
    }
    #[zbus(property)]
    fn set_volume(&mut self, volume: f64) {
        let level = volume.clamp(0.0, 1.0);
        self.view.volume = level;
        self.send(Command::SetVolume(level));
    }
    #[zbus(property)]
    fn rate(&self) -> f64 {
        self.view.rate
    }
    #[zbus(property)]
    fn set_rate(&mut self, _rate: f64) {}
    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        0.25
    }
    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        4.0
    }
    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.view.can_next
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.view.can_prev
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.view.can_play
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.view.can_pause
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.view.can_seek
    }
}

/// `org.mpris.MediaPlayer2` — the application object the player stands under.
pub struct RootIface {
    identity: String,
    id: String,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
#[allow(
    clippy::unused_self,
    reason = "the interface dispatches by exact method signature"
)]
impl RootIface {
    // Raising and quitting a Playback are not its actions: the app's window does those.
    fn raise(&self) {}
    fn quit(&self) {}
    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn identity(&self) -> String {
        self.identity.clone()
    }
    #[zbus(property)]
    fn desktop_entry(&self) -> String {
        self.id.clone()
    }
    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        Vec::new()
    }
    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::board::Status;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_published_playback_answers_the_desktop_and_its_controls_come_back() {
        let bus = tilo_test_env::PrivateBus::start();
        let (sent, received) = std::sync::mpsc::channel::<(String, Command)>();
        let on: crate::OnCommand = Arc::new(move |id: &str, command| {
            let _ = sent.send((id.to_owned(), command));
        });
        let mut controls = crate::MediaControls::start_on(
            Some(bus.address().to_owned()),
            &tokio::runtime::Handle::current(),
            on,
        );
        let view = PlayerView {
            id: "so.tilo.video:1".into(),
            identity: "Video".into(),
            title: "A clip".into(),
            status: Status::Playing,
            position_us: 5_000_000,
            length_us: 60_000_000,
            volume: 0.5,
            can_next: true,
            can_prev: true,
            can_seek: true,
            ..PlayerView::default()
        };
        controls.update(vec![view]);

        let connection = zbus::connection::Builder::address(bus.address())
            .unwrap()
            .build()
            .await
            .unwrap();
        let proxy = zbus::Proxy::new(
            &connection,
            bus_name("so.tilo.video:1"),
            OBJECT_PATH,
            "org.mpris.MediaPlayer2.Player",
        )
        .await
        .unwrap();
        // The player claims its name in its own task; wait for it to answer.
        let began = Instant::now();
        loop {
            match proxy.get_property::<String>("PlaybackStatus").await {
                Ok(status) => {
                    assert_eq!(status, "Playing");
                    break;
                }
                Err(error) => {
                    assert!(
                        began.elapsed() < Duration::from_secs(10),
                        "the player never answered: {error}"
                    );
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
        }
        let volume = proxy.get_property::<f64>("Volume").await.unwrap();
        assert!((volume - 0.5).abs() < f64::EPSILON);
        assert!(proxy.get_property::<bool>("CanGoNext").await.unwrap());

        proxy.call_method("Next", &()).await.unwrap();
        let (id, command) = received.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(id, "so.tilo.video:1");
        assert_eq!(command, Command::Next);
    }
}
