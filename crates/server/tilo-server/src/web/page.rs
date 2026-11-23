//! A workspace's apps as web pages, for a browser that is already a device of
//! one of its members: `/w/<workspace>/<app>[/<target>]`.
//!
//! The address answers a small document: the page's stylesheet and a script,
//! no app code. The script proves the browser's device on a WebTransport
//! session at [`LINK_PAGE_PATH`] with the handshake every session opens with
//! (`tilo_server_protocol::auth`), then asks for the page
//! ([`PageAsk::Open`]). The app's view renders here as semantic HTML
//! ([`AppPages`]), and each click or edit the person makes comes back as one
//! [`PageAsk::Act`], answered with the elements that changed.
//!
//! A page is shown only where this server reads the workspace: it governs it
//! and holds its keys, or holds it unsealed. While the keys are on their way
//! the page is told it is loading; a server that only stores the workspace's
//! ciphertext refuses it ([`PageRefusal::StoredCopy`]).

use std::future::Future;
use std::time::Duration;

use tilo_actions::remote::Caller;
use tilo_server_client::frame::{read_frame_or_eof, write_frame};
use tilo_server_protocol::page::{
    MAX_PAGE_FRAME_BYTES, PageAddress, PageAnswer, PageAsk, PagePatch, PageRefusal,
    QUESTION_PATCH_ID,
};
use tilo_server_protocol::web::LINK_PAGE_PATH;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::WorkspaceId;
use crate::app_serving::AppPages;
use crate::service::ServerProtocol;

/// How often an open page asks what others changed while its person does
/// nothing: their edits reach it within this.
const FOLLOW_EVERY: Duration = Duration::from_secs(2);

/// The page's script: the handshake, the session and what the person does.
const SCRIPT: &str = include_str!("page/page.mjs");

/// Whether the workspace's documents are readable here now: `Ok(true)` once
/// the keys on their way have arrived, `Ok(false)` when they are readable.
async fn readiness(
    workspaces: Option<&crate::workspaces::Workspaces>,
    workspace_id: WorkspaceId,
    app: String,
) -> Result<bool, PageRefusal> {
    let workspaces = workspaces.ok_or(PageRefusal::Elsewhere)?;
    let name = workspaces
        .workspace_identity(workspace_id)
        .await
        .ok_or(PageRefusal::Elsewhere)?;
    if workspaces.stores_only(&name).await {
        return Err(PageRefusal::StoredCopy);
    }
    // The app's own document is the one its page shows first; a sealed
    // workspace whose keys this server holds none of waits for them all.
    Ok(workspaces.waits_for_keys(&name, &app, "").await)
}

/// Serve a WebTransport session at [`LINK_PAGE_PATH`]: its first stream proves
/// which device opened it, as every session's does, then carries its pages.
pub(crate) async fn serve_web(protocol: &ServerProtocol, session: tilo_net::webtransport::Session) {
    let link = tilo_server_client::link::Link::web(session);
    let Ok((mut send, mut recv)) = link.accept_bi().await else {
        return;
    };
    let Ok(device) = protocol.authenticate_member(&mut recv, &mut send).await else {
        link.close("the device did not prove itself");
        return;
    };
    let Some(pages) = protocol.pages() else {
        link.close("no app is served here");
        return;
    };
    let workspaces = protocol.admin.get().cloned();
    let person = crate::service::local_caller(&device.cert);
    // A device of the person answers what their hosts ask them on the page;
    // an agent's, or a stand-in's, is no device of theirs and answers
    // nothing for them, as on a native session's sheet.
    let answers = device.cert.capabilities.agent.is_none() && !device.cert.capabilities.stand_in;
    serve_pages(
        pages,
        &person,
        answers,
        |workspace_id, app| readiness(workspaces.as_deref(), workspace_id, app),
        recv,
        send,
    )
    .await;
    link.close("the page closed");
}

