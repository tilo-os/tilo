//! Thread-safe native filter matching for the Chromium helper.

use adblock::{Engine, request::Request};
use serde_json::{Value, json};
use std::ffi::{CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Mutex, OnceLock};

const MAX_RULE_BYTES: usize = 32 * 1024 * 1024;

/// Immutable compiled rules. Interior regex caching is protected by a mutex;
/// callers can use one profile matcher from CEF's IO and renderer service threads.
pub struct Blocker {
    engine: Mutex<Engine>,
}

fn web_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

impl Blocker {
    pub fn new(rules: &str) -> Self {
        static RESOURCES: OnceLock<Vec<adblock::resources::Resource>> = OnceLock::new();
        let resources = RESOURCES.get_or_init(|| {
            serde_json::from_str(include_str!("../resources/resources.json"))
                .expect("checked-in blocker resources must match the pinned adblock schema")
        });
        let mut engine = Engine::from_rules(rules.lines(), Default::default());
        engine.use_resources(resources.clone());
        Self {
            engine: Mutex::new(engine),
        }
    }

    pub fn check(&self, url: &str, source: &str, kind: &str) -> Option<Value> {
        if !web_url(url) {
            return Some(json!({"blocked": false, "redirect": null, "rewritten_url": null}));
        }
        let request = Request::new(url, source, kind).ok()?;
        let engine = self.engine.lock().ok()?;
        let result = engine.check_network_request(&request);
        Some(json!({
            "blocked": result.matched,
            "redirect": result.redirect,
            "rewritten_url": result.rewritten_url,
        }))
    }

    pub fn cosmetics(&self, url: &str, classes: &[String], ids: &[String]) -> Option<Value> {
        let empty =
            || json!({"hide_selectors": [], "injected_script": "", "procedural_actions": []});
        if !web_url(url) {
            return Some(empty());
        }
        let engine = self.engine.lock().ok()?;
        let mut resources = engine.url_cosmetic_resources(url);
        if !resources.generichide {
            resources
                .hide_selectors
                .extend(engine.hidden_class_id_selectors(classes, ids, &resources.exceptions));
        }
        Some(json!({
            "hide_selectors": resources.hide_selectors,
            "injected_script": resources.injected_script,
            "procedural_actions": resources.procedural_actions,
        }))
    }
}

fn foreign_json(operation: impl FnOnce() -> Option<Value>) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        let json = operation()?;
        let text = json.to_string();
        if text.len() > 4 * 1024 * 1024 {
            return None;
        }
        CString::new(text).ok().map(CString::into_raw)
    }))
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

/// # Safety
/// `rules` must point to `len` readable bytes for this call. Null is allowed only
/// when len is zero. The returned handle is owned; destroy once after all calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qos_blocker_create(rules: *const u8, len: usize) -> *mut Blocker {
    if len > MAX_RULE_BYTES || (rules.is_null() && len != 0) {
        return std::ptr::null_mut();
    }
    catch_unwind(AssertUnwindSafe(|| {
        let bytes = if len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(rules, len) }
        };
        let rules = std::str::from_utf8(bytes).ok()?;
        Some(Box::into_raw(Box::new(Blocker::new(rules))))
    }))
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

/// # Safety
/// The handle must be a live result of qos_blocker_create, with no concurrent
/// callers. Null is accepted. After this call the handle must never be used.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qos_blocker_destroy(handle: *mut Blocker) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle) });
    }
}

// Length validation precedes dereferencing and JSON allocation.
unsafe fn foreign_text<'a>(ptr: *const u8, len: usize, max: usize) -> Option<&'a str> {
    if len > max || ptr.is_null() {
        return None;
    }
    let bytes = if len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(ptr, len) }
    };
    std::str::from_utf8(bytes).ok()
}

/// # Safety
/// Handle must remain live. Each pointer must reference its declared readable
/// byte length for the call. Returned JSON is freed with qos_blocker_string_free.
/// Null means matcher failure, distinct from an allowed request.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qos_blocker_match(
    handle: *mut Blocker,
    url: *const u8,
    url_len: usize,
    source: *const u8,
    source_len: usize,
    kind: *const u8,
    kind_len: usize,
) -> *mut c_char {
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    foreign_json(|| unsafe {
        (*handle).check(
            foreign_text(url, url_len, 65536)?,
            foreign_text(source, source_len, 65536)?,
            foreign_text(kind, kind_len, 64)?,
        )
    })
}

/// # Safety
/// Same lifetime/ownership contract as qos_blocker_match. classes/ids contain
/// JSON arrays, bounded to 1 MiB each before parsing and 10000 entries after.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qos_blocker_cosmetics(
    handle: *mut Blocker,
    url: *const u8,
    url_len: usize,
    classes: *const u8,
    classes_len: usize,
    ids: *const u8,
    ids_len: usize,
) -> *mut c_char {
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    foreign_json(|| unsafe {
        let url = foreign_text(url, url_len, 65536)?;
        let classes = foreign_text(classes, classes_len, 1024 * 1024)?;
        let ids = foreign_text(ids, ids_len, 1024 * 1024)?;
        let classes: Vec<String> = serde_json::from_str(classes).ok()?;
        let ids: Vec<String> = serde_json::from_str(ids).ok()?;
        if classes.len() > 10000 || ids.len() > 10000 {
            return None;
        }
        (*handle).cosmetics(url, &classes, &ids)
    })
}

/// # Safety
/// Value must be null or a live string returned by this library, freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn qos_blocker_string_free(value: *mut c_char) {
    if !value.is_null() {
        drop(unsafe { CString::from_raw(value) });
    }
}

#[cfg(test)]
mod tests;
