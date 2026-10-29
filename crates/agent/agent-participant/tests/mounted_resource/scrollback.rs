//! Reading back through a chat's earlier parts on a device that joined
//! late, in the real compiled Chat mounted in a browser page.
//!
//! A writer's device posts enough to fill three parts of the chat's
//! messages and hands the room's host the chat and parts 1 and 2 only: the
//! host never had part 0. A reader's page joins later, so the host replays
//! it the head (part 2). Its reader scrolls back: part 1 is asked of the
//! host and its messages arrive, then part 0 is asked, the host has none of
//! it, and the timeline says earlier messages are not available, with
//! nothing loading.

use super::super::{Browser, CatalogueQuery, DeviceCapabilities, Duration, Path, Replica, Seed};
use super::super::{common, enrol, record_provider, start_browser};
use tilo_peer_state::join::MemberRole;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

/// Short messages each part keeps in `#general`: more than a view, so the
/// reader scrolls to reach the part before.
const PER_PART: usize = 150;

/// The channel the long posts that fill each part go to.
const ARCHIVE: &str = "archive";

/// A post as long as one may be: a part fills after a few dozen of them.
fn long_post(n: usize) -> String {
    let line = format!("filler {n} ");
    line.repeat(
        chat::record::MAX_MESSAGE_TEXT_BYTES
            .checked_div(line.len())
            .unwrap_or_default(),
    )
}

/// The resource `id` among `resources` and their children.
fn find<'a>(
    resources: &'a [ui::tilo_actions::ResourceSpec],
    id: &str,
) -> Option<&'a ui::tilo_actions::ResourceSpec> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource)
        } else {
            find(&resource.children, id)
        }
    })
}