/// Answer what a page asks on its stream until it closes: the person of
/// `person` opens pages and acts on the one open, and an open page hears what
/// others changed on it. `answers` says whether this device answers their
/// questions on the page; `ready` says whether a workspace's documents are
/// readable here ([`readiness`]).
#[allow(
    clippy::too_many_lines,
    reason = "one page loop read top to bottom: its asks, its questions and what others changed"
)]
pub(crate) async fn serve_pages<R, W, F, Fut>(
    pages: &dyn AppPages,
    person: &Caller,
    answers: bool,
    ready: F,
    mut recv: R,
    mut send: W,
) where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send,
    F: Fn(WorkspaceId, String) -> Fut + Sync,
    Fut: Future<Output = Result<bool, PageRefusal>> + Send,
{
    // The stream is read on its own, so a page whose person does nothing
    // still hears what others changed.
    let (asked, mut asks) = tokio::sync::mpsc::channel(16);
    let reader = tokio::spawn(async move {
        while let Ok(Some(frame)) = read_frame_or_eof(&mut recv, MAX_PAGE_FRAME_BYTES).await {
            if asked.send(frame).await.is_err() {
                break;
            }
        }
    });
    let mut open: Option<(WorkspaceId, u64)> = None;
    // The question sheet the open page draws, as last sent: a question that
    // comes or goes patches it in place.
    let mut sheet: Option<String> = None;
    let mut follow = tokio::time::interval(FOLLOW_EVERY);
    follow.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let answer = tokio::select! {
            frame = asks.recv() => {
                let Some(ask) = frame.and_then(|frame| serde_json::from_slice::<PageAsk>(&frame).ok()) else {
                    break;
                };
                match ask {
                    PageAsk::Open { address, signed } => {
                        if let Some((workspace_id, page)) = open.take() {
                            pages.close(workspace_id, page);
                        }
                        let opened =
                            open_page(pages, person, &ready, address, signed, &mut send).await;
                        if let Ok((workspace_id, page, _)) = &opened {
                            open = Some((*workspace_id, *page));
                            pages.opened(*workspace_id, *page, person);
                            sheet = answers.then(|| pages.question_sheet(*workspace_id, *page));
                        }
                        match opened {
                            // A device of the person has the questions their
                            // hosts ask them drawn over the app's page.
                            Ok((workspace_id, page, PageAnswer::Shown { title, css, mut body }))
                                if answers =>
                            {
                                body.push_str(&pages.question_sheet(workspace_id, page));
                                PageAnswer::Shown { title, css, body }
                            }
                            Ok((_, _, answer)) => answer,
                            Err(refusal) => {
                                tracing::debug!(?refusal, "a page was refused");
                                PageAnswer::Refused(refusal)
                            }
                        }
                    }
                    PageAsk::Act {
                        id,
                        target,
                        action,
                        signed,
                    } => match open {
                        Some((workspace_id, page)) if id == QUESTION_PATCH_ID && answers => {
                            // The person answered the question the page draws
                            // over the app: their answer runs the held call,
                            // and the sheet shows what waits next.
                            let asked = target.as_deref().unwrap_or_default();
                            match pages.answer_question(workspace_id, page, asked) {
                                Some(pending) => match pending.await {
                                    Some(Ok(patches)) => {
                                        sheet = Some(pages.question_sheet(workspace_id, page));
                                        PageAnswer::Patched(patches)
                                    }
                                    Some(Err(refusal)) => PageAnswer::Refused(refusal),
                                    None => PageAnswer::Refused(PageRefusal::Unavailable(
                                        "the question was dropped".to_owned(),
                                    )),
                                },
                                None => PageAnswer::Refused(PageRefusal::Unavailable(
                                    "no question is waiting".to_owned(),
                                )),
                            }
                        }
                        Some(_) if id == QUESTION_PATCH_ID => PageAnswer::Refused(
                            PageRefusal::Unavailable(
                                "this device answers no questions".to_owned(),
                            ),
                        ),
                        Some((workspace_id, page)) => patched(
                            pages
                                .act(
                                    workspace_id,
                                    page,
                                    Some((id, target, action)),
                                    signed,
                                    None,
                                )
                                .await,
                        ),
                        None => PageAnswer::Refused(PageRefusal::Unavailable(
                            "no page is open".to_owned(),
                        )),
                    },
                }
            }
            _ = follow.tick(), if open.is_some() => {
                let Some((workspace_id, page)) = open else { continue };
                let drawn = answers.then(|| pages.question_sheet(workspace_id, page));
                match drawn {
                    // A question came or went: the sheet is patched where it
                    // stands over the app.
                    Some(drawn) if sheet.as_deref() != Some(drawn.as_str()) => {
                        sheet = Some(drawn.clone());
                        PageAnswer::Changed(vec![PagePatch {
                            id: QUESTION_PATCH_ID,
                            html: drawn,
                        }])
                    }
                    _ => match pages.act(workspace_id, page, None, None, None).await {
                        Some(Ok(patches)) if patches.is_empty() => continue,
                        Some(Ok(patches)) => PageAnswer::Changed(patches),
                        // A pass the page did not ask for answers nothing: the
                        // page's own asks keep their order, and the next pass
                        // answers what changed. A page that left hears it on its
                        // own next ask.
                        answered => {
                            tracing::debug!(?answered, "a page's pass was not answered");
                            continue;
                        }
                    },
                }
            }
        };
        if write(&mut send, &answer).await.is_err() {
            break;
        }
    }
    reader.abort();
    if let Some((workspace_id, page)) = open {
        pages.close(workspace_id, page);
    }
}

