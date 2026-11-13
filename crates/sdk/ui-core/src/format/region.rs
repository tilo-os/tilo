//! The person's region and formats, as every formatter reads them: the `locale` section of their
//! settings (date style, 12/24-hour clock, number style, first day of the week) next to the desktop's
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

/// Changes whenever the person's formats change.
#[must_use]
pub fn revision() -> u64 {
    REVISION.load(Ordering::Acquire)
}