/// Run against an explicitly supplied, compiled web app bundle.
pub async fn run(bundle: &str) {
    use super::super::resource_support::{
        admit_person, on_messages, open_admitted_peer, part_route, published_message,
    };
    let driver =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mounted_resource/scrollback.mjs");
    let mut child = tokio::process::Command::new("node")
        .args([driver.as_os_str(), Path::new(bundle).as_os_str()])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    let tls = tilo_crypto::generate_tls_fixture(std::net::Ipv4Addr::LOCALHOST.into()).unwrap();
    let credentials = serde_json::json!({
        "key": tls.server_private_key_pem,
        "cert": tls.server_certificate_pem,
    });
    input
        .write_all(format!("{credentials}\n").as_bytes())
        .await
        .unwrap();
    let page = output
        .next_line()
        .await
        .unwrap()
        .expect("browser fixture origin");
    let page: serde_json::Value = serde_json::from_str(&page).unwrap();
    let page = page["origin"].as_str().unwrap();

    let (alice, _) = tilo_user_identity::UserIdentity::generate("Fixture owner", 1);
    let (bob, _) = tilo_user_identity::UserIdentity::generate("Fixture reader", 1);
    let host = enrol(
        &alice,
        "fixture-host",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let writer = enrol(&alice, "fixture-writer", DeviceCapabilities::primary());
    let reader = enrol(&bob, "fixture-reader", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start_browser(&alice, &host, state.path(), Some(page)).await;
    let rooms = hosted.relay.rooms().unwrap();
    let room = rooms.create("Scrollback acceptance").await.unwrap();
    let mut native = Browser::open_room(&hosted, &writer, &host, &room.room).await;

    let author = "Fixture owner".to_string();
    let mut source = Replica::open(
        &common::catalogue(),
        "so.tilo.chat",
        &room.room,
        &author,
        tilo_agent_participant::ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let mut doc = source.read(Clone::clone).unwrap();
    for name in ["general", ARCHIVE] {
        chat::demo::apply_as(
            &mut doc,
            &author,
            chat::Chat::CREATE_CHANNEL.with(chat::record::chat::CreateChannel {
                name: name.parse().unwrap(),
                category: None,
                kind: None,
                topic: None,
            }),
        )
        .unwrap();
    }
    let channels = chat::projection::read(&doc).channel_rows();
    let channel_of = |name: &str| {
        channels
            .iter()
            .find(|channel| channel.name == name)
            .map(|channel| channel.id.clone())
            .unwrap()
    };
    let (general, archive) = (channel_of("general"), channel_of(ARCHIVE));
    source
        .apply(&collab_types::DocumentMutation::Snapshot(doc.save()))
        .unwrap();

    // `#general` gets "<part>.<n>" in each part; long posts to `#archive`
    // fill each part until the next starts.
    let head = |source: &Replica| {
        source
            .records_beside()
            .iter()
            .filter_map(chat::parts::part_index)
            .max()
            .unwrap_or_default()
    };
    let mut last = String::new();
    for part in 0..3_u64 {
        for n in 0..PER_PART {
            last = on_messages(
                &mut source,
                "post",
                &serde_json::json!({"channel": general, "text": format!("{part}.{n}")}),
            );
        }
        assert_eq!(
            head(&source),
            part,
            "#general's posts of part {part} are in it"
        );
        if part == 2 {
            // `#archive` has a view of its own in the head too: whichever
            // channel the page shows first, nothing reads back before its
            // reader scrolls.
            for n in 0..PER_PART {
                on_messages(
                    &mut source,
                    "post",
                    &serde_json::json!({"channel": archive, "text": format!("a.{n}")}),
                );
            }
            break;
        }
        let mut n = 0;
        while head(&source) == part {
            on_messages(
                &mut source,
                "post",
                &serde_json::json!({"channel": archive, "text": long_post(n)}),
            );
            n = n.saturating_add(1);
        }
    }
    let route = super::super::DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: room.room.clone(),
        record: source.wire_record().into(),
    };
    native
        .peer
        .publish(route.clone(), &source.take_mutation().unwrap())
        .unwrap();
    let mut handed = Vec::new();
    for (changed, mutation) in source.take_record_mutations() {
        let part = chat::parts::part_index(&changed).expect("only parts change beside the chat");
        // The host never had part 0: nobody can send the reader its history.
        if part == 0 {
            continue;
        }
        native
            .peer
            .publish(part_route(&room.room, &changed), &mutation)
            .unwrap();
        handed.push(part);
    }
    handed.sort_unstable();
    handed.dedup();
    assert_eq!(handed, [1, 2]);
    let barrier = native.peer.barrier().unwrap();
    tilo_test_env::within("the host keeps what the writer handed it", async {
        loop {
            match barrier.try_recv() {
                Ok(result) => break result.unwrap(),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    panic!("durability barrier closed before acknowledgement");
                }
            }
        }
    })
    .await;

    // The head is in the room's catalogue before the reader comes.
    let head_part = chat::parts::part(2);
    let provider = record_provider(&part_route(&room.room, &head_part));
    let (message, message_kind) = published_message(&source, &last);
    let query = CatalogueQuery {
        provider: Some(provider.clone()),
        view_resource: Some(tilo_actions::remote::ViewResourceQuery {
            id: message.clone(),
            kind: message_kind.clone(),
        }),
        ..CatalogueQuery::default()
    };
    tilo_test_env::within("the head reaches the governed catalogue", async {
        while !native
            .catalogue(query.clone())
            .await
            .is_ok_and(|page| !page.rows.is_empty())
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    admit_person(&hosted, &host, &room.room, &reader, MemberRole::Editor).await;

    // Another of the reader's devices joining now is replayed the chat and
    // the head, and nothing of part 1 until it asks.
    let probe_device = enrol(&bob, "fixture-probe", DeviceCapabilities::member());
    admit_person(
        &hosted,
        &host,
        &room.room,
        &probe_device,
        MemberRole::Viewer,
    )
    .await;
    let mut probe = open_admitted_peer(&hosted, &probe_device, &host, &room.room).await;
    let head_route = part_route(&room.room, &head_part);
    tilo_test_env::within("a joining device is replayed the head", async {
        while probe.documents.get(&head_route).is_none_or(Vec::is_empty) {
            probe.pump();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let replayed: Vec<String> = probe
        .documents
        .keys()
        .map(|route| route.record.clone())
        .collect();
    println!("SCROLLBACK_REPLAYED={replayed:?}");
    assert!(
        !probe
            .documents
            .contains_key(&part_route(&room.room, &chat::parts::part(1))),
        "an earlier part is not replayed: {replayed:?}"
    );
    probe.peer.close();

    // The reader's page opens the chat on `#general`: a channel, not a
    // message, which a page that does not hold it yet would look for back
    // through the parts.
    let channel_id = chat::projection::item_id("channels", &general);
    let channel_kind = find(&source.resources(), &channel_id)
        .map(|resource| resource.kind.clone())
        .expect("the chat publishes its channel");
    let provider = record_provider(&route);

    let reader_root = hex::encode(reader.cert.user_public_key);
    let decoy = "ab".repeat(32);
    let bootstrap = serde_json::json!({
        "origin": hosted.web.origin, "room": room.room, "provider": provider,
        "record": route.record,
        "reference": {"id": channel_id, "kind": channel_kind},
        "root": reader_root, "decoy": decoy, "seed": reader.key.to_bytes(),
        "certificate": {"cert": reader.cert, "hostCert": host.cert},
        "accounts": super::account_document(&reader_root, &hosted.web.origin, "Fixture reader"),
        "decoyAccounts": super::account_document(&decoy, &hosted.web.origin, "Unadmitted fixture account"),
        "perPart": PER_PART,
    });
    input
        .write_all(format!("{bootstrap}\n").as_bytes())
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(420), async {
        while let Some(line) = output.next_line().await.unwrap() {
            println!("SCROLLBACK_STAGE={line}");
        }
        child.wait().await.unwrap()
    })
    .await;
    native.peer.close();
    hosted.agents.shutdown();
    hosted.relay.shutdown().await;
    assert!(
        result.expect("bounded scrollback journey").success(),
        "scrollback journey failed"
    );
}
