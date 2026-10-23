use super::NativeDocumentReadContext;
use crate::collab_broker_capnp::delegate_browser_service_session_command;
use crate::services::{
    BrowserServiceSessionRequestContext, BrowserServiceSessionServices, HostServices,
};
use runtime_collab::{BrowserServiceSessionCommand, BrowserServiceSessionReply};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

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
        &self,
        _: &BrowserServiceSessionRequestContext,
        _: BrowserServiceSessionCommand,
    ) -> Result<BrowserServiceSessionReply, String> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        if self.revoke {
            self.live.store(false, Ordering::Release);
        }
        Ok(BrowserServiceSessionReply::List { sessions: vec![] })
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
    };
    tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
    let read = NativeDocumentReadContext {
        route: crate::collab_broker::CrdtRoute::record(
            app,
            "7851d1ef-c4fa-4d23-aa65-5ad85f80a6af",
            "record",
        ),
        certificate,
        record_kind: None,
        creation: None,
        revalidate: Arc::new(move || {
            live.load(Ordering::Acquire)
                .then_some(())
                .ok_or_else(|| "synthetic-private-provider-error".into())
        }),
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
        &self,
        _: &BrowserServiceSessionRequestContext,
        command: BrowserServiceSessionCommand,
    ) -> Result<BrowserServiceSessionReply, String> {
        let BrowserServiceSessionCommand::Forget { id, setup_binding } = command else {
            return Err("Expected Forget".into());
        };
        self.calls.fetch_add(1, Ordering::AcqRel);
        if self.revoke {
            self.admin.store(false, Ordering::Release);
        }
        Ok(BrowserServiceSessionReply::Forgotten { id, setup_binding })
    }
    fn disconnect(&self, _: u64) {}
}

#[test]
fn browser_forget_delegation_rechecks_admin_before_mutation_and_acknowledgement() {
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
        let services = HostServices {
            browser_service_sessions: Some(service.clone()),
            ..Default::default()
        };
        let result = delegate_browser_service_session_command(
            &services,
            &context("so.tilo.browser", live.clone()),
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

#[test]
fn browser_forget_cannot_use_read_only_provider_authority() {
    let live = Arc::new(AtomicBool::new(true));
    let service = Arc::new(Service {
        live: live.clone(),
        calls: AtomicUsize::new(0),
        revoke: false,
    });
    let services = HostServices {
        browser_service_sessions: Some(service.clone()),
        ..Default::default()
    };
    let command = BrowserServiceSessionCommand::Forget {
        id: "selected-slack".into(),
        setup_binding: "operation".into(),
    }
    .encode()
    .unwrap();
    assert!(
        delegate_browser_service_session_command(
            &services,
            &context("so.tilo.browser", live),
            &command,
        )
        .is_err()
    );
    assert_eq!(service.calls.load(Ordering::Acquire), 0);
}

#[test]
fn browser_private_delegation_checks_route_and_post_provider_custody() {
    let live = Arc::new(AtomicBool::new(true));
    let service = Arc::new(Service {
        live: live.clone(),
        calls: AtomicUsize::new(0),
        revoke: false,
    });
    let services = HostServices {
        browser_service_sessions: Some(service.clone()),
        ..Default::default()
    };
    let command = BrowserServiceSessionCommand::List.encode().unwrap();
    assert!(
        delegate_browser_service_session_command(
            &services,
            &context("so.tilo.files", live.clone()),
            &command
        )
        .is_err()
    );
    assert_eq!(service.calls.load(Ordering::Acquire), 0);
    let reply = delegate_browser_service_session_command(
        &services,
        &context("so.tilo.browser", live.clone()),
        &command,
    )
    .unwrap();
    assert!(
        BrowserServiceSessionReply::decode(&reply)
            .unwrap()
            .matches(&BrowserServiceSessionCommand::List)
    );
    assert_eq!(service.calls.load(Ordering::Acquire), 1);
    assert!(
        delegate_browser_service_session_command(
            &services,
            &context("so.tilo.browser", live.clone()),
            br#"{"kind":"list","person":"foreign"}"#
        )
        .is_err()
    );
    assert_eq!(service.calls.load(Ordering::Acquire), 1);
    let services = HostServices {
        browser_service_sessions: Some(Arc::new(Service {
            live: live.clone(),
            calls: AtomicUsize::new(0),
            revoke: true,
        })),
        ..Default::default()
    };
    let error = delegate_browser_service_session_command(
        &services,
        &context("so.tilo.browser", live),
        &command,
    )
    .unwrap_err();
    assert!(
        !error
            .to_string()
            .contains("synthetic-private-provider-error")
    );
    assert!(error.to_string().contains("request refused"));
}
