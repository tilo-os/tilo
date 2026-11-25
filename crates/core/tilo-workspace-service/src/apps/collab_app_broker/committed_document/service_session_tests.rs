use super::NativeDocumentReadContext;
use crate::apps::BrokerError;
use crate::apps::collab_app_broker::{
    delegate_browser_service_session_command, item_use::delegate_item_use, wire::Answer,
};
use crate::apps::services::{
    BrowserServiceSessionRequestContext, BrowserServiceSessionServices, ItemAnswering,
    ProcessServices,
};
use runtime_collab::{BrowserServiceSessionCommand, BrowserServiceSessionReply, ItemField};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

/// A vault item's reference, made up.
const ITEM: &str = "vault.item:0123456789abcdef0123456789abcdef";
/// What the made-up item keeps: nobody's password, and it says so.
const MADE_UP_TEXT: &str = "made-up item text, nobody's password";
const BROWSER: &str = "so.tilo.browser";

/// What whoever keeps the vault answers an item's use, once a test says what.
type Decided = Arc<Mutex<Option<Result<BrowserServiceSessionReply, String>>>>;

/// The answer to an item's use that is not there until a test puts it there.
struct Later(Decided);
impl Future for Later {
    type Output = Result<BrowserServiceSessionReply, String>;
    fn poll(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.0
            .lock()
            .unwrap()
            .take()
            .map_or(std::task::Poll::Pending, std::task::Poll::Ready)
    }
}

/// Whoever keeps the vault, stood in for: it counts the item uses it was asked for and
/// answers each when a test says what.
struct Items {
    asked: AtomicUsize,
    /// Whether its leave for Browser is withdrawn.
    withdrawn: AtomicBool,
    decided: Decided,
}
impl BrowserServiceSessionServices for Items {
    fn authorize(&self, context: &BrowserServiceSessionRequestContext) -> Result<(), String> {
        context.revalidate()?;
        if self.withdrawn.load(Ordering::Acquire) {
            return Err("leave withdrawn".into());
        }
        Ok(())
    }
    fn command(
        self: Arc<Self>,
        _: &BrowserServiceSessionRequestContext,
        _: BrowserServiceSessionCommand,
    ) -> ItemAnswering {
        Box::pin(std::future::ready(Err(
            "an item's use is never answered on the dispatch".into(),
        )))
    }
    fn item_use(
        &self,
        _: &BrowserServiceSessionRequestContext,
        _: BrowserServiceSessionCommand,
    ) -> ItemAnswering {
        self.asked.fetch_add(1, Ordering::AcqRel);
        Box::pin(Later(self.decided.clone()))
    }
    fn disconnect(&self, _: u64) {}
}
fn items() -> Arc<Items> {
    Arc::new(Items {
        asked: AtomicUsize::new(0),
        withdrawn: AtomicBool::new(false),
        decided: Decided::default(),
    })
}
/// `bytes` from `app`'s connection, put to `service` as the connection's dispatch puts it.
fn put(
    service: &Arc<impl BrowserServiceSessionServices + 'static>,
    app: &str,
    live: &Arc<AtomicBool>,
    bytes: &[u8],
) -> Option<Result<Answer<zeroize::Zeroizing<Vec<u8>>>, BrokerError>> {
    let service: Arc<dyn BrowserServiceSessionServices> = service.clone();
    delegate_item_use(Some(&service), &context(app, live.clone()), bytes)
}
/// `bytes` from `app`'s connection to `services`, answered as the connection's dispatch answers
/// it: at once, or once the service answered what it answers later. Every stand-in here
/// answers without waiting.
fn dispatch(
    services: &ProcessServices,
    app: &str,
    live: Arc<AtomicBool>,
    bytes: &[u8],
) -> Result<zeroize::Zeroizing<Vec<u8>>, BrokerError> {
    let context = context(app, live);
    match delegate_item_use(services.browser_service_sessions.as_ref(), &context, bytes) {
        Some(answer) => now(&mut answer?).expect("a stand-in answers without waiting"),
        None => {
            let answer = delegate_browser_service_session_command(services, &context, bytes);
            match std::pin::pin!(answer)
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            {
                std::task::Poll::Ready(answered) => answered,
                std::task::Poll::Pending => panic!("a stand-in answers without waiting"),
            }
        }
    }
}
/// What `answer` is now, without waiting for it: `None` while nobody answered.
fn now(
    answer: &mut Answer<zeroize::Zeroizing<Vec<u8>>>,
) -> Option<Result<zeroize::Zeroizing<Vec<u8>>, BrokerError>> {
    match answer
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    {
        std::task::Poll::Ready(answered) => Some(answered),
        std::task::Poll::Pending => None,
    }
}

