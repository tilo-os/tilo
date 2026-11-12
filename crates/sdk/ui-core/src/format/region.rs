//! The person's region and formats, as every formatter reads them: the
//! `locale` section of their settings (date style, 12/24-hour clock, number
//! style, first day of the week) next to the desktop's language.
//!
//! The section is read once and cached for the whole process. A host calls
//! [`refresh`] when its settings change (the toolkit's app host and the shell
//! do, on the same settings stamp that re-themes them); a change bumps
//! [`revision`], which [`crate::i18n::I18nManager::revision`] folds in, so
//! every view and retained widget that follows the language re-renders its
//! dates and numbers in the new formats without a restart.
//!
//! The formats themselves, and how they render, are `tilo-formats`': the same
//! code an app host without the toolkit runs, so what a window shows and what a
//! host writes in a sentence read alike.

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

/// Read the person's formats again. Returns whether they changed; a change
/// bumps [`revision`] so views re-render with them. Hosts call this when
/// their settings change.
pub fn refresh() -> bool {
    install(configured_formats())
}

/// Use `formats` from now on, as [`refresh`] would after reading them. For
/// hosts that hold the settings document themselves, and for tests.
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
