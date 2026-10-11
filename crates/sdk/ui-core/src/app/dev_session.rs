// Session handoff for `tilo-app dev`.
//
// A rebuild reloads the windows this process already has. The document, route,
// and caret are written here so a reload, or a later process if this one has
// exited, can put them back. The file lives outside the app tree so the
// watcher does not treat the write as another edit.

use std::path::Path;
#[cfg(all(feature = "native", unix))]
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use crate::widget::RouteSnapshot;

/// Directory set by `tilo-app dev`. The checkpoint file inside it is
/// `checkpoint.bin`.
pub const DEV_SESSION_ENV: &str = "TILO_DEV_SESSION";

const DEV_CHECKPOINT_VERSION: u32 = 1;
const CHECKPOINT_FILE: &str = "checkpoint.bin";

#[derive(serde::Serialize, serde::Deserialize)]
struct DevCheckpoint {
    version: u32,
    /// [`crate::view::FreezeThaw::freeze`] bytes: document plus view, including
    /// the focused control's caret.
    frozen: Vec<u8>,
    route: Option<RouteSnapshot>,
}

impl App {
    /// Load a dev checkpoint, replace the in-memory document and route, and
    /// return the view to apply once focus order exists.
    pub(crate) fn restore_dev_checkpoint_from(&mut self, path: &Path) -> Option<crate::view::View> {
        let bytes = std::fs::read(path).ok()?;
        let checkpoint: DevCheckpoint = postcard::from_bytes(&bytes).ok()?;
        if checkpoint.version != DEV_CHECKPOINT_VERSION {
            return None;
        }
        let frozen: crate::view::FrozenApp = postcard::from_bytes(&checkpoint.frozen).ok()?;
        if frozen.version != crate::view::FROZEN_VERSION {
            return None;
        }
        if crate::view::FreezeThaw::restore_state(self, &frozen.state).is_err() {
            return None;
        }
        if let Some(route) = checkpoint.route.as_ref() {
            self.root_widget.restore_dev_route(route);
            let root = self.state.root.clone();
            self.root_widget.sync(&self.state.doc, &root);
        }
        Some(frozen.view)
    }

    fn consume_dev_checkpoint(&mut self) -> Option<crate::view::View> {
        let dir = crate::host::environment().dev_session.clone()?;
        let path = dir.join(CHECKPOINT_FILE);
        if !path.is_file() {
            return None;
        }
        if let Some(view) = self.restore_dev_checkpoint_from(&path) {
            Some(view)
        } else {
            eprintln!(
                "tilo: ignoring dev checkpoint {} — it does not match this app",
                path.display()
            );
            None
        }
    }

    #[cfg(any(feature = "native", test))]
    fn dev_checkpoint_bytes(&self) -> Vec<u8> {
        let checkpoint = DevCheckpoint {
            version: DEV_CHECKPOINT_VERSION,
            frozen: crate::view::FreezeThaw::freeze(self),
            route: self.root_widget.dev_route(),
        };
        postcard::to_stdvec(&checkpoint).unwrap_or_default()
    }
}

/// Write the running session next to `dir`. A bad path leaves the previous
/// checkpoint in place.
#[cfg(any(feature = "native", test))]
pub(crate) fn write_dev_checkpoint(app: &App, dir: &Path) -> bool {
    let bytes = app.dev_checkpoint_bytes();
    if bytes.is_empty() {
        return false;
    }
    replace_file(&dir.join(CHECKPOINT_FILE), &bytes)
}

/// Write `bytes` beside `destination`, then rename them over it, so a reader
/// never sees a partial file.
#[cfg(any(feature = "native", test))]
fn replace_file(destination: &Path, bytes: &[u8]) -> bool {
    let mut temporary = destination.as_os_str().to_os_string();
    temporary.push(".partial");
    if std::fs::write(&temporary, bytes).is_err() {
        return false;
    }
    std::fs::rename(&temporary, destination).is_ok()
}

/// Appended to a reloaded library's path for the file that records where that
/// reload spent its time, written once the new code's first frame is
/// presented. `tilo-app dev --timings` joins it with the build.
#[cfg(feature = "native")]
const RELOAD_TIMINGS_SUFFIX: &str = ".timings.json";

/// Where one in-place reload spent its time inside the app.
#[cfg(feature = "native")]
pub(crate) struct DevReloadTiming {
    pub library: std::path::PathBuf,
    /// When the event loop took the reload request.
    pub received: crate::time::SystemTime,
    /// Freezing the running app into `checkpoint.bin`.
    pub checkpoint: std::time::Duration,
    /// `dlopen` and symbol lookup.
    pub load: std::time::Duration,
    /// The library's per-window apply and its new root.
    pub handoff: std::time::Duration,
    /// Installing that root and laying it out.
    pub install: std::time::Duration,
}

