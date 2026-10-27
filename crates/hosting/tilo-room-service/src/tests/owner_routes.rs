//! A throwaway room's owner routes over real HTTP, as `Service::handle`
//! serves them: minting invitations and watching members take the room's
//! owner key, which only the `POST /rooms` answer carries. Everything a
//! guest link shows (the room's host, its uuid, the link without its
//! ticket) opens neither route, while the invitee the link was made for
//! still gets in.

use crate::config::Config;
use crate::room::Room;
use crate::room::testing::{Relay, running_room_as};
use crate::tls::HostIdentity;
use crate::{Service, serve};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tilo_relay_protocol::room_operator::{OperatorCommand, RoomEvent};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The room the tests attack and its neighbour, by service id.
const ROOM: &str = "0a1b2c3d4e5f";
const NEIGHBOUR: &str = "a0b1c2d3e4f5";
/// A service id no room has.
const NO_ROOM: &str = "ffffffffffff";

/// A throwaway-room service with two rooms on in-memory pipes, serving
/// its API on a loopback port.
struct Served {
    /// `http://127.0.0.1:<port>`, also the service's public API.
    api: String,
    service: Arc<Service>,
    room: Arc<Room>,
    relay: Relay,
    neighbour: Arc<Room>,
    _neighbour_relay: Relay,
}

async fn served() -> Served {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let config =
        Config::from_lookup(|name| (name == "TILO_ROOMS_PUBLIC_API").then(|| api.clone())).unwrap();
    assert!(config.persistent.is_none(), "a throwaway-room service");
    let (room, relay) = running_room_as(ROOM).await;
    let (neighbour, neighbour_relay) = running_room_as(NEIGHBOUR).await;
    let service = Arc::new(Service {
        config: Arc::new(config),
        identity: HostIdentity::generate().unwrap(),
        persistent: None,
        rooms: Mutex::new(BTreeMap::from([
            (room.id.clone(), Arc::clone(&room)),
            (neighbour.id.clone(), Arc::clone(&neighbour)),
        ])),
        starting: Mutex::new(0),
    });
    let serving = Arc::clone(&service);
    tokio::spawn(async move {
        while let Ok((socket, peer)) = listener.accept().await {
            tokio::spawn(serve(Arc::clone(&serving), socket, peer));
        }
    });
    Served {
        api,
        service,
        room,
        relay,
        neighbour,
        _neighbour_relay: neighbour_relay,
    }
}

/// One answer, read from its own connection as far as a check needs.
struct Answer {
    status: u16,
    /// Everything read so far, head and body.
    text: String,
    socket: TcpStream,
}

impl Answer {
    /// Reads on until the answer holds `needle`; false when the server
    /// closed the connection first.
    async fn until(&mut self, needle: &str) -> bool {
        let mut chunk = [0u8; 4096];
        while !self.text.contains(needle) {
            match tilo_test_env::within(
                format!("{needle:?} in the answer"),
                self.socket.read(&mut chunk),
            )
            .await
            {
                Ok(read) if read > 0 => {
                    self.text
                        .push_str(&String::from_utf8_lossy(chunk.get(..read).unwrap()));
                }
                _ => return false,
            }
        }
        true
    }

    /// The rest of an answer to a request sent with `Connection: close`, up
    /// to the server closing the connection. An event stream never ends, so
    /// its answer is left at its head.
    async fn finish(mut self) -> Self {
        if self.is_event_stream() {
            return self;
        }
        let mut chunk = [0u8; 4096];
        while let Ok(read @ 1..) = tilo_test_env::within(
            "the server to close the answer",
            self.socket.read(&mut chunk),
        )
        .await
        {
            self.text
                .push_str(&String::from_utf8_lossy(chunk.get(..read).unwrap()));
        }
        self
    }

    fn is_event_stream(&self) -> bool {
        self.text
            .to_ascii_lowercase()
            .contains("content-type: text/event-stream")
    }

    /// The JSON body of a finished, non-streaming answer.
    fn json(&self) -> serde_json::Value {
        let (_, body) = self.text.split_once("\r\n\r\n").unwrap();
        serde_json::from_str(body).unwrap_or_else(|_| panic!("not JSON: {body:?}"))
    }
}

