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
    RegionFormats::from_section(crate::settings_source::load_section("locale"))
}

static CURRENT: RwLock<Option<RegionFormats>> = RwLock::new(None);
static REVISION: AtomicU64 = AtomicU64::new(0);

/// The person's formats, read from their settings on first use and kept
/// until the next [`refresh`].
#[must_use]
pub fn current() -> RegionFormats {
    // A thread the workspace renders for may have its own formats installed
    // ([`install_here`]); a page installs only a zone, so its formats are the
    // person's own.
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

/// Read local times on this thread in `zone` from now on, keeping the formats it reads dates
/// in, as [`install_here`] does. `None` goes back to the device's or the browser's own zone.
pub fn install_zone_here(zone: Option<crate::clock::Zone>) -> bool {
    let formats = HERE.with(|here| here.borrow().0);
    install_here(formats, zone)
}

/// The zone a person who chose `chosen` reads their dates in, where it is not the one this
/// device or browser is in: `None` for none chosen, for this device's own zone, and for a name
/// it does not know, each of which reads in the device's own.
#[must_use]
pub fn chosen_zone(chosen: Option<&str>) -> Option<crate::clock::Zone> {
    let chosen = chosen?;
    if tilo_clock::local_zone_name().as_deref() == Some(chosen) {
        return None;
    }
    crate::clock::Zone::named(chosen)
}

/// Changes whenever the person's formats change.
#[must_use]
pub fn revision() -> u64 {
    REVISION.load(Ordering::Acquire)
}
