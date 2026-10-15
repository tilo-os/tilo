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

#[cfg(not(all(unix, feature = "native")))]
#[test]
fn launcher_is_explicitly_unavailable_without_platform_support() {
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
