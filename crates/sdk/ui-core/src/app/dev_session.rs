// Session handoff for `tilo-app dev`.
//
// A Rust edit replaces the process. The document, route, and caret are written
// here before that process exits and read back on the next startup. The file
// lives outside the app tree so the watcher does not treat the write as
// another edit.

use std::path::Path;
#[cfg(feature = "native")]
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
            self.root_widget.sync(&mut self.state.doc, &root);
        }
        Some(frozen.view)
    }

    fn consume_dev_checkpoint(&mut self) -> Option<crate::view::View> {
        let dir = crate::host::environment().dev_session.clone()?;
        let path = dir.join(CHECKPOINT_FILE);
        if !path.is_file() {
            return None;
        }
        match self.restore_dev_checkpoint_from(&path) {
            Some(view) => Some(view),
            None => {
                eprintln!(
                    "tilo: ignoring dev checkpoint {} — it does not match this app",
                    path.display()
                );
                None
            }
        }
    }

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
pub(crate) fn write_dev_checkpoint(app: &App, dir: &Path) -> bool {
    let bytes = app.dev_checkpoint_bytes();
    if bytes.is_empty() {
        return false;
    }
    let destination = dir.join(CHECKPOINT_FILE);
    let temporary = dir.join("checkpoint.bin.partial");
    if std::fs::write(&temporary, &bytes).is_err() {
        return false;
    }
    std::fs::rename(&temporary, &destination).is_ok()
}

#[cfg(feature = "native")]
static DEV_STOP_PIPE: AtomicI32 = AtomicI32::new(-1);
#[cfg(feature = "native")]
static DEV_STOP_INSTALLED: AtomicBool = AtomicBool::new(false);

#[cfg(feature = "native")]
extern "C" fn request_dev_session_stop(_signal: libc::c_int) {
    let fd = DEV_STOP_PIPE.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = [1u8];
        // SAFETY: `write` of one byte to a pipe opened for this handler is
        // async-signal-safe. The UI thread turns that byte into a normal event.
        unsafe {
            libc::write(fd, byte.as_ptr().cast(), 1);
        }
    }
}

/// When `TILO_DEV_SESSION` is set, SIGTERM asks the UI thread to flush and exit
/// instead of dying inside the frame. The dev loop sends that signal before it
/// starts the rebuilt binary.
#[cfg(feature = "native")]
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
                if proxy.send_event(UserEvent::DevSessionStop).is_err() {
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
    }
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
    fn sync(&mut self, _state: &mut automerge::AutoCommit, _path: &automerge::ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &crate::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget>,
    ) -> taffy::NodeId {
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn crate::widget::Widget>(self));
        node
    }

    fn apply_layout(
        &mut self,
        _taffy: &taffy::TaffyTree<()>,
        _node: taffy::NodeId,
        _origin: vello::kurbo::Point,
    ) {
    }

    fn draw(
        &mut self,
        _scene: &mut vello::Scene,
        _transform: vello::kurbo::Affine,
        _text_renderer: &mut crate::text_render::TextRenderer,
        _theme: &crate::Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
    }

    fn name(&self) -> &str {
        "DevCaretField"
    }

    fn get_rect(&self) -> vello::kurbo::Rect {
        vello::kurbo::Rect::ZERO
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn get_state(&self) -> Option<String> {
        Some(self.caret.clone())
    }

    fn set_state(&mut self, state: &str) {
        self.caret = state.to_string();
    }
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
            Ok(Box::new(Button::new(route, route, None)))
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
    app.root_widget.sync(&mut app.state.doc, &root);
    app.autofocus_first = true;
    app.init_layout();
    let focused = app.focused_node.expect("settings caret field is focused");
    app.mutate_node(focused, |widget| widget.set_state("12"))
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
        .inspect_node(restored_focus, |widget| widget.get_state())
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
