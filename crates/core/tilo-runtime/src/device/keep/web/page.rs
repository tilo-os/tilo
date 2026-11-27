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
//! A page is shown only where this server reads the workspace: it hosts it
//! and holds its keys, or holds it unsealed. While the keys are on their way
//! the page is told it is loading; a server that only stores the workspace's
//! ciphertext refuses it ([`PageRefusal::StoredCopy`]).

use std::future::Future;

use tilo_actions::remote::Caller;
use tilo_server_client::frame::{read_frame_or_eof, write_frame};
use tilo_server_protocol::page::{
    MAX_PAGE_FRAME_BYTES, PageAction, PageAddress, PageAnswer, PageAsk, PageCall, PageNotice,
    PageRefusal, PointerEvent,
};
use tilo_server_protocol::web::LINK_PAGE_PATH;
use tilo_user_identity::DeviceCert;
use tokio::io::{AsyncRead, AsyncWrite};

use tilo_server_protocol::WorkspaceId;
use tilo_workspace_service::app_serving::AppPages;
use tilo_workspace_service::service::ServerProtocol;

/// How often an open page asks what changed while its person does nothing: their edits reach it
/// within this, and a view whose call tiles settle far sooner
/// ([`AppPages::follow_every`](tilo_workspace_service::app_serving::AppPages::follow_every)).
use tilo_workspace_service::app_serving::FOLLOW_EVERY;

/// The page's script: the handshake, the session and what the person does.
const SCRIPT: &str = include_str!("page/page.mjs");

/// Whether the workspace's documents are readable here now: `Ok(true)` once
/// the keys on their way have arrived, `Ok(false)` when they are readable.
async fn readiness(
    workspaces: Option<&tilo_workspace_service::workspaces::Workspaces>,
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
    let person = tilo_workspace_service::service::local_caller(&device.cert);
    serve_pages(
        pages,
        (&person, &device.cert),
        |workspace_id, app| readiness(workspaces.as_deref(), workspace_id, app),
        recv,
        send,
    )
    .await;
    link.close("the page closed");
}

/// Answer what a page asks on its stream until it closes: the person of
/// `person`, on the device whose `certificate` the link verified, opens pages
/// and acts on the one open, and an open page hears what others changed on it.
/// `ready` says whether a workspace's documents are readable here
/// ([`readiness`]).
pub(crate) async fn serve_pages<R, W, F, Fut>(
    pages: &dyn AppPages,
    (person, certificate): (&Caller, &DeviceCert),
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
    // The notifications the person's devices showed when the page was last told, and whether
    // the last turn was the pass that asks for what changed: what others changed and what the
    // record of the person's devices holds now are read together.
    let mut notices: Vec<PageNotice> = Vec::new();
    let mut followed = false;
    // When the record of the person's devices was last read: a page whose call tiles settle is
    // followed far faster than its person's notifications change. It starts a pass behind, so the
    // page is told what the record holds as soon as it opens.
    let mut notices_read = std::time::Instant::now()
        .checked_sub(FOLLOW_EVERY)
        .unwrap_or_else(std::time::Instant::now);
    // What wakes this stream when the page's view asks for a call step: the browser carries
    // the call, and hears of a step the moment the view asks for it.
    let calls = pages.calls_waker();
    loop {
        // How soon this page is asked what changed: a view whose call tiles settle is followed
        // closely while they do, one showing nothing live every couple of seconds.
        let follow_in = open.map_or(FOLLOW_EVERY, |(_, page)| pages.follow_every(page));
        let asked_for_a_call = async {
            match &calls {
                Some(calls) => calls.notified().await,
                None => std::future::pending::<()>().await,
            }
        };
        let answer: Option<PageAnswer> = tokio::select! {
            frame = asks.recv() => {
                let Some(ask) = frame.and_then(|frame| serde_json::from_slice::<PageAsk>(&frame).ok()) else {
                    break;
                };
                answer_ask(
                    pages,
                    (person, certificate),
                    &ready,
                    &mut open,
                    &mut notices,
                    &mut send,
                    ask,
                )
                .await
            }
            () = tokio::time::sleep(follow_in), if open.is_some() => {
                followed = true;
                let Some((workspace_id, page)) = open else { continue };
                match pages.act(workspace_id, page, None).await {
                    Some(Ok(patches)) if patches.is_empty() => None,
                    Some(Ok(patches)) => Some(PageAnswer::Changed(patches)),
                    // A pass the page did not ask for answers nothing: the
                    // page's own asks keep their order, and the next pass
                    // answers what changed. A page that left hears it on its
                    // own next ask.
                    answered => {
                        tracing::debug!(?answered, "a page's pass was not answered");
                        None
                    }
                }
            }
            () = asked_for_a_call, if open.is_some() => None,
        };
        if let Some(answer) = answer
            && write(&mut send, &answer).await.is_err()
        {
            break;
        }
        // What the person's devices show now, read from the record they all share: a page raised
        // as a browser notification while it is not in front, and taken away when the record no
        // longer shows it — the person dismissed it on a device of theirs, or its time ran out.
        if followed && open.is_some() && notices_read.elapsed() >= FOLLOW_EVERY {
            notices_read = std::time::Instant::now();
            let now_ms = i64::try_from(tilo_clock::unix_now_ms()).unwrap_or(i64::MAX);
            if let Some(shown) = pages.notices(person, now_ms).await
                && shown != notices
            {
                notices.clone_from(&shown);
                if write(&mut send, &PageAnswer::Notices(shown)).await.is_err() {
                    break;
                }
            }
        }
        followed = false;
        // Every step the view asked for since the browser was last given one: the page's own
        // answer to the press that asked for it is already on its way.
        if let Some((workspace_id, page)) = open {
            let mut steps = pages.take_calls(workspace_id, page);
            steps.sort_by_key(|(request_id, _)| *request_id);
            let mut closed = false;
            for (request_id, command) in steps {
                let call = PageAnswer::Call(PageCall {
                    request_id,
                    command,
                });
                if write(&mut send, &call).await.is_err() {
                    closed = true;
                    break;
                }
            }
            if closed {
                break;
            }
        }
    }
    reader.abort();
    if let Some((workspace_id, page)) = open {
        pages.close(workspace_id, page);
    }
}