struct Service {
    live: Arc<AtomicBool>,
    calls: AtomicUsize,
    revoke: bool,
}
impl BrowserServiceSessionServices for Service {
    fn authorize(&self, context: &BrowserServiceSessionRequestContext) -> Result<(), String> {
        context.revalidate()
    }
    fn command(
        self: Arc<Self>,
        _: &BrowserServiceSessionRequestContext,
        _: BrowserServiceSessionCommand,
    ) -> ItemAnswering {
        Box::pin(std::future::ready(Err(
            "a website session is never answered on the dispatch".into(),
        )))
    }
    fn answered_later(
        &self,
        _: &BrowserServiceSessionRequestContext,
        _: BrowserServiceSessionCommand,
    ) -> ItemAnswering {
        self.calls.fetch_add(1, Ordering::AcqRel);
        if self.revoke {
            self.live.store(false, Ordering::Release);
        }
        Box::pin(std::future::ready(Ok(BrowserServiceSessionReply::List {
            sessions: vec![],
        })))
    }
    fn disconnect(&self, _: u64) {}
}
fn context(app: &str, live: Arc<AtomicBool>) -> BrowserServiceSessionRequestContext {
    let root = ed25519_dalek::SigningKey::from_bytes(&[21; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[22; 32]);
    let mut certificate = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Synthetic fixture".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
    let read = NativeDocumentReadContext {
        resource_type: None,
        route: crate::apps::collab_broker::CrdtRoute::record(
            app,
            "7851d1ef-c4fa-4d23-aa65-5ad85f80a6af",
            "record",
        ),
        certificate,
        revalidate: Arc::new(move || {
            live.load(Ordering::Acquire)
                .then_some(())
                .ok_or_else(|| "synthetic-private-provider-error".into())
        }),
        still_reads: super::reads_still(),
    };
    BrowserServiceSessionRequestContext::authenticated(41, "certified-person".into(), read)
}
struct AdminService {
    admin: AtomicBool,
    calls: AtomicUsize,
    revoke: bool,
}
impl BrowserServiceSessionServices for AdminService {
    fn authorize(&self, context: &BrowserServiceSessionRequestContext) -> Result<(), String> {
        context.revalidate()
    }
    fn authorize_command(
        &self,
        context: &BrowserServiceSessionRequestContext,
        command: &BrowserServiceSessionCommand,
    ) -> Result<(), String> {
        self.authorize(context)?;
        if matches!(command, BrowserServiceSessionCommand::Forget { .. })
            && !self.admin.load(Ordering::Acquire)
        {
            return Err("Admin withdrawn".into());
        }
        Ok(())
    }
    fn command(
        self: Arc<Self>,
        _: &BrowserServiceSessionRequestContext,
        _: BrowserServiceSessionCommand,
    ) -> ItemAnswering {
        Box::pin(std::future::ready(Err(
            "a website session is never answered on the dispatch".into(),
        )))
    }
    fn answered_later(
        &self,
        _: &BrowserServiceSessionRequestContext,
        command: BrowserServiceSessionCommand,
    ) -> ItemAnswering {
        let BrowserServiceSessionCommand::Forget { id, setup_binding } = command else {
            return Box::pin(std::future::ready(Err("Expected Forget".into())));
        };
        self.calls.fetch_add(1, Ordering::AcqRel);
        if self.revoke {
            self.admin.store(false, Ordering::Release);
        }
        Box::pin(std::future::ready(Ok(
            BrowserServiceSessionReply::Forgotten { id, setup_binding },
        )))
    }
    fn disconnect(&self, _: u64) {}
}

tilo_test_env::scenario! {
    fn browser_session_delegation_rechecks_authority;

    phase browser_forget_delegation_rechecks_admin_before_mutation_and_acknowledgement;
    {
        let live = Arc::new(AtomicBool::new(true));
        let command = BrowserServiceSessionCommand::Forget {
            id: "selected-slack".into(),
            setup_binding: "operation".into(),
        };
        for (admin, revoke, success, calls) in [
            (false, false, false, 0),
            (true, false, true, 1),
            (true, true, false, 1),
        ] {
            let service = Arc::new(AdminService {
                admin: AtomicBool::new(admin),
                calls: AtomicUsize::new(0),
                revoke,
            });
            let services = ProcessServices {
                browser_service_sessions: Some(service.clone()),
                ..Default::default()
            };
            let result = dispatch(
                &services,
                "so.tilo.browser",
                live.clone(),
                &command.encode().unwrap(),
            );
            assert_eq!(result.is_ok(), success);
            assert_eq!(service.calls.load(Ordering::Acquire), calls);
            if let Ok(bytes) = result {
                assert!(
                    BrowserServiceSessionReply::decode(&bytes)
                        .unwrap()
                        .matches(&command)
                );
            }
        }
    }

    phase browser_forget_cannot_use_read_only_provider_authority;
    {
        let live = Arc::new(AtomicBool::new(true));
        let service = Arc::new(Service {
            live: live.clone(),
            calls: AtomicUsize::new(0),
            revoke: false,
        });
        let services = ProcessServices {
            browser_service_sessions: Some(service.clone()),
            ..Default::default()
        };
        let command = BrowserServiceSessionCommand::Forget {
            id: "selected-slack".into(),
            setup_binding: "operation".into(),
        }
        .encode()
        .unwrap();
        assert!(dispatch(&services, "so.tilo.browser", live, &command).is_err());
        assert_eq!(service.calls.load(Ordering::Acquire), 0);
    }

    phase browser_private_delegation_checks_route_and_post_provider_ownership;
    {
        let live = Arc::new(AtomicBool::new(true));
        let service = Arc::new(Service {
            live: live.clone(),
            calls: AtomicUsize::new(0),
            revoke: false,
        });
        let services = ProcessServices {
            browser_service_sessions: Some(service.clone()),
            ..Default::default()
        };
        let command = BrowserServiceSessionCommand::List {}.encode().unwrap();
        assert!(dispatch(&services, "so.tilo.files", live.clone(), &command).is_err());
        assert_eq!(service.calls.load(Ordering::Acquire), 0);
        let reply = dispatch(&services, "so.tilo.browser", live.clone(), &command).unwrap();
        assert!(
            BrowserServiceSessionReply::decode(&reply)
                .unwrap()
                .matches(&BrowserServiceSessionCommand::List {})
        );
        assert_eq!(service.calls.load(Ordering::Acquire), 1);
        assert!(
            dispatch(
                &services,
                "so.tilo.browser",
                live.clone(),
                br#"{"kind":"list","person":"foreign"}"#
            )
            .is_err()
        );
        assert_eq!(service.calls.load(Ordering::Acquire), 1);
        let services = ProcessServices {
            browser_service_sessions: Some(Arc::new(Service {
                live: live.clone(),
                calls: AtomicUsize::new(0),
                revoke: true,
            })),
            ..Default::default()
        };
        let error = dispatch(&services, "so.tilo.browser", live, &command).unwrap_err();
        assert!(
            !error
                .to_string()
                .contains("synthetic-private-provider-error")
        );
        assert!(error.to_string().contains("access ended"));
    }

    phase an_items_use_is_waited_for_off_the_dispatch_and_answered_with_its_text_or_its_reason;
    {
        let invocation = tilo_actions::InvocationId::new();
        let command = BrowserServiceSessionCommand::UseItem {
            invocation,
            item: ITEM.into(),
            origin: "https://login.example.test".into(),
        };
        let bytes = command.encode().unwrap();
        let text = |invocation| BrowserServiceSessionReply::ItemText {
            invocation,
            text: zeroize::Zeroizing::new(MADE_UP_TEXT.to_owned()),
            into: ItemField::Password,
        };
        let carried = |answered: Option<Result<zeroize::Zeroizing<Vec<u8>>, BrokerError>>| {
            BrowserServiceSessionReply::decode(&answered.unwrap().unwrap()).unwrap()
        };
        // What a refused answer says, or nothing where it was not refused.
        let refusal = |answered: Option<Result<zeroize::Zeroizing<Vec<u8>>, BrokerError>>| {
            answered.and_then(Result::err).map(|error| error.to_string())
        };
        let live = Arc::new(AtomicBool::new(true));

        // Whether the session lends items, or what is no command, is answered at once.
        let stood_in = items();
        for other in [
            BrowserServiceSessionCommand::LendsItems {}.encode().unwrap().to_vec(),
            b"not a command".to_vec(),
        ] {
            assert!(put(&stood_in, BROWSER, &live, &other).is_none());
        }

        // An app that is not Browser is told so, and whoever keeps the vault is never asked.
        let mut answer = put(&stood_in, "so.tilo.mail", &live, &bytes)
            .unwrap()
            .unwrap();
        let told = carried(now(&mut answer));
        assert!(told.matches(&command));
        assert!(matches!(
            &told,
            BrowserServiceSessionReply::ItemRefused { reason, .. } if reason.contains("Only Browser")
        ));
        assert_eq!(stood_in.asked.load(Ordering::Acquire), 0);

        // Browser's ask comes back before anybody answered it: the answer waits, the
        // dispatch does not. Then the text arrives, for the page action that asked.
        let mut answer = put(&stood_in, BROWSER, &live, &bytes).unwrap().unwrap();
        assert_eq!(stood_in.asked.load(Ordering::Acquire), 1);
        assert!(now(&mut answer).is_none());
        assert!(now(&mut answer).is_none());
        *stood_in.decided.lock().unwrap() = Some(Ok(text(invocation)));
        let released = carried(now(&mut answer));
        assert!(released.matches(&command));
        assert!(matches!(
            &released,
            BrowserServiceSessionReply::ItemText { text, into: ItemField::Password, .. }
                if text.as_str() == MADE_UP_TEXT
        ));

        // A refusal's reason and a question's words come back as they were decided, also
        // where the service's leave for Browser was withdrawn meanwhile: only a text needs it.
        for (decided, said, withdrawn) in [
            (
                BrowserServiceSessionReply::item_refused(invocation, "Ada said no."),
                "Ada said no.",
                false,
            ),
            (
                BrowserServiceSessionReply::item_refused(invocation, "Nobody answered in time."),
                "Nobody answered in time.",
                true,
            ),
            (
                BrowserServiceSessionReply::item_asked(invocation, "Ada is asked first."),
                "Ada is asked first.",
                false,
            ),
        ] {
            let stood_in = items();
            let mut answer = put(&stood_in, BROWSER, &live, &bytes).unwrap().unwrap();
            stood_in.withdrawn.store(withdrawn, Ordering::Release);
            *stood_in.decided.lock().unwrap() = Some(Ok(decided));
            let told = carried(now(&mut answer));
            assert!(told.matches(&command));
            assert!(matches!(
                &told,
                BrowserServiceSessionReply::ItemRefused { reason: words, .. }
                    | BrowserServiceSessionReply::ItemAsked { words, .. }
                    if words == said
            ));
        }

        // A text that came leaves for nobody where the connection is gone, where the
        // service's leave for Browser was withdrawn while it was waited for, or where it
        // answers another page action. Nor does what a fault says leave.
        for case in 0..4 {
            let live = Arc::new(AtomicBool::new(true));
            let stood_in = items();
            let mut answer = put(&stood_in, BROWSER, &live, &bytes).unwrap().unwrap();
            let decided = match case {
                0 => {
                    live.store(false, Ordering::Release);
                    Ok(text(invocation))
                }
                1 => {
                    stood_in.withdrawn.store(true, Ordering::Release);
                    Ok(text(invocation))
                }
                2 => Ok(text(tilo_actions::InvocationId::new())),
                _ => Err(format!("a fault that says {MADE_UP_TEXT}")),
            };
            *stood_in.decided.lock().unwrap() = Some(decided);
            let said = refusal(now(&mut answer));
            assert!(
                said.as_deref().is_some_and(|said| {
                    said.contains("request refused") && !said.contains(MADE_UP_TEXT)
                }),
                "case {case}: a text or a fault's words left"
            );
        }

        // A connection that is gone is refused before anybody is asked.
        let gone = Arc::new(AtomicBool::new(false));
        let stood_in = items();
        assert!(put(&stood_in, BROWSER, &gone, &bytes).unwrap().is_err());
        assert_eq!(stood_in.asked.load(Ordering::Acquire), 0);

        // A service that has no vault items used says so, as an answer of its own.
        let without = Arc::new(Service {
            live: live.clone(),
            calls: AtomicUsize::new(0),
            revoke: false,
        });
        let mut answer = put(&without, BROWSER, &live, &bytes).unwrap().unwrap();
        let told = carried(now(&mut answer));
        assert!(told.matches(&command));
        assert!(matches!(
            &told,
            BrowserServiceSessionReply::ItemRefused { reason, .. } if reason.contains("no vault items")
        ));
    }
}

/// The desktop session's seat as the broker reaches it over the session's link: each call is
/// written to a socket, and the session's end of it, served on the thread that asked, writes the
/// answer back.
struct SeatOverSocket;
impl tilo_actions::remote::ActionProvider for SeatOverSocket {
    fn query(
        &self,
        _: &tilo_actions::remote::Caller,
        _: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>,
    > {
        tilo_actions::remote::Pending::ready(Err(
            tilo_actions::remote::ActionCallError::UnknownAction,
        ))
    }
    fn invoke(
        &self,
        _: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let (answer, answered) = tilo_actions::remote::pending();
        let invocation = request.invocation;
        let tilo_actions::remote::InvokeArgument::Json(arguments) = request.argument else {
            panic!("the seat is asked in JSON");
        };
        let (mut broker, mut session) = tokio::net::UnixStream::pair().unwrap();
        tokio::spawn(async move {
            let mut call = Vec::new();
            session.read_to_end(&mut call).await.unwrap();
            let call: serde_json::Value = serde_json::from_slice(&call).unwrap();
            let BrowserServiceSessionCommand::ReportAccountSync { account, .. } =
                serde_json::from_value(call["command"].clone()).unwrap()
            else {
                panic!("the test reports an account's sync");
            };
            let reply = BrowserServiceSessionReply::AccountSyncReported { account };
            session
                .write_all(&serde_json::to_vec(&reply).unwrap())
                .await
                .unwrap();
        });
        tokio::spawn(async move {
            broker
                .write_all(&serde_json::to_vec(&arguments).unwrap())
                .await
                .unwrap();
            broker.shutdown().await.unwrap();
            let mut reply = Vec::new();
            broker.read_to_end(&mut reply).await.unwrap();
            answer.complete(Ok(tilo_actions::remote::InvocationReceipt {
                invocation,
                status: tilo_actions::remote::ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput {
                        value: Some(serde_json::from_slice(&reply).unwrap()),
                        resources: Vec::new(),
                    },
                },
            }));
        });
        answered
    }
    fn observe(
        &self,
        _: &tilo_actions::remote::Caller,
        _: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }
    fn cancel(
        &self,
        _: &tilo_actions::remote::Caller,
        _: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        tilo_actions::remote::Pending::ready(Ok(()))
    }
}

/// An app's command the seat answers over a socket the asking thread serves itself is answered:
/// the broker awaits the seat, never holds the thread until the seat answered.
#[tokio::test(flavor = "current_thread")]
async fn a_seat_answer_over_the_asking_threads_socket_arrives() {
    let providers = crate::workspaces::providers::Providers::default();
    providers.serve_seat(Arc::new(SeatOverSocket), None);
    let seat = crate::apps::seat::Seat::new(providers, tokio::runtime::Handle::current());
    let services = ProcessServices {
        browser_service_sessions: Some(Arc::new(crate::apps::seat::SeatBrowserServiceSessions(
            seat,
        ))),
        ..Default::default()
    };
    let command = BrowserServiceSessionCommand::ReportAccountSync {
        account: "mail".into(),
        health: "healthy".into(),
        signed_in: false,
        message: "Synced".into(),
    };
    let context = context("so.tilo.settings", Arc::new(AtomicBool::new(true)));
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        delegate_browser_service_session_command(&services, &context, &command.encode().unwrap()),
    )
    .await
    .expect("the seat's answer arrives on the thread that serves its socket")
    .unwrap();
    assert!(
        BrowserServiceSessionReply::decode(&reply)
            .unwrap()
            .matches(&command)
    );
}
