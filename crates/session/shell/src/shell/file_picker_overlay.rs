//! Portal file-chooser overlay.
//!
//! This component owns the entire modal lifecycle: queued portal requests,
//! the active picker and its reply channel, security-scoped mounting,
//! painting, and exclusive event routing. The shell overlay host only decides
//! where this modal sits in the global z-order.

use crate::shell::state::WorkspaceAuthority as _;
use taffy::TaffyTree;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::WindowEvent;

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::shell::chrome_state::Chrome;
use crate::shell::overlay_host::{MODAL_SCRIM_ALPHA, fill_scrim};

/// The portal file chooser's complete on-screen state.
#[derive(Default)]
pub(crate) struct FilePickerOverlay {
    queue: Option<crate::portal::FilePickerQueue>,
    active: Option<Box<ActiveFilePicker>>,
}

impl FilePickerOverlay {
    pub(crate) fn set_queue(&mut self, queue: crate::portal::FilePickerQueue) {
        self.queue = Some(queue);
    }

    pub(crate) fn widget(&self) -> Option<&ui::widget::FilePicker> {
        self.active.as_ref().map(|active| &active.widget)
    }

    pub(crate) fn widget_mut(&mut self) -> Option<&mut ui::widget::FilePicker> {
        self.active.as_mut().map(|active| &mut active.widget)
    }

    /// Resolves a completed picker and mounts the next queued request.
    ///
    /// Only one request is allowed on screen. Requests arriving while a
    /// picker is active are cancelled immediately instead of becoming stale,
    /// unattributed modals later in the session.
    pub(crate) fn pump(&mut self, chrome: Option<&Chrome>) -> bool {
        let mut dirty = false;

        if let Some(active) = self.active.as_mut()
            && let Some(outcome) = active.widget.take_outcome()
        {
            // `self.active` was confirmed `Some` just above; `take()` can
            // only observe that same state, but a real fallback keeps this
            // panic-free regardless.
            if let Some(mut active) = self.active.take() {
                let picked = match outcome {
                    ui::widget::FilePickerOutcome::Selected(paths) => Some(paths),
                    ui::widget::FilePickerOutcome::Cancelled => None,
                };
                active.answer(picked);
                dirty = true;
            }
        }

        let Some(queue) = self.queue.as_ref() else {
            return dirty;
        };
        for request in queue.drain() {
            if self.active.is_some() {
                let _ = request.reply.send(None);
                continue;
            }
            let scope = request
                .scope
                .clone()
                .unwrap_or_else(|| picker_scope_for(chrome, &request.caller));
            let active = ActiveFilePicker::mount(request, scope);
            tracing::info!(
                app = %active.app_id,
                dir = %active.widget.current_dir().display(),
                "portal: file dialog opened"
            );
            self.active = Some(Box::new(active));
            dirty = true;
        }
        dirty
    }

    pub(crate) fn draw(
        &mut self,
        viewport: Rect,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        fill_scrim(scene, transform, viewport, MODAL_SCRIM_ALPHA, theme);
        active.widget.set_rect(file_picker_rect(viewport));
        active
            .widget
            .draw(scene, transform, text_renderer, theme, presence);
    }

    /// Routes an event to the active picker and reports whether the modal
    /// consumed the shell's input. An active picker always consumes input,
    /// even when its inner widget has no action for a particular event.
    /// The picker is dispatched on a tree of its own; `ctx.node_id` and
    /// `ctx.taffy` are not forwarded.
    pub(crate) fn handle_event(
        &mut self,
        viewport: Rect,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        active.widget.set_rect(file_picker_rect(viewport));
        let mut taffy = TaffyTree::<()>::new();
        // This tree exists only to hand `handle_event` a valid node id; a
        // failure here means we can't dispatch at all, so decline the
        // event instead of unwrapping into a panic.
        let Ok(node) = taffy.new_leaf(taffy::Style::default()) else {
            return false;
        };
        active.widget.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: ctx.scale_factor,
                modifiers: ctx.modifiers,
                state: &mut *ctx.state,
                path: ctx.path,
                handled: &mut *ctx.handled,
                focus_target: &mut *ctx.focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        *ctx.handled = true;
        true
    }
}

