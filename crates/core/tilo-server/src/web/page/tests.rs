use std::sync::Mutex;

use tilo_actions::remote::{Caller, Pending};
use tilo_server_client::frame::{read_frame, write_frame};
use tilo_server_protocol::auth::{Hello, HelloAck, client_challenge_digest};
use tilo_server_protocol::page::{
    OpenedPage, PageAction, PageAddress, PageAnswer, PageAsk, PageNotice, PagePatch, PageRefusal,
};
use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert};

use super::{AppPages, serve_pages};
use crate::WorkspaceId;

const WORKSPACE: &str = "6f1d3a52-1c7e-4b6a-9d2e-5a4b3c2d1e0f";

fn cert(key: u8, label: &str, capabilities: DeviceCapabilities) -> DeviceCert {
    DeviceCert {
        user_public_key: [key; 32],
        device_public_key: [key.wrapping_add(1); 32],
        device_label: label.to_owned(),
        capabilities,
        // Past what a JavaScript number holds exactly.
        issued_at_ns: 1_763_600_000_123_456_789,
        expires_at_ns: None,
        signature: [key.wrapping_add(2); 64],
        issuer: None,
    }
}

/// What the page's script must write and read in the handshake, from the
/// handshake's own types: a browser's certificate of its own, one a
/// person's stand-in issued, and a server's whose grant names a workspace.
fn handshake() -> serde_json::Value {
    let own = cert(1, "Browser", DeviceCapabilities::member());
    let mut issuer = cert(
        4,
        "Stand-in",
        DeviceCapabilities {
            stand_in: true,
            ..DeviceCapabilities::member()
        },
    );
    issuer.expires_at_ns = Some(1_763_700_000_987_654_321);
    let stood_in = DeviceCert {
        issuer: Some(Box::new(issuer)),
        ..cert(
            7,
            "Laptop — Firefox",
            DeviceCapabilities {
                foreign: true,
                ..DeviceCapabilities::member()
            },
        )
    };
    let cases: Vec<serde_json::Value> = [own, stood_in]
        .into_iter()
        .map(|cert| {
            let certified = serde_json::to_string(&serde_json::json!({ "cert": cert })).unwrap();
            let hello = Hello {
                protocol: 7,
                ..Hello::new(cert, [3; 32])
            };
            serde_json::json!({
                "certified": certified,
                "hello": hex::encode(postcard::to_allocvec(&hello).unwrap()),
            })
        })
        .collect();
    let server = cert(
        9,
        "Server",
        DeviceCapabilities {
            agent: Some(AgentScope::Workspace([5; 16])),
            ..DeviceCapabilities::member()
        },
    );
    let ack = HelloAck {
        protocol: 7,
        cert: server,
        server_nonce: [9; 32],
        sig_over_client_nonce: [6; 64],
    };
    serde_json::json!({
        "protocol": 7,
        "nonce": hex::encode([3; 32]),
        "cases": cases,
        "ack": hex::encode(postcard::to_allocvec(&ack).unwrap()),
        "server_nonce": hex::encode([9; 32]),
        "digest": hex::encode(client_challenge_digest(&[9; 32])),
        "signing": signing(),
    })
}

/// What the page's script signs for an act, from the types that check it: BLAKE3 across blocks
/// and chunks, and one act's signed call with the message its key signs.
fn signing() -> serde_json::Value {
    use tilo_actions::remote::signed_invocation::{
        InvocationStatement, SignedInvocation, argument_hash,
    };
    use tilo_actions::remote::{InvokeArgument, RemoteActionTarget, SignedCall};
    let hashes: Vec<serde_json::Value> = [0_usize, 1, 64, 65, 1024, 1025, 2048, 3000, 5121]
        .into_iter()
        .map(|len| {
            let input: Vec<u8> = (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect();
            serde_json::json!({ "len": len, "hash": blake3::hash(&input).to_hex().to_string() })
        })
        .collect();
    let signer = cert(1, "Browser", DeviceCapabilities::member());
    // Text past one chunk, with what JSON escapes in it.
    let text = format!("Milk \"2\" ü \u{1}\n{}", "x".repeat(1100));
    let act = serde_json::json!({ "id": 7, "target": "notes.block-text:b1", "action": { "value": text } });
    let argument = InvokeArgument::Json(act.as_object().unwrap().clone());
    let statement = InvocationStatement {
        workspace: WORKSPACE.to_owned(),
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(
            0x0123_4567_89ab_4def_8123_4567_89ab_cdef,
        )),
        target: RemoteActionTarget {
            provider: "notes".into(),
            address: tilo_actions::ActionAddress::new("notes", "act"),
        },
        arguments: argument_hash(&argument),
        on_behalf_of: None,
        // Whole seconds: a JavaScript number holds them exactly.
        issued_at_ns: 1_763_600_000_000_000_000,
        signer: signer.device_public_key,
    };
    let message = statement.signing_message();
    let call = SignedCall {
        signed: SignedInvocation::new(statement, [4; 64], postcard::to_allocvec(&signer).unwrap()),
        argument: Some(argument),
        allowed: None,
    };
    serde_json::json!({
        "hashes": hashes,
        "act": act,
        "message": hex::encode(message),
        "call": call,
    })
}

