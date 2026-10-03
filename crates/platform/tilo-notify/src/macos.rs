//! The macOS backend: `UNUserNotificationCenter`.
//!
//! The notification center belongs to an app bundle, and asking for it from a
//! process without a bundle identifier (a binary run from `target/`) raises an
//! Objective-C exception. Such a process gets a notifier that answers
//! [`NotifyError::Unavailable`] and says why once in the log; run the
//! development `.app` to see notifications.
//!
//! The first notification asks the user for permission. Showing and
//! scheduling are asynchronous: a request the user has refused is dropped by
//! the system and logged, not returned.

use std::ptr::NonNull;
use std::sync::{Arc, OnceLock, mpsc};
use std::time::Duration;

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{AllocAnyThread, define_class, msg_send};
use objc2_foundation::{
    NSArray, NSBundle, NSDictionary, NSError, NSObject, NSObjectProtocol, NSString,
};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
    UNTimeIntervalNotificationTrigger, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

use crate::{Notification, Notifier, NotifyError, Scheduled, SystemTime};

/// How long [`Notifier::scheduled`] waits for the center's answer.
const QUERY_BUDGET: Duration = Duration::from_secs(3);

/// The `userInfo` key a notification's activation URI travels under.
const ACTIVATION_KEY: &str = "tilo.activation";

pub(crate) fn notifier() -> Arc<dyn Notifier> {
    if NSBundle::mainBundle().bundleIdentifier().is_some() {
        install_delegate();
        Arc::new(CenterNotifier)
    } else {
        static WARNED: OnceLock<()> = OnceLock::new();
        WARNED.get_or_init(|| {
            tracing::info!(
                "notifications need an app bundle on macOS; this process has no bundle \
                 identifier, so they are unavailable (run the development .app)"
            );
        });
        crate::unavailable()
    }
}

struct CenterNotifier;

fn center() -> Retained<UNUserNotificationCenter> {
    UNUserNotificationCenter::currentNotificationCenter()
}

fn tags(tag: &str) -> Retained<NSArray<NSString>> {
    NSArray::from_retained_slice(&[NSString::from_str(tag)])
}

/// Ask for permission (the system asks the user only once), then add the
/// request `make` builds.
fn authorized_add(make: impl Fn() -> Retained<UNNotificationRequest> + 'static) {
    let handler = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
        if !granted.as_bool() {
            tracing::info!("the user has not allowed notifications for this app");
            return;
        }
        let failed = RcBlock::new(|error: *mut NSError| {
            // SAFETY: the center passes a valid NSError or null.
            if let Some(error) = unsafe { error.as_ref() } {
                tracing::warn!(error = %error.localizedDescription(), "notification refused");
            }
        });
        center().addNotificationRequest_withCompletionHandler(&make(), Some(&failed));
    });
    center().requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
        &handler,
    );
}

fn content(notification: &Notification) -> Retained<UNMutableNotificationContent> {
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(&notification.title));
    content.setBody(&NSString::from_str(&notification.body));
    if let Some(uri) = &notification.activation {
        let key = NSString::from_str(ACTIVATION_KEY);
        let value: Retained<AnyObject> =
            Retained::into_super(Retained::into_super(NSString::from_str(uri)));
        let info = NSDictionary::from_retained_objects(&[&*key], &[value]);
        // SAFETY: a dictionary of property-list strings, as `userInfo` requires.
        unsafe { content.setUserInfo(Retained::cast_unchecked::<NSDictionary>(info).as_ref()) };
    }
    content
}

/// The activation URI a delivered notification carries, if any.
fn activation_of(notification: &UNNotification) -> Option<String> {
    let info = notification.request().content().userInfo();
    let value = info.objectForKey(&NSString::from_str(ACTIVATION_KEY))?;
    Some(value.downcast::<NSString>().ok()?.to_string())
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `Delegate`
    // does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = AllocAnyThread]
    #[name = "TiloNotificationDelegate"]
    struct Delegate;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for Delegate {}

    // SAFETY: the method signatures match the protocol's.
    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        /// A click (or another response): hand the activation URI over.
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &DynBlock<dyn Fn()>,
        ) {
            if let Some(uri) = activation_of(&response.notification()) {
                crate::activated(&uri);
            }
            completion.call(());
        }

        /// macOS hides notifications of the frontmost app unless asked;
        /// show them as banners there too.
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }
    }
);

/// Make this process's delegate the center's, once. The center holds its
/// delegate weakly, so the delegate lives as long as the process. Set as
/// the host builds its services, before the app finishes launching, so the
/// click that launched it is delivered too.
fn install_delegate() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        // SAFETY: NSObject's `init` has this signature.
        let delegate: Retained<Delegate> =
            unsafe { msg_send![super(Delegate::alloc().set_ivars(())), init] };
        center().setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        std::mem::forget(delegate);
    });
}

impl Notifier for CenterNotifier {
    fn show(&self, notification: &Notification) -> Result<(), NotifyError> {
        crate::validate(notification)?;
        let notification = notification.clone();
        authorized_add(move || {
            UNNotificationRequest::requestWithIdentifier_content_trigger(
                &NSString::from_str(&notification.tag),
                &content(&notification),
                None,
            )
        });
        Ok(())
    }

    fn withdraw(&self, tag: &str) -> Result<(), NotifyError> {
        crate::validate_tag(tag)?;
        center().removeDeliveredNotificationsWithIdentifiers(&tags(tag));
        Ok(())
    }

    fn schedule(&self, notification: &Notification, at: SystemTime) -> Result<(), NotifyError> {
        crate::validate(notification)?;
        // A time-interval trigger must lie in the future; a past time shows
        // the notification a moment from now.
        let seconds = at
            .duration_since(tilo_clock::wall())
            .map_or(1.0, |ahead| ahead.as_secs_f64().max(1.0));
        let notification = notification.clone();
        authorized_add(move || {
            let trigger =
                UNTimeIntervalNotificationTrigger::triggerWithTimeInterval_repeats(seconds, false);
            UNNotificationRequest::requestWithIdentifier_content_trigger(
                &NSString::from_str(&notification.tag),
                &content(&notification),
                Some(&trigger),
            )
        });
        Ok(())
    }

    fn scheduled(&self) -> Result<Vec<Scheduled>, NotifyError> {
        let (tx, rx) = mpsc::channel();
        let handler = RcBlock::new(move |requests: NonNull<NSArray<UNNotificationRequest>>| {
            // SAFETY: the center passes a valid array for the call's duration.
            let requests = unsafe { requests.as_ref() };
            let pending = requests
                .iter()
                .filter_map(|request| {
                    let at = request
                        .trigger()?
                        .downcast::<UNTimeIntervalNotificationTrigger>()
                        .ok()?
                        .nextTriggerDate()?;
                    let since_epoch =
                        Duration::try_from_secs_f64(at.timeIntervalSince1970()).ok()?;
                    Some(Scheduled {
                        tag: request.identifier().to_string(),
                        at: SystemTime::UNIX_EPOCH.checked_add(since_epoch)?,
                    })
                })
                .collect::<Vec<_>>();
            let _ = tx.send(pending);
        });
        center().getPendingNotificationRequestsWithCompletionHandler(&handler);
        rx.recv_timeout(QUERY_BUDGET)
            .map_err(|_| NotifyError::Failed("the notification center did not answer".into()))
    }

    fn cancel_scheduled(&self, tag: &str) -> Result<(), NotifyError> {
        crate::validate_tag(tag)?;
        center().removePendingNotificationRequestsWithIdentifiers(&tags(tag));
        Ok(())
    }
}