/// Open the page `address` names for `person`: its workspace, its number and
/// the page whole. A workspace whose keys are on their way is said to be
/// loading first.
async fn open_page<W, F, Fut>(
    pages: &dyn AppPages,
    person: &Caller,
    ready: &F,
    address: PageAddress,
    signed: Option<Box<tilo_actions::remote::SignedCall>>,
    send: &mut W,
) -> Result<(WorkspaceId, u64, PageAnswer), PageRefusal>
where
    W: AsyncWrite + Unpin + Send,
    F: Fn(WorkspaceId, String) -> Fut + Sync,
    Fut: Future<Output = Result<bool, PageRefusal>> + Send,
{
    let workspace_id = WorkspaceId::parse(&address.workspace).ok_or(PageRefusal::Elsewhere)?;
    if ready(workspace_id, address.app.clone()).await? {
        // The keys arrive on their own; the open below waits for them.
        let _ = write(send, &PageAnswer::Loading).await;
    }
    let caller = person.clone().in_workspace(workspace_id.to_string());
    let opened = pages
        .open(workspace_id, &caller, address, signed, None)
        .await
        .unwrap_or_else(|| Err(PageRefusal::Unavailable("the page was dropped".to_owned())))?;
    let page = opened.page;
    Ok((
        workspace_id,
        page,
        PageAnswer::Shown {
            title: opened.title,
            css: opened.css,
            body: opened.body,
        },
    ))
}

fn patched(
    answered: Option<Result<Vec<tilo_server_protocol::page::PagePatch>, PageRefusal>>,
) -> PageAnswer {
    match answered {
        Some(Ok(patches)) => PageAnswer::Patched(patches),
        Some(Err(refusal)) => PageAnswer::Refused(refusal),
        None => PageAnswer::Refused(PageRefusal::Unavailable("the page was dropped".to_owned())),
    }
}

async fn write<W: AsyncWrite + Unpin>(send: &mut W, answer: &PageAnswer) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(answer).map_err(std::io::Error::other)?;
    write_frame(send, &bytes).await
}

/// The question sheet a page draws over its app: the card a person answers in,
/// in the theme's colours, over the app's content.
const SHEET_CSS: &str = "\
.tilo-question-sheet{position:fixed;inset:0;display:grid;place-items:center;z-index:20;\
background:color-mix(in srgb,var(--tilo-ground,#000) 55%,transparent)}\
.tilo-question-sheet[hidden]{display:none}\
.tilo-question-card{max-width:32rem;margin:1rem;padding:1.25rem 1.5rem;border-radius:var(--tilo-radius,12px);\
background:var(--tilo-overlay,#1b1c20);color:var(--tilo-text,#f4f4f5);\
box-shadow:0 1rem 3rem rgba(0,0,0,.4)}\
.tilo-question-card h2{margin:0 0 .5rem;font-size:1.1rem;font-weight:600}\
.tilo-question-heading{margin:0 0 .5rem;font-weight:500;opacity:.85}\
.tilo-question-detail{margin:0 0 .75rem}\
.tilo-question-facts{display:grid;grid-template-columns:auto 1fr;gap:.25rem .75rem;margin:0 0 .75rem}\
.tilo-question-facts dt{opacity:.7}\
.tilo-question-facts dd{margin:0;overflow-wrap:anywhere}\
.tilo-question-caution{margin:0 0 .75rem;opacity:.75}\
.tilo-question-actions{display:flex;gap:.5rem;justify-content:flex-end}\
.tilo-question-actions button{font:inherit;padding:.45rem .9rem;border-radius:calc(var(--tilo-radius,12px)/1.5);\
border:1px solid color-mix(in srgb,currentColor 25%,transparent);background:transparent;color:inherit;cursor:pointer}\
.tilo-question-actions button:last-child{background:var(--tilo-accent,#5b6cff);color:var(--tilo-on-accent,#fff);border-color:transparent}";

