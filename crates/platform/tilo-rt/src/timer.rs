//! Waiting without a thread: the sanctioned replacement for
//! `std::thread::sleep` inside async work.

use std::time::Duration;

/// Wait `duration`, then resolve. Natively this is tokio's timer, so it must
/// run on a runtime with timers: the app runtime, a daemon or
/// [`block_on`](crate::block_on). In a page it is one `setTimeout`, which a
/// background tab may throttle.
pub async fn sleep(duration: Duration) {
    imp::sleep(duration).await;
}

#[cfg(not(target_family = "wasm"))]
mod imp {
    use std::time::Duration;

    pub(super) async fn sleep(duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
mod imp {
    use std::time::Duration;

    use wasm_bindgen::JsCast as _;

    /// The callback owns the sending half, so the future holds no browser
    /// object and stays `Send`; the closure frees itself when it fires.
    pub(super) async fn sleep(duration: Duration) {
        let (fired, wait) = tokio::sync::oneshot::channel::<()>();
        let armed = {
            let callback = wasm_bindgen::closure::Closure::once_into_js(move || {
                let _ = fired.send(());
            });
            // `setTimeout` caps delays at 2^31 - 1 ms.
            #[allow(
                clippy::cast_precision_loss,
                reason = "a clamped millisecond count is exact in an f64"
            )]
            let millis = duration.as_millis().min(i32::MAX as u128) as f64;
            let global = js_sys::global();
            js_sys::Reflect::get(&global, &"setTimeout".into())
                .ok()
                .and_then(|set_timeout| set_timeout.dyn_into::<js_sys::Function>().ok())
                .is_some_and(|set_timeout| {
                    set_timeout
                        .call2(&global, &callback, &millis.into())
                        .is_ok()
                })
        };
        if armed {
            let _ = wait.await;
        }
    }
}

#[cfg(all(target_family = "wasm", not(target_os = "unknown")))]
mod imp {
    use std::time::Duration;

    pub(super) async fn sleep(duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;

    #[test]
    fn sleep_waits_on_the_runtime_timer() {
        let started = tilo_clock::now();
        crate::block_on(sleep(Duration::from_millis(30))).unwrap();
        assert!(started.elapsed() >= Duration::from_millis(30));
    }

    #[test]
    fn sleep_is_send() {
        fn send<T: Send>(_: &T) {}
        send(&sleep(Duration::ZERO));
    }
}
