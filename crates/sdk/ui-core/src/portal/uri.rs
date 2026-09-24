//! Original-caller custody for asynchronous desktop URI effects.
use crate::invocation_scope::ResourceEffectScope;
use std::sync::mpsc::Receiver;

/// Dispatch one URI effect without blocking the UI thread. The original
/// admitted caller is rechecked on the worker immediately before the backend.
/// Apps retain their own URL policy (for example, allowed link schemes).
pub fn open_uri_scoped(uri: String, scope: ResourceEffectScope) -> Receiver<Result<(), String>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    #[cfg(any(test, feature = "test-support"))]
    let backend = SCOPED_URI_FIXTURE.with(|current| current.borrow().clone());
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (uri, scope);
        let _ = sender.send(Err("opening URI unavailable in this host".into()));
    }
    #[cfg(not(target_arch = "wasm32"))]
    super::spawn_worker("scoped-open-uri", move || {
        let outcome = scope.with_effect(|| {
            #[cfg(any(test, feature = "test-support"))]
            if let Some(backend) = backend {
                return backend(&uri);
            }
            if super::suppressed() {
                return Err("opening URI suppressed in this host".into());
            }
            open_on_platform(&uri)
        });
        let _ = sender.send(outcome);
    });
    receiver
}

#[cfg(all(feature = "portal", unix, not(target_arch = "wasm32")))]
fn open_on_platform(uri: &str) -> Result<(), String> {
    let uri = url::Url::parse(uri).map_err(|error| format!("bad URI: {error}"))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("portal runtime: {error}"))?;
    runtime.block_on(async {
        ashpd::desktop::open_uri::OpenFileRequest::default()
            .send_uri(&uri)
            .await
            .map_err(|error| error.to_string())?
            .response()
            .map_err(|error| error.to_string())
    })
}

#[cfg(all(not(unix), not(target_arch = "wasm32")))]
fn open_on_platform(uri: &str) -> Result<(), String> {
    crate::widget::try_open_external_url(uri).map_err(|error| error.to_string())
}

#[cfg(all(unix, not(feature = "portal"), not(target_arch = "wasm32")))]
fn open_on_platform(_: &str) -> Result<(), String> {
    Err("opening URI requires the portal feature in this host".into())
}

#[cfg(any(test, feature = "test-support"))]
type ScopedUriBackend = std::sync::Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;
#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static SCOPED_URI_FIXTURE: std::cell::RefCell<Option<ScopedUriBackend>> = const { std::cell::RefCell::new(None) };
}
#[cfg(any(test, feature = "test-support"))]
pub struct ScopedUriFixture {
    _owner_thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
/// Select a backend only for requests dispatched on this test UI thread.
/// The real original-caller check runs before the captured backend is called.
#[cfg(any(test, feature = "test-support"))]
pub fn scoped_uri_fixture(backend: ScopedUriBackend) -> Result<ScopedUriFixture, String> {
    SCOPED_URI_FIXTURE.with(|current| {
        let mut current = current.borrow_mut();
        if current.is_some() {
            return Err("this UI thread already owns a URI fixture".into());
        }
        *current = Some(backend);
        Ok(ScopedUriFixture {
            _owner_thread: std::marker::PhantomData,
        })
    })
}
#[cfg(any(test, feature = "test-support"))]
impl Drop for ScopedUriFixture {
    fn drop(&mut self) {
        SCOPED_URI_FIXTURE.with(|current| *current.borrow_mut() = None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invocation_scope::{
        PublishedInvocationLease, PublishedInvocationRequest, ResourceInvocationScope,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    };

    fn worker_case(expired: bool) {
        let fixture = crate::bound_dispatch::legacy_document_tests::Fixture::in_room(
            1970 + u64::from(expired),
            "77777777-7777-4777-8777-777777777777",
        );
        fixture
            .store
            .require_invocation_provenance(&fixture.checkout)
            .unwrap();
        let time = Arc::new(AtomicU64::new(99));
        let sampled = time.clone();
        let lease = PublishedInvocationLease::new(
            PublishedInvocationRequest {
                wire_id: 1970,
                invocation: tilo_actions::InvocationId::new(),
                address: tilo_actions::ActionAddress::new("fixture", "open-uri"),
                argument: None,
                actor: "original-caller".into(),
            },
            fixture.target.owner.broker_route().unwrap(),
            Arc::new(move || {
                (sampled.load(Ordering::Acquire) < 100)
                    .then_some(())
                    .ok_or_else(|| "original caller expired".into())
            }),
        )
        .with_owner_token(Arc::new("retained fixture URI caller"))
        .unwrap();
        let scope =
            ResourceInvocationScope::for_owner(fixture.target.owner.clone(), lease).unwrap();
        let effect = scope.effect_scope().unwrap();
        if expired {
            time.store(100, Ordering::Release);
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let owner_thread = std::thread::current().id();
        let guard = scoped_uri_fixture(Arc::new(move |uri| {
            assert_eq!(uri, "https://example.test/original-caller");
            assert_ne!(std::thread::current().id(), owner_thread);
            observed.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }))
        .unwrap();
        let result = open_uri_scoped("https://example.test/original-caller".into(), effect);
        drop(guard); // The dispatched request must own the captured backend.
        let result = result
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        assert_eq!(calls.load(Ordering::Acquire), usize::from(!expired));
        if expired {
            assert_eq!(result.unwrap_err(), "original caller expired");
        } else {
            result.unwrap();
        }
    }
    #[test]
    fn scoped_uri_live_original_caller_reaches_worker_backend() {
        worker_case(false);
    }
    #[test]
    fn scoped_uri_expired_admitted_caller_never_reaches_worker_backend() {
        worker_case(true);
    }
    #[test]
    fn scoped_uri_fixture_is_thread_local_and_restored() {
        let guard = scoped_uri_fixture(Arc::new(|_| Ok(()))).unwrap();
        assert!(scoped_uri_fixture(Arc::new(|_| Ok(()))).is_err());
        assert!(
            std::thread::spawn(|| SCOPED_URI_FIXTURE.with(|value| value.borrow().is_none()))
                .join()
                .unwrap()
        );
        drop(guard);
        assert!(SCOPED_URI_FIXTURE.with(|value| value.borrow().is_none()));
    }
}