#[test]
fn the_page_script_speaks_the_handshake_as_its_types_encode_it() {
    // `page.test.mjs` checks the script against these same bytes.
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("handshake.json")).expect("the golden handshake");
    let now = handshake();
    assert!(
        golden == now,
        "the handshake's encoding moved: write this to src/web/page/handshake.json and \
         update page.mjs until page.test.mjs passes:\n{}",
        serde_json::to_string_pretty(&now).unwrap()
    );
}

/// Pages that answer from memory: each act echoed as one changed element.
#[derive(Default)]
struct Echo {
    closed: Mutex<Vec<u64>>,
    /// The call steps its view asks for, and what the browser answered.
    calls: Mutex<Vec<(u64, ui::native_media::Request)>>,
    answered: Mutex<Vec<(u64, u64, tilo_server_protocol::page::CallAnswer)>>,
    /// Wakes the page's stream when a step is queued.
    waker: std::sync::Arc<tokio::sync::Notify>,
    /// The notifications the person's devices show, as the record of them holds them.
    notices: Mutex<Vec<PageNotice>>,
    /// What the browser answered of them: the person it was for, the device, and the notice.
    answered_notices: Mutex<Vec<([u8; 32], [u8; 32], String)>>,
}

impl Echo {
    /// The view asks the browser for `request`, as a press in a call view does.
    fn ask_for_a_call(&self, request_id: u64, request: ui::native_media::Request) {
        self.calls.lock().unwrap().push((request_id, request));
        self.waker.notify_waiters();
        self.waker.notify_one();
    }
}

impl AppPages for Echo {
    fn open(
        &self,
        _: WorkspaceId,
        caller: &Caller,
        address: PageAddress,
    ) -> Pending<Result<OpenedPage, PageRefusal>> {
        assert_eq!(
            caller.workspace(),
            Some(WORKSPACE),
            "the caller is the page's workspace's"
        );
        Pending::ready(Ok(OpenedPage {
            page: 1,
            title: "Notes".into(),
            css: String::new(),
            body: format!(
                "<div data-p=\"0\">{}</div>",
                address.show.unwrap_or_default()
            ),
        }))
    }

    fn act(
        &self,
        _: WorkspaceId,
        _: u64,
        act: Option<crate::app_serving::PageAct>,
    ) -> Pending<Result<Vec<PagePatch>, PageRefusal>> {
        Pending::ready(Ok(act
            .map(|act| PagePatch::Replace {
                id: act.id,
                html: format!("{:?}", act.action),
            })
            .into_iter()
            .collect()))
    }

    fn close(&self, _: WorkspaceId, page: u64) {
        self.closed.lock().unwrap().push(page);
    }

    fn take_calls(&self, _: WorkspaceId, _: u64) -> Vec<(u64, ui::native_media::Request)> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }

    fn answer_call(
        &self,
        page: u64,
        request_id: u64,
        answer: tilo_server_protocol::page::CallAnswer,
    ) {
        self.answered
            .lock()
            .unwrap()
            .push((page, request_id, answer));
    }

    fn calls_waker(&self) -> Option<std::sync::Arc<tokio::sync::Notify>> {
        Some(std::sync::Arc::clone(&self.waker))
    }

    fn notices(&self, caller: &Caller, _: i64) -> Pending<Vec<PageNotice>> {
        let person = caller
            .user_public_key()
            .copied()
            .expect("the page's person is the caller's");
        let notices = self
            .notices
            .lock()
            .unwrap()
            .iter()
            .map(|notice| {
                // The record is the person's own: an empty one reaches nobody.
                assert_eq!(person, [1; 32], "the record read is the page's person's");
                notice.clone()
            })
            .collect();
        Pending::ready(notices)
    }

    fn answer_notice(
        &self,
        caller: &Caller,
        device: &[u8; 32],
        id: &str,
    ) -> Pending<Result<(), String>> {
        self.answered_notices.lock().unwrap().push((
            *caller.user_public_key().unwrap(),
            *device,
            id.to_owned(),
        ));
        Pending::ready(Ok(()))
    }
}

