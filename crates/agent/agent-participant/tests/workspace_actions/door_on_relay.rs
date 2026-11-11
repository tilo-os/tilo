//! The MCP front door on a relay as tilo.sh runs them: the relay's own
//! person is its operator, its app workers answer every workspace with no
//! tab of anyone's open, the site's home workspace lets people in through
//! its door, and a person approves keys for their backend at the door.
//!
//! The door's tests drive it in process; [`serve`] serves it on loopback
//! for a client outside this process (an SDK's end-to-end run), with the
//! key a person approved there, and plays another person who follows the
//! invitation links the run hands them ([`accept_invitation`]).

use super::{
    Arc, Device, DeviceCapabilities, Hosted, ModelChoice, Options, SigningKey, common, enrol,
    harness, support,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use tilo_front_door::{Config, Door, store::Store};
use tilo_relay_protocol::directory::Listing;
use tilo_relay_protocol::workspaces::WorkspaceRole;
use tilo_user_identity::{DeviceCert, SignInProof};

/// A reply's status and JSON body.
pub async fn json_of(reply: hyper::Response<http_body_util::Full<bytes::Bytes>>) -> (u16, Value) {
    use http_body_util::BodyExt as _;
    let status = reply.status().as_u16();
    let body = reply.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

pub fn headers(pairs: &[(&'static str, &str)]) -> hyper::HeaderMap {
    let mut map = hyper::HeaderMap::new();
    for (name, value) in pairs {
        map.insert(*name, value.parse().unwrap());
    }
    map
}

/// The query of `url`, its plain values.
fn query_of(url: &str) -> HashMap<String, String> {
    url.split_once('?')
        .map(|(_, query)| query)
        .unwrap_or_default()
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

/// The relay as its operator runs it, the site whose home workspace lets
/// anyone in through its door, and the front door before both.
pub struct OnRelay {
    pub door: Arc<Door>,
    /// The door's public origin, as agents reach it.
    pub issuer: String,
    /// The site's home workspace.
    pub lobby: String,
    /// The relay's origin, where a page joins its workspaces.
    pub relay: String,
    /// The site people's links open under, which the door and the relay
    /// both announce.
    pub web_entry: String,
    /// The relay's own origin, which the links to its records name.
    pub host_origin: String,
    /// The relay's own device, which a page joining its workspaces trusts.
    pub host: Device,
    pub hosted: Hosted,
    _site: test_http::Responder,
    /// Where the relay keeps its state.
    pub state: tempfile::TempDir,
}

impl OnRelay {
    /// The relay as a person's devices and pages reach it over iroh.
    pub fn relay_host(&self) -> tilo_relay_client::Host {
        tilo_relay_client::Host::at(self.hosted.relay.node_addr())
    }
}

/// The door at `issuer` before a relay serving its app workers.
pub async fn door_on_relay(issuer: &str) -> OnRelay {
    door_on_relay_limiting(issuer, None).await
}

/// [`door_on_relay`], its relay keeping each person to `member_limits`, as a
/// relay whose workspaces admit the public does.
pub async fn door_on_relay_limiting(
    issuer: &str,
    member_limits: Option<tilo_relay::member_limits::MemberLimitsConfig>,
) -> OnRelay {
    // The site the door and the relay hand people's links out under: its
    // `entry.json` names the home workspace's door once the relay made it.
    let entry = Arc::new(std::sync::OnceLock::<String>::new());
    let answered = Arc::clone(&entry);
    // Its shelf keeps the sealed invitations links name (`/i/<handle>`).
    let shelf = std::sync::Mutex::new(HashMap::<String, Value>::new());
    let site = test_http::Responder::handle(move |request| {
        use tilo_relay_protocol::sealed_invitations::{Fetched, Stored, handle_of};
        let reply = |body: String| {
            test_http::Reply::status(200)
                .header("content-type", "application/json")
                .body(body)
        };
        if request.target == "/entry.json" {
            return Some(reply(answered.get()?.clone()));
        }
        let handle = handle_of(&request.target)?.to_owned();
        let mut shelf = shelf.lock().unwrap();
        if request.method == "POST" {
            let stored: Stored = serde_json::from_slice(&request.body).ok()?;
            let fetched = Fetched {
                sealed: stored.sealed,
                host: stored.host,
            };
            shelf.insert(handle, serde_json::to_value(fetched).unwrap());
            return Some(reply("{}".into()));
        }
        shelf.get(&handle).map(|fetched| reply(fetched.to_string()))
    });
    // The relay, as its operator runs it.
    let (operator, _) = tilo_user_identity::UserIdentity::generate("Relay operator", 1);
    let host = enrol(
        &operator,
        "relay",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let state = support::scratch_dir();
    let hosted = harness::hosted::start(
        &operator,
        &host,
        state.path(),
        Options {
            plain: true,
            member_limits,
            web_entry: Some(format!("http://127.0.0.1:{}/", site.port())),
            ..Options::offering(common::catalogue, ModelChoice::Scripted)
        },
    )
    .await;
    let relay = hosted.web.plain_origin.clone().unwrap();
    let workspaces = hosted.relay.workspaces().unwrap();
    workspaces
        .offer_own_workspaces(tilo_relay::workspaces::OwnWorkspaceOffer::free(
            "relay.tilo.test",
        ))
        .unwrap();
    // The site's home workspace, whose door lets anyone in: the people it
    // lets in make workspaces of their own here.
    let lobby = workspaces.create("Lounge").await.unwrap().workspace;
    assert!(workspaces.admit_own_through(&lobby));
    workspaces
        .publish(
            &lobby,
            workspaces.serving_certificate(),
            Listing {
                description: "Everyone's".into(),
                default_role: WorkspaceRole::Editor,
            },
        )
        .await
        .unwrap();
    // The site names that door in its `entry.json`.
    let _ = entry.set(
        json!({"home": {"host": {"origin": relay},
                        "admission": format!("{relay}/peer/v1/directory/{lobby}/")}})
        .to_string(),
    );
    let web_entry = format!("http://127.0.0.1:{}/", site.port());
    let door = Door::new(
        &Config {
            issuer: issuer.into(),
            web_entry: web_entry.trim_end_matches('/').to_owned(),
            relay: relay.clone(),
        },
        Store::in_memory(),
    )
    .unwrap();

    OnRelay {
        door,
        issuer: issuer.to_owned(),
        lobby,
        relay,
        web_entry,
        host_origin: hosted.web.origin.clone(),
        host,
        hosted,
        _site: site,
        state,
    }
}

/// A key called `name`, asked for as a terminal does and approved by the
/// person's Tilo on the web, which this plays: it signs the door's
/// challenge with a device of `root`'s and certifies the grant's key as the
/// server key the door asked for, lasting until revoked.
pub async fn approved_key(
    on_relay: &OnRelay,
    root: &SigningKey,
    device: &Device,
    name: &str,
) -> String {
    // The person approves on the site, where their page came in through its
    // door: the door answers the page with an invitation, and the page enters
    // on its session with it. The person is a member of the site's home
    // workspace from then on, so every key of theirs reaches the relay as
    // them.
    let (issued, token) = on_relay
        .hosted
        .relay
        .workspaces()
        .unwrap()
        .admit_listed(
            &on_relay.lobby,
            &tilo_relay_protocol::directory::DoorJoin {
                user_public_key: hex::encode(root.verifying_key().to_bytes()),
                display_name: device.cert.device_label.clone(),
            },
        )
        .await
        .unwrap();
    enter_page(
        on_relay,
        device,
        &issued.signed_invitation,
        Some(token.as_bytes()),
    )
    .await;
    let door = &on_relay.door;
    let (status, asked) = json_of(
        door.dispatch(
            &hyper::Method::POST,
            tilo_front_door::keys::NEW_PATH,
            "",
            &hyper::HeaderMap::new(),
            json!({"name": name}).to_string().as_bytes(),
        )
        .await,
    )
    .await;
    assert_eq!(status, 201, "{asked}");
    let approve = asked["approve_url"].as_str().unwrap();
    let to_sign_in = door
        .dispatch(
            &hyper::Method::GET,
            approve.strip_prefix(on_relay.issuer.as_str()).unwrap(),
            "",
            &hyper::HeaderMap::new(),
            b"",
        )
        .await;
    let location = to_sign_in.headers()[hyper::header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    let query = query_of(&location);
    let challenge: [u8; 32] = hex::decode(&query["challenge"])
        .unwrap()
        .try_into()
        .unwrap();
    let grant_key: [u8; 32] = hex::decode(&query["grant_key"])
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(query["grant_scope"], "all");
    assert_eq!(query["grant_kind"], "server_key");
    assert_eq!(query["grant_expires"], "never");
    let now_ns = tilo_clock::unix_now_ns();
    let mut grant = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: grant_key,
        device_label: format!("{name} via {}", on_relay.issuer),
        capabilities: DeviceCapabilities::server_key_grant(),
        issued_at_ns: now_ns.saturating_sub(1_000_000_000),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut grant, root).unwrap();
    let proof = SignInProof::sign(
        device.cert.clone(),
        &device.key,
        &on_relay.issuer,
        challenge,
    )
    .unwrap();
    let form = format!(
        "pending={}&proof={}&grant={}",
        query["pending"],
        tilo_front_door::sign_in::encode_proof(&proof).unwrap(),
        tilo_front_door::sign_in::encode_cert(&grant).unwrap()
    );
    let signed_in = door
        .dispatch(
            &hyper::Method::POST,
            "/sign-in",
            "",
            &headers(&[("content-type", "application/x-www-form-urlencoded")]),
            form.as_bytes(),
        )
        .await;
    assert!(
        signed_in.status().is_success() || signed_in.status().is_redirection(),
        "{}",
        signed_in.status()
    );
    let (status, polled) = json_of(
        door.dispatch(
            &hyper::Method::POST,
            tilo_front_door::keys::POLL_PATH,
            "",
            &hyper::HeaderMap::new(),
            json!({"poll": asked["poll"]}).to_string().as_bytes(),
        )
        .await,
    )
    .await;
    assert_eq!(status, 200, "{polled}");
    polled["key"].as_str().unwrap().to_owned()
}

/// A person following `link` on a page of theirs, as the site's join flow
/// takes them (`admission.mjs`): the invitation opened from the site's shelf
/// (or read from the link itself), what it offers inspected, then joined
/// with a new device `root` certifies, named `name`. Answers that device and
/// what the host admitted it to: `{workspace, name, role}`.
pub async fn accept_invitation(
    on_relay: &OnRelay,
    link: &str,
    root: &SigningKey,
    name: &str,
) -> (Device, Value) {
    use base64::Engine as _;
    let text = if let Some((site, sealed)) = link.rsplit_once("/i/")
        && let Some((handle, key)) = sealed.split_once('#')
    {
        tilo_workspace_directory::sealed_invitation(site, &format!("{handle}{key}"))
            .await
            .expect("the site's shelf opens the link")
            .invitation
    } else {
        let (_, text) = link
            .split_once("invite=")
            .expect("a link names its invitation");
        text.split('&').next().unwrap_or_default().to_owned()
    };
    let invitation = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .expect("an invitation's text");
    let page = enrol(root, name, DeviceCapabilities::member());
    let joined = enter_page(on_relay, &page, &invitation, None).await;
    (page, joined)
}

/// `page`, a device `root` holds, enters the workspace `invitation` (its
/// signed bytes) names on its own session, under the pre-admission `token`
/// where the door issued one: what the workspace admitted it to,
/// `{workspace, name, role}`.
pub async fn enter_page(
    on_relay: &OnRelay,
    page: &Device,
    invitation: &[u8],
    token: Option<&[u8]>,
) -> Value {
    use tilo_relay_protocol::workspaces::JoinReply;
    use tilo_session_host::device::{Entering, Held, entered, invitation_entry};
    let client = tilo_net::Client::builder().build().unwrap();
    let host = tilo_relay_client::https::Origin::new(&client, &on_relay.relay);
    let offer = tilo_relay_client::https::invitation(&host, invitation)
        .await
        .unwrap();
    assert!(matches!(offer, JoinReply::Invitation { .. }), "{offer:?}");
    // The page's session carries the invitation in its first message.
    let info = tilo_relay_client::https::host_info(&host).await.unwrap();
    let label = page.cert.device_label.clone();
    let held = Held {
        cert: page.cert.clone(),
        host_cert: Some(info.host_cert.clone()),
    };
    let (_, entering) = invitation_entry(
        &info.host_cert,
        invitation,
        &page.key,
        Entering {
            held: Some(&held),
            root: None,
            label: &label,
            name: &label,
            token,
        },
    )
    .await
    .unwrap();
    let address = info.address.expect("the relay takes device sessions");
    let session = tilo_session_host::PeerHandle::start(
        tilo_session_host::PeerConfig {
            host: tilo_session_host::Host::published(&address),
            cert: page.cert.clone(),
            host_cert: info.host_cert,
            revocations: tilo_user_identity::RevocationList::new(),
            entering: Some(entering),
        },
        Arc::new(page.key.clone()),
        Arc::new(tilo_session_host::MemoryPeerStore::default()),
    );
    let admitted = entered(&session)
        .await
        .expect("the invitation admits the page");
    session.close();
    let role = format!("{:?}", admitted.role).to_lowercase();
    json!({"workspace": admitted.workspace, "name": admitted.name, "role": role})
}

/// Answer each `POST {link}` to `listener` as the person `root` would:
/// they follow the link on a page of theirs ([`accept_invitation`]), then
/// approve a key of their own at the door ([`approved_key`]). Each answer
/// is `{key, person, joined}`.
#[cfg(not(test))]
async fn invitee(on_relay: &OnRelay, listener: tokio::net::TcpListener, root: SigningKey) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    while let Ok((mut stream, _)) = listener.accept().await {
        let mut request = Vec::new();
        let mut piece = [0; 4096];
        let body = loop {
            let Ok(read) = stream.read(&mut piece).await else {
                break None;
            };
            if read == 0 {
                break None;
            }
            request.extend_from_slice(&piece[..read]);
            let text = String::from_utf8_lossy(&request);
            let Some((head, body)) = text.split_once("\r\n\r\n") else {
                continue;
            };
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or_default();
            if body.len() >= length {
                break Some(body.to_owned());
            }
        };
        let link = body
            .and_then(|body| serde_json::from_str::<Value>(&body).ok())
            .and_then(|asked| asked["link"].as_str().map(str::to_owned));
        let answer = match link {
            Some(link) => {
                let (page, joined) = accept_invitation(on_relay, &link, &root, "Rosa Ortiz").await;
                let key = approved_key(on_relay, &root, &page, "Rosa's review").await;
                json!({
                    "key": key,
                    "person": hex::encode(root.verifying_key().to_bytes()),
                    "joined": joined,
                })
            }
            None => json!({"error": "post a JSON object naming the link"}),
        }
        .to_string();
        let reply = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
            answer.len()
        );
        let _ = stream.write_all(reply.as_bytes()).await;
    }
}

/// Serve the door on a relay with its app workers on loopback, for a client
/// outside this process. A person approves a key reaching all their
/// workspaces there, for their backend; another person, their reviewer,
/// follows the invitation links posted to the invitee endpoint. It prints
/// one JSON line naming the door, the key and that endpoint, then runs
/// `command` with them in its environment (`TILO_DOOR`, `TILO_API_KEY`,
/// `TILO_INVITEE`) and answers its exit status, or with no command serves
/// until its standard input closes.
#[cfg(not(test))]
pub async fn serve(command: &[std::ffi::OsString]) -> std::process::ExitCode {
    use std::io::Write as _;
    use tokio::io::AsyncReadExt as _;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let on_relay = door_on_relay(&issuer).await;
    let (person, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&person, "Ada's browser", DeviceCapabilities::member());
    let key = approved_key(&on_relay, &person, &page, "Backend").await;
    tokio::spawn(tilo_front_door::serve(Arc::clone(&on_relay.door), listener));
    let accepting = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let invitee_url = format!("http://{}/accept", accepting.local_addr().unwrap());
    let (reviewer, _) = tilo_user_identity::UserIdentity::generate("Rosa Ortiz", 1);
    let ready = json!({
        "door": issuer,
        "key": key,
        "invitee": invitee_url,
        "person": hex::encode(person.verifying_key().to_bytes()),
    });
    let mut out = std::io::stdout().lock();
    writeln!(out, "{ready}").unwrap();
    out.flush().unwrap();
    drop(out);
    // The reviewer answers on this task, beside whatever runs.
    let reviewing = invitee(&on_relay, accepting, reviewer);
    let Some((program, arguments)) = command.split_first() else {
        let mut rest = Vec::new();
        let mut input = tokio::io::stdin();
        tokio::select! {
            _ = input.read_to_end(&mut rest) => {}
            () = reviewing => {}
        }
        return std::process::ExitCode::SUCCESS;
    };
    let mut run = tokio::process::Command::new(program);
    run.args(arguments)
        .env("TILO_DOOR", &issuer)
        .env("TILO_API_KEY", &key)
        .env("TILO_INVITEE", &invitee_url);
    let status = tokio::select! {
        status = async {
            use tilo_test_env::SpawnUnderEnvLockAsync as _;
            let mut child = run.spawn_locked()?;
            child.wait().await
        } => status,
        () = reviewing => Err(std::io::Error::other("the reviewer stopped answering")),
    };
    drop(on_relay);
    match status.map(|status| status.code()) {
        Ok(Some(0)) => std::process::ExitCode::SUCCESS,
        Ok(Some(code)) => std::process::ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Ok(None) | Err(_) => std::process::ExitCode::FAILURE,
    }
}
