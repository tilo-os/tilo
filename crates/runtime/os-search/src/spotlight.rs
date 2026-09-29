//! Spotlight: its own copy of the index.
//!
//! Spotlight keeps what it is given, so [`apply`] hands it each walk's
//! [`Changes`] and [`reset`] clears what a previous run left. Items belong to
//! the app bundle that indexed them: a process without a bundle identifier
//! (a binary run from `target/`) cannot index, and [`bundled`] says so.
//! Choosing a result starts or wakes that bundle's app (in `Tilo.app`, the
//! Store), which hands the entry back to the daemon (`tilo-host-search`).

use block2::RcBlock;
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2_core_spotlight::{CSSearchableIndex, CSSearchableItem, CSSearchableItemAttributeSet};
use objc2_foundation::{NSArray, NSBundle, NSError, NSString};
use objc2_uniform_type_identifiers::UTTypeItem;

use crate::{Changes, SearchEntry};

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
