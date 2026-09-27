//! What an app side module and its loader (`web/dylink.mjs`) call in the web
//! core. The loader has checked the module's runtime ABI id against this
//! core's before instantiating it, so its types are this core's types.
use std::alloc::Layout;
use std::ffi::c_void;
use ui::application::ApplicationDefinition;

/// [`tilo_runtime_register_app`] installed the app.
const REGISTERED: u32 = 0;
/// An installed app already has the definition's id; it is kept.
const DUPLICATE: u32 = 1;
/// The module passed no definition.
const MISSING: u32 = 2;

/// Zeroed memory for a side module's data and bss, or null when the request
/// is malformed or cannot be met. A linked module lives as long as the page,
/// so this is never freed.
#[unsafe(no_mangle)]
pub extern "C" fn tilo_dl_alloc(size: usize, align: usize) -> *mut u8 {
    let Ok(layout) = Layout::from_size_align(size.max(1), align.max(1)) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the layout has a non-zero size.
    unsafe { std::alloc::alloc_zeroed(layout) }
}

/// Install the app a side module defines. Its generated registration shim
/// passes `<app crate>::definition()`.
///
/// # Safety
/// `definition` is null or points at an `ApplicationDefinition` that lives
/// for the rest of the page, built against this core's runtime ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tilo_runtime_register_app(definition: *const c_void) -> u32 {
    if definition.is_null() {
        return MISSING;
    }
    // SAFETY: the caller passes a definition of this ABI with static lifetime.
    let definition: &'static ApplicationDefinition = unsafe { &*definition.cast() };
    match crate::catalog::install(definition) {
        Ok(()) => REGISTERED,
        Err(_) => DUPLICATE,
    }
}
