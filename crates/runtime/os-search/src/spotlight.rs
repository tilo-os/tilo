//! Spotlight: its own copy of the index, and the activity that opens a result.
//!
//! Spotlight keeps what it is given, so [`apply`] hands it each walk's
//! [`Changes`] and [`reset`] clears what a previous run left. Items belong to
//! the app bundle that indexed them: a process without a bundle identifier
//! (a binary run from `target/`) cannot index, and [`bundled`] says so.
//! Choosing a result starts or wakes that bundle's app with a
//! `CSSearchableItem` user activity naming the entry. The app is not the
//! daemon (in `Tilo.app` it is the Store), so [`continue_with`] takes the
//! activity there and [`crate::native::open`] hands the entry to the daemon, which
//! re-resolves and opens it as it does a GNOME Shell or `KRunner` result.

use std::ffi::CString;
use std::sync::OnceLock;

use block2::RcBlock;
use objc2::encode::Encode;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, Imp, NSObjectProtocol, Sel};
use objc2::{AllocAnyThread, MainThreadMarker, sel};
use objc2_app_kit::NSApplication;
use objc2_core_spotlight::{
    CSSearchableIndex, CSSearchableItem, CSSearchableItemActionType,
    CSSearchableItemActivityIdentifier, CSSearchableItemAttributeSet,
};
use objc2_foundation::{NSArray, NSBundle, NSError, NSString, NSUserActivity};
use objc2_uniform_type_identifiers::UTTypeItem;

use crate::{Changes, EntryId, SearchEntry};

/// The domain every item this runtime indexes carries, so a reset removes
/// exactly them.
const DOMAIN: &str = "so.tilo.runtime.resources";

/// Whether this process is an app bundle Spotlight can index for.
pub fn bundled() -> bool {
    NSBundle::mainBundle().bundleIdentifier().is_some()
}

fn index() -> Option<Retained<CSSearchableIndex>> {
    // SAFETY: both are class methods without preconditions.
    (bundled() && unsafe { CSSearchableIndex::isIndexingAvailable() })
        .then(|| unsafe { CSSearchableIndex::defaultSearchableIndex() })
}

fn logged(what: &'static str) -> RcBlock<dyn Fn(*mut NSError)> {
    RcBlock::new(move |error: *mut NSError| {
        // SAFETY: CoreSpotlight passes a valid NSError or null.
        if let Some(error) = unsafe { error.as_ref() } {
            tracing::warn!(error = %error.localizedDescription(), "Spotlight could not {what}");
        }
    })
}

/// Remove every item a previous run indexed: what it held may have been
/// revoked while no runtime was there to say so.
pub fn reset() {
    let Some(index) = index() else { return };
    let domains = NSArray::from_retained_slice(&[NSString::from_str(DOMAIN)]);
    // SAFETY: the array holds strings and the handler takes an optional error.
    unsafe {
        index.deleteSearchableItemsWithDomainIdentifiers_completionHandler(
            &domains,
            Some(&logged("clear the previous index")),
        );
    }
}

/// Hand Spotlight one walk's changes.
pub fn apply(changes: &Changes) {
    let Some(index) = index() else { return };
    if !changes.removed.is_empty() {
        let ids: Vec<_> = changes
            .removed
            .iter()
            .map(|id| NSString::from_str(&id.to_string()))
            .collect();
        // SAFETY: the array holds strings and the handler takes an optional error.
        unsafe {
            index.deleteSearchableItemsWithIdentifiers_completionHandler(
                &NSArray::from_retained_slice(&ids),
                Some(&logged("remove resources")),
            );
        }
    }
    if !changes.indexed.is_empty() {
        let items: Vec<_> = changes.indexed.iter().map(item).collect();
        // SAFETY: the array holds searchable items and the handler takes an
        // optional error.
        unsafe {
            index.indexSearchableItems_completionHandler(
                &NSArray::from_retained_slice(&items),
                Some(&logged("index resources")),
            );
        }
    }
}

fn item(entry: &SearchEntry) -> Retained<CSSearchableItem> {
    // SAFETY: `UTTypeItem` is an immutable framework constant, and the
    // initialisers and setters take the types given.
    unsafe {
        let attributes = CSSearchableItemAttributeSet::initWithContentType(
            CSSearchableItemAttributeSet::alloc(),
            UTTypeItem,
        );
        attributes.setTitle(Some(&NSString::from_str(&entry.title)));
        if let Some(detail) = &entry.detail {
            attributes.setContentDescription(Some(&NSString::from_str(detail)));
        }
        CSSearchableItem::initWithUniqueIdentifier_domainIdentifier_attributeSet(
            CSSearchableItem::alloc(),
            Some(&NSString::from_str(&entry.id.to_string())),
            Some(&NSString::from_str(DOMAIN)),
            &attributes,
        )
    }
}

/// The entry a Spotlight user activity asks to open, if it is one.
pub fn activity_entry(activity: &NSUserActivity) -> Option<EntryId> {
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
    id.to_string().parse().ok()
}

/// Where [`continue_with`] hands chosen results.
static OPEN: OnceLock<Box<dyn Fn(EntryId) + Send + Sync>> = OnceLock::new();

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
    match (entry, OPEN.get()) {
        (Some(entry), Some(open)) => {
            open(entry);
            Bool::YES
        }
        _ => Bool::NO,
    }
}

/// Hand every Spotlight result the person chooses to `open`. Call once, on
/// the main thread, after the event loop has made the application's
/// delegate and before the app finishes launching, so the result that
/// launched it arrives too.
///
/// The delegate is the windowing library's, which must stay the delegate, so
/// the continuation is added to its class rather than set as a new delegate.
/// Returns whether it was added: not without a delegate, a second time, or
/// when the delegate already continues activities itself.
pub fn continue_with(
    mtm: MainThreadMarker,
    open: impl Fn(EntryId) + Send + Sync + 'static,
) -> bool {
    let Some(delegate) = NSApplication::sharedApplication(mtm).delegate() else {
        return false;
    };
    let selector = sel!(application:continueUserActivity:restorationHandler:);
    if delegate.respondsToSelector(selector) || OPEN.set(Box::new(open)).is_err() {
        return false;
    }
    let types = format!("{}@:@@@?", Bool::ENCODING);
    let Ok(types) = CString::new(types) else {
        return false;
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
        objc2::ffi::class_addMethod(class.cast_mut(), selector, imp, types.as_ptr()).as_bool()
    }
}

#[cfg(test)]
mod tests {
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
        let id: EntryId = "00112233aabbccdd".parse().unwrap();
        // SAFETY: an immutable framework constant.
        let spotlight = unsafe { CSSearchableItemActionType };
        assert_eq!(
            activity_entry(&activity(spotlight, "00112233aabbccdd")),
            Some(id)
        );
        assert_eq!(activity_entry(&activity(spotlight, "../../etc")), None);
        let handoff = NSString::from_str("so.tilo.handoff");
        assert_eq!(
            activity_entry(&activity(&handoff, "00112233aabbccdd")),
            None
        );
    }
}