#[cfg(feature = "native")]
impl DevReloadTiming {
    /// Record this reload, with the time its first frame was handed to the
    /// renderer and the time that frame was presented.
    pub(crate) fn record(
        &self,
        submitted: crate::time::SystemTime,
        presented: crate::time::SystemTime,
    ) {
        let unix_ms = |at: crate::time::SystemTime| {
            at.duration_since(crate::time::UNIX_EPOCH)
                .map_or(0.0, |since| since.as_secs_f64() * 1000.0)
        };
        let ms = |duration: std::time::Duration| duration.as_secs_f64() * 1000.0;
        let line = serde_json::json!({
            "library": self.library,
            "received_unix_ms": unix_ms(self.received),
            "checkpoint_ms": ms(self.checkpoint),
            "load_ms": ms(self.load),
            "handoff_ms": ms(self.handoff),
            "install_ms": ms(self.install),
            "submitted_unix_ms": unix_ms(submitted),
            "presented_unix_ms": unix_ms(presented),
        });
        let mut path = self.library.as_os_str().to_os_string();
        path.push(RELOAD_TIMINGS_SUFFIX);
        if !replace_file(Path::new(&path), line.to_string().as_bytes()) {
            eprintln!(
                "tilo: cannot record reload timings beside {}",
                self.library.display()
            );
        }
    }
}

#[cfg(all(feature = "native", unix))]
static DEV_STOP_PIPE: AtomicI32 = AtomicI32::new(-1);
#[cfg(all(feature = "native", unix))]
static DEV_STOP_INSTALLED: AtomicBool = AtomicBool::new(false);

#[cfg(all(feature = "native", unix))]
extern "C" fn request_dev_session_stop(signal: libc::c_int) {
    let fd = DEV_STOP_PIPE.load(Ordering::Relaxed);
    if fd >= 0 {
        // 1 stops the process. 2 reloads the open windows in place.
        let byte = [if signal == libc::SIGUSR1 { 2 } else { 1 }];
        // SAFETY: `write` of one byte to a pipe opened for this handler is
        // async-signal-safe. The UI thread turns that byte into a normal event.
        unsafe {
            libc::write(fd, byte.as_ptr().cast(), 1);
        }
    }
}

/// When `TILO_DEV_SESSION` is set, SIGTERM asks the UI thread to flush and exit.
/// SIGUSR1 asks it to reload the contents of the windows it already has. The
/// dev loop uses that after a successful build so a new process, and therefore
/// a new window, is not opened.
#[cfg(all(feature = "native", unix))]
#[allow(
    clippy::disallowed_methods,
    reason = "the signal pipe is read for the life of the process; a pool job would pin a worker on a blocking read"
)]
pub(crate) fn install_dev_session_stop(proxy: winit::event_loop::EventLoopProxy<UserEvent>) {
    if crate::host::environment().dev_session.is_none()
        || DEV_STOP_INSTALLED.swap(true, Ordering::Relaxed)
    {
        return;
    }
    let mut fds = [0i32; 2];
    // SAFETY: pipe fills a two-int array the caller owns.
    let opened = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if opened != 0 {
        DEV_STOP_INSTALLED.store(false, Ordering::Relaxed);
        return;
    }
    DEV_STOP_PIPE.store(fds[1], Ordering::Relaxed);
    let read_fd = fds[0];
    std::thread::Builder::new()
        .name("tilo-dev-session".into())
        .spawn(move || {
            let mut byte = [0u8; 1];
            loop {
                // SAFETY: `read_fd` stays open for this thread's lifetime.
                let read = unsafe { libc::read(read_fd, byte.as_mut_ptr().cast(), 1) };
                if read <= 0 {
                    break;
                }
                let event = if byte[0] == 2 {
                    UserEvent::DevSessionReload
                } else {
                    UserEvent::DevSessionStop
                };
                if proxy.send_event(event).is_err() {
                    break;
                }
            }
        })
        .ok();
    // SAFETY: the handler only writes one byte to the pipe above.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            request_dev_session_stop as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGUSR1,
            request_dev_session_stop as *const () as libc::sighandler_t,
        );
    }
}

/// Windows has no signals for the dev loop to send; it ends the process.
#[cfg(all(feature = "native", not(unix)))]
pub(crate) fn install_dev_session_stop(_proxy: winit::event_loop::EventLoopProxy<UserEvent>) {}

