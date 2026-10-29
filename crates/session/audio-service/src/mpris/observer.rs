//! One signal-driven observer of the session's MPRIS players.
//!
//! The observer subscribes to the bus's `NameOwnerChanged` for MPRIS names
//! and to every player's `PropertiesChanged` and `Seeked`, then seeds its
//! [`PlayerTable`](super::table::PlayerTable) with a single `ListNames`.
//! From then on it calls the bus only when something happens: a player
//! appears (`GetConnectionUnixProcessID` and one `GetAll` per interface), a
//! player invalidates properties (`GetAll` again), or a status, rate or
//! track change leaves the extrapolated playhead stale (one `Get` of
//! `Position`). An idle desktop makes no MPRIS calls at all.

use std::collections::HashMap;
use std::time::Instant;

use futures_util::StreamExt;
use zbus::zvariant::OwnedValue;

use super::table::{Applied, PlayerEvent};
use super::{
    MPRIS_PATH, Mpris, PLAYER_CALL_TIMEOUT, PLAYER_IFACE, ROOT_IFACE, Seed, is_player_name,
};

const BUS_NAME: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";
const PROPERTIES_IFACE: &str = "org.freedesktop.DBus.Properties";

/// Signals buffered per subscription while the observer waits on a player.
const MAX_QUEUED_SIGNALS: usize = 256;

fn apply(mpris: &Mpris, event: PlayerEvent) -> Applied {
    let applied = mpris.0.state.lock().map_or_else(
        |_| Applied::default(),
        |mut state| state.table.apply(event, Instant::now()),
    );
    if applied.changed {
        mpris.publish();
    }
    applied
}

fn admits(mpris: &Mpris, name: &str) -> bool {
    mpris
        .0
        .state
        .lock()
        .is_ok_and(|state| state.table.admits(name))
}

/// The three subscriptions, made before the bus is listed so nothing that
/// happens during seeding is missed.
struct Signals {
    owners: zbus::MessageStream,
    properties: zbus::MessageStream,
    seeked: zbus::MessageStream,
}

impl Signals {
    async fn subscribe(connection: &zbus::Connection) -> zbus::Result<Self> {
        let signal = || zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal);
        let owners = signal()
            .sender(BUS_NAME)?
            .interface(BUS_NAME)?
            .member("NameOwnerChanged")?
            .path(BUS_PATH)?
            .arg0ns("org.mpris.MediaPlayer2")?
            .build();
        let properties = signal()
            .interface(PROPERTIES_IFACE)?
            .member("PropertiesChanged")?
            .path(MPRIS_PATH)?
            .build();
        let seeked = signal()
            .interface(PLAYER_IFACE)?
            .member("Seeked")?
            .path(MPRIS_PATH)?
            .build();
        let stream =
            |rule| zbus::MessageStream::for_match_rule(rule, connection, Some(MAX_QUEUED_SIGNALS));
        Ok(Self {
            owners: stream(owners).await?,
            properties: stream(properties).await?,
            seeked: stream(seeked).await?,
        })
    }
}

/// Follow every MPRIS player on the bus into `mpris`'s table until a
/// connection closes.
///
/// Signals arrive on `listener`, a connection of their own; every call goes
/// out on `connection`. zbus stops reading a connection while one of its
/// signal queues is full, so a player flooding signals while the observer
/// waits on a call must only ever hold up the listener, never the replies
/// the observer, the transport keys and the controls wait for.
pub(super) async fn observe(
    mpris: &Mpris,
    connection: &zbus::Connection,
    listener: &zbus::Connection,
) {
    let mut signals = match Signals::subscribe(listener).await {
        Ok(signals) => signals,
        Err(error) => {
            tracing::debug!(target: "tilo::mpris", %error, "cannot follow MPRIS players");
            return;
        }
    };
    let Ok(dbus) = zbus::fdo::DBusProxy::new(connection).await else {
        return;
    };
    let Ok(names) = dbus.list_names().await else {
        return;
    };
    for name in names {
        if !is_player_name(name.as_str()) {
            continue;
        }
        if !admits(mpris, name.as_str()) {
            break;
        }
        let Ok(owner) = dbus
            .get_name_owner(zbus::names::BusName::from(name.clone()))
            .await
        else {
            continue;
        };
        if let Some(event) =
            observe_owner(connection, &dbus, name.to_string(), owner.to_string()).await
        {
            apply(mpris, event);
        }
    }
    mpris.publish();
    mpris.0.seed.send_replace(Seed::Ready);
    loop {
        tokio::select! {
            message = signals.owners.next() => {
                let Some(message) = message else { break };
                if let Ok(message) = message {
                    on_owner_changed(mpris, connection, &dbus, &message).await;
                }
            }
            message = signals.properties.next() => {
                let Some(message) = message else { break };
                if let Ok(message) = message {
                    on_properties_changed(mpris, connection, &message).await;
                }
            }
            message = signals.seeked.next() => {
                let Some(message) = message else { break };
                if let Ok(message) = message {
                    on_seeked(mpris, &message);
                }
            }
        }
    }
}

