//! The person's region and formats, as every formatter reads them: the `locale` section of their
//! settings (date style, 12/24-hour clock, number style, first day of the week) next to the shell's
//! language.

use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use tilo_lock::RwLockRecover as _;

pub use tilo_formats::{DateStyle, FirstDayOfWeek, NumberStyle, RegionFormats};

/// The person's formats from their settings, uncached.
#[must_use]
pub fn configured_formats() -> RegionFormats {
    RegionFormats::from_section(settings_store::load_section("locale"))
}

static CURRENT: RwLock<Option<RegionFormats>> = RwLock::new(None);
static REVISION: AtomicU64 = AtomicU64::new(0);

/// The person's formats, read from their settings on first use and kept
/// until the next [`refresh`].
#[must_use]
pub fn current() -> RegionFormats {
    #[cfg(not(target_family = "wasm"))]
    if let Some(formats) = HERE.with(|here| here.borrow().0) {
        return formats;
    }
    if let Some(formats) = *CURRENT.read_recover() {
        return formats;
    }
    refresh();
    CURRENT.read_recover().unwrap_or_default()
}

/// Read the person's formats again.
pub fn refresh() -> bool {
    install(configured_formats())
}

/// Use `formats` from now on, as [`refresh`] would after reading them.
pub fn install(formats: RegionFormats) -> bool {
    let mut current = CURRENT.write_recover();
    if *current == Some(formats) {
        return false;
    }
    let first = current.is_none();
    *current = Some(formats);
    if !first {
        REVISION.fetch_add(1, Ordering::AcqRel);
    }
    !first
}

#[cfg(not(target_family = "wasm"))]
std::thread_local! {
    /// The formats and the zone, by its name, [`install_here`] handed this thread: none for the
    /// person's own.
    static HERE: std::cell::RefCell<(Option<RegionFormats>, Option<String>)> =
        const { std::cell::RefCell::new((None, None)) };
}

/// Read dates on this thread in `formats` and local times in `zone` from now on, in place of
/// the person's own and the device's: a worker that renders views for several people at once
/// renders each one's on a thread of its own, in their formats and on their clock
/// ([`tilo_clock::local::install`]). `None` goes back to the person's and the device's. Changes
/// the [`revision`], as new formats do, and answers `true`, when either differs from the
/// thread's.
#[cfg(not(target_family = "wasm"))]
pub fn install_here(formats: Option<RegionFormats>, zone: Option<crate::clock::Zone>) -> bool {
    let next = (formats, zone.as_ref().and_then(crate::clock::Zone::name));
    tilo_clock::local::install(zone);
    let changed = HERE.with(|here| {
        let mut here = here.borrow_mut();
        let changed = *here != next;
        *here = next;
        changed
    });
    if changed {
        REVISION.fetch_add(1, Ordering::AcqRel);
    }
    changed
}

/// Changes whenever the person's formats change.
#[must_use]
pub fn revision() -> u64 {
    REVISION.load(Ordering::Acquire)
}
