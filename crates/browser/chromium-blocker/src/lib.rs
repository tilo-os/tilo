//! Thread-safe native filter matching for the Chromium helper.

use adblock::{Engine, request::Request};
use serde_json::{Value, json};
use std::ffi::{CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Mutex, OnceLock, mpsc};
use std::thread::{self, JoinHandle};

const MAX_RULE_BYTES: usize = 32 * 1024 * 1024;

/// A profile matcher whose engine lives on a dedicated worker thread. The
/// pinned adblock engine is not `Send`, so requests cross a bounded channel
/// instead of moving or sharing the engine between CEF service threads.
pub struct Blocker {
    commands: mpsc::SyncSender<Command>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

enum Command {
    Check {
        url: String,
        source: String,
        kind: String,
        reply: mpsc::SyncSender<Option<Value>>,
    },
    Cosmetics {
        url: String,
        classes: Vec<String>,
        ids: Vec<String>,
        reply: mpsc::SyncSender<Option<Value>>,
    },
    Shutdown,
    #[cfg(test)]
    Panic,
}

fn web_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

impl Blocker {
    pub fn new(rules: &str) -> Self {
        let rules = rules.to_owned();
        let (commands, requests) = mpsc::sync_channel(64);
        let (ready, initialized) = mpsc::sync_channel(0);
        let worker = thread::Builder::new()
            .name("qos-blocker".into())
            .spawn(move || worker_loop(&rules, requests, ready))
            .expect("blocker worker thread must start");
        initialized
            .recv()
            .expect("blocker worker must initialize its engine");
        Self {
            commands,
            worker: Mutex::new(Some(worker)),
        }
    }

    pub fn check(&self, url: &str, source: &str, kind: &str) -> Option<Value> {
        if !web_url(url) {
            return Some(json!({"blocked": false, "redirect": null, "rewritten_url": null}));
        }
        let (reply, response) = mpsc::sync_channel(0);
        self.commands
            .send(Command::Check {
                url: url.to_owned(),
                source: source.to_owned(),
                kind: kind.to_owned(),
                reply,
            })
            .ok()?;
        response.recv().ok()?
    }

    pub fn cosmetics(&self, url: &str, classes: &[String], ids: &[String]) -> Option<Value> {
        let empty =
            || json!({"hide_selectors": [], "injected_script": "", "procedural_actions": []});
        if !web_url(url) {
            return Some(empty());
        }
        let (reply, response) = mpsc::sync_channel(0);
        self.commands
            .send(Command::Cosmetics {
                url: url.to_owned(),
                classes: classes.to_vec(),
                ids: ids.to_vec(),
                reply,
            })
            .ok()?;
        response.recv().ok()?
    }

    #[cfg(test)]
    fn panic_worker_for_test(&self) {
        let _ = self.commands.send(Command::Panic);
        if let Ok(mut worker) = self.worker.lock()
            && let Some(worker) = worker.take()
        {
            let _ = worker.join();
        }
    }
}

impl Drop for Blocker {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Ok(worker) = self.worker.get_mut()
            && let Some(worker) = worker.take()
        {
            let _ = worker.join();
        }
    }
}

fn worker_loop(rules: &str, requests: mpsc::Receiver<Command>, ready: mpsc::SyncSender<()>) {
    static RESOURCES: OnceLock<Vec<adblock::resources::Resource>> = OnceLock::new();
    let resources = RESOURCES.get_or_init(|| {
        serde_json::from_str(include_str!("../resources/resources.json"))
            .expect("checked-in blocker resources must match the pinned adblock schema")
    });
    let mut engine = Engine::from_rules(rules.lines(), Default::default());
    engine.use_resources(resources.clone());
    if ready.send(()).is_err() {
        return;
    }
    for command in requests {
        match command {
            Command::Check {
                url,
                source,
                kind,
                reply,
            } => {
                let value = Request::new(&url, &source, &kind).ok().map(|request| {
                    let result = engine.check_network_request(&request);
                    json!({
                        "blocked": result.matched,
                        "redirect": result.redirect,
                        "rewritten_url": result.rewritten_url,
                    })
                });
                let _ = reply.send(value);
            }
            Command::Cosmetics {
                url,
                classes,
                ids,
                reply,
            } => {
                let mut resources = engine.url_cosmetic_resources(&url);
                if !resources.generichide {
                    resources
                        .hide_selectors
                        .extend(engine.hidden_class_id_selectors(
                            &classes,
                            &ids,
                            &resources.exceptions,
                        ));
                }
                let _ = reply.send(Some(json!({
                    "hide_selectors": resources.hide_selectors,
                    "injected_script": resources.injected_script,
                    "procedural_actions": resources.procedural_actions,
                })));
            }
            Command::Shutdown => break,
            #[cfg(test)]
            Command::Panic => panic!("simulated matcher panic"),
        }
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