/// What a page's address answers: [`document`], kept by no cache, since the
/// script in it is this server's.
pub(super) fn answer(
    address: &PageAddress,
    link: Option<&tilo_server_protocol::web::WebLink>,
    peer: Option<&str>,
) -> hyper::Response<super::Body> {
    let mut response = super::reply(
        hyper::StatusCode::OK,
        document(address, link, peer).into_bytes(),
    );
    let headers = response.headers_mut();
    headers.insert(
        hyper::header::CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(
        hyper::header::CACHE_CONTROL,
        hyper::header::HeaderValue::from_static("no-cache"),
    );
    response
}

/// The document a page's address answers: its title, a stylesheet for the
/// moment before the page arrives, and the script with where to reach this
/// server. Nothing in it is the person's: the page itself arrives once their
/// device proved itself.
pub(super) fn document(
    address: &PageAddress,
    link: Option<&tilo_server_protocol::web::WebLink>,
    peer: Option<&str>,
) -> String {
    let link = link.map(|link| {
        serde_json::json!({
            "origin": link.origin,
            "path": LINK_PAGE_PATH,
            "certificate_sha256": link.certificate_sha256.map(|hash| hash.to_vec()),
        })
    });
    let config = serde_json::json!({
        "link": link,
        "peer": peer,
        "protocol": tilo_server_protocol::auth::PEER_PROTOCOL_VERSION,
        "address": address,
    });
    // `</` never closes the script early: JSON escapes it.
    let config = config.to_string().replace("</", "<\\/");
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{app}</title><style id=\"tilo-theme\">:root{{color-scheme:light dark}}html,body{{margin:0;height:100%}}body{{font-family:system-ui,-apple-system,\"Segoe UI\",sans-serif}}#tilo-page[data-state]{{display:grid;place-items:center;height:100dvh;color:GrayText}}#tilo-page[data-working]{{cursor:progress}}#tilo-page[data-working]::before{{content:\"\";position:fixed;inset:0 0 auto;height:2px;z-index:9;background:var(--tilo-accent,Highlight);animation:tilo-working 1s ease-in-out infinite alternate}}@keyframes tilo-working{{from{{opacity:.15}}to{{opacity:.7}}}}#tilo-sign-in{{position:fixed;inset:auto 16px 16px;z-index:20;align-items:center;gap:12px;padding:12px 16px;border-radius:12px;background:Canvas;color:CanvasText;box-shadow:0 8px 30px rgb(0 0 0 / .25)}}#tilo-sign-in:not([hidden]){{display:flex}}#tilo-sign-in p{{margin:0;flex:1}}#tilo-sign-in button{{font:inherit;padding:6px 14px;border-radius:8px;border:1px solid ButtonBorder;background:ButtonFace;color:ButtonText;cursor:pointer}}#tilo-sign-in button:hover{{background:Highlight;color:HighlightText}}</style><style id=\"tilo-page-css\"></style><style id=\"tilo-sheet-css\">{sheet_css}</style></head><body><div id=\"tilo-page\" data-state=\"opening\"></div><script type=\"application/json\" id=\"tilo-page-config\">{config}</script><script type=\"module\">{SCRIPT}</script></body></html>",
        app = ui_title(&address.app),
        sheet_css = SHEET_CSS,
    )
}

/// The app's name as a title before its page says its own: `notes` → `Notes`.
/// An address naming no app is the workspace's front page, which the product
/// names.
fn ui_title(app: &str) -> String {
    if app.is_empty() {
        return "Tilo".to_owned();
    }
    let mut chars = app.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(test)]
mod tests;
