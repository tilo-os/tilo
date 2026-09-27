//! TalkBack for the NativeActivity runner: the AccessKit tree through a node
//! provider injected into the activity's window.
//!
//! `accesskit_winit`'s Android glue reads GameActivity's `mSurfaceView`,
//! which a NativeActivity does not have, so the injecting adapter is driven
//! here. Its requests arrive as the same [`accesskit_winit::Event`]s as on
//! every other platform, so the runner answers them in one place.
//!
//! The provider sits on the decor view: NativeActivity draws into the whole
//! window surface, and the adapter places node bounds relative to its host
//! view's position on screen.
//!
//! NativeActivity takes the window's input queue, so the hover events
//! TalkBack's explore-by-touch produces reach native code rather than the
//! view's hover listener. Swipe navigation and double-tap activation work;
//! touch exploration needs those hover events forwarded to the view.

use accesskit::{ActionHandler, ActionRequest, ActivationHandler, TreeUpdate};
use accesskit_android::InjectingAdapter;
use accesskit_android::jni::objects::JObject;
use accesskit_android::jni::{JNIEnv, JavaVM};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::platform::android::ActiveEventLoopExtAndroid;
use winit::window::{Window, WindowId};

pub(crate) struct Adapter {
    adapter: InjectingAdapter,
}

impl Adapter {
    /// Inject the provider into `window`'s activity. Must run on the thread
    /// that later calls [`Self::update_if_active`].
    pub(crate) fn new<T: From<accesskit_winit::Event> + Send + 'static>(
        event_loop: &ActiveEventLoop,
        window: &Window,
        proxy: EventLoopProxy<T>,
    ) -> Result<Self, String> {
        let app = event_loop.android_app();
        // SAFETY: android-activity owns the process's `JavaVM*` and the
        // activity's global reference for as long as the event loop runs.
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.map_err(|e| e.to_string())?;
        // Permanent: the adapter looks the environment up again on every
        // update.
        let mut env = vm
            .attach_current_thread_permanently()
            .map_err(|e| e.to_string())?;
        // SAFETY: as above; the reference is borrowed, never deleted.
        let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
        let host = decor_view(&mut env, &activity).map_err(|e| {
            let _ = env.exception_clear();
            e.to_string()
        })?;
        let window_id = window.id();
        let adapter = InjectingAdapter::new(
            &mut env,
            &host,
            Activation {
                window_id,
                proxy: proxy.clone(),
            },
            Actions { window_id, proxy },
        );
        Ok(Self { adapter })
    }

    pub(crate) fn update_if_active(&mut self, updater: impl FnOnce() -> TreeUpdate) {
        self.adapter.update_if_active(updater);
    }

    #[allow(
        clippy::unused_self,
        reason = "mirrors accesskit_winit::Adapter, which the runner drives identically"
    )]
    pub(crate) fn process_event(&self, _window: &Window, _event: &winit::event::WindowEvent) {}
}

fn decor_view<'local>(
    env: &mut JNIEnv<'local>,
    activity: &JObject<'_>,
) -> accesskit_android::jni::errors::Result<JObject<'local>> {
    let window = env
        .call_method(activity, "getWindow", "()Landroid/view/Window;", &[])?
        .l()?;
    env.call_method(&window, "getDecorView", "()Landroid/view/View;", &[])?
        .l()
}

struct Activation<T: From<accesskit_winit::Event> + Send + 'static> {
    window_id: WindowId,
    proxy: EventLoopProxy<T>,
}

impl<T: From<accesskit_winit::Event> + Send + 'static> ActivationHandler for Activation<T> {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        let event = accesskit_winit::Event {
            window_id: self.window_id,
            window_event: accesskit_winit::WindowEvent::InitialTreeRequested,
        };
        let _ = self.proxy.send_event(event.into());
        None
    }
}

struct Actions<T: From<accesskit_winit::Event> + Send + 'static> {
    window_id: WindowId,
    proxy: EventLoopProxy<T>,
}

impl<T: From<accesskit_winit::Event> + Send + 'static> ActionHandler for Actions<T> {
    fn do_action(&mut self, request: ActionRequest) {
        let event = accesskit_winit::Event {
            window_id: self.window_id,
            window_event: accesskit_winit::WindowEvent::ActionRequested(request),
        };
        let _ = self.proxy.send_event(event.into());
    }
}