async fn on_owner_changed(
    mpris: &Mpris,
    connection: &zbus::Connection,
    dbus: &zbus::fdo::DBusProxy<'_>,
    message: &zbus::Message,
) {
    let Ok((name, _old_owner, new_owner)) =
        message.body().deserialize::<(String, String, String)>()
    else {
        return;
    };
    if !is_player_name(&name) {
        return;
    }
    if new_owner.is_empty() {
        apply(mpris, PlayerEvent::Vanished { bus_name: name });
        return;
    }
    if !admits(mpris, &name) {
        return;
    }
    // An owner that does not answer is not a player anyone can show or
    // drive; whatever the name described before is gone.
    let event = observe_owner(connection, dbus, name.clone(), new_owner)
        .await
        .unwrap_or(PlayerEvent::Vanished { bus_name: name });
    apply(mpris, event);
}

async fn on_properties_changed(
    mpris: &Mpris,
    connection: &zbus::Connection,
    message: &zbus::Message,
) {
    let Some(unique_name) = message.header().sender().map(ToString::to_string) else {
        return;
    };
    let Ok((interface, changed, invalidated)) =
        message
            .body()
            .deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
    else {
        return;
    };
    if interface != PLAYER_IFACE && interface != ROOT_IFACE {
        return;
    }
    let reread = !invalidated.is_empty();
    let applied = apply(
        mpris,
        PlayerEvent::Changed {
            unique_name: unique_name.clone(),
            changed,
            invalidated,
        },
    );
    if !applied.changed {
        return;
    }
    if reread && let Some(changed) = get_all(connection, &unique_name, &interface).await {
        apply(
            mpris,
            PlayerEvent::Changed {
                unique_name: unique_name.clone(),
                changed,
                invalidated: Vec::new(),
            },
        );
    }
    if applied.position_stale
        && let Some(position_us) = get_position(connection, &unique_name).await
    {
        apply(
            mpris,
            PlayerEvent::Seeked {
                unique_name,
                position_us,
            },
        );
    }
}

fn on_seeked(mpris: &Mpris, message: &zbus::Message) {
    let Some(unique_name) = message.header().sender().map(ToString::to_string) else {
        return;
    };
    let Ok(position_us) = message.body().deserialize::<i64>() else {
        return;
    };
    apply(
        mpris,
        PlayerEvent::Seeked {
            unique_name,
            position_us,
        },
    );
}

/// Everything about a new owner of `bus_name`: its process, for the
/// compositor to attribute to a launch, and every property of both
/// interfaces.
async fn observe_owner(
    connection: &zbus::Connection,
    dbus: &zbus::fdo::DBusProxy<'_>,
    bus_name: String,
    unique_name: String,
) -> Option<PlayerEvent> {
    let owner = zbus::names::BusName::try_from(unique_name.as_str()).ok()?;
    let pid = bounded(dbus.get_connection_unix_process_id(owner)).await;
    let mut properties = get_all(connection, &unique_name, ROOT_IFACE)
        .await
        .unwrap_or_default();
    properties.extend(get_all(connection, &unique_name, PLAYER_IFACE).await?);
    Some(PlayerEvent::Appeared {
        bus_name,
        unique_name,
        pid,
        package_id: None,
        properties,
    })
}

async fn bounded<T, E>(call: impl Future<Output = Result<T, E>> + Send) -> Option<T>
where
    T: Send,
    E: Send,
{
    tokio::time::timeout(PLAYER_CALL_TIMEOUT, call)
        .await
        .ok()?
        .ok()
}

