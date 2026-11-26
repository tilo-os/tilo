//! The signed page stream used by hosted app tests.

use super::{Device, Hosted};
use tilo_net::webtransport;
use tilo_server_client::frame::{read_frame, write_frame};
use tilo_server_client::link::{Link, LinkRecv, LinkSend};
use tilo_server_protocol::page::{
    MAX_PAGE_FRAME_BYTES, PageAction, PageAddress, PageAnswer, PageAsk, PageRefusal,
};
use tilo_server_protocol::web::LINK_PAGE_PATH;
use tilo_user_identity::RevocationList;

/// The same signed stream as the page script. The host pushes changes while no click is pending.
pub(super) struct SignedPage {
    exclude: Option<&'static str>,
    _link: Link,
    send: LinkSend,
    recv: LinkRecv,
}

impl SignedPage {
    pub(super) async fn open(
        hosted: &Hosted,
        device: &Device,
        host: &Device,
        workspace: &str,
        app: &str,
        show: &str,
    ) -> Self {
        let web = &hosted.web.link;
        let url = format!("{}{LINK_PAGE_PATH}", web.origin);
        let pinned = webtransport::Options::pinned(vec![web.certificate_sha256.unwrap()]);
        let link = Link::web(webtransport::connect(&url, pinned).await.unwrap());
        let (mut send, mut recv) = link.open_bi().await.unwrap();
        tilo_server::client::prove(
            &mut send,
            &mut recv,
            &device.cert,
            &device.key,
            &RevocationList::new(),
            host.cert.user_public_key,
            Some(host.cert.device_public_key),
        )
        .await
        .expect("the enrolled device signs the host's challenge");
        let mut page = Self {
            exclude: None,
            _link: link,
            send,
            recv,
        };
        page.ask(&PageAsk::Open(PageAddress {
            workspace: workspace.to_owned(),
            app: app.into(),
            show: Some(show.into()),
        }))
        .await;
        page
    }

    /// Reject the test secret even on intermediate patches.
    pub(super) fn excluding(mut self, secret: &'static str) -> Self {
        self.exclude = Some(secret);
        self
    }

    pub(super) async fn ask(&mut self, ask: &PageAsk) {
        write_frame(&mut self.send, &serde_json::to_vec(ask).unwrap())
            .await
            .unwrap();
    }

    /// One decoded answer from the signed stream, including a response with no patches.
    pub(super) async fn next_answer(&mut self) -> PageAnswer {
        let bytes = read_frame(&mut self.recv, MAX_PAGE_FRAME_BYTES)
            .await
            .unwrap();
        let frame = std::str::from_utf8(&bytes).unwrap();
        if let Some(secret) = self.exclude {
            assert!(!frame.contains(secret), "the page sent the test secret");
        }
        serde_json::from_str(frame).unwrap()
    }

    /// Inspect every frame, including the initial page and the changes the worker publishes later.
    pub(super) async fn until<T>(
        &mut self,
        waiting_for: &str,
        mut find: impl FnMut(&str) -> Option<T>,
    ) -> Result<T, PageRefusal> {
        tilo_test_env::within(waiting_for, async {
            loop {
                match self.next_answer().await {
                    PageAnswer::Shown { body, .. } => {
                        if let Some(value) = find(&body) {
                            return Ok(value);
                        }
                    }
                    PageAnswer::Changed(patches) | PageAnswer::Patched(patches) => {
                        if let Some(value) = patches.iter().find_map(|patch| find(&patch.html)) {
                            return Ok(value);
                        }
                    }
                    PageAnswer::Refused(refusal) => return Err(refusal),
                    PageAnswer::Loading => {}
                }
            }
        })
        .await
    }
}

/// Follow the action and target supplied by the rendered control, just as page.mjs does.
pub(super) fn control(html: &str, marks: &[&str], typed: Option<&str>) -> Option<PageAsk> {
    html.split('<').find_map(|part| {
        let tag = part.split('>').next()?;
        if !marks.iter().all(|mark| tag.contains(mark)) {
            return None;
        }
        let attr = |name: &str| tag.split_once(&format!(" {name}=\""))?.1.split('"').next();
        let action = match attr("data-a")? {
            "activate" => PageAction::Activate,
            "value" => PageAction::Value(typed.or_else(|| attr("data-v"))?.to_owned()),
            _ => return None,
        };
        Some(PageAsk::Act {
            id: attr("data-p")?.parse().ok()?,
            target: attr("data-t").map(str::to_owned),
            action,
        })
    })
}

/// The numbered button surrounding this visible caption.
pub(super) fn button(html: &str, words: &str) -> Option<PageAsk> {
    let (before, _) = html.split_once(&format!(">{words}<"))?;
    control(
        &before[before.rfind("<button")?..],
        &["data-a=\"activate\""],
        None,
    )
}
