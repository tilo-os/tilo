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
//! TalkBack's explore-by-touch produces reach native code first. The
//! vendored winit leaves them unhandled, which sends them on to the view
//! hierarchy and the provider's hover listener on the decor view.
//!
//! The framework throws when an accessibility event is sent from the UI
//! thread while no service is enabled, and accesskit_android 0.8 raises its
//! events there unchecked (0.9 checks, but needs a newer `accesskit` than the
//! workspace's fork). Updates are therefore sent only while a service is
//! enabled. A provider that has asked for its tree keeps what it was given,
//! or a placeholder, until the next update, and nothing redraws when a
//! service is enabled again; so such a provider is replaced while no service
//! listens, and the next query asks for the whole tree afresh.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use accesskit::{ActionHandler, ActionRequest, ActivationHandler, TreeUpdate};
use accesskit_android::InjectingAdapter;
use accesskit_android::jni::objects::{GlobalRef, JObject};
use accesskit_android::jni::{JNIEnv, JavaVM};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::platform::android::ActiveEventLoopExtAndroid;
use winit::window::{Window, WindowId};

type MakeAdapter = Box<dyn Fn(&mut JNIEnv<'_>, &JObject<'_>) -> InjectingAdapter>;

pub(crate) struct Adapter {
    /// `None` only while one provider is swapped for the next.
    provider: Option<InjectingAdapter>,
    /// Whether `provider` has asked for its tree.
    asked: Arc<AtomicBool>,
    make: MakeAdapter,
    host: GlobalRef,
    vm: JavaVM,
    /// `android.view.accessibility.AccessibilityManager`.
    manager: GlobalRef,
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
        let manager = accessibility_manager(&mut env, &activity).map_err(|e| {
            let _ = env.exception_clear();
            e.to_string()
        })?;
        let host = env.new_global_ref(host).map_err(|e| e.to_string())?;
        let window_id = window.id();
        let asked = Arc::new(AtomicBool::new(false));
        let make_asked = Arc::clone(&asked);
        let make: MakeAdapter = Box::new(move |env, host| {
            InjectingAdapter::new(
                env,
                host,
                Activation {
                    window_id,
                    proxy: proxy.clone(),
                    asked: Arc::clone(&make_asked),
                },
                Actions {
                    window_id,
                    proxy: proxy.clone(),
                },
            )
        });
        let adapter = make(&mut env, host.as_obj());
        Ok(Self {
            provider: Some(adapter),
            asked,
            make,
            host,
            vm,
            manager,
        })
    }

    pub(crate) fn update_if_active(&mut self, updater: impl FnOnce() -> TreeUpdate) {
        if self.service_enabled() {
            if let Some(adapter) = self.provider.as_mut() {
                adapter.update_if_active(updater);
            }
        } else if self.asked.load(Ordering::Acquire) {
            self.discard_tree();
        }
    }

    fn discard_tree(&mut self) {
        let Ok(mut env) = self.vm.get_env() else {
            return;
        };
        // The old provider's removal is posted to the view before the new
        // one's installation, which refuses a view that still has one.
        drop(self.provider.take());
        self.asked.store(false, Ordering::Release);
        self.provider = Some((self.make)(&mut env, self.host.as_obj()));
    }

    fn service_enabled(&self) -> bool {
        let Ok(mut env) = self.vm.get_env() else {
            return false;
        };
        let enabled = env
            .call_method(self.manager.as_obj(), "isEnabled", "()Z", &[])
            .and_then(|value| value.z());
        if enabled.is_err() {
            let _ = env.exception_clear();
        }
        enabled.unwrap_or(false)
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

fn accessibility_manager(
    env: &mut JNIEnv<'_>,
    activity: &JObject<'_>,
) -> accesskit_android::jni::errors::Result<GlobalRef> {
    let name = env.new_string("accessibility")?;
    let manager = env
        .call_method(
            activity,
            "getSystemService",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            &[(&name).into()],
        )?
        .l()?;
    env.new_global_ref(manager)
}

struct Activation<T: From<accesskit_winit::Event> + Send + 'static> {
    window_id: WindowId,
    proxy: EventLoopProxy<T>,
    asked: Arc<AtomicBool>,
}

impl<T: From<accesskit_winit::Event> + Send + 'static> ActivationHandler for Activation<T> {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.asked.store(true, Ordering::Release);
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