/// Sends `method url` (an absolute `http://` URL, as pages hold them) with
/// a JSON `body` and reads the answer's head.
async fn send(method: &str, url: &str, body: &str) -> Answer {
    let rest = url.strip_prefix("http://").unwrap();
    let (host, path) = rest.split_at(rest.find('/').unwrap());
    let mut socket = TcpStream::connect(host).await.unwrap();
    socket
        .write_all(
            format!(
                "{method} {path} HTTP/1.1\r\nhost: {host}\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut answer = Answer {
        status: 0,
        text: String::new(),
        socket,
    };
    assert!(
        answer.until("\r\n\r\n").await,
        "no answer to {method} {url}"
    );
    answer.status = answer
        .text
        .split(' ')
        .nth(1)
        .and_then(|status| status.parse().ok())
        .unwrap();
    answer
}

async fn invite(url: &str) -> Answer {
    send("POST", url, "{}").await.finish().await
}

/// The answer a caller gets for a room that does not exist, which a
/// refused owner route must not be told apart from.
async fn no_such_room(api: &str, route: &str) -> serde_json::Value {
    let answer = match route {
        "invitations" => invite(&format!("{api}/rooms/{NO_ROOM}/invitations")).await,
        _ => {
            send("GET", &format!("{api}/rooms/{NO_ROOM}/events"), "")
                .await
                .finish()
                .await
        }
    };
    assert_eq!(answer.status, 404, "{}", answer.text);
    answer.json()
}

/// What anyone who saw a guest link, or guessed, may try against `route`.
fn url_only_attempts(served: &Served, route: &str) -> Vec<String> {
    let (api, room) = (&served.api, &served.room);
    let key = room.owner_key();
    let (id, uuid) = (&room.id, &room.uuid);
    vec![
        format!("{api}/rooms/{id}/{route}"),
        format!("{api}/rooms/{uuid}/{route}"),
        format!("{api}/rooms/{id}/{route}?key="),
        format!("{api}/rooms/{id}/{route}?key={}", "0".repeat(64)),
        format!(
            "{api}/rooms/{id}/{route}?key={}",
            served.neighbour.owner_key()
        ),
        format!("{api}/rooms/{id}/{route}?key={}", key.get(..32).unwrap()),
        format!("{api}/rooms/{id}/{route}?key={uuid}"),
        format!("{api}/rooms/{id}/{route}?ticket={key}"),
        format!("{api}/rooms/{id}/{route}?key=x&key={key}"),
        // The uuid with the right key: the owner's routes name the room
        // only by its service id.
        format!("{api}/rooms/{uuid}/{route}?key={key}"),
    ]
}

#[tokio::test]
async fn minting_an_invitation_takes_the_rooms_owner_key() {
    let served = served().await;
    let unknown = no_such_room(&served.api, "invitations").await;
    for url in url_only_attempts(&served, "invitations") {
        let answer = invite(&url).await;
        assert_eq!(answer.status, 404, "{url} minted: {}", answer.text);
        assert_eq!(answer.json(), unknown, "{url} told a live room apart");
    }
    let key = served.room.owner_key();
    let owner = invite(&format!(
        "{}/rooms/{}/invitations?key={key}",
        served.api, served.room.id
    ))
    .await;
    assert_eq!(owner.status, 201, "{}", owner.text);
    let link = owner.json()["joinUrl"].as_str().unwrap().to_owned();
    assert!(
        !link.contains(key),
        "a guest link carries the owner key: {link}"
    );
}

#[tokio::test]
async fn watching_a_rooms_members_takes_its_owner_key() {
    let served = served().await;
    let unknown = no_such_room(&served.api, "events").await;
    for url in url_only_attempts(&served, "events") {
        let answer = send("GET", &url, "").await.finish().await;
        assert_eq!(answer.status, 404, "{url} streamed: {}", answer.text);
        assert!(!answer.is_event_stream(), "{url}: {}", answer.text);
        assert_eq!(answer.json(), unknown, "{url} told a live room apart");
    }
    let mut owner = send(
        "GET",
        &format!(
            "{}/rooms/{}/events?key={}",
            served.api,
            served.room.id,
            served.room.owner_key()
        ),
        "",
    )
    .await;
    assert_eq!(owner.status, 200, "{}", owner.text);
    assert!(owner.is_event_stream(), "{}", owner.text);
    assert!(owner.until(": connected").await, "{}", owner.text);
}

#[tokio::test]
async fn callers_without_the_key_cannot_spend_the_rooms_guest_slots() {
    let served = served().await;
    for _ in 0..3 {
        for url in url_only_attempts(&served, "invitations") {
            assert_eq!(invite(&url).await.status, 404, "{url}");
        }
    }
    // Every slot is still the owner's: the room takes the page and
    // `max_guests` guests, and none was spent above.
    let owner = format!(
        "{}/rooms/{}/invitations?key={}",
        served.api,
        served.room.id,
        served.room.owner_key()
    );
    for slot in 0..=served.service.config.max_guests {
        assert_eq!(invite(&owner).await.status, 201, "slot {slot}");
    }
    let full = invite(&owner).await;
    assert_eq!(full.status, 429, "{}", full.text);
}

#[tokio::test]
async fn the_creator_uses_its_urls_as_given_and_its_invitee_still_gets_in() {
    let mut served = served().await;
    let created = served.service.created(&served.room).unwrap();
    let field = |name: &str| created[name].as_str().unwrap().to_owned();
    for name in ["joinUrl", "admit"] {
        assert!(
            !field(name).contains(served.room.owner_key()),
            "{name} carries the owner key: {}",
            field(name)
        );
    }

    // What rooms.mjs does with the answer: watchMembers(room) and
    // createInvitation(room), each on the URL exactly as given.
    let mut members = send("GET", &field("events"), "").await;
    assert_eq!(members.status, 200, "{}", members.text);
    assert!(members.is_event_stream(), "{}", members.text);
    let invited = invite(&field("invitations")).await;
    assert_eq!(invited.status, 201, "{}", invited.text);
    let ticket = invited.json()["ticket"].as_str().unwrap().to_owned();
    assert!(
        invited.json()["joinUrl"]
            .as_str()
            .unwrap()
            .contains(&format!("/w/{}/", served.room.uuid)),
        "{}",
        invited.text
    );

    // The invitee's entry page redeems the ticket at `<tickets><room>/admit`,
    // naming the room by the uuid its link carries.
    let admit = format!("{}/rooms/{}/admit", served.api, served.room.uuid);
    let body = serde_json::json!({"ticket": ticket, "pairingCode": "424242"}).to_string();
    let admitting = tokio::spawn(async move { send("POST", &admit, &body).await.finish().await });
    assert_eq!(
        served.relay.command().await,
        OperatorCommand::Admit {
            pairing_code: "424242".into()
        }
    );
    served
        .relay
        .report(&RoomEvent::MemberCertified {
            device: "cd".repeat(32),
            label: "Phone".into(),
            pairing_code: "424242".into(),
        })
        .await;
    let admitted = admitting.await.unwrap();
    assert_eq!(admitted.status, 200, "{}", admitted.text);
    assert_eq!(admitted.json()["label"], "Phone");
    assert!(
        members.until("event: member").await && members.until("\"Phone\"").await,
        "the owner did not see the invitee join: {}",
        members.text
    );
}
