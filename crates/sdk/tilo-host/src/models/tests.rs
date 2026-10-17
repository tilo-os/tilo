use super::*;
use std::future::Future;
use std::task::{Context, Poll, Waker};

// Rc deliberately proves that browser-host backends need not be Send.
struct Session(Rc<String>);
impl ModelBackend for Session {
    fn complete<'a>(&'a self, _: &'a ModelRequest) -> CompletionFuture<'a> {
        Box::pin(async move {
            Err(ModelError {
                kind: ErrorKind::PermissionDenied,
                message: (*self.0).clone(),
            })
        })
    }
}

fn request() -> ModelRequest {
    ModelRequest {
        messages: vec![Message::text(Role::User, "Hello")],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: 16,
    }
}

fn error(models: &Models) -> ModelError {
    let request = request();
    let mut future = std::pin::pin!(models.complete(&request));
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(Err(error)) => error,
        _ => panic!("fixture must resolve immediately with its session's error"),
    }
}

struct Administration;
impl ProviderAdmin for Administration {
    fn get_config(&self) -> agent_provider_config::management::AdminFuture<'_, ProviderSnapshot> {
        Box::pin(async { Err(ModelError::new(ErrorKind::PermissionDenied)) })
    }
    fn set_config(
        &self,
        _: String,
        _: agent_provider_config::ProviderConfig,
    ) -> agent_provider_config::management::AdminFuture<'_, ProviderSnapshot> {
        Box::pin(async { Err(ModelError::new(ErrorKind::PermissionDenied)) })
    }
    fn credential_status(
        &self,
        _: String,
    ) -> agent_provider_config::management::AdminFuture<'_, CredentialStatus> {
        Box::pin(async { Err(ModelError::new(ErrorKind::PermissionDenied)) })
    }
    fn set_api_key(
        &self,
        _: String,
        _: String,
    ) -> agent_provider_config::management::AdminFuture<'_, ()> {
        Box::pin(async { Err(ModelError::new(ErrorKind::PermissionDenied)) })
    }
    fn remove_api_key(&self, _: String) -> agent_provider_config::management::AdminFuture<'_, ()> {
        Box::pin(async { Err(ModelError::new(ErrorKind::PermissionDenied)) })
    }
    fn migrate_api_key(
        &self,
        _: String,
        _: String,
    ) -> agent_provider_config::management::AdminFuture<'_, ()> {
        Box::pin(async { Err(ModelError::new(ErrorKind::PermissionDenied)) })
    }
    fn remove_legacy_api_key(
        &self,
        _: String,
    ) -> agent_provider_config::management::AdminFuture<'_, ()> {
        Box::pin(async { Err(ModelError::new(ErrorKind::PermissionDenied)) })
    }
}

#[test]
fn administration_retains_only_its_own_host_authority() {
    use std::sync::Arc;
    let unavailable = ProviderAdministration::default();
    let first = ProviderAdministration::new(Arc::new(Administration));
    let retained = first.clone();
    let second = ProviderAdministration::new(Arc::new(Administration));
    assert!(!unavailable.is_available());
    assert!(matches!(
        unavailable.get(),
        Err(ModelError {
            kind: ErrorKind::Unavailable,
            ..
        })
    ));
    assert!(first.is_available());
    assert!(Arc::ptr_eq(&first.get().unwrap(), &retained.get().unwrap()));
    assert!(!Arc::ptr_eq(&first.get().unwrap(), &second.get().unwrap()));
    drop(first);
    assert!(retained.is_available());
}

#[cfg(not(feature = "native"))]
#[test]
fn launcher_is_explicitly_unavailable_without_platform_support() {
    assert!(matches!(
        ProviderAdministration::from_launcher(),
        Err(ModelError {
            kind: ErrorKind::Unavailable,
            ..
        })
    ));
    let Err(error) = Models::from_launcher() else {
        panic!("unsupported hosts must not discover launcher model access");
    };
    assert_eq!(error.kind, ErrorKind::Unavailable);
}

#[test]
fn unavailable_does_not_discover_another_hosts_session() {
    let unavailable = Models::default();
    let first = Models::new(Rc::new(Session(Rc::new("session one".into()))));
    let retained = first.clone();
    let second = Models::new(Rc::new(Session(Rc::new("session two".into()))));
    drop(first);
    assert!(!unavailable.is_available());
    assert!(retained.is_available());
    assert_eq!(error(&unavailable).kind, ErrorKind::Unavailable);
    assert_eq!(error(&retained).message, "session one");
    assert_eq!(error(&second).message, "session two");
}
