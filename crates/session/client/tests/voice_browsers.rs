//! Two people in one call, each in a browser: Ada and Bo each run the production call modules
//! (`web/voice-peer.mjs`, `web/voice.mjs`, `web/voice-audio.mjs`) in a headless Chromium page with
//! a synthetic microphone, over a certified session of their own device. Joining is presence on
//! the workspace's live session, their voices travel as the call's tracks through the host's live
//! origin, and each page plays the other's. Ada moderates the call from her page: she mutes Bo,
//! and both pages show he may no longer speak; she lets him back, then denies him the call, and
//! his page shows he was put out of it while hers lists only herself. From then on neither page
//! plays anything of the other: the live origin carries none of it, and what Ada says is sealed
//! under keys Bo never holds. A call made of pages alone, beside a server the call leaves out,
//! carries its media too: the pages found its keys themselves, and the server holds none of them.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use test_http::{Reply, Responder};
use tilo_session_client::{CallFrame, Cmd, Event, Outbox as _};
use tilo_test_env::SpawnUnderEnvLockAsync as _;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};

use crate::carriers::{Ada, Caller, live_session_keys, live_session_route, moderate};
use crate::support::{WORKSPACE, enrol, in_workspace, start_peer};

/// One call member's page, as the fixture answers it: its session and what it says.
struct Member {
    caller: Mutex<Caller>,
    person: [u8; 32],
}