/// One `GetAll`, addressed to the owner itself so an answer can never
/// describe a later owner of the name.
async fn get_all(
    connection: &zbus::Connection,
    unique_name: &str,
    interface: &str,
) -> Option<HashMap<String, OwnedValue>> {
    let reply = bounded(connection.call_method(
        Some(unique_name),
        MPRIS_PATH,
        Some(PROPERTIES_IFACE),
        "GetAll",
        &(interface,),
    ))
    .await?;
    reply.body().deserialize().ok()
}

async fn get_position(connection: &zbus::Connection, unique_name: &str) -> Option<i64> {
    let reply = bounded(connection.call_method(
        Some(unique_name),
        MPRIS_PATH,
        Some(PROPERTIES_IFACE),
        "Get",
        &(PLAYER_IFACE, "Position"),
    ))
    .await?;
    let position = reply.body().deserialize::<OwnedValue>().ok()?;
    i64::try_from(position).ok()
}

#[cfg(test)]
mod tests {
    use super::super::test_bus::{
        PROBE, PrivateBus, eventually, pause_and_signal, record_calls, serve_playing_probe,
    };
    use super::*;
    use crate::{Event, Player, Status};
    use std::sync::{Arc, Mutex};

    /// The client under test on `bus`, its calls going out on `calls`, and
    /// the players it last published.
    fn observe_on(
        bus: &PrivateBus,
        calls: Option<zbus::Connection>,
    ) -> (Mpris, Arc<Mutex<Vec<Player>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let published = Arc::clone(&seen);
        let mpris = Mpris::with_calls(
            tokio::runtime::Handle::current(),
            Some(bus.address.clone()),
            calls,
            move |event| {
                if let Event::Players { players } = event {
                    *published.lock().unwrap() = players;
                }
            },
        );
        (mpris, seen)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn signals_update_players_and_an_idle_player_costs_no_calls() {
        let bus = PrivateBus::start();
        let connection = bus.connect().await;
        let calls = record_calls(&bus, connection.unique_name().unwrap().as_str()).await;
        let player = serve_playing_probe(&bus).await;

        let observer_calls = connection.clone();
        let (mpris, seen) = observe_on(&bus, Some(connection));
        assert!(
            mpris.with_table(|_| ()).await.is_some(),
            "the observer lists the bus"
        );
        let players = seen.lock().unwrap().clone();
        assert_eq!(players.len(), 1, "{players:?}");
        assert_eq!(players[0].bus_name, PROBE);
        assert_eq!(players[0].identity.as_deref(), Some("Probe"));
        assert_eq!(players[0].title.as_deref(), Some("Song"));
        assert_eq!(players[0].status, Status::Playing);
        assert!(players[0].position_us >= 12_000_000);
        assert!(players[0].can_pause);

        // The monitor sees the start-up: one listing, then the player's two
        // interfaces read with one `GetAll` each.
        eventually("the start-up calls reaching the monitor", || {
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|member| *member == "GetAll")
                .count()
                == 2
        })
        .await;
        let startup = calls.lock().unwrap().clone();
        assert_eq!(
            startup
                .iter()
                .filter(|member| *member == "ListNames")
                .count(),
            1,
            "{startup:?}"
        );
        assert!(
            !startup.iter().any(|member| member == "Get"),
            "no property is read one at a time: {startup:?}"
        );

        // A signal, not a poll, moves the players.
        pause_and_signal(&player).await;
        eventually("the paused status being published", || {
            seen.lock()
                .unwrap()
                .first()
                .is_some_and(|player| player.status == Status::Paused)
        })
        .await;

        // An idle player costs nothing: the only call after start-up is the
        // one fresh playhead the status change made stale.
        eventually("the playhead re-read", || {
            calls.lock().unwrap().len() > startup.len()
        })
        .await;
        assert_eq!(
            calls.lock().unwrap().get(startup.len()..),
            Some(&["Get".to_string()][..]),
            "after start-up the observer only re-reads the playhead"
        );

        player.release_name(PROBE).await.unwrap();
        eventually("the player leaving", || seen.lock().unwrap().is_empty()).await;
        // A call of our own on the observer's connection: once the monitor
        // sees it, it has seen every call the observer made before it.
        observer_calls
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "GetId",
                &(),
            )
            .await
            .unwrap();
        eventually("the marker call reaching the monitor", || {
            calls.lock().unwrap().iter().any(|member| member == "GetId")
        })
        .await;
        assert_eq!(
            calls.lock().unwrap().get(startup.len()..),
            Some(&["Get".to_string(), "GetId".to_string()][..]),
            "an idle player and a player leaving cost no call"
        );
    }
}