/// Centre the file chooser in `viewport`, clamping its fraction so it stays
/// usable on a small output without walling a large one.
pub fn file_picker_rect(viewport: Rect) -> Rect {
    const MIN_W: f64 = 420.0;
    const MIN_H: f64 = 320.0;
    const MAX_W: f64 = 900.0;
    const MAX_H: f64 = 640.0;
    let w = (viewport.width() * 0.62)
        .clamp(MIN_W, MAX_W)
        .min(viewport.width());
    let h = (viewport.height() * 0.72)
        .clamp(MIN_H, MAX_H)
        .min(viewport.height());
    let x0 = viewport.x0 + (viewport.width() - w) / 2.0;
    let y0 = viewport.y0 + (viewport.height() - h) / 2.0;
    Rect::new(x0, y0, x0 + w, y0 + h)
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "both guards already end at the block that binds them, right where `granted` \
              (the value built from them) is handed back to with_granted_paths; nothing to \
              tighten without cloning the tree/capabilities out for no real gain"
)]
fn picker_scope_for(
    chrome: Option<&Chrome>,
    caller: &crate::portal::PortalCaller,
) -> crate::portal::PickerScope {
    let supplied = chrome.and_then(|chrome| chrome.borrow().hosted_inputs.picker_scope.clone());
    let base = supplied.unwrap_or_else(|| {
        #[cfg(all(target_os = "linux", not(test)))]
        {
            crate::portal::PickerScope::for_user()
        }
        #[cfg(any(not(target_os = "linux"), test))]
        {
            crate::portal::PickerScope {
                initial_dir: std::path::PathBuf::new(),
                roots: Vec::new(),
            }
        }
    });
    let Some(chrome) = chrome else {
        return base;
    };
    // Scoped so the `RefCell` borrow guard drops before
    // `with_granted_paths` runs, rather than lingering to the end of the
    // function for data it no longer needs.
    let granted: Vec<std::path::PathBuf> = {
        let state = chrome.borrow();
        let tree = state.tree();
        let Some(workspace) = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == caller.workspace_id)
        else {
            return base;
        };
        workspace
            .app_capabilities(&caller.app_id)
            .into_iter()
            .filter_map(|capability| match &capability.scope {
                collab_types::caps::Scope::FsPath(path) => Some(std::path::PathBuf::from(path)),
                _ => None,
            })
            .collect()
    };
    base.with_granted_paths(granted)
}

/// An on-screen dialog and the portal request it must answer.
struct ActiveFilePicker {
    widget: ui::widget::FilePicker,
    app_id: crate::shell::state::AppId,
    multiple: bool,
    reply: Option<tokio::sync::oneshot::Sender<Option<Vec<std::path::PathBuf>>>>,
}

impl ActiveFilePicker {
    fn mount(request: crate::portal::FilePickerRequest, scope: crate::portal::PickerScope) -> Self {
        use ui::widget::file_picker::FileFilter;
        use ui::widget::{FilePicker, FilePickerMode};

        let mut filters = request
            .filters
            .iter()
            .filter(|filter| !filter.is_catch_all())
            .map(|filter| {
                let extensions = filter
                    .extensions
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                FileFilter::new(filter.label.clone(), &extensions)
            })
            .collect::<Vec<_>>();
        filters.push(FileFilter::all());

        let mode = if request.save {
            FilePickerMode::Save
        } else if request.directory {
            FilePickerMode::Directory
        } else {
            FilePickerMode::Open
        };
        let mut widget = FilePicker::new(request.title.clone(), &scope.initial_dir, scope.roots)
            .with_mode(mode)
            .with_multiple(request.multiple)
            .with_accept_label(request.accept_label.clone())
            .with_filters(filters);
        if let Some(name) = request.current_name.as_deref() {
            widget = widget.with_filename(name);
        }
        Self {
            widget,
            app_id: request.caller.app_id.clone(),
            multiple: request.multiple,
            reply: Some(request.reply),
        }
    }

    fn answer(&mut self, picked: Option<Vec<std::path::PathBuf>>) {
        let picked = constrain_selection(picked, self.multiple);
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(picked);
        }
    }
}

fn constrain_selection(
    picked: Option<Vec<std::path::PathBuf>>,
    multiple: bool,
) -> Option<Vec<std::path::PathBuf>> {
    picked.map(|mut paths| {
        if !multiple {
            paths.truncate(1);
        }
        paths
    })
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "deterministic overlay geometry fixtures use exactly representable values"
)]
mod tests {
    use super::*;
    use crate::portal::{FilePickerRequest, FilePickerSpawner};
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::WorkspaceTree;
    use tokio::sync::oneshot;

