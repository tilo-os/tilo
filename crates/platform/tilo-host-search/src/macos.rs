//! Spotlight: the `CSSearchableItem` user activity a chosen result wakes the
//! bundle's app with.

use std::ffi::CString;
use std::path::PathBuf;
use std::sync::OnceLock;

use objc2::encode::Encode;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, Imp, NSObjectProtocol, Sel};
use objc2::{MainThreadMarker, sel};
use objc2_app_kit::NSApplication;
use objc2_core_spotlight::{CSSearchableItemActionType, CSSearchableItemActivityIdentifier};
use objc2_foundation::{NSString, NSUserActivity};

/// The runtime's native socket chosen results go to.
static SOCKET: OnceLock<PathBuf> = OnceLock::new();

/// The entry a Spotlight user activity asks to open, if it is one.
pub(crate) fn activity_entry(activity: &NSUserActivity) -> Option<String> {
    // SAFETY: both keys are immutable framework constants.
    let (action, key) = unsafe {
        (
            CSSearchableItemActionType,
            CSSearchableItemActivityIdentifier,
        )
    };
    if &*activity.activityType() != action {
        return None;
    }
    let info = activity.userInfo()?;
    let id = info.objectForKey(key)?.downcast::<NSString>().ok()?;
    Some(id.to_string())
}

/// `application:continueUserActivity:restorationHandler:`, added to the
/// application delegate's class.
unsafe extern "C-unwind" fn continue_activity(
    _delegate: *mut AnyObject,
    _selector: Sel,
    _application: *mut AnyObject,
    activity: *mut NSUserActivity,
    _restore: *mut AnyObject,
) -> Bool {
    // SAFETY: AppKit passes a valid user activity or null.
    let entry = unsafe { activity.as_ref() }.and_then(activity_entry);
    let (Some(entry), Some(socket)) = (entry, SOCKET.get()) else {
        return Bool::NO;
    };
    if let Err(error) = crate::open(socket, &entry) {
        tracing::warn!(%error, "a Spotlight result did not open");
    }
    Bool::YES
}

/// The delegate is the windowing library's, which must stay the delegate, so
/// the continuation is added to its class rather than set as a new delegate;
/// a delegate that already continues activities keeps doing so.
pub(crate) fn forward_to_runtime() {
    let (Some(mtm), Some(profile)) = (
        MainThreadMarker::new(),
        std::env::var_os("TILO_RUNTIME_PROFILE"),
    ) else {
        return;
    };
    let Some(delegate) = NSApplication::sharedApplication(mtm).delegate() else {
        return;
    };
    let selector = sel!(application:continueUserActivity:restorationHandler:);
    if delegate.respondsToSelector(selector)
        || SOCKET
            .set(PathBuf::from(profile).join("native.sock"))
            .is_err()
    {
        return;
    }
    let Ok(types) = CString::new(format!("{}@:@@@?", Bool::ENCODING)) else {
        return;
    };
    // SAFETY: `continue_activity` has the method's ABI: the receiver, the
    // selector, then the three object arguments, answering a BOOL, as the
    // type string says. Adding a method the class lacks changes nothing it
    // already does.
    unsafe {
        let imp: Imp = std::mem::transmute::<
            unsafe extern "C-unwind" fn(
                *mut AnyObject,
                Sel,
                *mut AnyObject,
                *mut NSUserActivity,
                *mut AnyObject,
            ) -> Bool,
            Imp,
        >(continue_activity);
        let class = objc2::ffi::object_getClass(Retained::as_ptr(&delegate).cast::<AnyObject>());
        objc2::ffi::class_addMethod(class.cast_mut(), selector, imp, types.as_ptr());
    }
}

#[cfg(test)]
mod tests {
    use objc2::AllocAnyThread;
    use objc2_foundation::NSDictionary;

    use super::*;

    fn activity(kind: &NSString, id: &str) -> Retained<NSUserActivity> {
        let activity = NSUserActivity::initWithActivityType(NSUserActivity::alloc(), kind);
        // SAFETY: the key is an immutable framework constant.
        let key = unsafe { CSSearchableItemActivityIdentifier };
        let info = NSDictionary::from_retained_objects(&[key], &[NSString::from_str(id)]);
        // SAFETY: a dictionary of strings is a dictionary of property-list
        // objects, which is what a user activity carries.
        unsafe {
            activity.setUserInfo(Some(&Retained::cast_unchecked::<NSDictionary>(info)));
        }
        activity
    }

    #[test]
    fn a_spotlight_activity_names_the_entry_it_opens() {
        // SAFETY: an immutable framework constant.
        let spotlight = unsafe { CSSearchableItemActionType };
        assert_eq!(
            activity_entry(&activity(spotlight, "00112233aabbccdd")).as_deref(),
            Some("00112233aabbccdd")
        );
        let handoff = NSString::from_str("so.tilo.handoff");
        assert_eq!(
            activity_entry(&activity(&handoff, "00112233aabbccdd")),
            None
        );
    }
}