/// Answer one ask from the page.
async fn answer_ask<W, F, Fut>(
    pages: &dyn AppPages,
    (person, certificate): (&Caller, &DeviceCert),
    ready: &F,
    open: &mut Option<(WorkspaceId, u64)>,
    notices: &mut Vec<PageNotice>,
    send: &mut W,
    ask: PageAsk,
) -> Option<PageAnswer>
where
    W: AsyncWrite + Unpin + Send,
    F: Fn(WorkspaceId, String) -> Fut + Sync,
    Fut: Future<Output = Result<bool, PageRefusal>> + Send,
{
    match ask {
        PageAsk::Open(address) => {
            if let Some((workspace_id, page)) = open.take() {
                pages.close(workspace_id, page);
            }
            // Another page of the same browser: what it was told of the person's
            // notifications is told again for the one it opens now.
            notices.clear();
            let opened = open_page(pages, (person, certificate), ready, address, send).await;
            if let Ok((workspace_id, page, _)) = &opened {
                *open = Some((*workspace_id, *page));
            }
            Some(match opened {
                Ok((_, _, answer)) => answer,
                Err(refusal) => {
                    tracing::debug!(?refusal, "a page was refused");
                    PageAnswer::Refused(refusal)
                }
            })
        }
        PageAsk::Act {
            id,
            target,
            action,
            signed,
        } => Some(answer_act(pages, *open, id, target, action, signed).await),
        // The browser carried a call step the page's view asked for: an answer to
        // an answer, which the page is not told again.
        PageAsk::CallResult { request_id, answer } => {
            if let Some((_, page)) = *open {
                pages.answer_call(page, request_id, answer);
            }
            None
        }
        PageAsk::Appearance { dark } => Some(answer_appearance(pages, *open, dark).await),
        PageAsk::Pointer(event) => Some(answer_pointer(pages, *open, event).await),
        // The person dismissed a notification the record of their devices holds: it
        // is answered there, so it leaves every device of theirs.
        PageAsk::AnswerNotice { id } => {
            if let (Some(_), Some(device)) = (*open, person.device_public_key()) {
                pages.answer_notice(person, device, &id).await;
            }
            None
        }
    }
}

/// What the person did on the open page: `action` on the element numbered `id`.
async fn answer_act(
    pages: &dyn AppPages,
    open: Option<(WorkspaceId, u64)>,
    id: u32,
    target: Option<String>,
    action: PageAction,
    signed: Option<Box<tilo_actions::remote::SignedCall>>,
) -> PageAnswer {
    match open {
        Some((workspace_id, page)) => patched(
            pages
                .act(
                    workspace_id,
                    page,
                    Some(tilo_workspace_service::app_serving::PageAct {
                        id,
                        target,
                        action,
                        signed,
                    }),
                )
                .await,
        ),
        None => PageAnswer::Refused(PageRefusal::Unavailable("no page is open".to_owned())),
    }
}