    #[test]
    fn file_picker_is_clamped_and_replies_with_the_requested_kind_and_count() {
        fn picker_rect_is_centered_and_clamped_to_viewport() {
            let viewport = Rect::new(30.0, 40.0, 330.0, 240.0);
            let picker = file_picker_rect(viewport);
            assert_eq!(picker, viewport);

            let viewport = Rect::new(10.0, 20.0, 2010.0, 1220.0);
            let picker = file_picker_rect(viewport);
            assert_eq!(picker.width(), 900.0);
            assert_eq!(picker.height(), 640.0);
            assert_eq!(picker.center(), viewport.center());
        }

        fn single_file_request_cannot_receive_multiple_paths() {
            let paths = vec!["first".into(), "second".into()];
            assert_eq!(constrain_selection(Some(paths), false).unwrap().len(), 1);

            let paths = vec!["first".into(), "second".into()];
            assert_eq!(constrain_selection(Some(paths), true).unwrap().len(), 2);
            assert_eq!(constrain_selection(None, false), None);
        }

        fn directory_request_flows_through_overlay_to_folder_reply() {
            let temp = tempfile::tempdir().expect("tempdir");
            let root = temp.path().to_path_buf();
            let child = root.join("folder");
            std::fs::create_dir(&child).unwrap();
            std::fs::write(root.join("file.txt"), b"file").unwrap();

            let tree = WorkspaceTree::bootstrap_demo();
            let workspace = tree
                .desktop
                .active_workspace()
                .expect("active workspace")
                .id;
            let chrome = ChromeState::new(tree);
            chrome.borrow_mut().hosted_inputs.picker_scope = Some(crate::portal::PickerScope {
                initial_dir: root.clone(),
                roots: vec![root],
            });

            let queue = crate::portal::FilePickerQueue::new();
            let (reply, mut received) = oneshot::channel();
            queue.spawn(FilePickerRequest {
                caller: crate::portal::PortalCaller::new(workspace, "org.example"),
                title: "Choose project".into(),
                save: false,
                directory: true,
                multiple: false,
                accept_label: Some("Attach folder".into()),
                current_name: None,
                filters: Vec::new(),
                scope: None,
                reply,
            });

            let mut overlay = FilePickerOverlay::default();
            overlay.set_queue(queue);
            assert!(overlay.pump(Some(&chrome)));
            let widget = overlay.widget_mut().expect("mounted picker");
            assert_eq!(widget.confirm_label(), "Attach folder");
            assert_eq!(widget.entries().len(), 1, "files must be hidden");
            assert!(widget.select_by_path(&child));
            widget.click_open();
            assert!(overlay.pump(Some(&chrome)));

            assert_eq!(received.try_recv().unwrap(), Some(vec![child]));
        }

        picker_rect_is_centered_and_clamped_to_viewport();
        single_file_request_cannot_receive_multiple_paths();
        directory_request_flows_through_overlay_to_folder_reply();
    }

    #[test]
    fn a_scope_decided_with_the_request_replaces_the_users_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let granted = temp.path().join("in");
        std::fs::create_dir(&granted).unwrap();
        std::fs::write(granted.join("x.xlsx"), b"sheet").unwrap();
        let elsewhere = temp.path().join("home");
        std::fs::create_dir(&elsewhere).unwrap();

        let tree = WorkspaceTree::bootstrap_demo();
        let workspace = tree
            .desktop
            .active_workspace()
            .expect("active workspace")
            .id;
        let chrome = ChromeState::new(tree);
        chrome.borrow_mut().hosted_inputs.picker_scope = Some(crate::portal::PickerScope {
            initial_dir: elsewhere.clone(),
            roots: vec![elsewhere],
        });

        let queue = crate::portal::FilePickerQueue::new();
        let (reply, _received) = oneshot::channel();
        queue.spawn(FilePickerRequest {
            caller: crate::portal::PortalCaller::new(workspace, "org.example"),
            title: "Import".into(),
            save: false,
            directory: false,
            multiple: false,
            accept_label: None,
            current_name: None,
            filters: Vec::new(),
            scope: Some(crate::portal::PickerScope {
                initial_dir: granted.clone(),
                roots: vec![granted.clone()],
            }),
            reply,
        });
        let mut overlay = FilePickerOverlay::default();
        overlay.set_queue(queue);
        assert!(overlay.pump(Some(&chrome)));
        let widget = overlay.widget_mut().expect("mounted picker");
        assert_eq!(widget.current_dir(), granted.as_path());
        assert!(widget.select_by_path(&granted.join("x.xlsx")));
    }
}
