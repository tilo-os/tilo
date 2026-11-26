use std::sync::Mutex;

use tilo_actions::remote::{Caller, Pending};
use tilo_server_client::frame::{read_frame, write_frame};
use tilo_server_protocol::auth::{Hello, HelloAck, client_challenge_digest};
use tilo_server_protocol::page::{
    OpenedPage, PageAction, PageAddress, PageAnswer, PageAsk, PagePatch, PageRefusal, PageUpdate,
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
        act: Option<(u32, Option<String>, PageAction)>,
    ) -> Pending<Result<PageUpdate, PageRefusal>> {
        Pending::ready(Ok(PageUpdate::Changed(
            act.map(|(id, _, action)| PagePatch {
                id,
                html: format!("{action:?}"),
            })
            .into_iter()
            .collect(),
        )))
    }

    fn close(&self, _: WorkspaceId, page: u64) {
        self.closed.lock().unwrap().push(page);
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
    let serving = serve_pages(&pages, &person, true, |_, _| async { Ok(true) }, recv, send);
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
            },
        )
        .await;
        assert_eq!(
            answer(&mut browser).await,
            PageAnswer::Patched(vec![PagePatch {
                id: 3,
                html: "Value(\"Milk\")".into()
            }])
        );
        drop(browser);
    };
    tokio::join!(serving, browsing);
    assert_eq!(*pages.closed.lock().unwrap(), vec![1], "the page is let go");
}

/// Pages that draw a question sheet over the app, and take the person's
/// answer on it: what a page holding their identity shows.
#[derive(Default)]
struct Asking {
    asked: Mutex<Vec<String>>,
}

/// The sheet the asking page draws: a question's method and its Allow.
fn sheet() -> String {
    let target = tilo_server_protocol::page::question_target([7; 32], "allow-once");
    format!(
        "<div data-p=\"{id}\" data-question-method=\"POST\"><button data-a=\"activate\" data-t=\"{target}\">Allow once</button></div>",
        id = tilo_server_protocol::page::QUESTION_PATCH_ID,
    )
}

impl AppPages for Asking {
    fn open(
        &self,
        _: WorkspaceId,
        _: &Caller,
        _: PageAddress,
    ) -> Pending<Result<OpenedPage, PageRefusal>> {
        Pending::ready(Ok(OpenedPage {
            page: 1,
            title: "Notes".into(),
            css: String::new(),
            body: "<div data-p=\"0\"></div>".into(),
        }))
    }

    fn act(
        &self,
        _: WorkspaceId,
        _: u64,
        _: Option<(u32, Option<String>, PageAction)>,
    ) -> Pending<Result<PageUpdate, PageRefusal>> {
        Pending::ready(Ok(PageUpdate::Changed(Vec::new())))
    }

    fn close(&self, _: WorkspaceId, _: u64) {}

    fn question_sheet(&self, _: WorkspaceId, _: u64) -> String {
        sheet()
    }

    fn answer_question(
        &self,
        _: WorkspaceId,
        page: u64,
        target: &str,
    ) -> Option<Pending<Result<Vec<PagePatch>, PageRefusal>>> {
        self.asked.lock().unwrap().push(target.to_owned());
        Some(Pending::ready(Ok(vec![PagePatch {
            id: tilo_server_protocol::page::QUESTION_PATCH_ID,
            html: format!("<div data-p=\"{page}\" hidden></div>"),
        }])))
    }
}

#[tokio::test]
async fn a_questions_answer_on_the_sheet_runs_it_and_patches_the_sheet() {
    let pages = Asking::default();
    let person = Caller::authenticated([1; 32], [2; 32]);
    let (mut browser, served) = tokio::io::duplex(1 << 16);
    let (recv, send) = tokio::io::split(served);
    let target = tilo_server_protocol::page::question_target([7; 32], "allow-once");
    let serving = serve_pages(&pages, &person, true, |_, _| async { Ok(true) }, recv, send);
    let browsing = async {
        ask(
            &mut browser,
            &PageAsk::Open(PageAddress {
                workspace: WORKSPACE.into(),
                app: "notes".into(),
                show: None,
            }),
        )
        .await;
        assert!(matches!(answer(&mut browser).await, PageAnswer::Loading));
        let PageAnswer::Shown { body, .. } = answer(&mut browser).await else {
            panic!("the page is shown");
        };
        assert!(body.contains("data-question-method=\"POST\""), "{body}");
        ask(
            &mut browser,
            &PageAsk::Act {
                id: tilo_server_protocol::page::QUESTION_PATCH_ID,
                target: Some(target.clone()),
                action: PageAction::Activate,
            },
        )
        .await;
        assert!(matches!(
            answer(&mut browser).await,
            PageAnswer::Patched(patches)
                if patches.len() == 1 && patches[0].html.contains("hidden")
        ));
        drop(browser);
    };
    tokio::join!(serving, browsing);
    assert_eq!(*pages.asked.lock().unwrap(), vec![target]);
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
        true,
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

/// A page its app had drawn nothing into when it was opened, and drew later: what a page opened
/// on an app still reading its document is.
#[derive(Default)]
struct Late;

impl AppPages for Late {
    fn open(
        &self,
        _: WorkspaceId,
        _: &Caller,
        _: PageAddress,
    ) -> Pending<Result<OpenedPage, PageRefusal>> {
        Pending::ready(Ok(OpenedPage {
            page: 1,
            title: "Notes".into(),
            css: String::new(),
            body: String::new(),
        }))
    }

    fn act(
        &self,
        _: WorkspaceId,
        _: u64,
        _: Option<(u32, Option<String>, PageAction)>,
    ) -> Pending<Result<PageUpdate, PageRefusal>> {
        Pending::ready(Ok(PageUpdate::Shown {
            title: "Notes".into(),
            css: "body{}".into(),
            body: "<div data-p=\"0\">New note</div>".into(),
        }))
    }

    fn close(&self, _: WorkspaceId, _: u64) {}
}

/// A page that had drawn nothing when it was asked for is written whole once its app draws:
/// there is no element of the empty page to patch, so a pass answering patches alone would
/// leave it empty for good.
#[tokio::test]
async fn a_pass_answers_the_page_whole_once_a_page_that_had_drawn_nothing_draws() {
    let pages = Late;
    let person = Caller::authenticated([1; 32], [2; 32]);
    let (mut browser, served) = tokio::io::duplex(1 << 16);
    let (recv, send) = tokio::io::split(served);
    let address = PageAddress {
        workspace: WORKSPACE.into(),
        app: "notes".into(),
        show: None,
    };
    let serving = serve_pages(&pages, &person, true, |_, _| async { Ok(true) }, recv, send);
    let browsing = async {
        ask(&mut browser, &PageAsk::Open(address)).await;
        assert_eq!(answer(&mut browser).await, PageAnswer::Loading);
        let PageAnswer::Shown { body, .. } = answer(&mut browser).await else {
            panic!("the page is shown, empty");
        };
        assert_eq!(body, "", "the app had drawn nothing yet");
        // A pass with no act answers the page whole: what the app drew since.
        let PageAnswer::Shown { title, css, body } = answer(&mut browser).await else {
            panic!("the page is written whole again");
        };
        assert_eq!(
            (title.as_str(), css.as_str(), body.as_str()),
            ("Notes", "body{}", "<div data-p=\"0\">New note</div>")
        );
        drop(browser);
    };
    tokio::join!(serving, browsing);
}