/// Replace the baked catalogs with the `.ftl` files in `dir`.
///
/// Returns whether a catalog was installed. A missing directory leaves the
/// translations this process already has.
#[cfg(any(feature = "native", test))]
pub(crate) fn reload_dev_locale_catalogs_from(dir: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut owned = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("ftl"))
        {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        owned.push((stem.to_owned(), text));
    }
    if owned.is_empty() {
        return false;
    }
    let source = if owned.iter().any(|(stem, _)| stem == "en-US") {
        "en-US"
    } else {
        owned[0].0.as_str()
    };
    let borrowed = owned
        .iter()
        .map(|(stem, text)| (stem.as_str(), text.as_str()))
        .collect::<Vec<_>>();
    crate::i18n::I18nManager::install_system(source, &borrowed).is_ok()
}

#[cfg(feature = "native")]
pub(crate) fn checkpoint_and_exit(app: &App, event_loop: &winit::event_loop::ActiveEventLoop) {
    if let Some(dir) = crate::host::environment().dev_session.as_deref() {
        write_dev_checkpoint(app, dir);
    }
    event_loop.exit();
}

#[cfg(test)]
struct DevCaretField {
    caret: String,
}

#[cfg(test)]
impl crate::widget::Widget for DevCaretField {
    crate::testing::inert_widget_methods!(
        apply_layout,
        draw,
        get_rect,
        leaf_layout,
        name = "DevCaretField"
    );
    crate::provides!(dyn crate::undo::UndoPoints);

    fn is_focusable(&self) -> bool {
        true
    }
}

#[cfg(test)]
impl crate::undo::UndoPoints for DevCaretField {
    fn get_state(&self) -> Option<String> {
        Some(self.caret.clone())
    }
    fn set_state(&mut self, state: &str) {
        self.caret = state.to_string();
    }
}

#[cfg(test)]
#[test]
fn dev_reload_replaces_a_baked_translation_in_place() {
    let directory = std::env::temp_dir().join(format!("tilo-dev-catalog-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("en-US.ftl"), "document-heading = After\n").unwrap();
    crate::i18n::I18nManager::install_system("en-US", &[("en-US", "document-heading = Before\n")])
        .expect("baked catalog installs");
    assert_eq!(
        crate::i18n::I18nManager::lookup_plain("document-heading"),
        "Before"
    );
    assert!(reload_dev_locale_catalogs_from(&directory));
    assert_eq!(
        crate::i18n::I18nManager::lookup_plain("document-heading"),
        "After"
    );
    let _ = std::fs::remove_dir_all(&directory);
}

#[cfg(test)]
#[test]
fn dev_checkpoint_restores_document_and_route() {
    use crate::theme::Theme;
    use crate::widget::{Button, RouteHandle, Router};
    use automerge::{ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};

    let directory =
        std::env::temp_dir().join(format!("tilo-dev-checkpoint-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();

    let theme = Theme::default();
    fn screen(
        route: &str,
        _: &Theme,
        _: &RouteHandle,
    ) -> Result<Box<dyn crate::widget::Widget>, String> {
        if route == "settings" {
            Ok(Box::new(DevCaretField {
                caret: String::new(),
            }))
        } else {
            Ok(Box::new(Button::new(route, route)))
        }
    }
    let router = Router::new("home", ["home", "settings"], &theme, screen).unwrap();
    let mut app = App::new(crate::State::new(), Box::new(router));
    app.state
        .doc
        .put(ROOT, "title", "kept")
        .expect("document accepts a title");
    app.root_widget.restore_dev_route(&RouteSnapshot {
        past: vec!["home".into()],
        current: "settings".into(),
        future: Vec::new(),
    });
    let root = app.state.root.clone();
    app.root_widget.sync(&app.state.doc, &root);
    app.autofocus_first = true;
    app.init_layout();
    let focused = app.focused_node.expect("settings caret field is focused");
    app.mutate_node(focused, |widget| crate::undo::set_state(widget, "12"))
        .expect("caret state is writable");
    assert!(write_dev_checkpoint(&app, &directory));

    let restored_router = Router::new("home", ["home", "settings"], &theme, screen).unwrap();
    let restored_handle = restored_router.handle();
    let mut restored = App::new(crate::State::new(), Box::new(restored_router));
    let view = restored
        .restore_dev_checkpoint_from(&directory.join("checkpoint.bin"))
        .expect("checkpoint reloads");
    restored.init_layout_with_dev_view(Some(view));

    assert_eq!(restored_handle.current(), "settings");
    let restored_focus = restored
        .focused_node
        .expect("restored settings field is focused");
    let caret = restored
        .inspect_node(restored_focus, |widget| crate::undo::get_state(widget))
        .expect("restored field is addressable");
    assert_eq!(caret.as_deref(), Some("12"));
    let title = restored
        .state
        .doc
        .get(ROOT, "title")
        .expect("title reads back")
        .expect("title was stored");
    assert!(
        matches!(title.0, Value::Scalar(scalar) if matches!(scalar.as_ref(), ScalarValue::Str(text) if text == "kept"))
    );
    let _ = std::fs::remove_dir_all(&directory);
}
