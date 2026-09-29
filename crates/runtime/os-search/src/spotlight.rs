//! Spotlight: its own copy of the index, and the activity that opens a result.
//!
//! Spotlight keeps what it is given, so [`apply`] hands it each walk's
//! [`Changes`] and [`reset`] clears what a previous run left. Items belong to
//! the app bundle that indexed them: a process without a bundle identifier
//! (a binary run from `target/`) cannot index, and [`bundled`] says so.
//! Choosing a result starts or wakes that bundle with a `CSSearchableItem`
//! user activity naming the entry, which [`run_continuations`] hands over.

use std::ptr::NonNull;

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{
    AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSUserActivityRestoring,
};
use objc2_core_spotlight::{
    CSSearchableIndex, CSSearchableItem, CSSearchableItemActionType,
    CSSearchableItemActivityIdentifier, CSSearchableItemAttributeSet,
};
use objc2_foundation::{
    NSArray, NSBundle, NSError, NSObject, NSObjectProtocol, NSString, NSUserActivity,
};
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

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `Continuation`
    // does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "TiloSpotlightContinuation"]
    #[ivars = Box<dyn Fn(EntryId)>]
    struct Continuation;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for Continuation {}

    // SAFETY: the method signature matches the protocol's.
    unsafe impl NSApplicationDelegate for Continuation {
        #[unsafe(method(application:continueUserActivity:restorationHandler:))]
        fn continue_activity(
            &self,
            _application: &NSApplication,
            activity: &NSUserActivity,
            _restore: &DynBlock<dyn Fn(NonNull<NSArray<ProtocolObject<dyn NSUserActivityRestoring>>>)>,
        ) -> bool {
            match activity_entry(activity) {
                Some(entry) => {
                    (self.ivars())(entry);
                    true
                }
                None => false,
            }
        }
    }
);

/// Run the app on the main thread, handing every Spotlight result the person
/// chooses to `open`. Returns only once the app terminates.
///
/// # Errors
///
/// Refuses to run off the main thread, where `AppKit` cannot.
pub fn run_continuations(open: impl Fn(EntryId) + 'static) -> std::io::Result<()> {
    let mtm = MainThreadMarker::new().ok_or_else(|| {
        std::io::Error::other("Spotlight results are continued on the main thread")
    })?;
    let app = NSApplication::sharedApplication(mtm);
    // A background service: no Dock icon, no menu bar.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let delegate = Continuation::alloc(mtm).set_ivars(Box::new(open) as Box<dyn Fn(EntryId)>);
    // SAFETY: NSObject's `init` has this signature.
    let delegate: Retained<Continuation> = unsafe { msg_send![super(delegate), init] };
    // The application holds its delegate weakly; `delegate` lives as long as
    // this frame, which is as long as the app runs.
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
    Ok(())
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