async fn ask(stream: &mut tokio::io::DuplexStream, ask: &PageAsk) {
    write_frame(stream, &serde_json::to_vec(ask).unwrap())
        .await
        .unwrap();
}

async fn answer(stream: &mut tokio::io::DuplexStream) -> PageAnswer {
    serde_json::from_slice(&read_frame(stream, 1 << 20).await.unwrap()).unwrap()
}

#[tokio::test]
async fn a_page_opens_acts_and_lets_go_when_its_browser_leaves() {
    let pages = Echo::default();
    let person = Caller::authenticated([1; 32], [2; 32]);
    let (mut browser, served) = tokio::io::duplex(1 << 16);
    let (recv, send) = tokio::io::split(served);
    let address = PageAddress {
        workspace: WORKSPACE.into(),
        app: "notes".into(),
        show: Some("notes:page:a".into()),
    };
    let serving = serve_pages(&pages, &person, |_, _| async { Ok(true) }, recv, send);
    let browsing = async {
        ask(&mut browser, &PageAsk::Open(address)).await;
        // Its keys were on their way: it is told so, then shown.
        assert_eq!(answer(&mut browser).await, PageAnswer::Loading);
        let PageAnswer::Shown { title, body, .. } = answer(&mut browser).await else {
            panic!("the page is shown");
        };
        assert_eq!(
            (title.as_str(), body.as_str()),
            ("Notes", "<div data-p=\"0\">notes:page:a</div>")
        );
        ask(
            &mut browser,
            &PageAsk::Act {
                id: 3,
                target: None,
                action: PageAction::Value("Milk".into()),
                signed: None,
            },
        )
        .await;
        assert_eq!(
            answer(&mut browser).await,
            PageAnswer::Patched(vec![PagePatch::Replace {
                id: 3,
                html: "Value(\"Milk\")".into()
            }])
        );
        drop(browser);
    };
    tokio::join!(serving, browsing);
    assert_eq!(*pages.closed.lock().unwrap(), vec![1], "the page is let go");
}

#[tokio::test]
async fn a_workspace_only_stored_here_is_refused_and_opens_nothing() {
    let pages = Echo::default();
    let person = Caller::authenticated([1; 32], [2; 32]);
    let (mut browser, served) = tokio::io::duplex(1 << 16);
    let (recv, send) = tokio::io::split(served);
    let address = PageAddress {
        workspace: WORKSPACE.into(),
        app: "notes".into(),
        show: None,
    };
    let serving = serve_pages(
        &pages,
        &person,
        |_, _| async { Err(PageRefusal::StoredCopy) },
        recv,
        send,
    );
    let browsing = async {
        ask(&mut browser, &PageAsk::Open(address)).await;
        assert_eq!(
            answer(&mut browser).await,
            PageAnswer::Refused(PageRefusal::StoredCopy)
        );
        drop(browser);
    };
    tokio::join!(serving, browsing);
    assert!(
        pages.closed.lock().unwrap().is_empty(),
        "nothing was opened"
    );
}