/// The browser asked for the look it runs in: the page is drawn in it, as a
/// window follows the system it runs on.
async fn answer_appearance(
    pages: &dyn AppPages,
    open: Option<(WorkspaceId, u64)>,
    dark: bool,
) -> PageAnswer {
    match open {
        Some((workspace_id, page)) => match pages.appearance(workspace_id, page, dark).await {
            Some(Ok(Some(opened))) => PageAnswer::Shown {
                title: opened.title,
                css: opened.css,
                body: opened.body,
            },
            Some(Ok(None)) | None => PageAnswer::Patched(Vec::new()),
            Some(Err(refusal)) => PageAnswer::Refused(refusal),
        },
        // Nothing is shown yet: no page's look changes.
        None => PageAnswer::Patched(Vec::new()),
    }
}

/// The pointer moved on the page: the gesture is the view's, carried into its
/// widgets as a device's pointer is, and the page hears what it changed.
async fn answer_pointer(
    pages: &dyn AppPages,
    open: Option<(WorkspaceId, u64)>,
    event: PointerEvent,
) -> PageAnswer {
    match open {
        Some((workspace_id, page)) => patched(pages.pointer(workspace_id, page, event).await),
        None => PageAnswer::Refused(PageRefusal::Unavailable("no page is open".to_owned())),
    }
}

/// Open the page `address` names for `person`: its workspace, its number and
/// the page whole. A workspace whose keys are on their way is said to be
/// loading first.
async fn open_page<W, F, Fut>(
    pages: &dyn AppPages,
    (person, certificate): (&Caller, &DeviceCert),
    ready: &F,
    address: PageAddress,
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
        .open(workspace_id, (&caller, certificate), address)
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

/// Where a page's face is served from: `/fonts/<file>`, the address the shared page stylesheet's
/// `@font-face` rules name.
const FONTS_PREFIX: &str = "/fonts/";

/// One of the catalog's web cuts of the page's face, by the path the page's stylesheet names it
/// at, or `None` for a path that names none. A browser has no Tilo Sans of its own; the page's
/// host serves it from its own origin, so no font host learns who opened a page.
pub(super) fn font(path: &str) -> Option<hyper::Response<super::Body>> {
    let file = path.strip_prefix(FONTS_PREFIX)?;
    let &(_, bytes) = tilo_font_catalog::web::CUTS
        .iter()
        .find(|(name, _)| *name == file)?;
    let mut response =
        hyper::Response::new(super::Body::Full(Some(bytes::Bytes::from_static(bytes))));
    let headers = response.headers_mut();
    headers.insert(
        hyper::header::CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("font/woff2"),
    );
    // The name stays when a rebuilt cut replaces the bytes, so a week, not forever: a page drawn
    // in last week's cut of the same face is no harm.
    headers.insert(
        hyper::header::CACHE_CONTROL,
        hyper::header::HeaderValue::from_static("public, max-age=604800"),
    );
    Some(response)
}

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
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{app}</title><style id=\"tilo-theme\">:root{{color-scheme:light dark}}html,body{{margin:0;height:100%}}body{{font-family:system-ui,-apple-system,\"Segoe UI\",sans-serif}}#tilo-page[data-state]{{display:grid;place-items:center;height:100dvh;color:GrayText}}#tilo-page[data-working]{{cursor:progress}}#tilo-page[data-working]::before{{content:\"\";position:fixed;inset:0 0 auto;height:2px;z-index:9;background:var(--tilo-accent,Highlight);animation:tilo-working 1s ease-in-out infinite alternate}}@keyframes tilo-working{{from{{opacity:.15}}to{{opacity:.7}}}}#tilo-sign-in{{position:fixed;inset:auto 16px 16px;z-index:20;align-items:center;gap:12px;padding:12px 16px;border-radius:12px;background:Canvas;color:CanvasText;box-shadow:0 8px 30px rgb(0 0 0 / .25)}}#tilo-sign-in:not([hidden]){{display:flex}}#tilo-sign-in p{{margin:0;flex:1}}#tilo-sign-in button{{font:inherit;padding:6px 14px;border-radius:8px;border:1px solid ButtonBorder;background:ButtonFace;color:ButtonText;cursor:pointer}}#tilo-sign-in button:hover{{background:Highlight;color:HighlightText}}</style><style id=\"tilo-page-css\"></style></head><body><div id=\"tilo-page\" data-state=\"opening\"></div><script type=\"application/json\" id=\"tilo-page-config\">{config}</script><script type=\"module\">{SCRIPT}</script></body></html>",
        app = ui_title(&address.app),
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