impl Member {
    /// What the page polls: the workspace's calls as its session lists them, what each lets this
    /// person do, and the call's media since the last poll, as the wasm peer hands them over.
    fn poll(&self) -> Value {
        let mut caller = self.caller.lock().unwrap();
        caller.listen();
        caller.restate();
        let rules = caller.link.call_rules(WORKSPACE);
        let person = hex::encode(self.person);
        let listed = caller.channels();
        let mut allows = serde_json::Map::new();
        for channel in listed.iter().map(|channel| channel.channel.clone()).chain(
            caller
                .own
                .call
                .as_ref()
                .map(|call| call.call.channel.clone()),
        ) {
            let rule = rules.get(&channel).cloned().unwrap_or_default();
            let may = rule.may(Some(&person));
            allows.insert(
                channel,
                json!({"ended": rule.ended, "publish": may.publish, "subscribe": may.subscribe}),
            );
        }
        let channels: Vec<Value> = listed
            .into_iter()
            .map(|channel| {
                json!({
                    "channel": channel.channel,
                    "name": channel.name,
                    "ended": channel.rule.ended,
                    "members": channel.callers.iter().map(|one| json!({
                        "device": hex::encode(one.device),
                        "user": one.person.clone().unwrap_or_default(),
                        "publishes": channel.rule.may(one.person.as_deref()).publish,
                        "camera": one.camera,
                        "screen": one.screen,
                        "muted": one.muted,
                        "deafened": one.deafened,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        let events: Vec<Value> = std::mem::take(&mut caller.heard)
            .into_iter()
            .filter_map(|event| match event {
                Event::CallAudio {
                    from,
                    sequence,
                    timestamp_48k,
                    opus,
                    ..
                } => Some(json!({
                    "kind": "heard",
                    "workspace": WORKSPACE,
                    "from": hex::encode(from),
                    "sequence": sequence,
                    "timestamp48k": timestamp_48k,
                    "opus": hex::encode(opus),
                })),
                _ => None,
            })
            .collect();
        json!({"channels": channels, "allows": allows, "events": events})
    }

    /// Answer the page's `op` with `body`.
    fn answer(&self, op: &str, body: &[u8], host: [u8; 32]) -> Option<Reply> {
        let text = |body: String| Some(Reply::status(200).body(body));
        let parsed = || serde_json::from_slice::<Value>(body).ok();
        match op {
            "workspace" => text(WORKSPACE.to_owned()),
            "device" => text(hex::encode(self.caller.lock().unwrap().device)),
            "person" => text(hex::encode(self.person)),
            "poll" => text(self.poll().to_string()),
            "join" => {
                let said = parsed()?;
                let channel = said["channel"].as_str()?;
                let mut call = crate::carriers::in_call(channel, false, false);
                said["name"]
                    .as_str()
                    .unwrap_or_default()
                    .clone_into(&mut call.call.name);
                self.caller.lock().unwrap().say(Some(call));
                text(String::new())
            }
            "leave" => {
                self.caller.lock().unwrap().say(None);
                text(String::new())
            }
            "show" => {
                let said = parsed()?;
                let mut caller = self.caller.lock().unwrap();
                let flag = |name: &str| said[name].as_bool().unwrap_or(false);
                if let Some(call) = caller.own.call.clone() {
                    caller.say(Some(tilo_session_client::InCall {
                        camera: flag("camera"),
                        screen: flag("screen"),
                        muted: flag("muted"),
                        deafened: flag("deafened"),
                        ..call
                    }));
                }
                text(String::new())
            }
            "audio" => {
                let frames: Vec<Value> = serde_json::from_slice(body).ok()?;
                let frames = frames
                    .iter()
                    .filter_map(|frame| {
                        Some(CallFrame {
                            timestamp_48k: frame["timestamp48k"].as_u64()?,
                            opus: hex::decode(frame["opus"].as_str()?).ok()?,
                        })
                    })
                    .collect();
                let caller = self.caller.lock().unwrap();
                caller
                    .link
                    .send(Cmd::CallAudio(crate::support::workspace(), frames))
                    .unwrap();
                text(String::new())
            }
            "moderate" => {
                let asked = parsed()?;
                let channel = asked["channel"].as_str()?.to_owned();
                let link = self.caller.lock().unwrap().link.clone();
                text(moderate(&link, host, &channel, &asked).to_string())
            }
            _ => Some(Reply::status(404)),
        }
    }
}

/// The browser the members' pages run in, driven over the page driver's stdio.
struct Pages {
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
}

impl Pages {
    async fn ask(&mut self, request: Value) -> Value {
        self.stdin
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let line = tilo_test_env::within("the page driver to answer", self.lines.next_line())
            .await
            .unwrap()
            .expect("an answer");
        let answer: Value = serde_json::from_str(&line).unwrap();
        assert!(
            answer.get("error").is_none(),
            "{request} failed in the page: {answer}"
        );
        answer["value"].clone()
    }

    /// `expression`'s value in `who`'s page, from a gesture of the person's when `gesture`.
    async fn run(&mut self, who: &str, expression: &str, gesture: bool) -> Value {
        self.ask(json!({"tab": who, "run": expression, "gesture": gesture}))
            .await
    }

    /// Wait until `expression` holds in `who`'s page.
    async fn until(&mut self, who: &str, expression: &str) {
        self.ask(json!({"tab": who, "waitFor": expression, "timeout": 60_000}))
            .await;
    }

    /// How many frames `who`'s page played of `device` so far.
    async fn played(&mut self, who: &str, device: &str) -> u64 {
        self.run(who, &format!("window.playedOf({device:?})"), false)
            .await
            .as_u64()
            .unwrap()
    }
}

/// Someone Ada invited into her workspace, in it from a browser of theirs: their root, and their
/// page's session and device.
async fn guest(
    ada: &Ada,
    name: &str,
    keys: &key_group::ContentKeys,
) -> ([u8; 32], tilo_session_client::Link, [u8; 32]) {
    let (root, _) = UserIdentity::generate(name, 1);
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let person = root.verifying_key().to_bytes();
    crate::support::redeem(
        &ada.host,
        crate::support::entry_grant(&ada.host, &ada.root, &ada.host_device, WORKSPACE, person)
            .await,
        &browser.cert,
        &browser.key,
    )
    .await;
    let link = start_peer(
        ada.host.iroh(),
        browser.cert.clone(),
        ada.host_device.cert.clone(),
        browser.key.clone(),
    );
    link.send(Cmd::InstallGroupKeys(live_session_route(), keys.clone()))
        .unwrap();
    link.send(Cmd::Join(crate::support::workspace())).unwrap();
    in_workspace(&link, crate::support::workspace()).await;
    (person, link, browser.cert.device_public_key)
}

/// The members' pages, each in a tab of one headless Chromium, on a fixture that serves the
/// production call modules and answers each page from its member's session.
struct Browser {
    pages: Pages,
    child: tokio::process::Child,
    members: Arc<Vec<(&'static str, Member)>>,
    _fixture: Responder,
}

impl Browser {
    async fn open(members: Vec<(&'static str, Member)>, host: [u8; 32]) -> Self {
        let members = Arc::new(members);
        let web = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../browser/tilo-browser-runtime/web");
        let page = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/voice_browsers/page.html");
        let fixture = Responder::handle({
            let members = Arc::clone(&members);
            move |request| {
                let path = request.target.split('?').next().unwrap_or_default();
                if let Some(rest) = path.strip_prefix("/api/") {
                    let (who, op) = rest.split_once('/')?;
                    let (_, member) = members.iter().find(|(name, _)| *name == who)?;
                    return member.answer(op, &request.body, host);
                }
                let (file, kind) = if path == "/page.html" {
                    (page.clone(), "text/html")
                } else if let Some(name) = path.strip_prefix("/web/") {
                    if name.contains("..") {
                        return Some(Reply::status(404));
                    }
                    (web.join(name), "text/javascript")
                } else {
                    return Some(Reply::status(404));
                };
                Some(match std::fs::read(&file) {
                    Ok(body) => Reply::status(200).header("Content-Type", kind).body(body),
                    Err(_) => Reply::status(404),
                })
            }
        });
        let driver = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/support/page-driver.mjs");
        let names: Vec<&str> = members.iter().map(|(name, _)| *name).collect();
        let mut child = tokio::process::Command::new("node")
            .arg(driver)
            .arg("call")
            .arg(fixture.url())
            .args(&names)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn_locked()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let ready = tilo_test_env::within("the pages to open", lines.next_line())
            .await
            .unwrap()
            .expect("the pages' ready line");
        assert_eq!(
            serde_json::from_str::<Value>(&ready).unwrap()["ready"],
            json!(names)
        );
        Self {
            pages: Pages {
                stdin: child.stdin.take().unwrap(),
                lines,
            },
            child,
            members,
            _fixture: fixture,
        }
    }

    /// Each member joins the standup from a gesture and unmutes, and each page plays every other
    /// member's voice.
    async fn meet(&mut self, devices: &[(&str, &str)]) {
        let count = devices.len();
        for (who, _) in devices {
            self.pages
                .run(who, r#"window.voice.join("standup")"#, true)
                .await;
        }
        for (who, _) in devices {
            self.pages
                .until(
                    who,
                    &format!(
                        r#"window.voice.state.phase === "live" && window.voice.state.members.length === {count}"#
                    ),
                )
                .await;
            self.pages
                .run(who, "window.voice.setMuted(false)", true)
                .await;
        }
        for (who, _) in devices {
            for (_, other) in devices.iter().filter(|(name, _)| name != who) {
                self.pages
                    .until(who, &format!("window.playedOf({other:?}) > 5"))
                    .await;
            }
        }
    }

    /// Close the browser, each member's session, and `ada`'s host.
    async fn close(mut self, ada: Ada) {
        drop(self.pages);
        let closed =
            tokio::time::timeout(std::time::Duration::from_secs(20), self.child.wait()).await;
        assert!(
            closed.is_ok_and(|status| status.is_ok_and(|status| status.success())),
            "the browser did not close cleanly"
        );
        for (_, member) in self.members.iter() {
            member.caller.lock().unwrap().link.close();
        }
        ada.host.server.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_browsers_talk_in_a_call_their_workspace_moderates() {
    let ada = Ada::start().await;
    let host = ada.host_device.cert.device_public_key;
    let laptop = ada.laptop_peer(&ada.host_device.cert).await;
    let keys = live_session_keys();
    laptop
        .send(Cmd::InstallGroupKeys(live_session_route(), keys.clone()))
        .unwrap();
    in_workspace(&laptop, crate::support::workspace()).await;
    let (bo, browser, bo_browser) = guest(&ada, "Bo", &keys).await;

    let ada_person = ada.root.verifying_key().to_bytes();
    let ada_device = hex::encode(ada.laptop.cert.device_public_key);
    let bo_device = hex::encode(bo_browser);
    let mut browser = Browser::open(
        vec![
            (
                "ada",
                Member {
                    caller: Mutex::new(Caller::new(
                        laptop,
                        ada.laptop.cert.device_public_key,
                        &ada_person,
                    )),
                    person: ada_person,
                },
            ),
            (
                "bo",
                Member {
                    caller: Mutex::new(Caller::new(browser, bo_browser, &bo)),
                    person: bo,
                },
            ),
        ],
        host,
    )
    .await;
    browser
        .meet(&[("ada", &ada_device), ("bo", &bo_device)])
        .await;
    let pages = &mut browser.pages;

    // Ada mutes Bo from her page: his page says he may no longer speak, and hers shows him so.
    let bo_hex = hex::encode(bo);
    pages
        .run(
            "ada",
            &format!(
                r#"window.peerVoice.deny({{call: "standup", person: {bo_hex:?}, publish: true, subscribe: false}})"#
            ),
            false,
        )
        .await;
    pages
        .until("bo", "window.voice.state.canPublish === false")
        .await;
    pages
        .until(
            "ada",
            &format!(
                "window.voice.state.members.some(member => member.device === {bo_device:?} && member.publishes === false)"
            ),
        )
        .await;
    pages
        .run(
            "ada",
            &format!(r#"window.peerVoice.allow({{call: "standup", person: {bo_hex:?}}})"#),
            false,
        )
        .await;
    pages
        .until("bo", "window.voice.state.canPublish === true")
        .await;

    // Ada denies Bo the call: his page says he was put out of it, and hers lists only herself.
    pages
        .run(
            "ada",
            &format!(r#"window.peerVoice.deny({{call: "standup", person: {bo_hex:?}}})"#),
            false,
        )
        .await;
    pages
        .until(
            "bo",
            r#"window.voice.state.phase === "ended" && window.voice.state.reason === "you were denied this call""#,
        )
        .await;
    pages
        .until(
            "ada",
            &format!(
                "window.voice.state.members.length === 1 && window.voice.state.members[0].device === {ada_device:?}"
            ),
        )
        .await;

    // From then on neither page plays anything of the other, while Ada keeps speaking and Bo's
    // session is still on the workspace's live session.
    let (ada_heard, bo_heard) = (
        pages.played("ada", &bo_device).await,
        pages.played("bo", &ada_device).await,
    );
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert_eq!(
        pages.played("ada", &bo_device).await,
        ada_heard,
        "Ada's page played Bo after he was denied the call"
    );
    assert_eq!(
        pages.played("bo", &ada_device).await,
        bo_heard,
        "Bo's page played Ada after he was denied the call"
    );
    assert!(
        pages.run("ada", "window.published", false).await.as_u64() > Some(0),
        "Ada's page publishes its voice"
    );

    browser.close(ada).await;
}

/// Whether `server` holds any state of the standup's media key groups up to `generation`: what
/// it would open the call's media with.
fn holds_call_keys(server: &tilo_workspace_keys::SharedKeys, generation: u64) -> bool {
    let workspace = crate::support::workspace();
    let groups: Vec<_> = (0..=generation)
        .map(|generation| {
            neighbour_wire::KeyScope::Document {
                workspace,
                app: collab_types::scope::MEDIA_APP.into(),
                document: collab_types::calls::media_document("standup", generation),
                readers: None,
            }
            .group()
        })
        .collect();
    server.current_governance(workspace).iter().any(|message| {
        matches!(message, neighbour_wire::NeighbourMsg::MlsGovernance { frame, .. }
                if groups.contains(&frame.workspace))
    })
}

/// Bo and Cy meet in the standup, each from a browser, while Ada runs the workspace from her
/// server and keeps herself out of the call: no device but their pages holds the call's keys, so
/// the pages found each generation's keys and admit each other. Ada denies someone else the call
/// and both keep hearing each other within one key exchange; she denies Cy, and Cy's page plays
/// nothing Bo says after it. Ada's server, which carries every frame of the call, never holds any
/// of its keys.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_browsers_found_their_calls_keys_and_the_server_opens_none() {
    let ada = Ada::start().await;
    let host = ada.host_device.cert.device_public_key;
    let server = ada
        .host
        .server
        .workspace_keys()
        .expect("the server's key service");
    let laptop = ada.laptop_peer(&ada.host_device.cert).await;
    let keys = live_session_keys();
    laptop
        .send(Cmd::InstallGroupKeys(live_session_route(), keys.clone()))
        .unwrap();
    in_workspace(&laptop, crate::support::workspace()).await;
    let ada_person = ada.root.verifying_key().to_bytes();
    let rule = |link: &tilo_session_client::Link| {
        link.call_rules(WORKSPACE)
            .get("standup")
            .cloned()
            .unwrap_or_default()
    };
    moderate(
        &laptop,
        host,
        "standup",
        &json!({"operation": "deny", "person": hex::encode(ada_person)}),
    );
    let (bo, bo_link, bo_browser) = guest(&ada, "Bo", &keys).await;
    let (cy, cy_link, cy_browser) = guest(&ada, "Cy", &keys).await;
    let (bo_device, cy_device) = (hex::encode(bo_browser), hex::encode(cy_browser));
    let mut browser = Browser::open(
        vec![
            (
                "bo",
                Member {
                    caller: Mutex::new(Caller::new(bo_link, bo_browser, &bo)),
                    person: bo,
                },
            ),
            (
                "cy",
                Member {
                    caller: Mutex::new(Caller::new(cy_link, cy_browser, &cy)),
                    person: cy,
                },
            ),
        ],
        host,
    )
    .await;
    browser
        .meet(&[("bo", &bo_device), ("cy", &cy_device)])
        .await;
    assert!(
        !holds_call_keys(&server, rule(&laptop).generation),
        "the server holds keys of the call"
    );

    // Ada denies someone else the call: its keys move on, and each page hears the other again
    // within one key exchange.
    let before = rule(&laptop).generation;
    tokio::task::spawn_blocking({
        let laptop = laptop.clone();
        move || {
            moderate(
                &laptop,
                host,
                "standup",
                &json!({"operation": "deny", "person": hex::encode([0xc7; 32])}),
            )
        }
    })
    .await
    .unwrap();
    let moved = |member: &Member| rule(&member.caller.lock().unwrap().link).generation > before;
    crate::support::wait_for("both pages to hear of the deny", || {
        let done = browser.members.iter().all(|(_, member)| moved(member));
        async move { done.then_some(()) }
    })
    .await;
    let since = std::time::Instant::now();
    let heard = [
        browser.pages.played("bo", &cy_device).await,
        browser.pages.played("cy", &bo_device).await,
    ];
    loop {
        let now = [
            browser.pages.played("bo", &cy_device).await,
            browser.pages.played("cy", &bo_device).await,
        ];
        if now[0] > heard[0] + 5 && now[1] > heard[1] + 5 {
            break;
        }
        assert!(
            since.elapsed() < tilo_workspace_keys::founding_wait(),
            "the pages went {:?} without hearing each other after the call's keys moved on",
            since.elapsed()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    eprintln!(
        "the pages heard each other again {} ms after the call's keys moved on",
        since.elapsed().as_millis()
    );
    assert!(
        !holds_call_keys(&server, rule(&laptop).generation),
        "the server holds keys of the call"
    );

    // Ada denies Cy the call: Cy's page is put out of it and plays nothing Bo says after it.
    tokio::task::spawn_blocking({
        let laptop = laptop.clone();
        move || {
            moderate(
                &laptop,
                host,
                "standup",
                &json!({"operation": "deny", "person": hex::encode(cy)}),
            )
        }
    })
    .await
    .unwrap();
    browser
        .pages
        .until(
            "cy",
            r#"window.voice.state.phase === "ended" && window.voice.state.reason === "you were denied this call""#,
        )
        .await;
    let cy_heard = browser.pages.played("cy", &bo_device).await;
    let bo_published = browser.pages.run("bo", "window.published", false).await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(
        browser
            .pages
            .run("bo", "window.published", false)
            .await
            .as_u64()
            > bo_published.as_u64(),
        "Bo's page keeps speaking"
    );
    assert_eq!(
        browser.pages.played("cy", &bo_device).await,
        cy_heard,
        "Cy's page played Bo after Cy was denied the call"
    );
    assert!(
        !holds_call_keys(&server, rule(&laptop).generation),
        "the server holds keys of the call"
    );

    laptop.close();
    browser.close(ada).await;
}