#[tokio::test]
async fn a_pages_view_asks_the_browser_for_a_call_step_and_hears_its_answer() {
    use tilo_server_protocol::page::{CallAnswer, PageCall};
    use ui::native_media::{Command, Request, Snapshot, State};

    let pages = Echo::default();
    let person = Caller::authenticated([1; 32], [2; 32]);
    let (mut browser, served) = tokio::io::duplex(1 << 16);
    let (recv, send) = tokio::io::split(served);
    let address = PageAddress {
        workspace: WORKSPACE.into(),
        app: "chat".into(),
        show: None,
    };
    let serving = serve_pages(&pages, &person, |_, _| async { Ok(true) }, recv, send);
    let browsing = async {
        ask(&mut browser, &PageAsk::Open(address)).await;
        assert_eq!(answer(&mut browser).await, PageAnswer::Loading);
        assert!(matches!(
            answer(&mut browser).await,
            PageAnswer::Shown { .. }
        ));

        // The view asked to join: the page's stream carries the step to the browser at once.
        let join = Request {
            version: 1,
            intent: 1,
            command: Command::Join {
                channel_id: "standup".into(),
            },
        };
        pages.ask_for_a_call(7, join.clone());
        assert_eq!(
            answer(&mut browser).await,
            PageAnswer::Call(PageCall {
                request_id: 7,
                command: join,
            })
        );

        // What the browser's own call answered goes back the same way, to the view.
        let snapshot = Snapshot {
            version: 1,
            state: State::Live,
            muted: true,
            capture_active: false,
            input_id: None,
            inputs: Vec::new(),
            issue: None,
            inventory_revision: None,
            publication: None,
            publishers: Vec::new(),
            rosters: Vec::new(),
            speaking: Vec::new(),
            camera: false,
            screen: false,
            deafened: false,
            revision: 4,
            pictures: Vec::new(),
            pictures_cursor: 0,
            volumes: Vec::new(),
            cameras: Vec::new(),
            camera_id: None,
        };
        ask(
            &mut browser,
            &PageAsk::CallResult {
                request_id: 7,
                answer: CallAnswer::Answered(Box::new(snapshot.clone())),
            },
        )
        .await;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while pages.answered.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the browser's answer reached the page's view");
        assert!(
            matches!(
                pages.answered.lock().unwrap().as_slice(),
                [(1, 7, CallAnswer::Answered(got))] if **got == snapshot
            ),
            "the step's own answer, as the browser gave it"
        );
        drop(browser);
    };
    tokio::join!(serving, browsing);
}

#[test]
fn every_face_the_page_stylesheet_names_is_served_from_the_page_s_own_origin() {
    let css = ui::composition::page::PAGE_CSS;
    let named: Vec<&str> = css
        .split("url(\"")
        .skip(1)
        .filter_map(|rest| rest.split_once('"').map(|(url, _)| url))
        .collect();
    assert_eq!(
        named.len(),
        tilo_font_catalog::web::CUTS.len(),
        "every cut the host serves is one the stylesheet names: {named:?}"
    );
    for url in named {
        assert!(url.starts_with('/'), "{url} is on the page's own origin");
        let response = super::font(url).unwrap_or_else(|| panic!("{url} is served"));
        assert_eq!(
            response.headers()[hyper::header::CONTENT_TYPE],
            "font/woff2"
        );
        let super::super::Body::Full(Some(bytes)) = response.body() else {
            panic!("{url} answers its bytes whole");
        };
        assert!(bytes.starts_with(b"wOF2"), "{url} is WOFF2");
    }
    assert!(super::font("/fonts/TiloSans[wdth,wght].ttf").is_none());
    assert!(super::font("/fonts/../manifest.toml").is_none());
}

#[tokio::test]
async fn the_notifications_the_person_s_devices_show_reach_the_page_and_its_answer_reaches_them() {
    let pages = Echo::default();
    let told = PageNotice {
        id: "7f".repeat(32),
        app_name: "Mail".into(),
        title: "A message from Ada".into(),
        body: String::new(),
        urgency: 0,
    };
    *pages.notices.lock().unwrap() = vec![told.clone()];
    let person = Caller::authenticated([1; 32], [2; 32]);
    let (mut browser, served) = tokio::io::duplex(1 << 16);
    let (recv, send) = tokio::io::split(served);
    let address = PageAddress {
        workspace: WORKSPACE.into(),
        app: "mail".into(),
        show: None,
    };
    let serving = serve_pages(&pages, &person, |_, _| async { Ok(true) }, recv, send);
    let browsing = async {
        ask(&mut browser, &PageAsk::Open(address)).await;
        assert_eq!(answer(&mut browser).await, PageAnswer::Loading);
        assert!(matches!(
            answer(&mut browser).await,
            PageAnswer::Shown { .. }
        ));
        // The record of the person's devices holds a notice none of them answered: the page is
        // told, and raises it as a browser notification while it is not in front.
        assert_eq!(
            answer(&mut browser).await,
            PageAnswer::Notices(vec![told.clone()])
        );

        // The person dismissed it in the browser: the device they answered on is the browser's
        // own, so the record can tell the device it was sent on where they were.
        ask(
            &mut browser,
            &PageAsk::AnswerNotice {
                id: told.id.clone(),
            },
        )
        .await;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while pages.answered_notices.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the person's answer reached the record");
        assert_eq!(
            pages.answered_notices.lock().unwrap().as_slice(),
            [([1; 32], [2; 32], told.id.clone())]
        );
        drop(browser);
    };
    tokio::join!(serving, browsing);
}
