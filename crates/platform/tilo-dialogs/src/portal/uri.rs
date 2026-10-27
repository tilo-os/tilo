//! Original-caller custody for asynchronous desktop URI effects.
use super::EffectGate;
use std::sync::mpsc::Receiver;

/// Dispatch one URI effect without blocking the UI thread. The original
/// admitted caller is rechecked on the worker immediately before the host's
/// [`crate::UriOpener`] runs. Apps retain their own URL policy (for
/// example, allowed link schemes).
pub fn open_uri_scoped(uri: String, scope: impl EffectGate) -> Receiver<Result<(), String>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    #[cfg(any(test, feature = "test-support"))]
    let backend = SCOPED_URI_FIXTURE.with(|current| current.borrow().clone());
    // Resolve the host on the dispatching thread: services are scoped per thread.
    let opener = crate::current_opener();
    let failed = sender.clone();
    let spawned =
        tilo_rt::background::Background::current().spawn("ui-scoped-open-uri", move || {
            let outcome = scope.with_effect(|| {
                #[cfg(any(test, feature = "test-support"))]
                if let Some(backend) = backend {
                    return backend(&uri);
                }
                opener.open(&uri).map_err(|error| error.to_string())
            });
            let _ = sender.send(outcome);
        });
    if spawned.is_err() {
        let _ = failed.send(Err("Opening links isn't available here".into()));
    }
    receiver
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
    use std::sync::Arc;

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
